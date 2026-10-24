#![allow(clippy::unwrap_used)] // test code: a panic is the failure report
//! Two bots of a local team delegate over Unix sockets; a fake runtime answers each task.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use okbot_core::{
    Budget, CancellationToken, Channel, Clock, ConversationStore, Delegation, DelegationContext,
    InboundEvent, InboundSink, Origin, PeerError, PeerTaskState, Peers, Prompt, PromptAnswer,
    PromptButton, RenderedMessage, ReplyTarget, SinkClosed, SystemClock, async_trait,
};
use okbot_peer::manifest::{Link, Manifest, Member};
use okbot_peer::{ManifestSource, Node, NodeConfig, SigningKey, TeamConfig};
use okbot_testkit::MemoryStore;

/// What the fake runtime does with a task.
#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    /// Answers at once.
    Echo,
    /// Asks for approval first, answers once approved.
    Approve,
    /// Never answers.
    Silent,
}

/// A runtime stand-in: answers each event through the channel it came from.
struct FakeRuntime {
    channel: OnceLock<Arc<dyn Channel>>,
    behaviour: Behaviour,
    events: Mutex<Vec<InboundEvent>>,
    answers: Mutex<Vec<PromptAnswer>>,
}

#[async_trait]
impl InboundSink for FakeRuntime {
    async fn deliver(&self, event: InboundEvent) -> Result<(), SinkClosed> {
        self.events.lock().unwrap().push(event.clone());
        let channel = self.channel.get().unwrap().clone();
        let to = ReplyTarget::to_event(&event);
        match self.behaviour {
            Behaviour::Echo => {
                tokio::spawn(async move {
                    let text = format!("done: {}", event.text);
                    channel.send(&to, &RenderedMessage { text }).await.unwrap();
                });
            }
            Behaviour::Approve => {
                tokio::spawn(async move {
                    let prompt = Prompt {
                        id: "p1".into(),
                        text: "Restart nginx?".into(),
                        buttons: vec![
                            PromptButton {
                                id: "allow".into(),
                                label: "Allow".into(),
                                style: okbot_core::ButtonStyle::Success,
                                form: None,
                            },
                            PromptButton {
                                id: "deny".into(),
                                label: "Deny".into(),
                                style: okbot_core::ButtonStyle::Danger,
                                form: None,
                            },
                        ],
                        allowed: vec![event.author.id.clone()],
                    };
                    channel.prompt(&to, &prompt).await.unwrap();
                });
            }
            Behaviour::Silent => {}
        }
        Ok(())
    }

    async fn answer(&self, answer: PromptAnswer) -> Result<(), SinkClosed> {
        self.answers.lock().unwrap().push(answer.clone());
        let channel = self.channel.get().unwrap().clone();
        tokio::spawn(async move {
            let message = RenderedMessage {
                text: String::new(),
            };
            channel
                .close_prompt(&answer.sent, &message, false)
                .await
                .unwrap();
            let text = format!("restarted after {}", answer.button);
            channel
                .send(&answer.sent.target, &RenderedMessage { text })
                .await
                .unwrap();
        });
        Ok(())
    }
}

struct Bot {
    node: Node,
    runtime: Arc<FakeRuntime>,
    store: Arc<MemoryStore>,
}

fn bot_dir(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    SigningKey::for_bot(&dir.join("state")).unwrap();
    dir
}

fn manifest(root: &Path, names: &[&str], links: Vec<Link>) -> PathBuf {
    let mut m = Manifest::new("local");
    m.team.version = 1;
    for name in names {
        m.members.insert(
            (*name).to_string(),
            Member {
                dir: Some(root.join(name)),
                roles: vec![
                    if *name == "assistant" {
                        "main"
                    } else {
                        "member"
                    }
                    .into(),
                ],
                skills: vec![format!("{name}-skill")],
                ..Member::default()
            },
        );
    }
    m.links = links;
    let path = root.join("local.toml");
    std::fs::write(&path, m.to_toml()).unwrap();
    path
}

