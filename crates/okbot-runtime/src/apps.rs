//! Apps (ADR 0019): launch tickets, sessions, the access checks every App API call repeats,
//! the Open buttons under list commands, and `/app`.
//!
//! Identity always comes from the chat platform: a ticket is minted for the person a platform
//! reported, and a session only says who signed in and from where. What they may do is decided
//! again at every call, from the current lists.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use jiff::{SignedDuration, Timestamp};
use okbot_core::{
    AppDirectory, AppInfo, AppPermission, ConversationKey, InboundEvent, PrivateReply,
    PromptAnswer, PromptButton, UserRef,
};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::Shared;
use crate::ui::{AppButton, View};

/// How long a launch link works.
pub(crate) const TICKET_TTL: Duration = Duration::from_secs(5 * 60);
/// The session cookie.
pub(crate) const COOKIE: &str = "okbot_app";
/// App API calls one session may make per minute.
const CALLS_PER_MINUTE: usize = 240;
/// Items a list command shows before pointing at the app.
pub(crate) const SUMMARY_ITEMS: usize = 10;

/// The apps of a running bot.
pub(crate) struct Apps {
    pub(crate) directory: Arc<dyn AppDirectory>,
    /// `http://<loopback listen>` once the server is up: where the console opens apps.
    pub(crate) loopback: OnceLock<String>,
    /// The shell and app files (`okbot-apps`), until the HTTP server takes them.
    pub(crate) routes: Mutex<Option<axum::Router>>,
    calls: Mutex<HashMap<String, VecDeque<Timestamp>>>,
}

impl Apps {
    pub(crate) fn new(directory: Arc<dyn AppDirectory>, routes: axum::Router) -> Self {
        Self {
            directory,
            loopback: OnceLock::new(),
            routes: Mutex::new(Some(routes)),
            calls: Mutex::new(HashMap::new()),
        }
    }

    /// Counts a call of session `handle`; `false` past the limit.
    pub(crate) fn count_call(&self, handle: &str, now: Timestamp) -> bool {
        let mut calls = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let minute = SignedDuration::from_secs(60);
        calls.retain(|_, times| {
            times
                .back()
                .is_some_and(|t| now.duration_since(*t) < minute)
        });
        let times = calls.entry(handle.to_string()).or_default();
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= minute)
        {
            times.pop_front();
        }
        if times.len() >= CALLS_PER_MINUTE {
            return false;
        }
        times.push_back(now);
        true
    }
}

/// A launch ticket's state, kept under the hash of its secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Ticket {
    pub(crate) user: UserRef,
    pub(crate) conversation: ConversationKey,
    pub(crate) direct: bool,
    pub(crate) app: String,
    pub(crate) view: String,
    #[serde(default)]
    pub(crate) params: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) locale: Option<String>,
}

/// Where a session was proven: a direct chat or a room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Context {
    pub(crate) conversation: ConversationKey,
    pub(crate) direct: bool,
}

impl Context {
    /// A short name for the shell's switcher.
    pub(crate) fn label(&self) -> String {
        if self.direct {
            return "Direct chat".into();
        }
        let room = &self.conversation.room;
        let short: String = room
            .chars()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let dots = if room.chars().count() > 8 { "…" } else { "" };
        format!("{} · {dots}{short}", self.conversation.channel)
    }
}

/// A signed-in browser: who and from where, never what they may do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Session {
    pub(crate) user: UserRef,
    pub(crate) contexts: Vec<Context>,
    pub(crate) csrf: String,
    pub(crate) created: Timestamp,
    pub(crate) touched: Timestamp,
    #[serde(default)]
    pub(crate) locale: Option<String>,
}

impl Session {
    /// The account it belongs to.
    pub(crate) fn channel(&self) -> Option<&okbot_core::ChannelId> {
        self.contexts.first().map(|c| &c.conversation.channel)
    }
}

/// A random secret, base64url without padding.
pub(crate) fn secret() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

/// The handle a secret is stored under.
pub(crate) fn hash(secret: &str) -> String {
    hex::encode(sha2::Sha256::digest(secret.as_bytes()))
}

fn signed(d: Duration) -> SignedDuration {
    SignedDuration::try_from(d).unwrap_or(SignedDuration::MAX)
}

