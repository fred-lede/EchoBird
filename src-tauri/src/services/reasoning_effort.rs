//! Mother Agent capabilities shared with its composer. Unknown models keep
//! their upstream defaults; explicit unsupported choices fail before HTTP.
use super::llm_client::{LlmConfig, LlmProvider};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::OnceLock;

#[derive(Deserialize)]
struct Profile {
    models: Vec<String>,
    protocols: Vec<String>,
    levels: Vec<String>,
    thinking: Option<String>,
}

fn profile(config: &LlmConfig) -> Option<&'static Profile> {
    let endpoint = reqwest::Url::parse(&config.base_url).ok()?;
    if endpoint.host_str() == Some("openrouter.ai") {
        return None;
    }
    static PROFILES: OnceLock<Vec<Profile>> = OnceLock::new();
    let profiles = PROFILES.get_or_init(|| {
        serde_json::from_str(include_str!("../../../src/data/motherReasoning.json"))
            .expect("valid bundled reasoning capabilities")
    });
    let protocol = match config.provider {
        LlmProvider::OpenAI => "openai",
        LlmProvider::Anthropic => "anthropic",
    };
    let mut id = config.model.trim().to_ascii_lowercase();
    // DeepSeek's Messages endpoint maps Claude aliases to its own models.
    if endpoint.host_str() == Some("api.deepseek.com")
        && config.provider == LlmProvider::Anthropic
        && id.starts_with("claude-")
    {
        id = if id.starts_with("claude-opus-") {
            "deepseek-v4-pro"
        } else {
            "deepseek-flash"
        }
        .into();
    }
    profiles.iter().find(|profile| {
        profile.protocols.iter().any(|p| p == protocol)
            && profile.models.iter().any(|model| {
                id == *model
                    || id
                        .strip_prefix(model.as_str())
                        .and_then(|suffix| suffix.strip_prefix('-'))
                        .is_some_and(|suffix| {
                            let digits = suffix.replace('-', "");
                            (suffix.len() == 8
                                || (suffix.len() == 10
                                    && suffix.as_bytes()[4] == b'-'
                                    && suffix.as_bytes()[7] == b'-'))
                                && digits.len() == 8
                                && digits.bytes().all(|b| b.is_ascii_digit())
                        })
            })
    })
}

pub(crate) fn supports_native_anthropic_effort(base_url: &str, model: &str, effort: &str) -> bool {
    let Ok(endpoint) = reqwest::Url::parse(base_url) else {
        return false;
    };
    if endpoint.origin().ascii_serialization() != "https://api.anthropic.com" {
        return false;
    }
    let config = LlmConfig {
        provider: LlmProvider::Anthropic,
        base_url: base_url.into(),
        model: model.into(),
        api_key: String::new(),
        reasoning_effort: None,
    };
    profile(&config).is_some_and(|profile| profile.levels.iter().any(|level| level == effort))
}

