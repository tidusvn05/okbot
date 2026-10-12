//! Helpers for unit tests.

use okbot_core::InboundEvent;

/// A text event; `direct` for one-to-one chats, `mention` when the bot is mentioned.
pub(crate) fn event(text: &str, direct: bool, mention: bool) -> InboundEvent {
    let mut e = okbot_testkit::inbound("zalo:shop", "room-1", "ana", text);
    e.is_direct = direct;
    e.mentions_bot = mention;
    e.author.display_name = "Ana".into();
    e
}
