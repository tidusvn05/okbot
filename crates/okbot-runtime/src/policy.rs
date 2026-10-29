//! Access policy and rate limiting: should the bot answer this event?

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use jiff::{SignedDuration, Timestamp};
use okbot_core::{ChannelId, InboundEvent, UserId};

use crate::config::{AccessLists, AccessPolicy, RuntimeConfig};

/// The policy's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Process the event.
    Answer,
    /// Drop it silently (not addressed to the bot, bot author, not allowed).
    Ignore,
    /// Tell the user they are over the rate limit.
    RateLimited,
    /// Tell the user the message is too long.
    TooLong,
}

/// Applies an [`AccessPolicy`] and holds the current [`AccessLists`], which a reload replaces.
/// Rate-limit state lives in memory.
#[derive(Debug)]
pub(crate) struct Gate {
    policy: AccessPolicy,
    lists: RwLock<Arc<AccessLists>>,
    recent: Mutex<HashMap<(ChannelId, UserId), VecDeque<Timestamp>>>,
}

impl Gate {
    /// A gate with `config`'s policy and lists.
    pub(crate) fn new(config: &RuntimeConfig) -> Self {
        Self {
            policy: config.access.clone(),
            lists: RwLock::new(Arc::new(AccessLists::of(config))),
            recent: Mutex::new(HashMap::new()),
        }
    }

