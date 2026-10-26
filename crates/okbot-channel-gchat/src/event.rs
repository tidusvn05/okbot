//! Google Chat interaction events → [`InboundEvent`]s. Both the classic Chat app format
//! (`{"type": "MESSAGE", "message": …}`) and the Workspace add-on format
//! (`{"chat": {"messagePayload": …}}`) are accepted.

use std::collections::BTreeMap;

use jiff::Timestamp;
use okbot_core::{ChannelId, ConversationKey, InboundEvent, MessageId, Secret, UserId, UserRef};
use serde_json::Value;

/// `(message, space)` of a message event, or `None` for other events.
fn message_parts(event: &Value) -> Option<(&Value, &Value)> {
    if let Some(payload) = event.pointer("/chat/messagePayload") {
        return Some((&payload["message"], &payload["space"]));
    }
    if event["type"].as_str() == Some("MESSAGE") {
        let message = &event["message"];
        let space = if event["space"].is_object() {
            &event["space"]
        } else {
            &message["space"]
        };
        return Some((message, space));
    }
    None
}

/// Converts an event. Messages from other bots are kept (the runtime ignores them).
pub(crate) fn to_inbound(
    channel: &ChannelId,
    event: &Value,
    now: Timestamp,
) -> Option<InboundEvent> {
    let (message, space) = message_parts(event)?;
    let space_name = space["name"]
        .as_str()
        .or_else(|| message["space"]["name"].as_str())?;
    let direct = is_direct(space);
    let sender = &message["sender"];
    let user = sender["name"].as_str().unwrap_or("users/unknown");
    // `argumentText` is the text without the app's @mention.
    let text = message["argumentText"]
        .as_str()
        .or_else(|| message["text"].as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let thread = message["thread"]["name"].as_str().map(str::to_string);
    let name = message["name"].as_str().map(str::to_string);
    let sent_at = message["createTime"]
        .as_str()
        .and_then(|t| t.parse::<Timestamp>().ok())
        .unwrap_or(now);
    Some(InboundEvent {
        event_id: name
            .clone()
            .unwrap_or_else(|| format!("{space_name}:{}", sent_at.as_millisecond())),
        conversation: ConversationKey {
            channel: channel.clone(),
            space: None,
            room: space_name.to_string(),
            // Each thread of a space is its own conversation; direct messages are one.
            thread: if direct { None } else { thread },
        },
        author: UserRef {
            id: UserId::new(user),
            display_name: sender["displayName"].as_str().unwrap_or(user).to_string(),
            is_bot: sender["type"].as_str() == Some("BOT"),
        },
        text,
        attachments: Vec::new(),
        // In spaces, Chat delivers messages to an app only when it is @mentioned.
        mentions_bot: !direct,
        is_direct: direct,
        restricted: false,
        message_id: name.map(MessageId::new),
        sent_at,
        locale: None,
    })
}

/// Where a clicked prompt is: its message and conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    /// The message name.
    pub(crate) message: String,
    /// The space name.
    pub(crate) room: String,
    /// The thread, outside direct messages.
    pub(crate) thread: Option<String>,
}

/// A press of one of okbot's card buttons, a pick in a prompt's menu or a submitted dialog.
#[derive(Debug, Clone)]
pub(crate) struct Click {
    pub(crate) prompt: String,
    /// The button pressed or the option picked; `None` when the menu went back to its
    /// placeholder.
    pub(crate) button: Option<String>,
    /// A dialog's submit button: [`Click::inputs`] are the form's values.
    pub(crate) submit: bool,
    /// Text inputs of a dialog, by widget name.
    pub(crate) inputs: BTreeMap<String, Secret<String>>,
    pub(crate) user: UserRef,
    /// The event came in the Workspace add-on format (its synchronous answers differ).
    pub(crate) add_on: bool,
    /// Where the prompt is, when the event says.
    pub(crate) place: Option<Place>,
}

fn is_direct(space: &Value) -> bool {
    matches!(space["spaceType"].as_str(), Some("DIRECT_MESSAGE"))
        || matches!(space["type"].as_str(), Some("DM"))
        || space["singleUserBotDm"].as_bool() == Some(true)
}

/// The first string value of each form input (`{"name": {"stringInputs": {"value": [..]}}}`).
fn inputs(form: Option<&Value>) -> BTreeMap<String, Secret<String>> {
    form.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, input)| {
            let value = input.pointer("/stringInputs/value/0")?.as_str()?;
            Some((name.clone(), Secret::new(value.to_string())))
        })
        .collect()
}

