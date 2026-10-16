//! The okbot message pipeline: policy, conversation queues, context building and replies.
//!
//! ```text
//! channel ─► sink ─► dispatcher: dedupe → access policy → commands → store ─► conversation worker
//!                                                                              │
//!            channel ◄── render ◄── agent turn ◄── context (persona, knowledge, history)
//! ```
//!
//! The runtime depends only on ports from `okbot-core`; `okbot` provides the adapters.

mod command;
pub mod config;
mod context;
mod dispatch;
mod http;
mod live;
mod metrics;
mod policy;
mod schedule;
#[cfg(test)]
mod test_support;
mod turn;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use okbot_core::{
    AgentBackend, CancellationToken, Channel, ChannelId, Clock, ConversationStore, InboundEvent,
    InboundSink, KnowledgeProvider, MemoryStore, Renderer, SinkClosed, SystemClock, TaskStore,
    async_trait,
};
use tokio::sync::{Notify, mpsc};

pub use config::{ExtraMcp, OWNER, RuntimeConfig, ScheduleConfig};
pub use http::HttpOptions;

/// Building the runtime failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// A required port was not provided.
    #[error("missing {0}")]
    Missing(&'static str),
    /// Two channels share an id.
    #[error("duplicate channel id {0}")]
    DuplicateChannel(ChannelId),
}

/// Running the runtime failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// The HTTP server could not start.
    #[error("HTTP server on {addr}: {source}")]
    Http {
        /// Address.
        addr: SocketAddr,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
}

/// Counts events between delivery and the end of their processing.
#[derive(Debug, Default)]
pub(crate) struct InFlight {
    count: AtomicUsize,
    idle: Notify,
}

/// Decrements [`InFlight`] when dropped, so every path out of the pipeline is counted.
#[derive(Debug)]
pub(crate) struct InFlightGuard(Arc<InFlight>);

impl InFlight {
    fn enter(self: &Arc<Self>) -> InFlightGuard {
        self.count.fetch_add(1, Ordering::SeqCst);
        InFlightGuard(self.clone())
    }

    fn current(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

/// An event with its in-flight guard.
pub(crate) type Tracked = (InboundEvent, InFlightGuard);

/// Ports and settings shared by the dispatcher and the workers.
pub(crate) struct Shared {
    pub(crate) config: RuntimeConfig,
    pub(crate) store: Arc<dyn ConversationStore>,
    pub(crate) agent: Arc<dyn AgentBackend>,
    pub(crate) knowledge: Option<Arc<dyn KnowledgeProvider>>,
    pub(crate) memory: Option<Arc<dyn MemoryStore>>,
    pub(crate) tasks: Option<Arc<dyn TaskStore>>,
    pub(crate) renderer: Arc<dyn Renderer>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) channels: HashMap<ChannelId, Arc<dyn Channel>>,
    pub(crate) gate: policy::Gate,
    pub(crate) in_flight: Arc<InFlight>,
    pub(crate) metrics: metrics::Metrics,
}

impl Shared {
    /// The owner's accounts on a personal bot. Errors fail closed (no owner).
    pub(crate) async fn owners(&self) -> Vec<(ChannelId, okbot_core::UserId)> {
        if !self.config.personal {
            return Vec::new();
        }
        self.store.owners().await.unwrap_or_else(|error| {
            tracing::error!(%error, "cannot read the owner accounts");
            Vec::new()
        })
    }

    /// The person behind the author of `event`: the owner on a personal bot, or the person the
    /// operator linked the account to.
    pub(crate) async fn person_of(&self, event: &InboundEvent) -> Option<okbot_core::Person> {
        if self.config.personal {
            let mut owners = self.owners().await;
            let me = (event.conversation.channel.clone(), event.author.id.clone());
            // The console is the terminal of the machine the bot runs on: its user is the owner.
            let local = event.conversation.channel.platform() == "console";
            if local && !owners.contains(&me) {
                owners.push(me.clone());
            }
            return owners.contains(&me).then(|| okbot_core::Person {
                id: config::OWNER.to_string(),
                accounts: owners,
            });
        }
        self.config
            .person(&event.conversation.channel, &event.author.id)
            .cloned()
    }

