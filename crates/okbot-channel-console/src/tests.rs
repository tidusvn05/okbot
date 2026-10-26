use super::*;
use okbot_core::{ButtonStyle, FieldStyle, Form, FormField, PromptButton, SinkClosed};
use std::io;
use std::sync::Mutex as StdMutex;
use std::task::{Context, Poll};

#[derive(Default)]
struct Collect {
    events: StdMutex<Vec<InboundEvent>>,
    answers: StdMutex<Vec<PromptAnswer>>,
}

#[async_trait]
impl InboundSink for Collect {
    async fn deliver(&self, event: InboundEvent) -> Result<(), SinkClosed> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }

    async fn answer(&self, answer: PromptAnswer) -> Result<(), SinkClosed> {
        self.answers.lock().unwrap().push(answer);
        Ok(())
    }
}

/// Output kept in memory.
#[derive(Clone, Default)]
struct Buf(Arc<StdMutex<Vec<u8>>>);

impl Buf {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl AsyncWrite for Buf {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, b: &[u8]) -> Poll<io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(b);
        Poll::Ready(Ok(b.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn console(preset: Preset, input: &'static str) -> (ConsoleChannel, Buf) {
    let out = Buf::default();
    let options = ConsoleOptions {
        preset,
        ..ConsoleOptions::new("me")
    };
    let channel = ConsoleChannel::new(
        options,
        Box::new(BufReader::new(input.as_bytes())),
        Box::new(out.clone()),
    );
    (channel, out)
}

/// Runs `channel` until its input ends.
async fn read_all(channel: &ConsoleChannel) -> Arc<Collect> {
    let sink = Arc::new(Collect::default());
    let stop = CancellationToken::new();
    let closed = channel.closed();
    let run = channel.run(sink.clone(), stop.clone());
    tokio::pin!(run);
    tokio::select! {
        _ = &mut run => panic!("run ended before shutdown"),
        () = closed.cancelled() => {}
    }
    stop.cancel();
    run.await.unwrap();
    sink
}

fn text(s: &str) -> RenderedMessage {
    RenderedMessage { text: s.into() }
}

#[tokio::test]
async fn reads_lines_and_writes_replies() {
    let (channel, out) = console(Preset::Console, "hello\n\n  second line \n");
    let sink = read_all(&channel).await;
    let events = sink.events.lock().unwrap().clone();
    let texts: Vec<&str> = events.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["hello", "second line"]);
    assert!(events[0].is_direct);
    assert_eq!(events[0].conversation.room, "console");

    let to = ReplyTarget::to_event(&events[0]);
    channel.send(&to, &text("hi **there**")).await.unwrap();
    assert_eq!(out.text(), "hi **there**\n\n");
}

#[tokio::test]
async fn switches_users_rooms_and_threads() {
    let input = "\
:as alice
hi from alice
:room #team
@okbot, what is the price?
just chatting
:thread t1
in a thread
:dm
back
:as me
mine
:restricted
";
    let (channel, out) = console(Preset::Slack, input);
    let sink = read_all(&channel).await;
    let events = sink.events.lock().unwrap().clone();
    let got: Vec<_> = events
        .iter()
        .map(|e| {
            (
                e.author.id.as_str().to_string(),
                e.conversation.room.clone(),
                e.conversation.thread.clone(),
                e.is_direct,
                e.mentions_bot,
                e.restricted,
                e.text.clone(),
            )
        })
        .collect();
    let row = |user: &str, room: &str, thread: Option<&str>, direct, mention, text: &str| {
        (
            user.to_string(),
            room.to_string(),
            thread.map(str::to_string),
            direct,
            mention,
            false,
            text.to_string(),
        )
    };
    let mut restricted = row("me", "console", None, true, false, "");
    restricted.5 = true;
    assert_eq!(
        got,
        [
            row("alice", "dm-alice", None, true, false, "hi from alice"),
            row("alice", "team", None, false, true, "what is the price?"),
            row("alice", "team", None, false, false, "just chatting"),
            row("alice", "team", Some("t1"), false, false, "in a thread"),
            row("alice", "dm-alice", None, true, false, "back"),
            row("me", "console", None, true, false, "mine"),
            restricted,
        ]
    );
    insta::assert_snapshot!(out.text(), @r"
    [console] alice in dm alice (like slack)
    [console] alice in #team (like slack)
    [console] alice in #team/t1 (like slack)
    [console] alice in dm alice (like slack)
    [console] me in the direct chat (like slack)
    ");
}

#[tokio::test]
async fn attaches_files_to_the_next_message() {
    let (channel, _) = console(
        Preset::Console,
        ":attach shots/menu.PNG\n:attach https://x.test/price list.pdf\nsee these\nand this\n",
    );
    let sink = read_all(&channel).await;
    let events = sink.events.lock().unwrap().clone();
    assert_eq!(
        events[0].attachments,
        [
            Attachment::Image {
                url: "shots/menu.PNG".into(),
                caption: None
            },
            Attachment::File {
                url: "https://x.test/price list.pdf".into(),
                name: Some("price list.pdf".into())
            },
        ]
    );
    assert_eq!(events[1].attachments, []);
}

#[tokio::test]
async fn shows_rooms_edits_typing_and_prompts() {
    let (channel, out) = console(Preset::Discord, "");
    let dm = ReplyTarget {
        conversation: channel.open_direct(&UserId::new("me")).await.unwrap(),
        in_reply_to: None,
    };
    let team = ReplyTarget {
        conversation: ConversationKey::room(channel.id().clone(), "team"),
        in_reply_to: None,
    };
    channel.typing(&team).await.unwrap();
    channel.typing(&team).await.unwrap();
    let sent = channel.send(&team, &text("_Thinking…_")).await.unwrap();
    channel
        .edit(&sent, &text("The price is 10."))
        .await
        .unwrap();
    channel.send(&dm, &text("hello")).await.unwrap();
    let prompt = Prompt {
        id: "p1".into(),
        text: "Run `deploy`?".into(),
        buttons: vec![
            PromptButton {
                id: "allow".into(),
                label: "Allow".into(),
                style: ButtonStyle::default(),
                form: None,
                url: None,
            },
            PromptButton {
                id: "deny".into(),
                label: "Deny".into(),
                style: ButtonStyle::default(),
                form: Some(Form {
                    title: "Why?".into(),
                    fields: vec![FormField {
                        id: "reason".into(),
                        label: "Reason".into(),
                        style: FieldStyle::default(),
                        required: true,
                        placeholder: None,
                        min_len: None,
                        max_len: None,
                        value: None,
                    }],
                }),
                url: None,
            },
        ],
        allowed: vec![],
        cards: Vec::new(),
        select: None,
    };
    let sent = channel.prompt(&team, &prompt).await.unwrap();
    channel
        .close_prompt(&sent, &text("Run `deploy`? Allowed."), false)
        .await
        .unwrap();
    insta::assert_snapshot!(out.text(), @r"
    [#team · typing…]

    [#team · out-1]
    _Thinking…_

    [#team · edit out-1]
    The price is 10.

    [out-2]
    hello

    [#team · prompt out-3]
    Run `deploy`?

      [1] Allow
      [2] Deny (form: reason*)

    (:press <n> to answer)

    [#team · prompt out-3 closed]
    Run `deploy`? Allowed.

    ");
    assert_eq!(channel.state().prompts.len(), 0);
}

#[tokio::test]
async fn presses_prompt_buttons() {
    let (client, server) = tokio::io::duplex(4096);
    let out = Buf::default();
    let channel = ConsoleChannel::new(
        ConsoleOptions::new("me"),
        Box::new(BufReader::new(server)),
        Box::new(out.clone()),
    );
    let team = ReplyTarget {
        conversation: ConversationKey::room(channel.id().clone(), "team"),
        in_reply_to: None,
    };
    let form = Form {
        title: "Why?".into(),
        fields: vec![FormField {
            id: "reason".into(),
            label: "Reason".into(),
            style: FieldStyle::default(),
            required: true,
            placeholder: None,
            min_len: None,
            max_len: None,
            value: None,
        }],
    };
    let button = |id: &str, form: Option<Form>| PromptButton {
        id: id.into(),
        label: id.into(),
        style: ButtonStyle::default(),
        form,
        url: None,
    };
    let prompt = Prompt {
        id: "p1".into(),
        text: "Deploy?".into(),
        buttons: vec![button("allow", None), button("deny", Some(form))],
        allowed: vec![UserId::new("me")],
        cards: Vec::new(),
        select: None,
    };
    let sent = channel.prompt(&team, &prompt).await.unwrap();

    let mut client = client;
    client
        .write_all(
            b":room team\n:press 3\n:press 2\n:press 1 x=y\n:press 2 reason=late\n\
              :as bob\n:press 1\n:as me\n:dm\n:press 1\n",
        )
        .await
        .unwrap();
    drop(client);
    let sink = read_all(&channel).await;
    let answers = sink.answers.lock().unwrap().clone();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].prompt, "p1");
    assert_eq!(answers[0].button, "deny");
    assert_eq!(answers[0].values["reason"].expose(), "late");
    assert_eq!(answers[0].sent, sent);
    let notes: Vec<String> = out
        .text()
        .lines()
        .filter_map(|l| l.strip_prefix("[console] "))
        .map(str::to_string)
        .collect();
    assert_eq!(
        notes,
        [
            "me in #team (like console)",
            "the prompt has 2 button(s)",
            "fill in reason (:press 2 reason=...)",
            "`allow` has no form to fill in",
            "bob in #team (like console)",
            "this prompt is not for bob",
            "me in #team (like console)",
            "me in the direct chat (like console)",
            "no open prompt in this conversation",
        ]
    );
}

#[tokio::test]
async fn zalo_has_no_buttons_and_telegram_no_forms() {
    let prompt = |form: Option<Form>| Prompt {
        id: "p".into(),
        text: "?".into(),
        buttons: vec![PromptButton {
            id: "a".into(),
            label: "A".into(),
            style: ButtonStyle::default(),
            form,
            url: None,
        }],
        allowed: vec![],
        cards: Vec::new(),
        select: None,
    };
    let to = ReplyTarget {
        conversation: ConversationKey::room("console:local".parse().unwrap(), "console"),
        in_reply_to: None,
    };
    let (zalo, _) = console(Preset::Zalo, "");
    assert!(matches!(
        zalo.prompt(&to, &prompt(None)).await,
        Err(ChannelError::Unsupported(_))
    ));
    let (telegram, _) = console(Preset::Telegram, "");
    telegram.prompt(&to, &prompt(None)).await.unwrap();
    let form = Form {
        title: "f".into(),
        fields: vec![],
    };
    assert!(matches!(
        telegram.prompt(&to, &prompt(Some(form))).await,
        Err(ChannelError::Unsupported(_))
    ));
    assert_eq!(zalo.caps().dialect, MarkdownDialect::ZaloMd);
    assert_eq!("gchat".parse::<Preset>().unwrap(), Preset::GChat);
    assert!("irc".parse::<Preset>().is_err());
}

#[tokio::test]
async fn waits_for_the_bot() {
    let (channel, out) = console(Preset::Console, "hi\n:wait\nnext\n:wait 0\n");
    let channel = Arc::new(channel);
    let idle_calls = Arc::new(AtomicU64::new(0));
    let calls = idle_calls.clone();
    let answering = channel.clone();
    channel.set_idle_wait(Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        let channel = answering.clone();
        Box::pin(async move {
            let to = ReplyTarget {
                conversation: ConversationKey::room(channel.id().clone(), "console"),
                in_reply_to: None,
            };
            channel.send(&to, &text("answer")).await.unwrap();
        })
    }));
    let sink = read_all(&channel).await;
    assert_eq!(sink.events.lock().unwrap().len(), 2);
    assert_eq!(idle_calls.load(Ordering::SeqCst), 2);
    assert_eq!(out.text(), "answer\n\nanswer\n\n");
}

#[tokio::test]
async fn waits_for_the_next_message_without_an_idle_wait() {
    let (client, server) = tokio::io::duplex(1024);
    let out = Buf::default();
    let channel = Arc::new(ConsoleChannel::new(
        ConsoleOptions::new("me"),
        Box::new(BufReader::new(server)),
        Box::new(out.clone()),
    ));
    let mut client = client;
    client.write_all(b"hi\n:wait 1\n").await.unwrap();
    drop(client);
    let sink = read_all(&channel).await;
    assert_eq!(sink.events.lock().unwrap().len(), 1);
    assert_eq!(
        out.text(),
        "[console] still waiting for the bot; going on\n"
    );
}

/// A bot that answers each message at once with `reply(text)` (or shows a prompt for `ask`).
struct Bot {
    channel: std::sync::Weak<ConsoleChannel>,
    reply: fn(&str) -> String,
    seen: StdMutex<Vec<InboundEvent>>,
}

#[async_trait]
impl InboundSink for Bot {
    async fn deliver(&self, event: InboundEvent) -> Result<(), SinkClosed> {
        let channel = self.channel.upgrade().ok_or(SinkClosed)?;
        let to = ReplyTarget::to_event(&event);
        if event.text == "ask" {
            let prompt = Prompt {
                id: "p".into(),
                text: "Sure?".into(),
                buttons: vec![PromptButton {
                    id: "yes".into(),
                    label: "Yes".into(),
                    style: ButtonStyle::default(),
                    form: None,
                    url: None,
                }],
                allowed: vec![],
                cards: Vec::new(),
                select: None,
            };
            channel.prompt(&to, &prompt).await.unwrap();
        } else {
            channel
                .send(&to, &text(&(self.reply)(&event.text)))
                .await
                .unwrap();
        }
        self.seen.lock().unwrap().push(event);
        Ok(())
    }
}

/// Runs `input` against a [`Bot`]. The bot answers before `deliver` returns, so it is idle at once,
/// except after a prompt (it never is then, as a turn waiting for a button press).
async fn run_bot(
    input: &'static str,
    reply: fn(&str) -> String,
) -> (Arc<ConsoleChannel>, Buf, Vec<InboundEvent>) {
    let (channel, out) = console(Preset::Console, input);
    let channel = Arc::new(channel);
    let me = Arc::downgrade(&channel);
    channel.set_idle_wait(Arc::new(move || {
        let waiting = me
            .upgrade()
            .is_some_and(|c| c.transcript().iter().any(|s| s.prompt));
        Box::pin(async move {
            if waiting {
                std::future::pending::<()>().await;
            }
        })
    }));
    let bot = Arc::new(Bot {
        channel: Arc::downgrade(&channel),
        reply,
        seen: StdMutex::default(),
    });
    let stop = CancellationToken::new();
    let closed = channel.closed();
    {
        let run = channel.run(bot.clone(), stop.clone());
        tokio::pin!(run);
        tokio::select! {
            _ = &mut run => panic!("run ended before shutdown"),
            () = closed.cancelled() => {}
        }
        stop.cancel();
        run.await.unwrap();
    }
    let seen = bot.seen.lock().unwrap().clone();
    (channel, out, seen)
}

#[tokio::test]
async fn expect_checks_the_answer_to_the_last_message() {
    let input = "\
price?
:expect costs 10
:expect-not sold out
:expect nothing like this
other
:expect costs 10
";
    let (channel, out, _) = run_bot(input, |t| {
        if t == "price?" {
            "It costs 10.".into()
        } else {
            "Hm.".into()
        }
    })
    .await;
    assert_eq!(channel.failures(), 2);
    let text = out.text();
    let failed: Vec<&str> = text.lines().filter(|l| l.contains("FAILED")).collect();
    assert_eq!(
        failed,
        [
            "[console] FAILED :expect: missing `nothing like this` in the answer to the last message",
            "[console] FAILED :expect: missing `costs 10` in the answer to the last message",
        ]
    );
}

#[tokio::test]
async fn wait_comes_back_for_a_prompt() {
    let (channel, out, seen) =
        run_bot("ask\n:wait\n:expect Sure?\n:press 1\n", |_| String::new()).await;
    assert_eq!(channel.failures(), 0);
    assert_eq!(seen.len(), 1);
    assert!(out.text().contains("[1] Yes"));
}

#[tokio::test]
async fn pastes_several_lines_as_one_message() {
    let input = ":paste\nfirst line\n\n  indented\n:end\nafter\n:paste\n:end\n";
    let (_, _, seen) = run_bot(input, |_| "ok".into()).await;
    let texts: Vec<&str> = seen.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["first line\n\n  indented", "after"]);
}

#[tokio::test]
async fn lists_the_bots_commands() {
    let (channel, out) = console(Preset::Console, "");
    channel
        .set_commands(&[CommandInfo {
            name: "order".into(),
            description: "Track an order".into(),
            descriptions: vec![],
            params: vec![
                okbot_core::CommandParam {
                    name: "id".into(),
                    required: true,
                    rest: false,
                },
                okbot_core::CommandParam {
                    name: "note".into(),
                    required: false,
                    rest: true,
                },
            ],
        }])
        .await
        .unwrap();
    assert_eq!(
        out.text(),
        "[console] the bot's commands:\n[console]   /order <id> [note...]  Track an order\n"
    );
}

#[tokio::test]
async fn escapes_terminal_control_characters() {
    let (channel, out) = console(Preset::Console, "");
    let to = ReplyTarget {
        conversation: ConversationKey::room(channel.id().clone(), "console"),
        in_reply_to: None,
    };
    channel
        .send(&to, &text("a\u{1b}]52;c;ZXZpbA==\u{7}b\nnext"))
        .await
        .unwrap();
    assert_eq!(out.text(), "a\\u{1b}]52;c;ZXZpbA==\\u{7}b\nnext\n\n");
}

#[tokio::test]
async fn prints_cards_and_writes_charts_to_files() {
    let (channel, out) = console(Preset::Console, "");
    assert!(channel.caps().ui.cards && channel.caps().ui.images);
    assert!(
        !channel.caps().ui.buttons,
        "views keep their choices as text"
    );
    let dir = tempfile::tempdir().unwrap();
    channel.set_chart_dir(dir.path().to_path_buf());
    let to = ReplyTarget {
        conversation: channel.open_direct(&UserId::new("me")).await.unwrap(),
        in_reply_to: None,
    };
    let card = |image: Option<okbot_core::Image>| okbot_core::Card {
        title: Some("Weekly sales".into()),
        tone: okbot_core::Tone::Success,
        text: "Top: Phở bò".into(),
        image,
    };
    let image = okbot_core::Image {
        name: "chart.png".into(),
        alt: "Bar chart, Weekly sales".into(),
        width: 1200,
        height: 600,
        png: b"\x89PNG fake".to_vec(),
    };
    let prompt = Prompt {
        id: "v1a2b".into(),
        text: "This week:".into(),
        cards: vec![
            card(Some(image.clone())),
            okbot_core::Card {
                title: None,
                tone: okbot_core::Tone::Neutral,
                text: "No returns.".into(),
                image: None,
            },
        ],
        ..Prompt::default()
    };
    channel.prompt(&to, &prompt).await.unwrap();
    let path = dir.path().join("v1a2b-chart.png");
    assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG fake");
    // Read back from the store: no bytes, so nothing is written and no path is shown.
    let stored = Prompt {
        id: "v9".into(),
        cards: vec![card(Some(okbot_core::Image {
            png: Vec::new(),
            ..image
        }))],
        ..Prompt::default()
    };
    channel.prompt(&to, &stored).await.unwrap();
    assert!(!dir.path().join("v9-chart.png").exists());
    assert!(
        channel.state().prompts.is_empty(),
        "cards alone are not answerable"
    );

    let shown = out
        .text()
        .replace(&dir.path().display().to_string(), "<dir>");
    insta::assert_snapshot!(shown, @r"
    [prompt out-1]
    This week:

    **✅ Weekly sales**
    Top: Phở bò
    [chart: <dir>/v1a2b-chart.png] Bar chart, Weekly sales

    No returns.

    [prompt out-2]
    **✅ Weekly sales**
    Top: Phở bò

    ");
}