/// Where people's browsers reach the bot, from a conversation on `channel`: the public URL, or
/// the loopback listener for the console. `None`: apps cannot be opened from there.
pub(crate) fn base(shared: &Shared, channel: &okbot_core::ChannelId) -> Option<String> {
    let apps = shared.apps.as_ref()?;
    if !shared.config.apps.enabled {
        return None;
    }
    if let Some(url) = &shared.config.apps.public_url {
        return Some(url.clone());
    }
    (channel.platform() == "console")
        .then(|| apps.loopback.get().cloned())
        .flatten()
}

/// What someone may do in an app, from one place, now.
pub(crate) struct Admitted {
    /// As if they wrote there: for scopes, roles and the person.
    pub(crate) event: InboundEvent,
    pub(crate) info: AppInfo,
    /// The manifest's permissions, less those `[apps.<name>] permissions` leaves out.
    pub(crate) granted: Vec<AppPermission>,
}

/// A message they could have written in `conversation`: what scopes and roles are built from.
pub(crate) fn event_for(
    user: &UserRef,
    conversation: &ConversationKey,
    direct: bool,
    now: Timestamp,
) -> InboundEvent {
    InboundEvent {
        event_id: format!("app:{}", hex::encode(rand::random::<[u8; 8]>())),
        conversation: conversation.clone(),
        author: user.clone(),
        text: String::new(),
        attachments: Vec::new(),
        mentions_bot: true,
        is_direct: direct,
        restricted: false,
        message_id: None,
        sent_at: now,
        locale: None,
    }
}

/// Whether `user` may open `app` from `conversation` now; the error is a line for them.
pub(crate) async fn admit(
    shared: &Shared,
    user: &UserRef,
    conversation: &ConversationKey,
    direct: bool,
    app: &str,
) -> Result<Admitted, String> {
    let Some(apps) = shared.apps.as_ref().filter(|_| shared.config.apps.enabled) else {
        return Err("Apps are off for this assistant.".into());
    };
    let rule = shared
        .config
        .apps
        .rules
        .get(app)
        .cloned()
        .unwrap_or_default();
    let Some(info) = apps.directory.app(app).filter(|_| !rule.disabled) else {
        return Err(format!("There is no app `{app}` here. `/app` lists them."));
    };
    // An app built on something this bot does not keep has nothing to show.
    if let Some(missing) = info.permissions.iter().find_map(|p| missing(shared, p)) {
        return Err(format!("{missing} are not enabled for this bot."));
    }
    let event = event_for(user, conversation, direct, shared.clock.now());
    if crate::team::is_peer(&event) || !shared.gate.reaches(&event) {
        return Err("You cannot use this assistant here.".into());
    }
    if shared.config.personal && shared.person_of(&event).await.is_none() {
        return Err(shared.config.replies.private.clone());
    }
    let title = info.title(None).to_string();
    if !direct
        && !rule.rooms.is_empty()
        && !rule
            .rooms
            .contains(&(conversation.channel.clone(), conversation.room.clone()))
    {
        return Err(format!("{title} cannot be opened from this room."));
    }
    // Automations act for the whole bot: their app is for administrators (ADR 0013).
    if info.permissions.contains(&AppPermission::AutomationsRead) && !shared.is_admin(&event).await
    {
        return Err(format!(
            "{title} is for the administrators of this assistant."
        ));
    }
    if !rule.roles.is_empty() {
        let roles = shared.roles_of(&event).await;
        if !rule.roles.iter().any(|r| roles.contains(r)) {
            return Err(format!("You may not open {title}."));
        }
    }
    let granted = info
        .permissions
        .iter()
        .filter(|p| {
            rule.permissions
                .as_ref()
                .is_none_or(|only| only.contains(p))
        })
        .cloned()
        .collect();
    Ok(Admitted {
        event,
        info,
        granted,
    })
}

/// What `permission` reads or changes, when this bot does not keep it.
fn missing(shared: &Shared, permission: &AppPermission) -> Option<&'static str> {
    match permission {
        AppPermission::TasksRead | AppPermission::TasksWrite if shared.tasks.is_none() => {
            Some("Tasks")
        }
        AppPermission::MemoryRead | AppPermission::MemoryWrite | AppPermission::NotesWrite
            if shared.memory.is_none() =>
        {
            Some("Memory and notes")
        }
        AppPermission::NotesRead | AppPermission::KnowledgeRead if shared.knowledge.is_none() => {
            Some("The knowledge base and notes")
        }
        _ => None,
    }
}

/// Checks and completes the app buttons of `view`, made for `event`'s conversation: each app is
/// installed and on here, its view and params are declared, and its label defaults to
/// "Open <title>". Who may open it is checked at each press.
pub(crate) fn check_view(
    shared: &Shared,
    event: &InboundEvent,
    view: &mut View,
) -> Result<(), String> {
    check(
        shared,
        Some(&event.conversation.channel),
        event.locale.as_deref(),
        view,
    )
}