pub fn apply(body: &mut Value, config: &LlmConfig) -> Result<(), String> {
    let Some(effort) = config.reasoning_effort.as_deref() else {
        return Ok(());
    };
    let profile = profile(config)
        .filter(|p| p.levels.iter().any(|level| level == effort))
        .ok_or_else(|| {
            format!(
                "Reasoning effort '{}' is not supported for {} via {:?}",
                effort, config.model, config.provider
            )
        })?;
    match config.provider {
        LlmProvider::OpenAI => body["reasoning_effort"] = json!(effort),
        LlmProvider::Anthropic => {
            body["output_config"] = json!({ "effort": effort });
            // The existing 4K output budget includes thinking and would leave
            // little room for an answer or tool arguments at higher effort.
            body["max_tokens"] = json!(32768);
        }
    }
    if let Some(thinking) = &profile.thinking {
        body["thinking"] = json!({ "type": thinking });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(model: &str, provider: LlmProvider, effort: Option<&str>) -> LlmConfig {
        LlmConfig {
            model: model.into(),
            provider,
            base_url: "https://api.example.test/v1".into(),
            api_key: "fixture".into(),
            reasoning_effort: effort.map(str::to_owned),
        }
    }

    #[test]
    fn default_preserves_payload_for_known_and_unknown_models() {
        for model in ["deepseek-flash", "claude-opus-5", "unknown"] {
            for provider in [LlmProvider::OpenAI, LlmProvider::Anthropic] {
                let mut body = json!({ "max_tokens": 4096, "messages": [] });
                let original = body.clone();
                apply(&mut body, &config(model, provider, None)).unwrap();
                assert_eq!(body, original);
            }
        }
    }

    #[test]
    fn unsupported_choices_fail_without_changing_payload() {
        for (model, provider, effort) in [
            ("deepseek-flash", LlmProvider::OpenAI, "medium"),
            ("claude-opus-4-6", LlmProvider::Anthropic, "xhigh"),
            ("claude-opus-4-5", LlmProvider::Anthropic, "max"),
            ("claude-opus-5", LlmProvider::OpenAI, "high"),
            ("gpt-6.1-sol", LlmProvider::OpenAI, "high"),
            ("gpt-5.2", LlmProvider::OpenAI, "max"),
            ("unknown", LlmProvider::OpenAI, "high"),
            ("gpt-5.2", LlmProvider::OpenAI, "invalid"),
        ] {
            let mut body = json!({ "messages": [] });
            assert!(apply(&mut body, &config(model, provider, Some(effort))).is_err());
            assert_eq!(body, json!({ "messages": [] }));
        }
    }

    #[test]
    fn provider_specific_thinking_and_effort_are_separate() {
        let mut body = json!({ "max_tokens": 4096 });
        apply(
            &mut body,
            &config("deepseek-v4-pro", LlmProvider::Anthropic, Some("max")),
        )
        .unwrap();
        assert_eq!(body["thinking"], json!({ "type": "enabled" }));
        assert_eq!(body["output_config"], json!({ "effort": "max" }));
        assert_eq!(body["max_tokens"], 32768);
        let mut body = json!({});
        apply(
            &mut body,
            &config("claude-opus-4-5", LlmProvider::Anthropic, Some("high")),
        )
        .unwrap();
        assert!(body.get("thinking").is_none());
        assert_eq!(body["output_config"]["effort"], "high");
    }

    #[test]
    fn snapshots_match_exact_families_and_gateway_stays_default() {
        for id in ["gpt-5.2", "gpt-5.2-2025-12-11", "claude-opus-4-6-20260205"] {
            let provider = if id.starts_with("claude") {
                LlmProvider::Anthropic
            } else {
                LlmProvider::OpenAI
            };
            assert!(profile(&config(id, provider, None)).is_some());
        }
        for id in [
            "gpt-5.2-chat-latest",
            "gpt-5.2-pro",
            "gpt-5.20",
            "gpt-5.2-2025-1211",
        ] {
            assert!(profile(&config(id, LlmProvider::OpenAI, None)).is_none());
        }
        let mut cfg = config("gpt-5.2", LlmProvider::OpenAI, Some("high"));
        cfg.base_url = "https://openrouter.ai/api/v1".into();
        assert!(apply(&mut json!({}), &cfg).is_err());
    }

    #[test]
    fn messages_aliases_use_the_actual_provider_effort_and_thinking_mode() {
        let mut cfg = config("claude-opus-5", LlmProvider::Anthropic, Some("max"));
        cfg.base_url = "https://api.deepseek.com/anthropic".into();
        let mut body = json!({});
        apply(&mut body, &cfg).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        cfg.reasoning_effort = Some("medium".into());
        assert!(apply(&mut json!({}), &cfg).is_err());
        cfg.base_url = "https://api.anthropic.com".into();
        let mut body = json!({});
        apply(&mut body, &cfg).unwrap();
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "medium");
    }
}