async fn start(
    root: &Path,
    name: &str,
    manifest: &Path,
    accept: bool,
    behaviour: Behaviour,
) -> Bot {
    let dir = root.join(name);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = Arc::new(MemoryStore::new(clock.clone()));
    let node = Node::new(
        NodeConfig {
            key: SigningKey::for_bot(&dir.join("state")).unwrap(),
            state_dir: dir.join("state"),
            teams: vec![TeamConfig {
                id: "local".into(),
                member: name.into(),
                source: ManifestSource::File(manifest.to_path_buf()),
                accept,
                max_depth: 2,
                max_concurrent: 2,
                task_timeout: Duration::from_secs(60),
            }],
        },
        store.clone(),
        clock,
    )
    .unwrap();
    assert_eq!(node.start().await, [] as [std::string::String; 0]);
    let runtime = Arc::new(FakeRuntime {
        channel: OnceLock::new(),
        behaviour,
        events: Mutex::default(),
        answers: Mutex::default(),
    });
    if let Some(channel) = node.channels().into_iter().next() {
        runtime.channel.set(channel.clone()).ok().unwrap();
        let sink: Arc<dyn InboundSink> = runtime.clone();
        tokio::spawn(async move { channel.run(sink, CancellationToken::new()).await });
        let socket = dir.join("state").join(okbot_peer::SOCKET_FILE);
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    Bot {
        node,
        runtime,
        store,
    }
}

fn delegation(to: &str, message: &str, chain: Vec<String>) -> Delegation {
    Delegation {
        team: None,
        to: to.parse().unwrap(),
        task: "Find the release notes".into(),
        context: DelegationContext {
            origin: Origin {
                bot: String::new(),
                conversation: "zalo:shop|-|123|-".into(),
                person: Some("lan".into()),
                name: "Lan".into(),
            },
            chain,
            deadline: SystemClock.now() + jiff::SignedDuration::from_secs(300),
            budget: Budget { turns: 4 },
            trace: "trace-1".into(),
        },
        wait: Duration::from_secs(5),
        message_id: message.into(),
    }
}

fn links(from: &str, to: &str) -> Vec<Link> {
    vec![Link {
        from: vec![from.into()],
        to: vec![to.into()],
    }]
}

#[tokio::test]
async fn a_task_is_answered_once_and_retries_return_it() {
    let root = tempfile::tempdir().unwrap();
    for name in ["assistant", "research"] {
        bot_dir(root.path(), name);
    }
    let file = manifest(
        root.path(),
        &["assistant", "research"],
        links("role:main", "*"),
    );
    let assistant = start(root.path(), "assistant", &file, false, Behaviour::Echo).await;
    let research = start(root.path(), "research", &file, true, Behaviour::Echo).await;

    let directory = assistant.node.directory().await.unwrap();
    assert_eq!(directory.len(), 1);
    assert_eq!(directory[0].name, "research");

    let task = assistant
        .node
        .delegate(delegation("skill:research-skill", "m-1", Vec::new()))
        .await
        .unwrap();
    assert_eq!(task.state, PeerTaskState::Completed);
    assert_eq!(task.text.as_deref(), Some("done: Find the release notes"));

    let events = research.runtime.events.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.conversation.channel.to_string(), "peer:local");
    assert!(event.conversation.room.starts_with("assistant/"));
    assert_eq!(event.author.id.as_str(), "assistant");
    let caller = research.node.caller(&event.conversation).await;
    assert!(caller.is_none(), "forgotten once the task ended");

    // The same message again: the same task, no second turn.
    let again = assistant
        .node
        .delegate(delegation("research", "m-1", Vec::new()))
        .await
        .unwrap();
    assert_eq!(again.id, task.id);
    assert_eq!(research.runtime.events.lock().unwrap().len(), 1);

    let outbound = assistant
        .store
        .peer_task(false, &task.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outbound.state, PeerTaskState::Completed);
    assert_eq!(outbound.trace, "trace-1");
    let inbound = research
        .store
        .peer_task(true, &task.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inbound.peer, "assistant");
}

#[tokio::test]
async fn links_chains_and_acceptance_are_enforced() {
    let root = tempfile::tempdir().unwrap();
    for name in ["assistant", "research", "ops"] {
        bot_dir(root.path(), name);
    }
    let file = manifest(
        root.path(),
        &["assistant", "research", "ops"],
        links("role:main", "*"),
    );
    let assistant = start(root.path(), "assistant", &file, true, Behaviour::Echo).await;
    let research = start(root.path(), "research", &file, true, Behaviour::Echo).await;
    let _ops = start(root.path(), "ops", &file, false, Behaviour::Echo).await;

    // research has no link to ops: nothing to route to.
    let refused = research
        .node
        .delegate(delegation("ops", "m-1", Vec::new()))
        .await;
    assert!(matches!(refused, Err(PeerError::NoRoute(_))), "{refused:?}");

    // ops does not accept tasks, and says so.
    let refused = assistant
        .node
        .delegate(delegation("ops", "m-2", Vec::new()))
        .await;
    assert!(
        matches!(&refused, Err(PeerError::Refused(m)) if m.contains("does not take tasks")),
        "{refused:?}"
    );

    // A loop: the work already passed through research.
    let refused = assistant
        .node
        .delegate(delegation("research", "m-3", vec!["research".into()]))
        .await;
    assert!(
        matches!(&refused, Err(PeerError::Refused(m)) if m.contains("loop")),
        "{refused:?}"
    );

    // Too long a chain (max_depth 2: two bots before research).
    let refused = assistant
        .node
        .delegate(delegation("research", "m-4", vec!["x".into(), "y".into()]))
        .await;
    assert!(
        matches!(&refused, Err(PeerError::Refused(m)) if m.contains("longer")),
        "{refused:?}"
    );

    // A deadline already past.
    let mut late = delegation("research", "m-5", Vec::new());
    late.context.deadline = SystemClock.now() - jiff::SignedDuration::from_secs(1);
    let refused = assistant.node.delegate(late).await;
    assert!(
        matches!(&refused, Err(PeerError::Refused(m)) if m.contains("deadline")),
        "{refused:?}"
    );
    assert!(
        research.runtime.events.lock().unwrap().is_empty(),
        "no turn started"
    );
}