/// [`check_view`] for a notification, which may go to several chats: it needs the public
/// address.
pub(crate) fn check_apps(shared: &Shared, view: &mut View) -> Result<(), String> {
    check(shared, None, None, view)
}

fn check(
    shared: &Shared,
    channel: Option<&okbot_core::ChannelId>,
    locale: Option<&str>,
    view: &mut View,
) -> Result<(), String> {
    let buttons = view.apps_mut();
    if buttons.is_empty() {
        return Ok(());
    }
    let Some(apps) = shared.apps.as_ref().filter(|_| shared.config.apps.enabled) else {
        return Err("apps are off for this bot: leave `apps` out".into());
    };
    let reachable = match channel {
        Some(channel) => base(shared, channel).is_some(),
        None => shared.config.apps.public_url.is_some(),
    };
    if !reachable {
        return Err("apps cannot be opened from here (no public address): leave `apps` out".into());
    }
    for b in buttons.iter_mut() {
        let rule = shared.config.apps.rules.get(&b.app);
        let info = apps
            .directory
            .app(&b.app)
            .filter(|_| !rule.is_some_and(|r| r.disabled))
            .filter(|i| !i.permissions.iter().any(|p| missing(shared, p).is_some()));
        let Some(info) = info else {
            let names: Vec<String> = apps.directory.apps().into_iter().map(|a| a.name).collect();
            return Err(format!(
                "no app {:?} here; the apps are: {}",
                b.app,
                names.join(", ")
            ));
        };
        if b.view.is_empty() {
            b.view = info.default_view().to_string();
        }
        let Some(declared) = info.view(&b.view) else {
            let views: Vec<&str> = info.views.iter().map(|v| v.path.as_str()).collect();
            return Err(format!(
                "app {:?} has no view {:?}; its views: {}",
                b.app,
                b.view,
                views.join(", ")
            ));
        };
        if let Some(k) = b.params.keys().find(|k| !declared.params.contains(k)) {
            return Err(format!(
                "view {} of {:?} takes no param {k:?} (it takes: {})",
                b.view,
                b.app,
                declared.params.join(", ")
            ));
        }
        if b.label.trim().is_empty() {
            b.label = format!("Open {}", info.title(locale));
        }
    }
    Ok(())
}

/// The apps an agent's views may open in `event`'s conversation, as a line for its prompt;
/// `None` when it shows no views.
pub(crate) async fn prompt_note(
    shared: &Shared,
    event: &InboundEvent,
    views: bool,
) -> Option<String> {
    let apps = shared.apps.as_ref().filter(|_| views)?;
    base(shared, &event.conversation.channel)?;
    let mut lines = Vec::new();
    for info in apps.directory.apps() {
        if admit(
            shared,
            &event.author,
            &event.conversation,
            event.is_direct,
            &info.name,
        )
        .await
        .is_err()
        {
            continue;
        }
        let views: Vec<String> = info
            .views
            .iter()
            .map(|v| {
                if v.params.is_empty() {
                    v.path.clone()
                } else {
                    format!("{} ({})", v.path, v.params.join(", "))
                }
            })
            .collect();
        lines.push(format!(
            "- {}: {} — views {}",
            info.name,
            info.title(None),
            views.join(", ")
        ));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Apps: pages with everything at once, which each person opens signed in as themselves. \
         When a list or record is longer than a chat message should be, summarise it and add \
         an `apps` button to `show` instead of listing everything:\n{}",
        lines.join("\n")
    ))
}

/// A link that signs `user` in to `app`, valid once for [`TICKET_TTL`]; the error is a line for
/// them.
pub(crate) async fn mint(shared: &Shared, ticket: Ticket) -> Result<String, String> {
    let Some(base) = base(shared, &ticket.conversation.channel) else {
        return Err(
            "Apps need a public address for this bot ([http] public_url); ask its operator.".into(),
        );
    };
    let admitted = admit(
        shared,
        &ticket.user,
        &ticket.conversation,
        ticket.direct,
        &ticket.app,
    )
    .await?;
    let mut ticket = ticket;
    if admitted.info.view(&ticket.view).is_none() {
        ticket.view = admitted.info.default_view().to_string();
    }
    let allowed: Vec<String> = admitted
        .info
        .view(&ticket.view)
        .map(|v| v.params.clone())
        .unwrap_or_default();
    ticket
        .params
        .retain(|k, v| allowed.contains(k) && v.chars().count() <= 100);
    let secret = secret();
    let data = serde_json::to_string(&ticket).map_err(|e| e.to_string())?;
    let expires = shared.clock.now() + signed(TICKET_TTL);
    if let Err(error) = shared
        .store
        .put_app_ticket(&hash(&secret), data, expires)
        .await
    {
        tracing::error!(%error, "cannot keep an app ticket");
        return Err(shared.config.replies.error.clone());
    }
    tracing::info!(app = %ticket.app, user = %ticket.user.id, direct = ticket.direct, "app link issued");
    Ok(format!("{base}/apps/#t={secret}"))
}

