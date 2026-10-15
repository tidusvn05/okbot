//! Discord channel for okbot.
//!
//! The bot connects to the Discord gateway (twilight), answers direct messages and messages that
//! mention it or reply to it, and posts with the REST API. Without the privileged *message
//! content* intent Discord still delivers the text of those messages, so the intent is off unless
//! [`DiscordConfig::message_content`] asks for it. Replies never ping anyone: allowed mentions are
//! always empty, whatever the agent writes.

use std::sync::{Arc, OnceLock};

use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, ConversationKey,
    InboundEvent, InboundSink, LengthUnit, MarkdownDialect, MessageId, RenderedMessage,
    ReplyTarget, Secret, SentRef, UserId, UserRef, async_trait,
};
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client;
use twilight_model::channel::Message;
use twilight_model::channel::message::AllowedMentions;
use twilight_model::id::Id;
use twilight_model::id::marker::UserMarker;

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

/// Removes `<@id>` and `<@!id>` mentions of the bot.
fn strip_mention(text: &str, me: u64) -> String {
    text.replace(&format!("<@{me}>"), "")
        .replace(&format!("<@!{me}>"), "")
        .trim()
        .to_string()
}

/// Converts a gateway message. `None` for the bot's own messages.
pub(crate) fn to_inbound(
    channel: &ChannelId,
    message: &Message,
    me: Id<UserMarker>,
) -> Option<InboundEvent> {
    if message.author.id == me {
        return None;
    }
    let direct = message.guild_id.is_none();
    let mentioned = message.mentions.iter().any(|m| m.id == me)
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
        text: strip_mention(&message.content, me.get()),
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
        let mut intents = Intents::GUILD_MESSAGES | Intents::DIRECT_MESSAGES;
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
        tracing::info!(channel = %self.id, bot = %name, "connecting to the Discord gateway");
        let mut shard = Shard::new(ShardId::ONE, self.token.expose().clone(), self.intents);
        loop {
            let item = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                item = shard.next_event(EventTypeFlags::MESSAGE_CREATE | EventTypeFlags::READY) => item,
            };
            let Some(item) = item else { return Ok(()) };
            match item {
                Ok(Event::MessageCreate(message)) => {
                    if let Some(event) = to_inbound(&self.id, &message.0, me)
                        && sink.deliver(event).await.is_err()
                    {
                        return Ok(());
                    }
                }
                Ok(Event::Ready(_)) => tracing::info!(channel = %self.id, "Discord gateway ready"),
                Ok(_) => {}
                // twilight reconnects by itself; fatal errors end the stream.
                Err(error) => tracing::warn!(channel = %self.id, %error, "Discord gateway error"),
            }
        }
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

    async fn edit(&self, sent: &SentRef, message: &RenderedMessage) -> Result<(), ChannelError> {
        let channel = parse_id(&sent.target.conversation.room)?;
        let none = AllowedMentions::default();
        self.http
            .update_message(channel, parse_id(sent.message_id.as_str())?)
            .content(Some(&message.text))
            .allowed_mentions(Some(&none))
            .await
            .map(drop)
            .map_err(failed)
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

#[cfg(test)]
mod tests;