    /// The current lists (a snapshot: a reload does not change it).
    pub(crate) fn lists(&self) -> Arc<AccessLists> {
        self.lists
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replaces the lists; events checked from now on use the new ones.
    pub(crate) fn reload(&self, lists: AccessLists) {
        *self
            .lists
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(lists);
    }

    pub(crate) fn is_admin(&self, event: &InboundEvent) -> bool {
        self.lists()
            .admins
            .contains(&(event.conversation.channel.clone(), event.author.id.clone()))
    }

    /// Whether [`Gate::check`] would answer `event` whatever its addressing, without counting it
    /// against the rate limit. For presses on interactive replies (ADR 0015): a press that
    /// `check` would then drop must not use up the view.
    pub(crate) fn admits(&self, event: &InboundEvent, now: Timestamp) -> bool {
        let p = &self.policy;
        let lists = self.lists();
        let channel = &event.conversation.channel;
        let user = (channel.clone(), event.author.id.clone());
        (!event.author.is_bot || p.answer_bots)
            && !lists.blocked_users.contains(&user)
            && lists
                .allow_users
                .as_ref()
                .is_none_or(|users| users.contains(&user))
            && p.allow_rooms.as_ref().is_none_or(|rooms| {
                rooms.contains(&(channel.clone(), event.conversation.room.clone()))
            })
            && (p.max_input_chars == 0 || event.text.chars().count() <= p.max_input_chars)
            && !self.at_rate_limit(&user, now)
    }

    /// Whether `user` has used up the rate limit at `now`, counting nothing.
    fn at_rate_limit(&self, user: &(ChannelId, UserId), now: Timestamp) -> bool {
        let limit = self.policy.rate_limit;
        if limit.messages == 0 {
            return false;
        }
        let window = SignedDuration::try_from(limit.window).unwrap_or(SignedDuration::MAX);
        let recent = self
            .recent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        recent.get(user).is_some_and(|times| {
            times
                .iter()
                .filter(|t| now.duration_since(**t) < window)
                .count()
                >= limit.messages as usize
        })
    }

    /// Decides what to do with `event` at time `now`.
    pub(crate) fn check(&self, event: &InboundEvent, now: Timestamp) -> Verdict {
        let p = &self.policy;
        let lists = self.lists();
        let channel = &event.conversation.channel;
        let user = (channel.clone(), event.author.id.clone());
        if event.author.is_bot && !p.answer_bots {
            return Verdict::Ignore;
        }
        if lists.blocked_users.contains(&user) {
            return Verdict::Ignore;
        }
        let reply_on = p.reply_on_by_channel.get(channel).unwrap_or(&p.reply_on);
        let addressed = if event.is_direct {
            reply_on.direct
        } else {
            reply_on.all || (reply_on.mention && event.mentions_bot)
        };
        if !addressed {
            return Verdict::Ignore;
        }
        if let Some(users) = &lists.allow_users
            && !users.contains(&user)
        {
            return Verdict::Ignore;
        }
        if let Some(rooms) = &p.allow_rooms
            && !rooms.contains(&(channel.clone(), event.conversation.room.clone()))
        {
            return Verdict::Ignore;
        }
        if p.max_input_chars > 0 && event.text.chars().count() > p.max_input_chars {
            return Verdict::TooLong;
        }
        if self.over_rate_limit(user, now) {
            return Verdict::RateLimited;
        }
        Verdict::Answer
    }

    fn over_rate_limit(&self, user: (ChannelId, UserId), now: Timestamp) -> bool {
        let limit = self.policy.rate_limit;
        if limit.messages == 0 {
            return false;
        }
        let window = SignedDuration::try_from(limit.window).unwrap_or(SignedDuration::MAX);
        let mut recent = self
            .recent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let times = recent.entry(user).or_default();
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= window)
        {
            times.pop_front();
        }
        if times.len() >= limit.messages as usize {
            return true;
        }
        times.push_back(now);
        false
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{RateLimit, ReplyOn};
    use crate::test_support::event;

    fn gate(policy: AccessPolicy) -> Gate {
        let mut config = RuntimeConfig::new("t", "/nonexistent");
        config.access = policy;
        Gate::new(&config)
    }

    #[test]
    fn a_reload_changes_who_is_answered_and_who_is_admin() {
        let e = event("hi", true, false);
        let me = (e.conversation.channel.clone(), e.author.id.clone());
        let g = gate(AccessPolicy {
            allow_users: Some(std::collections::HashSet::new()),
            ..AccessPolicy::default()
        });
        let now = Timestamp::UNIX_EPOCH;
        assert_eq!(g.check(&e, now), Verdict::Ignore);
        assert!(!g.is_admin(&e));
        let before = g.lists();
        g.reload(AccessLists {
            allow_users: Some([me.clone()].into()),
            admins: [me.clone()].into(),
            ..AccessLists::default()
        });
        assert_eq!(g.check(&e, now), Verdict::Answer);
        assert!(g.is_admin(&e));
        assert!(before.admins.is_empty(), "a snapshot keeps the old lists");
        g.reload(AccessLists {
            blocked_users: [me].into(),
            ..AccessLists::default()
        });
        assert_eq!(g.check(&e, now), Verdict::Ignore);
        assert!(!g.admits(&e, now));
    }

    #[test]
    fn answers_direct_and_mentions_only() {
        let g = gate(AccessPolicy::default());
        let now = Timestamp::UNIX_EPOCH;
        assert_eq!(g.check(&event("hi", true, false), now), Verdict::Answer);
        assert_eq!(g.check(&event("hi", false, true), now), Verdict::Answer);
        assert_eq!(g.check(&event("hi", false, false), now), Verdict::Ignore);
        let all = gate(AccessPolicy {
            reply_on: ReplyOn {
                all: true,
                ..ReplyOn::default()
            },
            ..AccessPolicy::default()
        });
        assert_eq!(all.check(&event("hi", false, false), now), Verdict::Answer);

        let e = event("hi", true, false);
        let mut per_channel = AccessPolicy::default();
        per_channel.reply_on_by_channel.insert(
            e.conversation.channel.clone(),
            ReplyOn {
                direct: false,
                mention: true,
                all: false,
            },
        );
        assert_eq!(gate(per_channel).check(&e, now), Verdict::Ignore);
    }

    #[test]
    fn ignores_bots_and_blocked_users() {
        let g = gate(AccessPolicy::default());
        let mut e = event("hi", true, false);
        e.author.is_bot = true;
        assert_eq!(g.check(&e, Timestamp::UNIX_EPOCH), Verdict::Ignore);

        let e = event("hi", true, false);
        let mut policy = AccessPolicy::default();
        policy
            .blocked_users
            .insert((e.conversation.channel.clone(), e.author.id.clone()));
        assert_eq!(
            gate(policy).check(&e, Timestamp::UNIX_EPOCH),
            Verdict::Ignore
        );
    }

    #[test]
    fn allow_lists() {
        let e = event("hi", true, false);
        let policy = AccessPolicy {
            allow_users: Some(
                std::iter::once((e.conversation.channel.clone(), "other".into())).collect(),
            ),
            ..AccessPolicy::default()
        };
        assert_eq!(
            gate(policy).check(&e, Timestamp::UNIX_EPOCH),
            Verdict::Ignore
        );
    }

    #[test]
    fn too_long_and_rate_limit() {
        let g = gate(AccessPolicy {
            max_input_chars: 5,
            rate_limit: RateLimit {
                messages: 2,
                window: Duration::from_secs(60),
            },
            ..AccessPolicy::default()
        });
        let t0 = Timestamp::UNIX_EPOCH;
        assert_eq!(g.check(&event("123456", true, false), t0), Verdict::TooLong);
        assert_eq!(g.check(&event("a", true, false), t0), Verdict::Answer);
        assert_eq!(g.check(&event("b", true, false), t0), Verdict::Answer);
        assert_eq!(g.check(&event("c", true, false), t0), Verdict::RateLimited);
        let later = t0 + SignedDuration::from_secs(61);
        assert_eq!(g.check(&event("d", true, false), later), Verdict::Answer);
    }

    #[test]
    fn admits_counts_nothing_and_refuses_what_check_would_drop() {
        let g = gate(AccessPolicy {
            max_input_chars: 5,
            rate_limit: RateLimit {
                messages: 1,
                window: Duration::from_secs(60),
            },
            ..AccessPolicy::default()
        });
        let t0 = Timestamp::UNIX_EPOCH;
        assert!(!g.admits(&event("123456", true, false), t0), "too long");
        assert!(g.admits(&event("a", true, false), t0));
        assert!(
            g.admits(&event("a", true, false), t0),
            "admits counts nothing"
        );
        assert_eq!(g.check(&event("a", true, false), t0), Verdict::Answer);
        assert!(
            !g.admits(&event("b", true, false), t0),
            "over the rate limit"
        );
        let later = t0 + SignedDuration::from_secs(61);
        assert!(g.admits(&event("c", true, false), later));
    }
}
