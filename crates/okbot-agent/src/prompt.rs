//! Prompt helpers.

/// Combines a system prompt and a user prompt for backends without a separate system prompt.
///
/// The system part is fenced so the agent can tell instructions from the conversation.
pub fn inline_system_prompt(system: &str, prompt: &str) -> String {
    if system.trim().is_empty() {
        return prompt.to_string();
    }
    format!(
        "<instructions>\n{}\n</instructions>\n\n{}",
        system.trim(),
        prompt
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inlines_system() {
        assert_eq!(inline_system_prompt("", "hi"), "hi");
        assert_eq!(
            inline_system_prompt("be nice", "hi"),
            "<instructions>\nbe nice\n</instructions>\n\nhi"
        );
    }
}
