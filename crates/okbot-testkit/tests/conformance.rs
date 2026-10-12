//! The conformance suites pass on the testkit's own fakes.

use std::sync::Arc;

use okbot_core::{CancellationToken, ChannelCaps, ReplyTarget};
use okbot_testkit::{CollectSink, FakeChannel, inbound};

#[tokio::test]
async fn fake_channel_conforms() {
    for edit in [false, true] {
        let channel = FakeChannel::new(
            "fake:one",
            ChannelCaps {
                edit,
                ..ChannelCaps::basic()
            },
        );
        let to = ReplyTarget::to_event(&inbound("fake:one", "room", "u", "hi"));
        okbot_testkit::conformance::channel(&channel, &to).await;
    }
}

#[tokio::test]
async fn fake_channel_stops() {
    okbot_testkit::conformance::channel_stops(
        Arc::new(FakeChannel::new("fake:one", ChannelCaps::basic())),
        Arc::new(CollectSink::default()),
        CancellationToken::new(),
    )
    .await;
}
