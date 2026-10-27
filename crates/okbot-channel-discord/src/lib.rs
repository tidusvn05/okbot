//! Discord channel for okbot.
//!
//! The bot connects to the Discord gateway (twilight), answers direct messages and messages that
//! mention it or reply to it, and posts with the REST API. Without the privileged *message
//! content* intent Discord still delivers the text of those messages, so the intent is off unless
//! [`DiscordConfig::message_content`] asks for it. Replies never ping anyone: allowed mentions are
//! always empty, whatever the agent writes.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

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
use twilight_model::http::attachment::Attachment;
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

/// Permissions okbot needs in a server: View Channels, Send Messages, Attach Files (chart images,
/// ADR 0016), Read Message History and Send Messages in Threads.
pub const BOT_PERMISSIONS: u64 = (1 << 10) | (1 << 11) | (1 << 15) | (1 << 16) | (1 << 38);

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
    /// Prompts shown and not closed, by id (lost on restart: clicks then ask the runtime, and
    /// say the prompt expired when it does not know it either).
    prompts: Mutex<HashMap<String, (Instant, okbot_core::Prompt)>>,
    /// Prompt messages sent with Components V2 (cards), by message id, with their prompt id.
    /// Such a message cannot go back to plain content.
    v2: Mutex<HashMap<String, (Instant, String)>>,
    /// Commands for the `/` menu, registered when the gateway connects.
    commands: Mutex<Vec<okbot_core::CommandInfo>>,
}

/// How long this process remembers a prompt: as long as the runtime keeps its view (ADR 0015).
const REMEMBER: Duration = Duration::from_secs(7 * 24 * 3600);

