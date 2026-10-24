//! Live progress on platforms that edit messages: the "thinking…" message shows what the agent
//! is doing ("Searching the web…") and, with streaming backends, the answer as it is written.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use okbot_core::{AgentProgress, Channel, ProgressFn, RenderedMessage, Renderer, SentRef};

#[derive(Default)]
struct State {
    text: String,
    status: Option<&'static str>,
    changed: bool,
}

/// A short status for a tool name.
pub(crate) fn status_for(tool: &str) -> &'static str {
    let lower = tool.to_ascii_lowercase();
    if lower.contains("web") {
        "_Searching the web…_"
    } else if lower.contains("memory") {
        "_Updating my notes…_"
    } else if lower.contains("kb_") || lower.contains("data_") {
        "_Looking it up in the knowledge base…_"
    } else {
        "_Working…_"
    }
}

/// Edits `placeholder` with progress every `every` until dropped.
pub(crate) struct Live {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Live {
    pub(crate) fn start(
        channel: Arc<dyn Channel>,
        renderer: Arc<dyn Renderer>,
        placeholder: SentRef,
        every: Duration,
    ) -> (ProgressFn, Self) {
        let state = Arc::new(Mutex::new(State::default()));
        let sink = state.clone();
        let progress = ProgressFn::new(move |p| {
            let mut s = sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match p {
                AgentProgress::TextDelta(t) => s.text.push_str(&t),
                AgentProgress::Text(t) => {
                    if !s.text.is_empty() {
                        s.text.push_str("\n\n");
                    }
                    s.text.push_str(&t);
                }
                AgentProgress::ToolUse(name) => {
                    tracing::info!(tool = %name, "agent tool call");
                    s.status = Some(status_for(&name));
                    // Text written before a tool call was thinking aloud, not the answer.
                    s.text.clear();
                }
                _ => return,
            }
            s.changed = true;
        });
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let markdown = {
                    let mut s = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !s.changed {
                        continue;
                    }
                    s.changed = false;
                    if s.text.trim().is_empty() {
                        s.status.unwrap_or("_Thinking…_").to_string()
                    } else {
                        format!("{} …", s.text.trim_end())
                    }
                };
                // Only the first message's worth; the final answer is split properly later.
                let part = renderer
                    .render(&markdown, channel.caps())
                    .into_iter()
                    .next()
                    .unwrap_or(RenderedMessage { text: markdown });
                if let Err(error) = channel.edit(&placeholder, &part).await {
                    tracing::debug!(%error, "progress edit failed");
                }
            }
        });
        (progress, Self { task })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        assert_eq!(status_for("WebSearch"), "_Searching the web…_");
        assert_eq!(status_for("web_search"), "_Searching the web…_");
        assert_eq!(
            status_for("mcp__kb__kb_grep"),
            "_Looking it up in the knowledge base…_"
        );
        assert_eq!(status_for("mcp__bot__memory_save"), "_Updating my notes…_");
        assert_eq!(status_for("other"), "_Working…_");
    }
}
