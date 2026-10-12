use okbot_testkit::{CollectSink, conformance};

use super::*;

fn channel() -> (Arc<ExampleChannel>, Arc<Client>) {
    let client = Arc::new(Client::default());
    let id = ChannelId::new("example", "test").expect("valid id");
    (Arc::new(ExampleChannel::new(id, client.clone())), client)
}

#[tokio::test]
async fn delivers_and_replies() {
    let (channel, client) = channel();
    let sink = Arc::new(CollectSink::default());
    let stop = CancellationToken::new();
    let run = tokio::spawn({
        let (c, s, t) = (channel.clone(), sink.clone(), stop.clone());
        async move { c.run(s, t).await }
    });
    client.push(PlatformMessage {
        id: "1".into(),
        room: "room-1".into(),
        user: "u1".into(),
        user_name: "An".into(),
        text: "hello".into(),
        direct: true,
    });
    let event = sink.first().await;
    assert_eq!(event.text, "hello");
    assert!(event.is_direct);

    let to = ReplyTarget::to_event(&event);
    conformance::channel(channel.as_ref(), &to).await;
    assert_eq!(
        client.posted()[0],
        ("room-1".into(), "conformance: hello".into())
    );

    stop.cancel();
    run.await.expect("join").expect("run");
}

#[tokio::test]
async fn stops_on_shutdown() {
    let (channel, _) = channel();
    conformance::channel_stops(
        channel,
        Arc::new(CollectSink::default()),
        CancellationToken::new(),
    )
    .await;
}
