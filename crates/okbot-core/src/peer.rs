//! The peer port (ADR 0014): delegating work to other okbots of a team.
//!
//! Outbound, the runtime calls [`Peers`] when an agent delegates. Inbound, a peer adapter is also a
//! [`Channel`](crate::Channel) (`peer:<team>`): each accepted task arrives as an ordinary
//! [`InboundEvent`](crate::InboundEvent), and [`Peers::caller`] says who sent it.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::id::{ConversationKey, UserRef};
use crate::{BoxError, async_trait};

/// The platform name of peer channels (`peer:<team>`).
pub const PEER_PLATFORM: &str = "peer";

/// Event ids starting with this, on a peer channel, stop the conversation's running turn: the task
/// was cancelled by its caller. (Another bot's text never runs as a command, so `/stop` cannot.)
pub const PEER_CANCEL_PREFIX: &str = "peer-cancel:";

/// A peer operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PeerError {
    /// No member matches the target, or no link allows the call.
    #[error("no member of the team can take this: {0}")]
    NoRoute(String),
    /// The other bot refused the request (its reason, safe to show).
    #[error("refused: {0}")]
    Refused(String),
    /// The other bot could not be reached or did not answer in time.
    #[error("unreachable: {0}")]
    Unreachable(String),
    /// No such task.
    #[error("no such task")]
    UnknownTask,
    /// Any other failure.
    #[error("peer error: {0}")]
    Other(#[source] BoxError),
}

/// Who a delegation is for, where it started and how much it may still spend. It travels with
/// every request of a chain and is signed with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationContext {
    /// Where the work started.
    pub origin: Origin,
    /// Team members the work passed through, the first bot first.
    pub chain: Vec<String>,
    /// The work must be done by then; only shrinks along the chain.
    pub deadline: Timestamp,
    /// Agent turns the rest of the chain may still use.
    pub budget: Budget,
    /// An id shared by every bot's records of this work.
    pub trace: String,
}

/// The person and conversation a delegation started from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The member name of the first bot.
    pub bot: String,
    /// The conversation on the first bot (its storage text form).
    pub conversation: String,
    /// The person's id in the team manifest, when the team knows them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// The person's display name.
    pub name: String,
}

/// What a chain of delegations may still spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Agent turns.
    pub turns: u32,
    /// Input plus output tokens of finished tasks; `None`: no token limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

/// Whom to delegate to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PeerTarget {
    /// One member, by name.
    Member(String),
    /// Any member with this team role (`role:<name>`).
    Role(String),
    /// Any member with this skill (`skill:<name>`).
    Skill(String),
}

impl FromStr for PeerTarget {
    type Err = PeerError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let target = if let Some(role) = s.strip_prefix("role:") {
            Self::Role(role.to_string())
        } else if let Some(skill) = s.strip_prefix("skill:") {
            Self::Skill(skill.to_string())
        } else {
            Self::Member(s.to_string())
        };
        match &target {
            Self::Member(n) | Self::Role(n) | Self::Skill(n) if n.is_empty() => {
                Err(PeerError::NoRoute("an empty target".into()))
            }
            _ => Ok(target),
        }
    }
}

impl fmt::Display for PeerTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Member(n) => f.write_str(n),
            Self::Role(n) => write!(f, "role:{n}"),
            Self::Skill(n) => write!(f, "skill:{n}"),
        }
    }
}

/// A request to hand work to another bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delegation {
    /// The team; `None`: any team of this bot with a matching member.
    pub team: Option<String>,
    /// Whom.
    pub to: PeerTarget,
    /// The work, in plain language.
    pub task: String,
    /// Who it is for and what it may spend.
    pub context: DelegationContext,
    /// How long to wait for the result before returning a running task.
    pub wait: Duration,
    /// Identifies this request: sending it again returns the same task.
    pub message_id: String,
    /// Who asked, so a result that comes after a restart still reaches them.
    pub requester: Option<UserRef>,
    /// The origin conversation is a one-to-one chat.
    pub direct: bool,
}

/// Where a task stands (A2A task states).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PeerTaskState {
    /// Accepted, not started.
    Submitted,
    /// Running.
    Working,
    /// Waiting for a person (an approval).
    InputRequired,
    /// Done; the text is the answer.
    Completed,
    /// Failed; the text says why.
    Failed,
    /// Cancelled.
    Canceled,
    /// Refused by the other bot.
    Rejected,
}

impl PeerTaskState {
    /// The task will not change any more.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Rejected
        )
    }

    /// The A2A name (`input-required`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Working => "working",
            Self::InputRequired => "input-required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Rejected => "rejected",
        }
    }
}

impl FromStr for PeerTaskState {
    type Err = PeerError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "submitted" => Self::Submitted,
            "working" => Self::Working,
            "input-required" => Self::InputRequired,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            "rejected" => Self::Rejected,
            other => return Err(PeerError::Refused(format!("unknown task state {other}"))),
        })
    }
}

/// A task as the caller sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerTask {
    /// The team.
    pub team: String,
    /// The member doing it.
    pub peer: String,
    /// The task id (the other bot's).
    pub id: String,
    /// Where it stands.
    pub state: PeerTaskState,
    /// The answer, the reason for a failure, or the question while input is required.
    pub text: Option<String>,
    /// While input is required: the question's buttons, to show the origin person.
    pub prompt: Option<PeerPrompt>,
    /// Tokens the task used (its turn and its own delegations), once it ended.
    pub tokens: Option<u64>,
}