/// A dialog was closed without submitting (`CANCEL_DIALOG`): `Some(add_on)`.
pub(crate) fn dialog_cancelled(event: &Value) -> Option<bool> {
    if let Some(payload) = event.pointer("/chat/buttonClickedPayload") {
        return (payload["dialogEventType"].as_str() == Some("CANCEL_DIALOG")).then_some(true);
    }
    (event["dialogEventType"].as_str() == Some("CANCEL_DIALOG")).then_some(false)
}

/// A card click (`CARD_CLICKED`, or an add-on `buttonClickedPayload`) on an okbot prompt.
pub(crate) fn to_click(event: &Value) -> Option<Click> {
    let add_on = event.pointer("/chat/buttonClickedPayload");
    let (function, parameters, form, user, message, space) = if let Some(payload) = add_on {
        (
            event.pointer("/commonEventObject/invokedFunction"),
            event.pointer("/commonEventObject/parameters").cloned(),
            event.pointer("/commonEventObject/formInputs"),
            &payload["user"],
            &payload["message"],
            &payload["space"],
        )
    } else if event["type"].as_str() == Some("CARD_CLICKED") {
        // Classic events list parameters as `[{key, value}]`; turn them into a map.
        let list = event
            .pointer("/action/parameters")
            .and_then(Value::as_array);
        let map: serde_json::Map<String, Value> = list
            .into_iter()
            .flatten()
            .filter_map(|p| Some((p["key"].as_str()?.to_string(), p["value"].clone())))
            .collect();
        let parameters = if map.is_empty() {
            event.pointer("/common/parameters").cloned()
        } else {
            Some(Value::Object(map))
        };
        (
            event
                .pointer("/action/actionMethodName")
                .or_else(|| event.pointer("/common/invokedFunction")),
            parameters,
            event.pointer("/common/formInputs"),
            &event["user"],
            &event["message"],
            &event["space"],
        )
    } else {
        return None;
    };
    if function.and_then(Value::as_str) != Some(crate::prompt::FUNCTION) {
        return None;
    }
    let parameters = parameters?;
    let id = user["name"].as_str()?;
    let mut inputs = inputs(form);
    let button = if parameters["select"].as_str().is_some() {
        inputs
            .remove(crate::prompt::SELECT_INPUT)
            .map(|v| v.expose().clone())
            .filter(|v| !v.is_empty())
    } else {
        Some(parameters["button"].as_str()?.to_string())
    };
    let submit = parameters["submit"].as_str().is_some();
    if !submit {
        // Only a dialog's values are kept.
        inputs.clear();
    }
    let space = if space.is_object() {
        space
    } else {
        &message["space"]
    };
    let place = match (message["name"].as_str(), space["name"].as_str()) {
        (Some(message_name), Some(room)) => Some(Place {
            message: message_name.to_string(),
            room: room.to_string(),
            thread: if is_direct(space) {
                None
            } else {
                message["thread"]["name"].as_str().map(str::to_string)
            },
        }),
        _ => None,
    };
    Some(Click {
        prompt: parameters["prompt"].as_str()?.to_string(),
        button,
        submit,
        inputs,
        user: UserRef {
            id: UserId::new(id),
            display_name: user["displayName"].as_str().unwrap_or(id).to_string(),
            is_bot: false,
        },
        add_on: add_on.is_some(),
        place,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel() -> ChannelId {
        "gchat:corp".parse().unwrap()
    }

    #[test]
    fn classic_space_message() {
        let event = json!({
            "type": "MESSAGE",
            "message": {
                "name": "spaces/AAA/messages/M1",
                "sender": {"name": "users/111", "displayName": "An", "type": "HUMAN"},
                "text": "@okbot giá gói Pro?", "argumentText": " giá gói Pro?",
                "thread": {"name": "spaces/AAA/threads/T1"},
                "createTime": "2026-10-12T03:00:00Z"
            },
            "space": {"name": "spaces/AAA", "type": "ROOM", "spaceType": "SPACE"}
        });
        let e = to_inbound(&channel(), &event, Timestamp::UNIX_EPOCH).unwrap();
        assert_eq!(e.text, "giá gói Pro?");
        assert_eq!(e.conversation.room, "spaces/AAA");
        assert_eq!(
            e.conversation.thread.as_deref(),
            Some("spaces/AAA/threads/T1")
        );
        assert!(e.mentions_bot && !e.is_direct);
        assert_eq!(e.event_id, "spaces/AAA/messages/M1");
        assert_eq!(e.author.id.as_str(), "users/111");
    }

    #[test]
    fn add_on_direct_message() {
        let event = json!({"chat": {"messagePayload": {
            "space": {"name": "spaces/DM1", "spaceType": "DIRECT_MESSAGE"},
            "message": {"name": "spaces/DM1/messages/M2", "text": "hello",
                        "sender": {"name": "users/222", "displayName": "Bình", "type": "HUMAN"},
                        "thread": {"name": "spaces/DM1/threads/X"}}}}});
        let e = to_inbound(&channel(), &event, Timestamp::UNIX_EPOCH).unwrap();
        assert!(e.is_direct);
        assert_eq!(e.conversation.thread, None);
        assert_eq!(e.text, "hello");
    }

    #[test]
    fn card_clicks() {
        let classic = json!({"type": "CARD_CLICKED",
            "user": {"name": "users/7", "displayName": "Lan"},
            "action": {"actionMethodName": "okp", "parameters": [
                {"key": "prompt", "value": "abc123"}, {"key": "button", "value": "allow"}]},
            "space": {"name": "spaces/AAA"}});
        let click = to_click(&classic).unwrap();
        assert_eq!(
            (click.prompt.as_str(), click.button.as_deref()),
            ("abc123", Some("allow"))
        );
        assert!(!click.add_on && !click.submit && click.place.is_none());
        assert_eq!(click.user.id.as_str(), "users/7");
        let add_on = json!({"chat": {"buttonClickedPayload": {"user": {"name": "users/8"}}},
            "commonEventObject": {"invokedFunction": "okp",
                                  "parameters": {"prompt": "abc123", "button": "deny"}}});
        let click = to_click(&add_on).unwrap();
        assert_eq!(click.button.as_deref(), Some("deny"));
        assert!(click.add_on);
        let other = json!({"type": "CARD_CLICKED", "user": {"name": "users/7"},
            "action": {"actionMethodName": "something", "parameters": []}});
        assert!(to_click(&other).is_none());
        assert!(to_inbound(&channel(), &classic, Timestamp::UNIX_EPOCH).is_none());
    }

    #[test]
    fn menu_picks_and_dialog_submissions() {
        let pick = |value: &str| {
            json!({"type": "CARD_CLICKED", "user": {"name": "users/7"},
                "action": {"actionMethodName": "okp", "parameters": [
                    {"key": "prompt", "value": "v1a2b3c4d"}, {"key": "select", "value": "1"}]},
                "common": {"formInputs": {"okp_select": {"stringInputs": {"value": [value]}}}},
                "message": {"name": "spaces/AAA/messages/M9",
                            "thread": {"name": "spaces/AAA/threads/T"}},
                "space": {"name": "spaces/AAA", "spaceType": "SPACE"}})
        };
        let click = to_click(&pick("o2")).unwrap();
        assert_eq!(click.button.as_deref(), Some("o2"));
        assert!(click.inputs.is_empty());
        assert_eq!(
            click.place,
            Some(Place {
                message: "spaces/AAA/messages/M9".into(),
                room: "spaces/AAA".into(),
                thread: Some("spaces/AAA/threads/T".into()),
            })
        );
        assert_eq!(to_click(&pick("")).unwrap().button, None, "the placeholder");

        let submit = json!({"chat": {"buttonClickedPayload": {
                "user": {"name": "users/8"}, "isDialogEvent": true,
                "dialogEventType": "SUBMIT_DIALOG",
                "message": {"name": "spaces/DM/messages/M1",
                            "thread": {"name": "spaces/DM/threads/X"}},
                "space": {"name": "spaces/DM", "spaceType": "DIRECT_MESSAGE"}}},
            "commonEventObject": {"invokedFunction": "okp",
                "parameters": {"prompt": "v1a2b3c4d", "button": "b2", "submit": "1"},
                "formInputs": {"f1": {"stringInputs": {"value": ["Lan"]}}}}});
        let click = to_click(&submit).unwrap();
        assert!(click.submit && click.add_on);
        assert_eq!(click.button.as_deref(), Some("b2"));
        assert_eq!(click.inputs["f1"].expose(), "Lan");
        assert_eq!(
            click.place.unwrap().thread,
            None,
            "direct messages have no thread"
        );
        let cancel = json!({"type": "CARD_CLICKED", "dialogEventType": "CANCEL_DIALOG"});
        assert_eq!(dialog_cancelled(&cancel), Some(false));
        assert_eq!(dialog_cancelled(&pick("o1")), None);
    }

    #[test]
    fn other_events_are_ignored() {
        let added = json!({"type": "ADDED_TO_SPACE", "space": {"name": "spaces/A"}});
        assert!(to_inbound(&channel(), &added, Timestamp::UNIX_EPOCH).is_none());
    }
}