/// A press on an app button: the presser's own link, privately (ADR 0019).
pub(crate) async fn launch(shared: Arc<Shared>, press: PromptAnswer) -> Option<PrivateReply> {
    let (button, conversation, direct) = crate::ui::launch_target(&shared, &press).await?;
    // The checks a typed message gets, without counting the press.
    let event = event_for(&press.user, &conversation, direct, shared.clock.now());
    if !shared.gate.admits(&event, shared.clock.now()) {
        return Some(PrivateReply {
            text: "Not now: try again in a moment.".into(),
            link: None,
        });
    }
    let ticket = Ticket {
        user: press.user.clone(),
        conversation,
        direct,
        app: button.app.clone(),
        view: button.view.clone(),
        params: button.params.clone(),
        locale: None,
    };
    Some(match mint(&shared, ticket).await {
        Ok(url) => {
            let title = shared
                .apps
                .as_ref()
                .and_then(|a| a.directory.app(&button.app))
                .map_or_else(|| button.app.clone(), |a| a.title(None).to_string());
            PrivateReply {
                text: format!(
                    "{title}: your own link, for you only. It works once, within {} minutes.",
                    TICKET_TTL.as_secs() / 60
                ),
                link: Some(PromptButton {
                    id: "open".into(),
                    label: format!("Open {title} ↗"),
                    style: okbot_core::ButtonStyle::Primary,
                    form: None,
                    url: Some(url),
                    launch: false,
                }),
            }
        }
        Err(reason) => PrivateReply {
            text: reason,
            link: None,
        },
    })
}

/// Offers `app` under a reply in `event`'s conversation: a button that answers each presser
/// privately where the platform can, else a link in a direct chat, else how to get one.
/// `quiet`: say nothing when the app cannot be offered here.
pub(crate) async fn offer(
    shared: &Shared,
    event: &InboundEvent,
    app: &str,
    view: Option<&str>,
    quiet: bool,
) {
    let Some(channel) = shared.channels.get(&event.conversation.channel) else {
        return;
    };
    if base(shared, &event.conversation.channel).is_none() {
        if !quiet {
            crate::turn::reply(
                shared,
                event,
                "Apps need a public address for this bot ([http] public_url); ask its operator.",
            )
            .await;
        }
        return;
    }
    let admitted = match admit(
        shared,
        &event.author,
        &event.conversation,
        event.is_direct,
        app,
    )
    .await
    {
        Ok(a) => a,
        Err(reason) => {
            if !quiet {
                crate::turn::reply(shared, event, &reason).await;
            }
            return;
        }
    };
    let title = admitted.info.title(event.locale.as_deref()).to_string();
    let view = view
        .filter(|v| admitted.info.view(v).is_some())
        .unwrap_or(admitted.info.default_view())
        .to_string();
    let to = crate::dispatch::target(event);
    let caps = channel.caps();
    let shown = if caps.ui.buttons && caps.ui.private_reply {
        View::app(
            format!("-# {title}: everything, with filters and actions."),
            AppButton {
                label: format!("Open {title}"),
                app: app.to_string(),
                view,
                params: BTreeMap::new(),
            },
        )
    } else if event.is_direct {
        let ticket = Ticket {
            user: event.author.clone(),
            conversation: event.conversation.clone(),
            direct: true,
            app: app.to_string(),
            view,
            params: BTreeMap::new(),
            locale: event.locale.clone(),
        };
        match mint(shared, ticket).await {
            Ok(url) if caps.ui.buttons && url.starts_with("https://") => View::links(
                format!(
                    "-# {title}: your own link; it works once, within {} minutes.",
                    TICKET_TTL.as_secs() / 60
                ),
                vec![(format!("Open {title} ↗"), url)],
            ),
            Ok(url) => View::links(
                format!(
                    "Open {title} (your own link; it works once, within {} minutes):\n{url}",
                    TICKET_TTL.as_secs() / 60
                ),
                Vec::new(),
            ),
            Err(reason) => {
                if !quiet {
                    crate::turn::reply(shared, event, &reason).await;
                }
                return;
            }
        }
    } else if quiet {
        return;
    } else {
        View::links(
            format!("-# {title} opens from a direct chat with me: send `/app {app}` there."),
            Vec::new(),
        )
    };
    if let Err(error) = crate::ui::deliver(shared, &to, event.is_direct, &shown).await {
        tracing::warn!(%error, "cannot offer an app");
    }
}