/// Drops what was remembered longer than [`REMEMBER`] ago.
fn forget_old<V>(map: &mut HashMap<String, (Instant, V)>) {
    map.retain(|_, (at, _)| at.elapsed() < REMEMBER);
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

/// Discord refused the request as malformed (for example plain content on a V2 message).
fn bad_request(e: &twilight_http::Error) -> bool {
    matches!(e.kind(), twilight_http::error::ErrorType::Response { status, .. } if status.get() == 400)
}

/// Whether a failed edit was refused as malformed, so a V2 edit may still succeed.
fn refused(e: &ChannelError) -> bool {
    matches!(e, ChannelError::Failed(source)
        if source.downcast_ref::<twilight_http::Error>().is_some_and(bad_request))
}

/// The picture of a prompt's cards (at most one carries an image).
fn card_image(prompt: &okbot_core::Prompt) -> Option<&okbot_core::Image> {
    prompt.cards.iter().find_map(|c| c.image.as_ref())
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
                ui: okbot_core::UiCaps::all(),
            },
            me: OnceLock::new(),
            app: OnceLock::new(),
            prompts: Mutex::new(HashMap::new()),
            v2: Mutex::new(HashMap::new()),
            commands: Mutex::new(Vec::new()),
        }
    }

    fn open_prompt(&self, id: &str) -> Option<okbot_core::Prompt> {
        self.prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .map(|(_, p)| p.clone())
    }

    /// The open prompt `id`, from memory or, when it was shown in `conversation` before a
    /// restart, from the runtime (which may still wait on it).
    async fn known_prompt(
        &self,
        sink: &Arc<dyn InboundSink>,
        id: &str,
        conversation: &ConversationKey,
    ) -> Option<okbot_core::Prompt> {
        if let Some(open) = self.open_prompt(id) {
            return Some(open);
        }
        let recovered = sink.prompt(id, conversation).await.filter(|p| p.id == id)?;
        self.remember(&recovered);
        Some(recovered)
    }

    /// Keeps `prompt` open (it can be answered) or forgets it.
    fn remember(&self, prompt: &okbot_core::Prompt) {
        let mut prompts = self
            .prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if prompt.is_answerable() {
            forget_old(&mut prompts);
            let mut kept = prompt.clone();
            // An empty picture keeps the one the message has: no need to hold the PNG.
            for card in &mut kept.cards {
                if let Some(image) = &mut card.image {
                    image.png = Vec::new();
                }
            }
            prompts.insert(prompt.id.clone(), (Instant::now(), kept));
        } else {
            prompts.remove(&prompt.id);
        }
    }

    fn mark_v2(&self, message: &MessageId, prompt: &str) {
        let mut v2 = self
            .v2
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        forget_old(&mut v2);
        v2.insert(
            message.as_str().to_string(),
            (Instant::now(), prompt.to_string()),
        );
    }

    /// The prompt id of a V2 message this process sent or edited.
    fn v2_prompt(&self, message: &MessageId) -> Option<String> {
        self.v2
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(message.as_str())
            .map(|(_, id)| id.clone())
    }

    /// Replaces a prompt message with Components V2 `components` (no content, no embeds).
    ///
    /// Without `upload` the message keeps its attachments: twilight then leaves `attachments`
    /// out of the request, and Discord's *Edit Message* documentation says omitted fields are
    /// unchanged. With `upload` those files become the message's only attachments (in API v10
    /// the `attachments` array lists every file the message keeps).
    async fn edit_v2(
        &self,
        sent: &SentRef,
        components: &[twilight_model::channel::message::Component],
        upload: Option<&[Attachment]>,
    ) -> Result<(), ChannelError> {
        let channel = parse_id(&sent.target.conversation.room)?;
        let none = AllowedMentions::default();
        let mut request = self
            .http
            .update_message(channel, parse_id(sent.message_id.as_str())?)
            .content(None)
            .embeds(None)
            .components(Some(components))
            .flags(MessageFlags::IS_COMPONENTS_V2 | MessageFlags::SUPPRESS_EMBEDS)
            .allowed_mentions(Some(&none));
        if let Some(upload) = upload {
            request = request.attachments(upload);
        }
        match request.await {
            Ok(_) => Ok(()),
            Err(e) if bad_request(&e) => Err(ChannelError::Failed(Box::new(e))),
            Err(e) => Err(failed(e)),
        }
    }

    /// Edits a prompt message to show `prompt` with cards (Components V2), keeping, replacing or
    /// dropping its picture.
    ///
    /// - An image with PNG data is uploaded with the edit and replaces the old file.
    /// - An image without data (a prompt read back from the store) means "keep the picture the
    ///   message was sent with". The message is read once to find that file, and the gallery
    ///   refers to it as `attachment://<file name>` while the edit leaves the attachments alone.
    ///   Verified from Discord's docs: omitted `attachments` keep the existing files, and V2
    ///   messages show files only through components. Assumed (not stated by the docs for
    ///   edits): an `attachment://` reference resolves against the files the message keeps.
    ///   If Discord refuses that edit, it is sent again without the gallery, so a click never
    ///   fails because of the picture.
    async fn update_v2(
        &self,
        sent: &SentRef,
        prompt: &okbot_core::Prompt,
    ) -> Result<(), ChannelError> {
        let Some(image) = card_image(prompt) else {
            return self
                .edit_v2(sent, &prompt::components_v2(prompt, None), None)
                .await;
        };
        if !image.png.is_empty() {
            let file = prompt::file_name(image);
            let upload = [Attachment::from_bytes(file.clone(), image.png.clone(), 0)];
            return self
                .edit_v2(
                    sent,
                    &prompt::components_v2(prompt, Some(&file)),
                    Some(&upload),
                )
                .await;
        }
        let Some(file) = self.kept_picture(sent, image).await else {
            return self
                .edit_v2(sent, &prompt::components_v2(prompt, None), None)
                .await;
        };
        match self
            .edit_v2(sent, &prompt::components_v2(prompt, Some(&file)), None)
            .await
        {
            Err(e) if refused(&e) => {
                tracing::warn!(channel = %self.id, "Discord refused the kept chart image; updating without it");
                self.edit_v2(sent, &prompt::components_v2(prompt, None), None)
                    .await
            }
            other => other,
        }
    }

    /// The file name of the picture a prompt message was sent with: the attachment named like
    /// the image, else its only attachment, else the file a media gallery of the message shows
    /// (Discord lists no `attachments` for Components V2 messages; checked live on 2026-10-27).
    /// `None` when the message has no such file. When the message cannot be read, the image's own
    /// name is tried (the edit falls back without it).
    async fn kept_picture(&self, sent: &SentRef, image: &okbot_core::Image) -> Option<String> {
        let wanted = prompt::file_name(image);
        let read = async {
            let channel = parse_id(&sent.target.conversation.room)?;
            let message = parse_id(sent.message_id.as_str())?;
            self.http
                .message(channel, message)
                .await
                .map_err(failed)?
                .model()
                .await
                .map_err(failed)
        };
        match read.await {
            Ok(message) => {
                let files = &message.attachments;
                files
                    .iter()
                    .find(|a| a.filename == wanted)
                    .or(match files.as_slice() {
                        [only] => Some(only),
                        _ => None,
                    })
                    .map(|a| a.filename.clone())
                    .or_else(|| gallery_file(&message.components))
            }
            Err(error) => {
                tracing::debug!(channel = %self.id, %error, "cannot read a prompt message");
                Some(wanted)
            }
        }
    }

    /// Replaces a classic prompt message's content and, unless `components` is `None`, its
    /// components (`Some(&[])` removes them).
    async fn edit_classic(
        &self,
        sent: &SentRef,
        text: &str,
        components: Option<&[twilight_model::channel::message::Component]>,
    ) -> Result<(), ChannelError> {
        let channel = parse_id(&sent.target.conversation.room)?;
        let none = AllowedMentions::default();
        let mut request = self
            .http
            .update_message(channel, parse_id(sent.message_id.as_str())?)
            .content(Some(text))
            .flags(MessageFlags::SUPPRESS_EMBEDS)
            .allowed_mentions(Some(&none));
        if let Some(components) = components {
            request = request.components(Some(components));
        }
        match request.await {
            Ok(_) => Ok(()),
            Err(e) if bad_request(&e) => Err(ChannelError::Failed(Box::new(e))),
            Err(e) => Err(failed(e)),
        }
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
            Some(InteractionData::MessageComponent(d)) => match prompt::parse_menu_id(&d.custom_id)
            {
                // The picked option is the answer's button.
                Some(prompt_id) => (
                    d.values.first().map(|option| (prompt_id, option.as_str())),
                    None,
                ),
                None => (prompt::parse_id(&d.custom_id, false), None),
            },
            Some(InteractionData::ModalSubmit(d)) => (
                prompt::parse_id(&d.custom_id, true),
                Some(prompt::values(d)),
            ),
            _ => return Ok(()),
        };
        let Some((prompt_id, button_id)) = ids else {
            return Ok(());
        };
        let conversation = ConversationKey {
            channel: self.id.clone(),
            space: interaction.guild_id.map(|g| g.to_string()),
            room: channel_id.to_string(),
            thread: None,
        };
        let Some(open) = self.known_prompt(sink, prompt_id, &conversation).await else {
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
        // Link buttons send nothing; anything else unknown is stale.
        if !open.answers_with(button_id) {
            self.respond(interaction, &prompt::deferred()).await;
            return Ok(());
        }
        if let (None, Some(form)) = (
            &values,
            open.button(button_id).and_then(|b| b.form.as_ref()),
        ) {
            self.respond(interaction, &prompt::modal(prompt_id, button_id, form))
                .await;
            return Ok(());
        }
        self.respond(interaction, &prompt::deferred()).await;
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
        let cards = !prompt.cards.is_empty();
        let request = self
            .http
            .create_message(channel)
            .allowed_mentions(Some(&none));
        let sent = if cards {
            // A card's picture goes out as a file the card's media gallery shows.
            let image = card_image(prompt).filter(|i| !i.png.is_empty());
            let file = image.map(prompt::file_name);
            let upload: Vec<Attachment> = image
                .zip(file.clone())
                .map(|(i, f)| Attachment::from_bytes(f, i.png.clone(), 0))
                .into_iter()
                .collect();
            let components = prompt::components_v2(prompt, file.as_deref());
            // The flag first: twilight validates the components by it.
            let mut request = request
                .flags(MessageFlags::IS_COMPONENTS_V2 | MessageFlags::SUPPRESS_EMBEDS)
                .components(&components);
            if !upload.is_empty() {
                // A failed upload fails the prompt: the runtime then sends the view as text.
                request = request.attachments(&upload);
            }
            request.await
        } else {
            let components = prompt::components(prompt);
            request
                .content(&prompt.text)
                .components(&components)
                // A prompt asks for a decision: a link preview would push its buttons out of view.
                .flags(MessageFlags::SUPPRESS_EMBEDS)
                .await
        }
        .map_err(failed)?
        .model()
        .await
        .map_err(failed)?;
        let message_id = MessageId::new(sent.id.to_string());
        if cards {
            self.mark_v2(&message_id, &prompt.id);
        }
        self.remember(prompt);
        Ok(SentRef {
            target: to.clone(),
            message_id,
        })
    }

    async fn close_prompt(
        &self,
        sent: &SentRef,
        message: &RenderedMessage,
        keep_buttons: bool,
    ) -> Result<(), ChannelError> {
        let v2_prompt = self.v2_prompt(&sent.message_id);
        let v2 = |prompt_id: Option<&str>| {
            let mut components = prompt::text_v2(&message.text);
            if keep_buttons && let Some(open) = prompt_id.and_then(|id| self.open_prompt(id)) {
                components.extend(prompt::components(&open));
            }
            components
        };
        if v2_prompt.is_some() {
            return self.edit_v2(sent, &v2(v2_prompt.as_deref()), None).await;
        }
        let rows: Option<&[_]> = if keep_buttons { None } else { Some(&[]) };
        match self.edit_classic(sent, &message.text, rows).await {
            // Sent with cards before a restart: it only takes V2 components.
            Err(e) if refused(&e) => self.edit_v2(sent, &v2(None), None).await,
            other => other,
        }
    }

    async fn update_prompt(
        &self,
        sent: &SentRef,
        prompt: &okbot_core::Prompt,
    ) -> Result<(), ChannelError> {
        let v2 = !prompt.cards.is_empty() || self.v2_prompt(&sent.message_id).is_some();
        let result = if v2 {
            self.update_v2(sent, prompt).await
        } else {
            let rows = prompt::components(prompt);
            match self.edit_classic(sent, &prompt.text, Some(&rows)).await {
                // Sent with cards before a restart.
                Err(e) if refused(&e) => self.update_v2(sent, prompt).await,
                other => other,
            }
        };
        result?;
        if v2 {
            self.mark_v2(&sent.message_id, &prompt.id);
        }
        self.remember(prompt);
        Ok(())
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

/// The file name a media gallery among `components` shows (the last path segment of its first
/// item's URL), searching containers and sections.
fn gallery_file(components: &[twilight_model::channel::message::Component]) -> Option<String> {
    find_gallery_file(&serde_json::to_value(components).ok()?)
}

fn find_gallery_file(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Array(items) => items.iter().find_map(find_gallery_file),
        serde_json::Value::Object(o) => {
            if o.get("type").and_then(serde_json::Value::as_u64) == Some(12) {
                let url = o.get("items")?.get(0)?.get("media")?.get("url")?.as_str()?;
                let path = url.split(['?', '#']).next()?;
                let name = path.rsplit('/').next()?;
                return (!name.is_empty()).then(|| name.to_string());
            }
            o.get("components").and_then(find_gallery_file)
        }
        _ => None,
    }
}

mod commands;
mod prompt;

#[cfg(test)]
mod tests;
