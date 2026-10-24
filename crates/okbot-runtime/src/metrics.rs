//! Counters for `GET /metrics` (Prometheus text format).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Outcome {
    Answered,
    Failed,
    Cancelled,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Default)]
struct TurnStats {
    count: u64,
    seconds: f64,
}

/// Process-wide counters.
#[derive(Default)]
pub(crate) struct Metrics {
    events: AtomicU64,
    duplicates: AtomicU64,
    ignored: AtomicU64,
    turns: Mutex<BTreeMap<(String, Outcome), TurnStats>>,
    /// Delegations by team and outcome (ADR 0014).
    delegations: Mutex<BTreeMap<(String, String), TurnStats>>,
    /// Tasks from other bots by team and outcome.
    peer_tasks: Mutex<BTreeMap<(String, Outcome), u64>>,
}

fn label(text: &str) -> String {
    text.replace(['"', '\\', '\n'], "_")
}

impl Metrics {
    pub(crate) fn event(&self) {
        self.events.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn duplicate(&self) {
        self.duplicates.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn ignored(&self) {
        self.ignored.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn turn(&self, backend: &str, outcome: Outcome, took: Duration) {
        let mut turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stats = turns.entry((backend.to_string(), outcome)).or_default();
        stats.count += 1;
        stats.seconds += took.as_secs_f64();
    }

    /// A delegation ended: `outcome` is the task's state, or why it did not start.
    pub(crate) fn delegation(&self, team: &str, outcome: &str, took: Duration) {
        let mut all = self
            .delegations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stats = all
            .entry((team.to_string(), outcome.to_string()))
            .or_default();
        stats.count += 1;
        stats.seconds += took.as_secs_f64();
    }

    /// A turn for a task from another bot ended.
    pub(crate) fn peer_task(&self, team: &str, outcome: Outcome) {
        *self
            .peer_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry((team.to_string(), outcome))
            .or_default() += 1;
    }

    /// The Prometheus exposition text; `in_flight` is the current queue depth.
    pub(crate) fn render(&self, bot: &str, in_flight: usize) -> String {
        let mut out = String::new();
        let bot = bot.replace(['"', '\\', '\n'], "_");
        let counter = |out: &mut String, name: &str, help: &str, value: u64| {
            let _ = writeln!(
                out,
                "# HELP {name} {help}\n# TYPE {name} counter\n{name}{{bot=\"{bot}\"}} {value}"
            );
        };
        counter(
            &mut out,
            "okbot_events_total",
            "Events received from channels.",
            self.events.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "okbot_duplicate_events_total",
            "Events dropped as duplicates.",
            self.duplicates.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "okbot_ignored_events_total",
            "Events not addressed to the bot or not allowed.",
            self.ignored.load(Ordering::Relaxed),
        );
        let _ = writeln!(
            out,
            "# HELP okbot_in_flight Events received and not yet fully handled.\n# TYPE okbot_in_flight gauge\nokbot_in_flight{{bot=\"{bot}\"}} {in_flight}"
        );
        let turns = self
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(
            out,
            "# HELP okbot_turns_total Agent turns by backend and outcome.\n# TYPE okbot_turns_total counter"
        );
        for ((backend, outcome), stats) in turns.iter() {
            let backend = backend.replace(['"', '\\', '\n'], "_");
            let _ = writeln!(
                out,
                "okbot_turns_total{{bot=\"{bot}\",backend=\"{backend}\",outcome=\"{}\"}} {}",
                outcome.label(),
                stats.count
            );
        }
        let _ = writeln!(
            out,
            "# HELP okbot_turn_seconds_sum Time spent in agent turns.\n# TYPE okbot_turn_seconds_sum counter"
        );
        for ((backend, outcome), stats) in turns.iter() {
            let backend = backend.replace(['"', '\\', '\n'], "_");
            let _ = writeln!(
                out,
                "okbot_turn_seconds_sum{{bot=\"{bot}\",backend=\"{backend}\",outcome=\"{}\"}} {:.3}",
                outcome.label(),
                stats.seconds
            );
        }
        drop(turns);
        self.render_teams(&mut out, &bot);
        out
    }

    fn render_teams(&self, out: &mut String, bot: &str) {
        let delegations = self
            .delegations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !delegations.is_empty() {
            let _ = writeln!(
                out,
                "# HELP okbot_delegations_total Tasks handed to other bots, by team and outcome.\n# TYPE okbot_delegations_total counter"
            );
            for ((team, outcome), stats) in delegations.iter() {
                let _ = writeln!(
                    out,
                    "okbot_delegations_total{{bot=\"{bot}\",team=\"{}\",outcome=\"{}\"}} {}",
                    label(team),
                    label(outcome),
                    stats.count
                );
            }
            let _ = writeln!(
                out,
                "# HELP okbot_delegation_seconds_sum Time agents waited for other bots.\n# TYPE okbot_delegation_seconds_sum counter"
            );
            for ((team, outcome), stats) in delegations.iter() {
                let _ = writeln!(
                    out,
                    "okbot_delegation_seconds_sum{{bot=\"{bot}\",team=\"{}\",outcome=\"{}\"}} {:.3}",
                    label(team),
                    label(outcome),
                    stats.seconds
                );
            }
        }
        let tasks = self
            .peer_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !tasks.is_empty() {
            let _ = writeln!(
                out,
                "# HELP okbot_peer_tasks_total Turns for tasks from other bots, by team and outcome.\n# TYPE okbot_peer_tasks_total counter"
            );
            for ((team, outcome), count) in tasks.iter() {
                let _ = writeln!(
                    out,
                    "okbot_peer_tasks_total{{bot=\"{bot}\",team=\"{}\",outcome=\"{}\"}} {count}",
                    label(team),
                    outcome.label()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_prometheus_text() {
        let m = Metrics::default();
        m.event();
        m.event();
        m.duplicate();
        m.turn("claude", Outcome::Answered, Duration::from_millis(1500));
        m.turn("claude", Outcome::Answered, Duration::from_millis(500));
        m.turn("codex", Outcome::Failed, Duration::from_secs(3));
        let text = m.render("shop", 1);
        assert!(text.contains("okbot_events_total{bot=\"shop\"} 2"));
        assert!(text.contains("okbot_duplicate_events_total{bot=\"shop\"} 1"));
        assert!(text.contains("okbot_in_flight{bot=\"shop\"} 1"));
        assert!(
            text.contains(
                "okbot_turns_total{bot=\"shop\",backend=\"claude\",outcome=\"answered\"} 2"
            )
        );
        assert!(text.contains(
            "okbot_turn_seconds_sum{bot=\"shop\",backend=\"codex\",outcome=\"failed\"} 3.000"
        ));
        assert!(!text.contains("okbot_delegations_total"), "only once used");
        m.delegation("acme", "completed", Duration::from_secs(2));
        m.delegation("acme", "unreachable", Duration::from_millis(500));
        m.peer_task("acme", Outcome::Answered);
        let text = m.render("shop", 0);
        assert!(text.contains(
            "okbot_delegations_total{bot=\"shop\",team=\"acme\",outcome=\"completed\"} 1"
        ));
        assert!(text.contains(
            "okbot_delegation_seconds_sum{bot=\"shop\",team=\"acme\",outcome=\"unreachable\"} 0.500"
        ));
        assert!(
            text.contains(
                "okbot_peer_tasks_total{bot=\"shop\",team=\"acme\",outcome=\"answered\"} 1"
            )
        );
    }
}
