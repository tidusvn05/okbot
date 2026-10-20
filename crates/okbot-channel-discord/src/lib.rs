//! Discord channel for okbot.
//!
//! The bot connects to the Discord gateway (twilight), answers direct messages and messages that
//! mention it or reply to it, and posts with the REST API. Without the privileged *message
//! content* intent Discord still delivers the text of those messages, so the intent is off unless
//! [`DiscordConfig::message_content`] asks for it. Replies never ping anyone: allowed mentions are
//! always empty, whatever the agent writes.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, ConversationKey,
    InboundEvent, InboundSink, LengthUnit, MarkdownDialect, MessageId, RenderedMessage,
    ReplyTarget, Secret, SentRef, UserId, UserRef, async_trait,
};
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client;
use twilight_model::application::interaction::{Interaction, InteractionData};
use twilight_model::channel::Message;
use twilight_model::channel::message::{AllowedMentions, MessageFlags};
use twilight_model::id::Id;
use twilight_model::id::marker::{ApplicationMarker, RoleMarker, UserMarker};

/// Settings of one Discord bot.
#[derive(Debug, Clone)]
pub struct DiscordConfig {
    /// Channel id, `discord:<instance>`.
    pub id: ChannelId,
    /// Bot token.
    pub token: Secret<String>,
    /// Request the privileged message-content intent (needed to answer group messages that do
    /// not mention the bot). Enable it in the Developer Portal first.
    pub message_content: bool,
    /// Route REST calls through this HTTP proxy (tests, or a rate-limit proxy).
    pub http_proxy: Option<String>,
}

/// Permissions okbot needs in a server: View Channels, Send Messages, Read Message History and
/// Send Messages in Threads.
pub const BOT_PERMISSIONS: u64 = (1 << 10) | (1 << 11) | (1 << 16) | (1 << 38);

/// The link that adds the bot with `application_id` to a server, with [`BOT_PERMISSIONS`].
pub fn install_url(application_id: u64) -> String {
    format!(
        "https://discord.com/oauth2/authorize?client_id={application_id}&scope=bot&permissions={BOT_PERMISSIONS}&integration_type=0"
    )
}

/// A Discord bot as an okbot [`Channel`].
pub struct DiscordChannel {
    id: ChannelId,
    token: Secret<String>,
    intents: Intents,
    http: Arc<Client>,
    caps: ChannelCaps,
    me: OnceLock<Id<UserMarker>>,
    app: OnceLock<Id<ApplicationMarker>>,
    /// Prompts shown and not closed, by id (lost on restart: their buttons then say so).
    prompts: Mutex<HashMap<String, okbot_core::Prompt>>,
    /// Commands for the `/` menu, registered when the gateway connects.
    commands: Mutex<Vec<okbot_core::CommandInfo>>,
}

impl std::fmt::Debug for DiscordChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordChannel")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

fn failed(e: impl std::error::Error + Send + Sync + 'static) -> ChannelError {
    let text = e.to_string();
    if text.contains("401") || text.contains("Unauthorized") {
        ChannelError::Auth(text)
    } else if text.contains("429") {
        ChannelError::RateLimited { retry_after: None }
    } else {
        ChannelError::Transient(Box::new(e))
    }
}

fn parse_id<T>(value: &str) -> Result<Id<T>, ChannelError> {
    value
        .parse::<u64>()
        .ok()
        .and_then(Id::new_checked)
        .ok_or_else(|| ChannelError::Failed(format!("not a Discord id: {value}").into()))
}

/// Removes `<@id>` and `<@!id>` mentions of the bot, and `<@&id>` of its roles.
fn strip_mention(text: &str, me: u64, roles: &HashSet<Id<RoleMarker>>) -> String {
    let mut text = text
        .replace(&format!("<@{me}>"), "")
        .replace(&format!("<@!{me}>"), "");
    for role in roles {
        text = text.replace(&format!("<@&{role}>"), "");
    }
    text.trim().to_string()
}

