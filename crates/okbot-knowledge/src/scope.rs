//! Visibility of the bundle for one requester (design §5.3).

use okbase::{MetaFilter, Scope};
use okbot_core::Requester;

use crate::paths;

/// The scope for `who`: shared folders, the room's notes and the user's own profile and notes.
///
/// okbase's `deny` always wins over `allow`, so the scope is an allow-list: memory of other users
/// and rooms simply never matches.
pub(crate) fn for_requester(who: &Requester) -> Scope {
    let channel = &who.conversation.channel;
    let mut patterns: Vec<String> = paths::SHARED_DIRS
        .iter()
        .map(|d| (*d).to_string())
        .collect();
    patterns.push(paths::room_dir(channel, &who.conversation.room));
    patterns.push(paths::user_dir(channel, &who.user.id));
    build(&patterns, who.internal)
}

/// Shared folders only (no personal or room memory), for the catalog and tool descriptions.
pub(crate) fn shared() -> Scope {
    let patterns: Vec<String> = paths::SHARED_DIRS
        .iter()
        .map(|d| (*d).to_string())
        .collect();
    build(&patterns, false)
}

/// Matches nothing (unknown or expired run tokens).
pub(crate) fn nothing() -> Scope {
    build(&["__okbot_no_access__".to_string()], false)
}

fn build(patterns: &[String], internal: bool) -> Scope {
    let mut scope = Scope::all();
    for pattern in patterns {
        // Patterns come from fixed folders and `paths::segment`, which only yields valid globs.
        scope = scope
            .allow(pattern)
            .expect("okbot builds valid scope patterns");
    }
    if internal {
        scope
    } else {
        scope.filter(MetaFilter::not_audience("private"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::{ConversationKey, UserRef};

    fn who(user: &str, room: &str) -> Requester {
        Requester {
            conversation: ConversationKey::room("zalo:shop".parse().unwrap(), room),
            user: UserRef {
                id: user.into(),
                display_name: user.into(),
                is_bot: false,
            },
            internal: false,
        }
    }

    #[test]
    fn requester_sees_own_memory_only() {
        let s = for_requester(&who("u1", "r1"));
        assert!(s.permits_path("knowledge/getting-started"));
        assert!(s.permits_path("memory/global/team-lunch"));
        assert!(s.permits_path("memory/users/zalo-shop/u1"));
        assert!(s.permits_path("memory/users/zalo-shop/u1/coffee"));
        assert!(s.permits_path("memory/rooms/zalo-shop/r1/notes"));
        assert!(!s.permits_path("memory/users/zalo-shop/u2"));
        assert!(
            !s.permits_path("memory/users/zalo-shop/u10"),
            "prefix must not leak"
        );
        assert!(!s.permits_path("memory/rooms/zalo-shop/r2/notes"));
        assert!(!s.permits_path("secrets/x"));
    }

    #[test]
    fn shared_and_nothing() {
        assert!(shared().permits_path("system/privacy"));
        assert!(!shared().permits_path("memory/users/zalo-shop/u1"));
        assert!(!nothing().permits_path("system/privacy"));
    }
}
