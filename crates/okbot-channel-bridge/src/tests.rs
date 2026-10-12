use super::*;

use okbot_testkit::CollectSink as Collect;

fn node() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn adapter(id: &str, script: &str, args: Vec<String>) -> BridgeChannel {
    BridgeChannel::new(BridgeConfig {
        id: id.parse().unwrap(),
        command: "node".into(),
        args: std::iter::once(script.to_string()).chain(args).collect(),
        env: vec![],
        env_remove: vec![],
        settings: json!({"greeting": "xin chào"}),
        caps: ChannelCaps {
            edit: true,
            ..ChannelCaps::basic()
        },
    })
}

fn reference() -> String {
    format!(
        "{}/../../examples/bridge/echo-channel.mjs",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[tokio::test]
async fn reference_adapter_round_trip() {
    if !node() {
        eprintln!("skipped: node is not installed");
        return;
    }
    let channel = Arc::new(adapter("demo:test", &reference(), vec![]));
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (c, s, t) = (channel.clone(), sink.clone(), stop.clone());
        async move { c.run(s, t).await }
    });
    let event = sink.first().await;
    assert_eq!(event.text, "xin chào");
    assert_eq!(event.conversation.channel.to_string(), "demo:test");

    let to = ReplyTarget::to_event(&event);
    let sent = channel
        .send(
            &to,
            &RenderedMessage {
                text: "**hi**".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(sent.message_id.as_str(), "m1");
    channel
        .edit(
            &sent,
            &RenderedMessage {
                text: "edited".into(),
            },
        )
        .await
        .unwrap();
    channel.typing(&to).await.unwrap();
    okbot_testkit::conformance::channel(channel.as_ref(), &to).await;

    stop.cancel();
    run.await.unwrap().unwrap();
    assert!(
        channel
            .send(
                &to,
                &RenderedMessage {
                    text: "late".into()
                }
            )
            .await
            .is_err(),
        "not running"
    );
}

#[tokio::test]
async fn rejects_other_protocols_and_foreign_events() {
    if !node() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("bad.mjs");
    std::fs::write(
        &script,
        r#"import { createInterface } from "node:readline";
createInterface({ input: process.stdin }).on("line", (l) => {
  const m = JSON.parse(l);
  if (m.method === "initialize") {
    // An event for a channel this adapter does not own: must be dropped.
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", method: "inbound", params: { event: {
      event_id: "x", conversation: { channel: "other:chan", space: null, room: "r", thread: null },
      author: { id: "u", display_name: "u", is_bot: false }, text: "spoof", attachments: [],
      mentions_bot: false, is_direct: true, restricted: false, message_id: null,
      sent_at: "2026-10-12T00:00:00Z" } } }) + "\n");
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: m.id, result: { protocol: 99 } }) + "\n");
  }
});"#,
    )
    .unwrap();
    let channel = adapter("demo:test", script.to_str().unwrap(), vec![]);
    let collect = Arc::new(Collect::default());
    let sink: Arc<dyn InboundSink> = collect.clone();
    let error = channel
        .session(&sink, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("protocol 99"), "{error}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        collect.events().len(),
        0,
        "events for other channels are dropped"
    );
}

#[tokio::test]
async fn stops_on_shutdown() {
    if !node() {
        return;
    }
    okbot_testkit::conformance::channel_stops(
        Arc::new(adapter("demo:test", &reference(), vec![])),
        Arc::new(Collect::default()),
        CancellationToken::new(),
    )
    .await;
}