/// The roles Discord manages for the bot itself (one per server, named like the bot), from a
/// guild's role list. Picking the bot in the `@` menu often inserts this role instead of the user.
fn own_roles<'a>(
    roles: impl IntoIterator<Item = &'a twilight_model::guild::Role>,
    me: Id<UserMarker>,
) -> impl Iterator<Item = Id<RoleMarker>> {
    roles
        .into_iter()
        .filter(move |r| r.tags.as_ref().and_then(|t| t.bot_id) == Some(me))
        .map(|r| r.id)
}

/// Converts a gateway message. `None` for the bot's own messages. `roles` are the bot's own
/// managed roles: mentioning one counts as mentioning the bot.
pub(crate) fn to_inbound(
    channel: &ChannelId,
    message: &Message,
    me: Id<UserMarker>,
    roles: &HashSet<Id<RoleMarker>>,
) -> Option<InboundEvent> {
    if message.author.id == me {
        return None;
    }
    let direct = message.guild_id.is_none();
    let mentioned = message.mentions.iter().any(|m| m.id == me)
        || message.mention_roles.iter().any(|r| roles.contains(r))
        || message
            .referenced_message
            .as_ref()
            .is_some_and(|r| r.author.id == me);
    let sent_at = jiff::Timestamp::from_microsecond(message.timestamp.as_micros())
        .unwrap_or_else(|_| jiff::Timestamp::now());
    Some(InboundEvent {
        event_id: message.id.to_string(),
        conversation: ConversationKey {
            channel: channel.clone(),
            space: message.guild_id.map(|g| g.to_string()),
            // Discord threads are channels, so the channel id already separates them.
            room: message.channel_id.to_string(),
            thread: None,
        },
        author: UserRef {
            id: UserId::new(message.author.id.to_string()),
            display_name: message
                .author
                .global_name
                .clone()
                .unwrap_or_else(|| message.author.name.clone()),
            is_bot: message.author.bot,
        },
        text: strip_mention(&message.content, me.get(), roles),
        attachments: message
            .attachments
            .iter()
            .map(|a| {
                if a.content_type
                    .as_deref()
                    .is_some_and(|t| t.starts_with("image/"))
                {
                    okbot_core::Attachment::Image {
                        url: a.url.clone(),
                        caption: None,
                    }
                } else {
                    okbot_core::Attachment::File {
                        url: a.url.clone(),
                        name: Some(a.filename.clone()),
                    }
                }
            })
            .collect(),
        mentions_bot: mentioned,
        is_direct: direct,
        restricted: false,
        message_id: Some(MessageId::new(message.id.to_string())),
        sent_at,
        // Gateway messages carry no locale; interactions do.
        locale: None,
    })
}

impl DiscordChannel {
    /// Creates the channel. Nothing is contacted until [`Channel::run`].
    pub fn new(config: DiscordConfig) -> Self {
        // twilight's TLS needs a process-wide rustls provider; okbot uses ring everywhere.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = Client::builder().token(config.token.expose().clone());
        if let Some(proxy) = config.http_proxy {
            builder = builder.proxy(proxy, true);
        }
        // GUILDS (not privileged) brings each server's roles, to recognise the bot's own role.
        let mut intents = Intents::GUILDS | Intents::GUILD_MESSAGES | Intents::DIRECT_MESSAGES;
        if config.message_content {
            intents |= Intents::MESSAGE_CONTENT;
        }
        Self {
            id: config.id,
            token: config.token,
            intents,
            http: Arc::new(builder.build()),
            caps: ChannelCaps {
                max_message_len: 2000,
                length_unit: LengthUnit::Chars,
                dialect: MarkdownDialect::DiscordMd,
                threads: true,
                edit: true,
                typing: true,
            },
            me: OnceLock::new(),
            app: OnceLock::new(),
            prompts: Mutex::new(HashMap::new()),
            commands: Mutex::new(Vec::new()),
        }
    }

