//! Model/effort syntax supported by the pinned agent-call adapter.

use crate::Provider;
use okbot_core::AgentError;

/// Splits the model and effort for CLIs that support `model@effort`.
pub fn model_parts(provider: Provider, model: &str) -> (&str, Option<&str>) {
    if matches!(provider, Provider::Claude | Provider::Codex) {
        model
            .rsplit_once('@')
            .map_or((model, None), |(m, e)| (m, Some(e)))
    } else {
        (model, None)
    }
}

/// Validates adapter syntax without guessing model availability or calling a provider.
pub fn validate_model(provider: Provider, model: &str) -> Result<(), AgentError> {
    let invalid = |message: &str| AgentError::Failed {
        message: message.into(),
        retryable: false,
    };
    let (name, effort) = model_parts(provider, model);
    if name.trim().is_empty() {
        return Err(invalid("model must not be empty"));
    }
    if let Some(effort) = effort {
        let valid: &[&str] = match provider {
            Provider::Claude => &["low", "medium", "high", "xhigh", "max"],
            Provider::Codex => &["low", "medium", "high", "xhigh", "max", "ultra"],
            _ => &[],
        };
        if !valid.contains(&effort) {
            return Err(invalid(&format!(
                "unsupported effort `{effort}`; use {}",
                valid.join(", ")
            )));
        }
    }
    if provider == Provider::OpenCodeV2 && !name.contains('/') {
        return Err(invalid("OpenCode model must be provider/model[#variant]"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_syntax_matches_pinned_adapter() {
        for (provider, model, valid) in [
            (Provider::Claude, "sonnet@max", true),
            (Provider::Claude, "sonnet@ultra", false),
            (Provider::Codex, "test-model@ultra", true),
            (Provider::Codex, "@high", false),
            (Provider::Codex, "test@wrong", false),
            (Provider::OpenCodeV2, "vendor/model#high", true),
            (Provider::OpenCodeV2, "model", false),
            (Provider::Devin, "test", true),
        ] {
            assert_eq!(validate_model(provider, model).is_ok(), valid, "{model}");
        }
    }
}