/// `/app`: lists apps; `/app <name> [view]` opens one; `/app sessions` and `/app logout`.
pub(crate) async fn command(shared: &Shared, rest: &str, event: &InboundEvent) {
    let mut words = rest.split_whitespace();
    let first = words.next().unwrap_or_default().to_ascii_lowercase();
    match first.as_str() {
        "" => {
            let text = list(shared, event).await;
            crate::turn::reply(shared, event, &text).await;
        }
        "logout" | "signout" => {
            let person = shared.person_key(event).await;
            let text = match shared.store.end_app_sessions(None, Some(&person)).await {
                Ok(0) => "You were not signed in to any app.".to_string(),
                Ok(n) => format!("Signed out of apps on {n} browser(s)."),
                Err(error) => {
                    tracing::error!(%error, "cannot end app sessions");
                    shared.config.replies.error.clone()
                }
            };
            crate::turn::reply(shared, event, &text).await;
        }
        "sessions" => {
            let person = shared.person_key(event).await;
            let text = match shared
                .store
                .app_sessions(Some(&person), shared.clock.now())
                .await
            {
                Ok(list) if list.is_empty() => "You are not signed in to any app.".to_string(),
                Ok(list) => {
                    let tz = shared.config.time_zone();
                    let mut text = format!("Signed in to apps on {} browser(s):", list.len());
                    for e in &list {
                        let since = serde_json::from_str::<Session>(&e.data)
                            .map(|s| {
                                s.created
                                    .to_zoned(tz.clone())
                                    .strftime("%d/%m %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_default();
                        let _ = write!(text, "\n- since {since}, `{}`", &e.handle[..8]);
                    }
                    text.push_str("\n\n`/app logout` signs all of them out.");
                    text
                }
                Err(error) => {
                    tracing::error!(%error, "cannot list app sessions");
                    shared.config.replies.error.clone()
                }
            };
            crate::turn::reply(shared, event, &text).await;
        }
        name => {
            let view = words.next().map(|v| {
                if v.starts_with('/') {
                    v.to_string()
                } else {
                    format!("/{v}")
                }
            });
            offer(shared, event, name, view.as_deref(), false).await;
        }
    }
}

/// The apps this person may open here.
async fn list(shared: &Shared, event: &InboundEvent) -> String {
    let Some(apps) = shared.apps.as_ref().filter(|_| shared.config.apps.enabled) else {
        return "Apps are off for this assistant.".into();
    };
    let mut lines = Vec::new();
    for info in apps.directory.apps() {
        if admit(
            shared,
            &event.author,
            &event.conversation,
            event.is_direct,
            &info.name,
        )
        .await
        .is_ok()
        {
            lines.push(format!(
                "- `/app {}`: {}{}",
                info.name,
                info.title(event.locale.as_deref()),
                if info.source == okbot_core::AppSource::Local {
                    " (unverified)"
                } else {
                    ""
                }
            ));
        }
    }
    if lines.is_empty() {
        return "No apps are available here.".into();
    }
    let mut text = format!("Apps:\n{}", lines.join("\n"));
    if base(shared, &event.conversation.channel).is_none() {
        text.push_str(
            "\n\nThey need a public address for this bot ([http] public_url); ask its operator.",
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_hashes() {
        let a = secret();
        assert_eq!(a.len(), 43, "32 bytes, base64url");
        assert_ne!(a, secret());
        assert_eq!(hash("x"), hash("x"));
        assert_eq!(hash("x").len(), 64);
    }

    #[test]
    fn labels() {
        let c = |room: &str, direct| Context {
            conversation: ConversationKey {
                channel: "discord:main".parse().unwrap(),
                space: None,
                room: room.into(),
                thread: None,
            },
            direct,
        };
        assert_eq!(c("1", true).label(), "Direct chat");
        assert_eq!(c("123", false).label(), "discord:main · 123");
        assert_eq!(
            c("1234567890123", false).label(),
            "discord:main · …67890123"
        );
    }
}