    fn open_prompt(&self, id: &str) -> Option<okbot_core::Prompt> {
        self.prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    async fn respond(
        &self,
        interaction: &Interaction,
        response: &twilight_model::http::interaction::InteractionResponse,
    ) {
        let Some(app) = self.app.get() else { return };
        if let Err(error) = self
            .http
            .interaction(*app)
            .create_response(interaction.id, &interaction.token, response)
            .await
        {
            tracing::warn!(channel = %self.id, %error, "cannot answer a Discord interaction");
        }
    }

    /// A slash command, or a button press or a submitted modal on one of our prompts. `Err` when the runtime stopped.
    async fn interaction(
        &self,
        sink: &Arc<dyn InboundSink>,
        interaction: &Interaction,
    ) -> Result<(), okbot_core::SinkClosed> {
        if let Some(InteractionData::ApplicationCommand(data)) = &interaction.data {
            return self.slash_command(sink, interaction, data).await;
        }
        let user = interaction
            .member
            .as_ref()
            .and_then(|m| m.user.as_ref())
            .or(interaction.user.as_ref());
        let (Some(user), Some(channel_id), Some(message)) = (
            user,
            interaction.channel.as_ref().map(|c| c.id),
            &interaction.message,
        ) else {
            return Ok(());
        };
        let (ids, values) = match &interaction.data {
            Some(InteractionData::MessageComponent(d)) => {
                (prompt::parse_id(&d.custom_id, false), None)
            }
            Some(InteractionData::ModalSubmit(d)) => (
                prompt::parse_id(&d.custom_id, true),
                Some(prompt::values(d)),
            ),
            _ => return Ok(()),
        };
        let Some((prompt_id, button_id)) = ids else {
            return Ok(());
        };
        let Some(open) = self.open_prompt(prompt_id) else {
            self.respond(
                interaction,
                &prompt::private_note("⌛ This prompt has expired."),
            )
            .await;
            return Ok(());
        };
        let author = UserRef {
            id: UserId::new(user.id.to_string()),
            display_name: user
                .global_name
                .clone()
                .unwrap_or_else(|| user.name.clone()),
            is_bot: user.bot,
        };
        if !open.may_answer(&author.id) {
            self.respond(
                interaction,
                &prompt::private_note("⛔ This is not yours to answer."),
            )
            .await;
            return Ok(());
        }
        let Some(button) = open.button(button_id) else {
            return Ok(());
        };
        if let (None, Some(form)) = (&values, &button.form) {
            self.respond(interaction, &prompt::modal(prompt_id, button_id, form))
                .await;
            return Ok(());
        }
        self.respond(interaction, &prompt::deferred()).await;
        let conversation = ConversationKey {
            channel: self.id.clone(),
            space: interaction.guild_id.map(|g| g.to_string()),
            room: channel_id.to_string(),
            thread: None,
        };
        sink.answer(okbot_core::PromptAnswer {
            prompt: prompt_id.to_string(),
            button: button_id.to_string(),
            values: values.unwrap_or_default(),
            user: author,
            conversation: conversation.clone(),
            sent: SentRef {
                target: ReplyTarget {
                    conversation,
                    in_reply_to: None,
                },
                message_id: MessageId::new(message.id.to_string()),
            },
        })
        .await
    }

    /// A chosen slash command: acknowledged at once, then delivered as the text `/name value…`.
    async fn slash_command(
        &self,
        sink: &Arc<dyn InboundSink>,
        interaction: &Interaction,
        data: &twilight_model::application::interaction::application_command::CommandData,
    ) -> Result<(), okbot_core::SinkClosed> {
        let user = interaction
            .member
            .as_ref()
            .and_then(|m| m.user.as_ref())
            .or(interaction.user.as_ref());
        let (Some(user), Some(channel_id)) = (user, interaction.channel.as_ref().map(|c| c.id))
        else {
            return Ok(());
        };
        let text = commands::invocation(data);
        // Discord needs an answer within 3 seconds; the reply itself follows as a message.
        self.respond(
            interaction,
            &twilight_model::http::interaction::InteractionResponse {
                kind: twilight_model::http::interaction::InteractionResponseType::ChannelMessageWithSource,
                data: Some(twilight_model::http::interaction::InteractionResponseData {
                    content: Some(format!("› `{}`", text.replace('`', "'"))),
                    allowed_mentions: Some(AllowedMentions::default()),
                    ..Default::default()
                }),
            },
        )
        .await;
        sink.deliver(InboundEvent {
            event_id: interaction.id.to_string(),
            conversation: ConversationKey {
                channel: self.id.clone(),
                space: interaction.guild_id.map(|g| g.to_string()),
                room: channel_id.to_string(),
                thread: None,
            },
            author: UserRef {
                id: UserId::new(user.id.to_string()),
                display_name: user
                    .global_name
                    .clone()
                    .unwrap_or_else(|| user.name.clone()),
                is_bot: user.bot,
            },
            text,
            attachments: Vec::new(),
            // A slash command is addressed to the bot.
            mentions_bot: true,
            is_direct: interaction.guild_id.is_none(),
            restricted: false,
            message_id: None,
            sent_at: jiff::Timestamp::now(),
            locale: interaction.locale.clone(),
        })
        .await
    }

    /// Replaces the bot's global slash commands with the listed ones.
    async fn register_commands(&self, app: Id<ApplicationMarker>) {
        let list = self
            .commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if list.is_empty() {
            return;
        }
        let definitions = commands::definitions(&list);
        match self
            .http
            .interaction(app)
            .set_global_commands(&definitions)
            .await
        {
            Ok(_) => {
                tracing::info!(channel = %self.id, count = list.len(), "slash commands registered");
            }
            Err(error) => {
                tracing::warn!(channel = %self.id, %error, "cannot register the slash commands");
            }
        }
    }

    /// The bot's application id (its OAuth2 client id), for [`install_url`].
    pub async fn application_id(&self) -> Result<u64, ChannelError> {
        let app = self
            .http
            .current_user_application()
            .await
            .map_err(failed)?
            .model()
            .await
            .map_err(failed)?;
        Ok(app.id.get())
    }

    /// The bot's own user (checks the token).
    pub async fn current_user(&self) -> Result<(Id<UserMarker>, String), ChannelError> {
        let user = self
            .http
            .current_user()
            .await
            .map_err(failed)?
            .model()
            .await
            .map_err(failed)?;
        Ok((user.id, user.name))
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn id(&self) -> &ChannelId {
        &self.id
    }

    fn caps(&self) -> &ChannelCaps {
        &self.caps
    }

    async fn run(
        &self,
        sink: Arc<dyn InboundSink>,
        shutdown: CancellationToken,
    ) -> Result<(), ChannelError> {
        let (me, name) = self.current_user().await?;
        let _ = self.me.set(me);
        let app = Id::new(self.application_id().await?);
        let _ = self.app.set(app);
        self.register_commands(app).await;
        tracing::info!(channel = %self.id, bot = %name, "connecting to the Discord gateway");
        let mut shard = Shard::new(ShardId::ONE, self.token.expose().clone(), self.intents);
        let mut roles = HashSet::new();
        let events = EventTypeFlags::MESSAGE_CREATE
            | EventTypeFlags::READY
            | EventTypeFlags::GUILD_CREATE
            | EventTypeFlags::ROLE_CREATE
            | EventTypeFlags::INTERACTION_CREATE;
        loop {
            let item = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                item = shard.next_event(events) => item,
            };
            let Some(item) = item else { return Ok(()) };
            match item {
                Ok(Event::MessageCreate(message)) => {
                    if let Some(event) = to_inbound(&self.id, &message.0, me, &roles)
                        && sink.deliver(event).await.is_err()
                    {
                        return Ok(());
                    }
                }
                Ok(Event::GuildCreate(guild)) => {
                    if let twilight_model::gateway::payload::incoming::GuildCreate::Available(g) =
                        guild.as_ref()
                    {
                        roles.extend(own_roles(&g.roles, me));
                    }
                }
                Ok(Event::InteractionCreate(interaction)) => {
                    if self.interaction(&sink, &interaction.0).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(Event::RoleCreate(role)) => roles.extend(own_roles([&role.role], me)),
                Ok(Event::Ready(_)) => tracing::info!(channel = %self.id, "Discord gateway ready"),
                Ok(_) => {}
                // twilight reconnects by itself; fatal errors end the stream.
                Err(error) => tracing::warn!(channel = %self.id, %error, "Discord gateway error"),
            }
        }
    }

    async fn set_commands(&self, list: &[okbot_core::CommandInfo]) -> Result<(), ChannelError> {
        *self
            .commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = list.to_vec();
        Ok(())
    }

    async fn send(
        &self,
        to: &ReplyTarget,
        message: &RenderedMessage,
    ) -> Result<SentRef, ChannelError> {
        let channel = parse_id(&to.conversation.room)?;
        let none = AllowedMentions::default();
        let mut request = self
            .http
            .create_message(channel)
            .content(&message.text)
            .allowed_mentions(Some(&none));
        if let Some(reply) = &to.in_reply_to {
            request = request
                .reply(parse_id(reply.as_str())?)
                .fail_if_not_exists(false);
        }
        let sent = request
            .await
            .map_err(failed)?
            .model()
            .await
            .map_err(failed)?;
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(sent.id.to_string()),
        })
    }