#[tokio::test]
async fn a_question_goes_back_to_the_caller_and_the_answer_continues_the_task() {
    let root = tempfile::tempdir().unwrap();
    for name in ["assistant", "ops"] {
        bot_dir(root.path(), name);
    }
    let file = manifest(root.path(), &["assistant", "ops"], links("role:main", "*"));
    let assistant = start(root.path(), "assistant", &file, false, Behaviour::Echo).await;
    let ops = start(root.path(), "ops", &file, true, Behaviour::Approve).await;

    let task = assistant
        .node
        .delegate(delegation("ops", "m-1", Vec::new()))
        .await
        .unwrap();
    assert_eq!(task.state, PeerTaskState::InputRequired);
    assert_eq!(task.text.as_deref(), Some("Restart nginx?"));
    let prompt = task.prompt.clone().unwrap();
    assert_eq!(prompt.buttons[0], ("allow".into(), "Allow".into()));

    assert!(
        assistant
            .node
            .answer(&task, &prompt.id, "nope")
            .await
            .is_err()
    );
    assistant
        .node
        .answer(&task, &prompt.id, "allow")
        .await
        .unwrap();
    let mut done = task.clone();
    for _ in 0..50 {
        done = assistant
            .node
            .status("local", "ops", &task.id)
            .await
            .unwrap();
        if done.state.is_final() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(done.state, PeerTaskState::Completed);
    assert_eq!(done.text.as_deref(), Some("restarted after allow"));
    let answers = ops.runtime.answers.lock().unwrap().clone();
    assert_eq!(answers.len(), 1);
    assert_eq!(
        answers[0].user.id.as_str(),
        "assistant",
        "the caller answers, as requester"
    );
}

#[tokio::test]
async fn a_task_that_runs_long_keeps_running_and_concurrency_is_capped() {
    let root = tempfile::tempdir().unwrap();
    for name in ["assistant", "slow"] {
        bot_dir(root.path(), name);
    }
    let file = manifest(root.path(), &["assistant", "slow"], links("role:main", "*"));
    let assistant = start(root.path(), "assistant", &file, false, Behaviour::Echo).await;
    let _slow = start(root.path(), "slow", &file, true, Behaviour::Silent).await;

    let mut quick = delegation("slow", "m-1", Vec::new());
    quick.wait = Duration::from_millis(200);
    let task = assistant.node.delegate(quick.clone()).await.unwrap();
    assert_eq!(task.state, PeerTaskState::Working);
    quick.message_id = "m-2".into();
    assistant.node.delegate(quick.clone()).await.unwrap();
    quick.message_id = "m-3".into();
    let busy = assistant.node.delegate(quick).await;
    assert!(
        matches!(&busy, Err(PeerError::Refused(m)) if m.contains("busy")),
        "{busy:?}"
    );
}

#[tokio::test]
async fn a_stopped_bot_is_unreachable() {
    let root = tempfile::tempdir().unwrap();
    for name in ["assistant", "gone"] {
        bot_dir(root.path(), name);
    }
    let file = manifest(root.path(), &["assistant", "gone"], links("role:main", "*"));
    let assistant = start(root.path(), "assistant", &file, false, Behaviour::Echo).await;
    let result = assistant
        .node
        .delegate(delegation("gone", "m-1", Vec::new()))
        .await;
    assert!(
        matches!(result, Err(PeerError::Unreachable(_))),
        "{result:?}"
    );
    assert!(matches!(
        assistant
            .node
            .delegate(delegation("nobody", "m-2", Vec::new()))
            .await,
        Err(PeerError::NoRoute(_))
    ));
}

/// Serves a channel's webhook (as the runtime's HTTP server does) on a loopback port.
async fn serve_http(channel: Arc<dyn Channel>) -> String {
    let handler = channel.webhook().unwrap();
    let path = format!("/{}", handler.path());
    let app = axum::Router::new().route(
        &path,
        axum::routing::any(move |request: axum::extract::Request| {
            let handler = handler.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = axum::body::to_bytes(body, 1 << 22).await.unwrap();
                let reply = handler
                    .handle(okbot_core::WebhookRequest {
                        method: parts.method.to_string(),
                        path: String::new(),
                        headers: parts
                            .headers
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
                            .collect(),
                        body: body.to_vec(),
                    })
                    .await;
                let mut response = axum::response::Response::builder().status(reply.status);
                for (k, v) in reply.headers {
                    response = response.header(k, v);
                }
                response.body(axum::body::Body::from(reply.body)).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}{path}")
}

#[tokio::test]
async fn members_on_other_machines_talk_over_https_endpoints() {
    let root = tempfile::tempdir().unwrap();
    for name in ["assistant", "writer"] {
        bot_dir(root.path(), name);
    }
    // The writer runs elsewhere: started first to learn its URL, then named in the manifest.
    let file = manifest(
        root.path(),
        &["assistant", "writer"],
        links("role:main", "*"),
    );
    let writer = start(root.path(), "writer", &file, true, Behaviour::Echo).await;
    let url = serve_http(writer.runtime.channel.get().unwrap().clone()).await;
    let mut m = Manifest::parse(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let entry = m.members.get_mut("writer").unwrap();
    entry.key = Some(okbot_peer::key::bot_public_key(&root.path().join("writer")).unwrap());
    entry.dir = None;
    entry.url = Some(url.clone());
    m.team.version = 2;
    std::fs::write(&file, m.to_toml()).unwrap();
    let assistant = start(root.path(), "assistant", &file, false, Behaviour::Echo).await;

    let task = assistant
        .node
        .delegate(delegation("writer", "m-1", Vec::new()))
        .await
        .unwrap();
    assert_eq!(task.text.as_deref(), Some("done: Find the release notes"));

    // Without a valid token, nothing.
    let plain = reqwest::Client::new()
        .post(&url)
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(plain.status().as_u16(), 401);
    assert!(plain.headers().get(okbot_peer::SIGNATURE_HEADER).is_none());
}

#[tokio::test]
async fn a_member_behind_nat_is_reached_through_its_relay() {
    let root = tempfile::tempdir().unwrap();
    for name in ["assistant", "lead", "research"] {
        bot_dir(root.path(), name);
    }
    let file = manifest(
        root.path(),
        &["assistant", "lead", "research"],
        vec![Link {
            from: vec!["*".into()],
            to: vec!["*".into()],
        }],
    );
    // The lead has a public endpoint; research only reaches out to it.
    let lead = start(root.path(), "lead", &file, true, Behaviour::Echo).await;
    let url = serve_http(lead.runtime.channel.get().unwrap().clone()).await;
    let mut m = Manifest::parse(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let key = |name: &str| okbot_peer::key::bot_public_key(&root.path().join(name)).unwrap();
    let lead_entry = m.members.get_mut("lead").unwrap();
    lead_entry.key = Some(key("lead"));
    lead_entry.dir = None;
    lead_entry.url = Some(url);
    let research_entry = m.members.get_mut("research").unwrap();
    research_entry.key = Some(key("research"));
    research_entry.dir = None;
    research_entry.relay = Some("lead".into());
    m.team.version = 2;
    std::fs::write(&file, m.to_toml()).unwrap();
    let research = start(root.path(), "research", &file, true, Behaviour::Echo).await;
    let assistant = start(root.path(), "assistant", &file, false, Behaviour::Echo).await;
    // The lead reads the new manifest when the first request names version 2.

    let task = assistant
        .node
        .delegate(delegation("research", "m-1", Vec::new()))
        .await
        .unwrap();
    assert_eq!(task.state, PeerTaskState::Completed, "{task:?}");
    assert_eq!(task.text.as_deref(), Some("done: Find the release notes"));
    assert_eq!(research.runtime.events.lock().unwrap().len(), 1);

    // The relay itself delegates to the member it relays.
    let from_lead = lead
        .node
        .delegate(delegation("research", "m-2", Vec::new()))
        .await
        .unwrap();
    assert_eq!(from_lead.state, PeerTaskState::Completed, "{from_lead:?}");
}