/// A question a task asks the origin person (an approval), answered with one of its buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerPrompt {
    /// The prompt id on the other bot.
    pub id: String,
    /// `(button id, label)`, in order.
    pub buttons: Vec<(String, String)>,
}

/// A member another bot may delegate to, as the agent sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerInfo {
    /// The team.
    pub team: String,
    /// The member's name.
    pub name: String,
    /// Team roles.
    pub roles: Vec<String>,
    /// Skills.
    pub skills: Vec<String>,
    /// What it does.
    pub description: String,
}

/// The authenticated sender of an inbound task, for the runtime's permission checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCaller {
    /// The team.
    pub team: String,
    /// The calling member.
    pub member: String,
    /// Its team roles (from the verified manifest).
    pub roles: Vec<String>,
    /// The delegation context it sent.
    pub context: DelegationContext,
}

/// The work done, as a direction-tagged record kept by the store (`peer_tasks`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerTaskRecord {
    /// This bot's id for the task: the A2A task id for inbound tasks, the callee's for outbound.
    pub id: String,
    /// Received from another bot (`true`) or sent to one.
    pub inbound: bool,
    /// The team.
    pub team: String,
    /// The other member.
    pub peer: String,
    /// The A2A context id.
    pub context_id: String,
    /// The A2A message id that created it (idempotency key, unique per team, peer and direction).
    pub message_id: String,
    /// Where it stands.
    pub state: PeerTaskState,
    /// The conversation that delegated it (outbound) or that runs it (inbound).
    pub conversation: Option<ConversationKey>,
    /// The trace id of the delegation.
    pub trace: String,
    /// The answer or the reason for a failure.
    pub text: Option<String>,
    /// Outbound: who asked, to bring a late result to them.
    pub requester: Option<UserRef>,
    /// Outbound: the conversation is a one-to-one chat.
    pub direct: bool,
    /// Tokens the task used, once it ended.
    pub tokens: Option<u64>,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it last changed.
    pub updated_at: Timestamp,
}

/// Delegates work to other bots of this bot's teams. Implemented by the peer adapter.
#[async_trait]
pub trait Peers: Send + Sync {
    /// Members this bot may delegate to (the manifest's links from this bot).
    async fn directory(&self) -> Result<Vec<PeerInfo>, PeerError>;

    /// Hands work to a member and waits up to [`Delegation::wait`] for it to end.
    async fn delegate(&self, delegation: Delegation) -> Result<PeerTask, PeerError>;

    /// The current state of a task this bot delegated.
    async fn status(&self, team: &str, peer: &str, task: &str) -> Result<PeerTask, PeerError>;

    /// Asks the other bot to stop a task this bot delegated.
    async fn cancel(&self, team: &str, peer: &str, task: &str) -> Result<PeerTask, PeerError>;

    /// The tokens the turn answering the task of `conversation` (`task`, or the oldest) used, its
    /// own delegations included; reported with the task's end.
    async fn report_usage(&self, conversation: &ConversationKey, task: Option<&str>, tokens: u64);

    /// The runtime could not answer the task of `conversation` (`task`, or the oldest): it fails
    /// with `reason` instead of completing with a fixed reply.
    async fn fail(&self, conversation: &ConversationKey, task: Option<&str>, reason: &str);

    /// Answers the question of a task waiting for input (the origin person pressed `button`).
    async fn answer(
        &self,
        task: &PeerTask,
        prompt: &str,
        button: &str,
    ) -> Result<PeerTask, PeerError>;

    /// The authenticated sender of an inbound event of a peer channel; `None` for other events.
    async fn caller(&self, conversation: &ConversationKey) -> Option<PeerCaller>;

    /// The team person whose accounts include `account` (`platform:instance/user-id`).
    fn team_person(&self, account: &str) -> Option<String>;

    /// This bot's member name in `team`.
    fn member_name(&self, team: &str) -> Option<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        assert_eq!(
            "ops".parse::<PeerTarget>().unwrap(),
            PeerTarget::Member("ops".into())
        );
        assert_eq!(
            "role:leader".parse::<PeerTarget>().unwrap(),
            PeerTarget::Role("leader".into())
        );
        assert_eq!(
            "skill:web".parse::<PeerTarget>().unwrap().to_string(),
            "skill:web"
        );
        assert!("role:".parse::<PeerTarget>().is_err());
        assert!("".parse::<PeerTarget>().is_err());
    }

    #[test]
    fn states_round_trip() {
        for state in [
            PeerTaskState::Submitted,
            PeerTaskState::Working,
            PeerTaskState::InputRequired,
            PeerTaskState::Completed,
            PeerTaskState::Failed,
            PeerTaskState::Canceled,
            PeerTaskState::Rejected,
        ] {
            assert_eq!(state.as_str().parse::<PeerTaskState>().unwrap(), state);
        }
        assert!(PeerTaskState::Completed.is_final());
        assert!(!PeerTaskState::InputRequired.is_final());
    }
}
