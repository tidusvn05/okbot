//! Terminal channel for okbot, for development and tests.
//!
//! Each input line is a direct message from the local user; replies are printed as Markdown.
//! When the input ends, [`ConsoleChannel::closed`] is cancelled so the caller can shut down after
//! the last answer.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use okbot_core::{
    CancellationToken, Channel, ChannelCaps, ChannelError, ChannelId, ConversationKey,
    InboundEvent, InboundSink, MarkdownDialect, MessageId, RenderedMessage, ReplyTarget, SentRef,
    UserId, UserRef, async_trait,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

type Reader = Box<dyn AsyncBufRead + Send + Unpin>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// A chat in the terminal.
pub struct ConsoleChannel {
    id: ChannelId,
    caps: ChannelCaps,
    user: String,
    input: Mutex<Option<Reader>>,
    output: Mutex<Writer>,
    closed: CancellationToken,
    next: AtomicU64,
}

impl std::fmt::Debug for ConsoleChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleChannel")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ConsoleChannel {
    /// Reads stdin and writes stdout, as `user`.
    pub fn stdio(user: &str) -> Self {
        Self::with_io(
            user,
            Box::new(BufReader::new(tokio::io::stdin())),
            Box::new(tokio::io::stdout()),
        )
    }

    /// Reads `input` and writes `output`, as `user`.
    pub fn with_io(user: &str, input: Reader, output: Writer) -> Self {
        Self {
            id: ChannelId::new("console", "local").expect("valid literal id"),
            caps: ChannelCaps {
                max_message_len: 100_000,
                dialect: MarkdownDialect::CommonMark,
                ..ChannelCaps::basic()
            },
            user: user.to_string(),
            input: Mutex::new(Some(input)),
            output: Mutex::new(output),
            closed: CancellationToken::new(),
            next: AtomicU64::new(1),
        }
    }

    /// Cancelled when the input has ended.
    pub fn closed(&self) -> CancellationToken {
        self.closed.clone()
    }

    fn event(&self, text: String) -> InboundEvent {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        InboundEvent {
            event_id: format!("console-{}-{n}", std::process::id()),
            conversation: ConversationKey::room(self.id.clone(), "console"),
            author: UserRef {
                id: UserId::new(self.user.clone()),
                display_name: self.user.clone(),
                is_bot: false,
            },
            text,
            attachments: Vec::new(),
            mentions_bot: false,
            is_direct: true,
            restricted: false,
            message_id: Some(MessageId::new(n.to_string())),
            sent_at: jiff::Timestamp::now(),
            locale: None,
        }
    }
}

fn io_error(e: std::io::Error) -> ChannelError {
    ChannelError::Failed(Box::new(e))
}

#[async_trait]
impl Channel for ConsoleChannel {
    fn id(&self) -> &ChannelId {
        &self.id
    }

    fn caps(&self) -> &ChannelCaps {
        &self.caps
    }

    async fn run(
        &self,
        sink: Arc<dyn InboundSink>,
        shutdown: CancellationToken,
    ) -> Result<(), ChannelError> {
        let Some(input) = self.input.lock().await.take() else {
            return Err(ChannelError::Unsupported("the console can only run once"));
        };
        let mut lines = input.lines();
        loop {
            let line = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                line = lines.next_line() => line.map_err(io_error)?,
            };
            let Some(line) = line else { break };
            let text = line.trim();
            if text.is_empty() {
                continue;
            }
            if sink.deliver(self.event(text.to_string())).await.is_err() {
                return Ok(());
            }
        }
        self.closed.cancel();
        shutdown.cancelled().await;
        Ok(())
    }

    async fn send(
        &self,
        to: &ReplyTarget,
        message: &RenderedMessage,
    ) -> Result<SentRef, ChannelError> {
        let mut out = self.output.lock().await;
        out.write_all(message.text.trim_end().as_bytes())
            .await
            .map_err(io_error)?;
        out.write_all(b"\n\n").await.map_err(io_error)?;
        out.flush().await.map_err(io_error)?;
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        Ok(SentRef {
            target: to.clone(),
            message_id: MessageId::new(format!("out-{n}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use okbot_core::SinkClosed;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct Collect(StdMutex<Vec<InboundEvent>>);

    #[async_trait]
    impl InboundSink for Collect {
        async fn deliver(&self, event: InboundEvent) -> Result<(), SinkClosed> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }

    #[tokio::test]
    async fn reads_lines_and_writes_replies() {
        let (client, server) = tokio::io::duplex(1024);
        let channel = ConsoleChannel::with_io(
            "me",
            Box::new(BufReader::new(&b"hello\n\n  second line \n"[..])),
            Box::new(server),
        );
        let sink = Arc::new(Collect::default());
        let stop = CancellationToken::new();
        let closed = channel.closed();
        let run = channel.run(sink.clone(), stop.clone());
        tokio::pin!(run);
        tokio::select! {
            _ = &mut run => panic!("run ended before shutdown"),
            () = closed.cancelled() => {}
        }
        let events = sink.0.lock().unwrap().clone();
        let texts: Vec<&str> = events.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["hello", "second line"]);
        assert!(events[0].is_direct);

        let to = ReplyTarget::to_event(&events[0]);
        channel
            .send(
                &to,
                &RenderedMessage {
                    text: "hi **there**".into(),
                },
            )
            .await
            .unwrap();
        stop.cancel();
        run.await.unwrap();
        // Close the writer so the reader sees the end.
        *channel.output.lock().await = Box::new(tokio::io::sink());
        let mut printed = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut { client }, &mut printed)
            .await
            .unwrap();
        assert_eq!(printed, "hi **there**\n\n");
    }
}