    async fn open_direct(&self, user: &UserId) -> Result<ConversationKey, ChannelError> {
        let dm = self
            .http
            .create_private_channel(parse_id(user.as_str())?)
            .await
            .map_err(failed)?
            .model()
            .await
            .map_err(failed)?;
        // The same key as the direct messages that user sends (no guild).
        Ok(ConversationKey::room(self.id.clone(), dm.id.to_string()))
    }

    async fn edit(&self, sent: &SentRef, message: &RenderedMessage) -> Result<(), ChannelError> {
        let channel = parse_id(&sent.target.conversation.room)?;
        let none = AllowedMentions::default();
        self.http
            .update_message(channel, parse_id(sent.message_id.as_str())?)
            .content(Some(&message.text))
            .flags(MessageFlags::SUPPRESS_EMBEDS)
            .allowed_mentions(Some(&none))
            .await
            .map(drop)
            .map_err(failed)
    }

    async fn prompt(
        &self,
        to: &ReplyTarget,
        prompt: &okbot_core::Prompt,
    ) -> Result<SentRef, ChannelError> {
        let channel = parse_id(&to.conversation.room)?;
        let none = AllowedMentions::default();
        let components = prompt::components(prompt);
        let sent = self
            .http
            .create_message(channel)
            .content(&prompt.text)
            .components(&components)
            .allowed_mentions(Some(&none))
            // A prompt asks for a decision: a link preview would push its buttons out of view.
            .flags(MessageFlags::SUPPRESS_EMBEDS)
            .await
            .map_err(failed)?
            .model()
            .await
            .map_err(failed)?;
        self.prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(prompt.id.clone(), prompt.clone());
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(sent.id.to_string()),
        })
    }

    async fn close_prompt(
        &self,
        sent: &SentRef,
        message: &RenderedMessage,
        keep_buttons: bool,
    ) -> Result<(), ChannelError> {
        let channel = parse_id(&sent.target.conversation.room)?;
        let none = AllowedMentions::default();
        let mut request = self
            .http
            .update_message(channel, parse_id(sent.message_id.as_str())?)
            .content(Some(&message.text))
            .flags(MessageFlags::SUPPRESS_EMBEDS)
            .allowed_mentions(Some(&none));
        if !keep_buttons {
            request = request.components(Some(&[]));
        }
        request.await.map(drop).map_err(failed)
    }

    async fn typing(&self, to: &ReplyTarget) -> Result<(), ChannelError> {
        let channel = parse_id(&to.conversation.room)?;
        self.http
            .create_typing_trigger(channel)
            .await
            .map(drop)
            .map_err(failed)
    }
}

mod commands;
mod prompt;

#[cfg(test)]
mod tests;
