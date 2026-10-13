//! Where knowledge and memory live inside the bundle, and path-safe ids.

use okbot_core::{ChannelId, Requester, UserId};

/// Folders every requester may read.
pub(crate) const SHARED_DIRS: [&str; 5] =
    ["persona", "system", "knowledge", "data", "memory/global"];

/// A path segment from an arbitrary platform id: `[A-Za-z0-9._-]` kept, anything else (and ids
/// starting with `.`) hex-encoded with an `x-` prefix, so ids can never traverse or collide.
pub(crate) fn segment(raw: &str) -> String {
    let safe = !raw.is_empty()
        && !raw.starts_with('.')
        && !raw.starts_with("x-")
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if safe {
        raw.to_string()
    } else {
        format!("x-{}", hex::encode(raw.as_bytes()))
    }
}

/// Concept id (no `.md`) of a user's profile: `memory/users/<channel>/<user>`.
pub(crate) fn user_profile(channel: &ChannelId, user: &UserId) -> String {
    format!("memory/users/{}/{}", channel.slug(), segment(user.as_str()))
}

/// Folder of a room's notes: `memory/rooms/<channel>/<room>`.
pub(crate) fn room_dir(channel: &ChannelId, room: &str) -> String {
    format!("memory/rooms/{}/{}", channel.slug(), segment(room))
}

/// Folder of a user's notes: `memory/users/<channel>/<user>` (profile) and `…/<user>/` (notes).
pub(crate) fn user_dir(channel: &ChannelId, user: &UserId) -> String {
    user_profile(channel, user)
}

/// Profile and notes of a linked person: `memory/people/<person>` (profile) and `…/<person>/`.
pub(crate) fn person_dir(person: &str) -> String {
    format!("memory/people/{}", segment(person))
}

/// Where `who`'s personal notes are written: the person's folder when the account is linked,
/// otherwise the account's own folder.
pub(crate) fn owner_dir(who: &Requester) -> String {
    match &who.person {
        Some(person) => person_dir(&person.id),
        None => user_dir(&who.conversation.channel, &who.user.id),
    }
}

/// Every personal folder `who` may read, the write folder first: a linked person also keeps the
/// notes each of their accounts saved before the link.
pub(crate) fn personal_dirs(who: &Requester) -> Vec<String> {
    let mut dirs = vec![owner_dir(who)];
    let accounts = who.person.as_ref().map_or_else(
        || vec![(who.conversation.channel.clone(), who.user.id.clone())],
        |p| p.accounts.clone(),
    );
    for (channel, user) in &accounts {
        let dir = user_dir(channel, user);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_safe() {
        assert_eq!(segment("6ede9afa66b88fe6d6a9"), "6ede9afa66b88fe6d6a9");
        assert_eq!(
            segment("spaces/AAA"),
            format!("x-{}", hex::encode("spaces/AAA"))
        );
        assert_eq!(segment(".."), format!("x-{}", hex::encode("..")));
        assert_eq!(segment(""), "x-");
        assert_ne!(
            segment("x-41"),
            "x-41",
            "an id looking like an encoding is re-encoded"
        );
    }

    #[test]
    fn memory_paths() {
        let ch: ChannelId = "zalo:shop".parse().unwrap();
        assert_eq!(user_profile(&ch, &"u1".into()), "memory/users/zalo-shop/u1");
        assert_eq!(person_dir("luan"), "memory/people/luan");
        assert_eq!(
            room_dir(&ch, "r/1"),
            format!("memory/rooms/zalo-shop/x-{}", hex::encode("r/1"))
        );
    }
}