    /// An administrator: listed in `[bot] admins`, or the owner of a personal bot.
    pub(crate) async fn is_admin(&self, event: &InboundEvent) -> bool {
        self.gate.is_admin(event) || (self.config.personal && self.person_of(event).await.is_some())
    }
}

/// Builds a [`Runtime`].
pub struct RuntimeBuilder {
    config: RuntimeConfig,
    store: Option<Arc<dyn ConversationStore>>,
    agent: Option<Arc<dyn AgentBackend>>,
    knowledge: Option<Arc<dyn KnowledgeProvider>>,
    memory: Option<Arc<dyn MemoryStore>>,
    tasks: Option<Arc<dyn TaskStore>>,
    renderer: Option<Arc<dyn Renderer>>,
    clock: Arc<dyn Clock>,
    channels: Vec<Arc<dyn Channel>>,
    http: Option<HttpOptions>,
}

impl RuntimeBuilder {
    /// Sets the conversation store (required).
    pub fn store(mut self, store: Arc<dyn ConversationStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Sets the agent backend (required).
    pub fn agent(mut self, agent: Arc<dyn AgentBackend>) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Sets the knowledge provider (optional; without it the bot answers from history only).
    pub fn knowledge(mut self, knowledge: Arc<dyn KnowledgeProvider>) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// Sets the memory store (optional; enables `/remember`, `/forget`, `/memories`).
    pub fn memory(mut self, memory: Arc<dyn MemoryStore>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Sets the task store (optional; enables `/tasks` and `/done`).
    pub fn tasks(mut self, tasks: Arc<dyn TaskStore>) -> Self {
        self.tasks = Some(tasks);
        self
    }

    /// Sets the renderer (required).
    pub fn renderer(mut self, renderer: Arc<dyn Renderer>) -> Self {
        self.renderer = Some(renderer);
        self
    }

    /// Replaces the system clock.
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Adds a channel.
    pub fn channel(mut self, channel: Arc<dyn Channel>) -> Self {
        self.channels.push(channel);
        self
    }

    /// Serves health and webhooks over HTTP.
    pub fn http(mut self, options: HttpOptions) -> Self {
        self.http = Some(options);
        self
    }

    /// Checks the configuration and builds the runtime.
    pub fn build(self) -> Result<Runtime, BuildError> {
        let mut channels = HashMap::new();
        for channel in self.channels {
            let id = channel.id().clone();
            if channels.insert(id.clone(), channel).is_some() {
                return Err(BuildError::DuplicateChannel(id));
            }
        }
        let (tx, rx) = mpsc::channel(1024);
        let in_flight = Arc::new(InFlight::default());
        let shared = Arc::new(Shared {
            gate: policy::Gate::new(self.config.access.clone()),
            config: self.config,
            store: self
                .store
                .ok_or(BuildError::Missing("conversation store"))?,
            agent: self.agent.ok_or(BuildError::Missing("agent backend"))?,
            knowledge: self.knowledge,
            memory: self.memory,
            tasks: self.tasks,
            renderer: self.renderer.ok_or(BuildError::Missing("renderer"))?,
            clock: self.clock,
            channels,
            in_flight: in_flight.clone(),
            metrics: metrics::Metrics::default(),
        });
        Ok(Runtime {
            shared,
            sink: Arc::new(EventSink { tx, in_flight }),
            rx,
            http: self.http,
        })
    }
}

/// Receives events from channels.
struct EventSink {
    tx: mpsc::Sender<Tracked>,
    in_flight: Arc<InFlight>,
}

#[async_trait]
impl InboundSink for EventSink {
    async fn deliver(&self, event: InboundEvent) -> Result<(), SinkClosed> {
        let guard = self.in_flight.enter();
        self.tx.send((event, guard)).await.map_err(|_| SinkClosed)
    }
}

/// Observes a running runtime.
#[derive(Clone)]
pub struct RuntimeHandle {
    sink: Arc<EventSink>,
}

impl RuntimeHandle {
    /// The sink channels deliver to (also usable to inject events in tests).
    pub fn sink(&self) -> Arc<dyn InboundSink> {
        self.sink.clone()
    }

    /// Resolves once every delivered event has been fully handled.
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.sink.in_flight.idle.notified();
            if self.sink.in_flight.current() == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// The message pipeline.
pub struct Runtime {
    shared: Arc<Shared>,
    sink: Arc<EventSink>,
    rx: mpsc::Receiver<Tracked>,
    http: Option<HttpOptions>,
}

impl Runtime {
    /// Starts building a runtime.
    pub fn builder(config: RuntimeConfig) -> RuntimeBuilder {
        RuntimeBuilder {
            config,
            store: None,
            agent: None,
            knowledge: None,
            memory: None,
            tasks: None,
            renderer: None,
            clock: Arc::new(SystemClock),
            channels: Vec::new(),
            http: None,
        }
    }

    /// A handle for observing the runtime after [`Runtime::run`] takes it.
    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            sink: self.sink.clone(),
        }
    }

    /// Runs channels, the HTTP server and the pipeline until `shutdown` is cancelled, then gives
    /// running turns [`RuntimeConfig::shutdown_grace`] to finish.
    pub async fn run(self, shutdown: CancellationToken) -> Result<(), RunError> {
        let Self {
            shared,
            sink,
            rx,
            http,
        } = self;
        let channel_stop = shutdown.child_token();
        let mut tasks = tokio::task::JoinSet::new();
        for channel in shared.channels.values() {
            let channel = channel.clone();
            let sink: Arc<dyn InboundSink> = sink.clone();
            let stop = channel_stop.clone();
            tasks.spawn(async move {
                let id = channel.id().clone();
                tracing::info!(channel = %id, "channel starting");
                match channel.run(sink, stop).await {
                    Ok(()) => tracing::info!(channel = %id, "channel stopped"),
                    Err(error) => tracing::error!(channel = %id, %error, "channel failed"),
                }
            });
        }
        if let Some(options) = http {
            let server = http::serve(options, shared.clone(), channel_stop.clone())
                .await
                .map_err(|(addr, source)| RunError::Http { addr, source })?;
            tasks.spawn(server);
        }
        if shared.tasks.is_some() && shared.config.schedule.enabled {
            let sink: Arc<dyn InboundSink> = sink.clone();
            tasks.spawn(schedule::run(shared.clone(), sink, channel_stop.clone()));
        }
        drop(sink);

        let work_stop = CancellationToken::new();
        dispatch::Dispatcher::new(shared.clone(), work_stop.clone())
            .run(rx, shutdown.clone())
            .await;

        // Shutdown: stop intake, let running turns finish within the grace period.
        channel_stop.cancel();
        let grace = shared.config.shutdown_grace;
        let in_flight = shared.in_flight.clone();
        let drained = tokio::time::timeout(grace, async {
            loop {
                let notified = in_flight.idle.notified();
                if in_flight.current() == 0 {
                    break;
                }
                notified.await;
            }
        })
        .await;
        if drained.is_err() {
            tracing::warn!(
                ?grace,
                "cancelling turns still running after the grace period"
            );
        }
        work_stop.cancel();
        while tasks.join_next().await.is_some() {}
        Ok(())
    }
}
