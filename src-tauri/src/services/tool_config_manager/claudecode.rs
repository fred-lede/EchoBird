//! Model configuration for claudecode.

#[cfg(test)]
use super::write_json_file;
use super::{echobird_dir, read_json_file, ApplyResult, ModelInfo};
use crate::services::{anthropic_proxy, native_account_files};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{fs, path::Path, path::PathBuf};

static CONFIG_MUTATIONS: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn normalize_model_info_for_tool(tool_id: &str, mut model_info: ModelInfo) -> ModelInfo {
    if tool_id == "claudecode" && model_info.protocol.as_deref() == Some("anthropic") {
        if let Some(ref mut base_url) = model_info.base_url {
            let trimmed = base_url.trim_end_matches('/').to_string();
            if let Some(without_v1) = trimmed.strip_suffix("/v1") {
                *base_url = without_v1.to_string();
            } else {
                *base_url = trimmed;
            }
        }
    }
    model_info
}

// Model-tier env vars Claude Code consults. Bridge mode clears them so Claude
// Code falls back to its built-in claude-* ids (the proxy rewrites them);
// relay mode pins supported tiers to the real upstream model. Hoisted to module
// scope so the set is lockable by tests — the apply path writes to
// ~/.claude/settings.json via dirs::home_dir(), which isn't injectable.
//
// `ANTHROPIC_SMALL_FAST_MODEL` is deliberately NOT here — Claude Code
// deprecated it in favor of `ANTHROPIC_DEFAULT_HAIKU_MODEL` (still pinned).
// `CLAUDE_CODE_SUBAGENT_MODEL` IS here so relay mode pins subagents to the
// upstream model too (otherwise subagents fall back to claude-* ids and
// bypass the third-party router, breaking the "全量" write-in).
// https://code.claude.com/docs/en/model-config
const CLAUDECODE_MODEL_VARS: [&str; 6] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
];

/// The 1M-capable subset of [`CLAUDECODE_MODEL_VARS`]: env vars that receive
/// a `[1m]` suffix when the user opts into the 1M context window. `HAIKU` and
/// `CLAUDE_CODE_SUBAGENT_MODEL` are deliberately excluded — Haiku has no 1M
/// tier, and subagents pin to the bare upstream id.
const CLAUDECODE_MODEL_VARS_1M: &[&str] = &[
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
];

const AUTO_MODE_SERVER: &str = "CLAUDE_CODE_AUTO_MODE_SERVER";

const CLOUD_PROVIDER_VARS: [&str; 6] = [
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    "CLAUDE_CODE_USE_MANTLE",
];

fn cloud_switch_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn inherited_provider(
    get: impl Fn(&str) -> Option<String>,
    env: Option<&Map<String, Value>>,
) -> Option<&'static str> {
    CLOUD_PROVIDER_VARS.into_iter().find(|key| {
        let overridden = env
            .and_then(|env| env.get(*key))
            .and_then(Value::as_str)
            .is_some_and(|value| !cloud_switch_enabled(value));
        !overridden && get(key).is_some_and(|value| cloud_switch_enabled(&value))
    })
}

fn managed_env_keys() -> impl Iterator<Item = &'static str> {
    CLAUDECODE_MODEL_VARS
        .into_iter()
        .chain(CLOUD_PROVIDER_VARS)
        .chain([
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "API_TIMEOUT_MS",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
            AUTO_MODE_SERVER,
        ])
}

fn managed_env(env: &Map<String, Value>) -> Map<String, Value> {
    managed_env_keys()
        .filter_map(|key| env.get(key).map(|value| (key.to_string(), value.clone())))
        .collect()
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManagedSettings {
    version: u8,
    path: PathBuf,
    settings_existed: bool,
    env_existed: bool,
    original_env: Map<String, Value>,
    applied_env: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    relay: Map<String, Value>,
}

fn relay_fields(relay: &Value) -> Map<String, Value> {
    [
        "baseUrl",
        "apiKey",
        "actualModel",
        "modelName",
        "relayMode",
        "fableTierManaged",
        "autoModeServerSettingsPath",
    ]
    .into_iter()
    .filter_map(|key| relay.get(key).map(|value| (key.to_string(), value.clone())))
    .collect()
}

fn saved_settings(relay: &Value) -> Result<Vec<ManagedSettings>, String> {
    let mut profiles: Vec<ManagedSettings> = serde_json::from_value(
        relay
            .get("otherSettings")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|_| "Claude Code's saved model configuration is invalid. Nothing was changed.")?;
    if let Some(value) = relay.get("managedSettings") {
        let mut current: ManagedSettings = serde_json::from_value(value.clone()).map_err(|_| {
            "Claude Code's saved model configuration is invalid. Nothing was changed."
        })?;
        current.relay = relay_fields(relay);
        profiles.push(current);
    }
    if profiles.iter().enumerate().any(|(index, saved)| {
        saved.version != 1
            || saved.path.as_os_str().is_empty()
            || saved.relay.get("baseUrl").and_then(Value::as_str).is_none()
            || saved.relay.get("apiKey").and_then(Value::as_str).is_none()
            || saved
                .relay
                .get("actualModel")
                .and_then(Value::as_str)
                .is_none()
            || saved
                .original_env
                .keys()
                .chain(saved.applied_env.keys())
                .any(|key| !managed_env_keys().any(|known| known == key))
            || profiles[..index]
                .iter()
                .any(|other| other.path == saved.path)
    }) {
        return Err(
            "Claude Code's saved model configuration is invalid. Nothing was changed.".into(),
        );
    }
    Ok(profiles)
}

fn read_object(path: &Path) -> Result<(Option<Vec<u8>>, Value), String> {
    let raw = native_account_files::read_optional(path)
        .map_err(|_| format!("Cannot read Claude Code configuration: {}", path.display()))?;
    if raw.is_none() && fs::symlink_metadata(path).is_ok() {
        return Err(format!(
            "Cannot read Claude Code configuration: {}",
            path.display()
        ));
    }
    let value = match raw.as_deref() {
        Some(bytes) => serde_json::from_slice::<Value>(bytes)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| format!("Invalid Claude Code configuration: {}", path.display()))?,
        None => serde_json::json!({}),
    };
    Ok((raw, value))
}

fn config_error(message: impl Into<String>) -> ApplyResult {
    ApplyResult {
        success: false,
        message: message.into(),
    }
}

/// Value written to a `CLAUDECODE_MODEL_VARS` entry in relay mode. Appends
/// `[1m]` only when the user opted in (`one_m`) AND `var` is in the 1M-capable
/// tier; `ANTHROPIC_DEFAULT_HAIKU_MODEL` / `CLAUDE_CODE_SUBAGENT_MODEL` always
/// return the bare id. Bridge mode never calls this — it removes the vars
/// instead of writing them, so the `relay` flag is the caller's responsibility.
fn claudecode_env_model_id(real_id: &str, var: &str, one_m: bool) -> String {
    if one_m && CLAUDECODE_MODEL_VARS_1M.contains(&var) {
        format!("{}[1m]", real_id)
    } else {
        real_id.to_string()
    }
}

fn is_fable_model(model: &str) -> bool {
    model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .starts_with("claude-fable-")
}

/// Claude Code 3P config. Mirrors `apply_claudedesktop` but targets Claude
/// Code's env-var config (~/.claude/settings.json) instead of Desktop's
/// profile JSON. Two routing modes, picked by `model_info.relay_mode`:
///
/// • Bridge (default): point ANTHROPIC_BASE_URL at our local Anthropic proxy
///   (127.0.0.1:<port>/claudecode) and write NO model id, so Claude Code
///   sends its built-in claude-* ids and the proxy rewrites every request to
///   the real upstream model (read fresh from ~/.echobird/claudecode.json).
///   This is the "first-class citizen" path — strict upstreams never see a
///   claude-* name they'd reject, matching how Claude Desktop already works.
///
/// • Relay (relay_mode = true): write the real upstream URL + key + model id
///   straight into settings.json so Claude Code talks to the upstream
///   directly, bypassing the proxy. For relay stations that natively serve
///   Anthropic Messages and map the ids themselves. (≈ the legacy full-write.)
///
/// The relay side-channel (~/.echobird/claudecode.json) is written in BOTH
/// modes so `read_claudecode` can round-trip the active model back to the UI.
pub(super) fn apply_claudecode(model_info: &ModelInfo) -> ApplyResult {
    let config_dir = match crate::services::claude_code_accounts::config_dir() {
        Ok(dir) => dir,
        Err(message) => {
            return ApplyResult {
                success: false,
                message,
            }
        }
    };
    let settings_path = config_dir.join("settings.json");
    apply_claudecode_with_environment(
        model_info,
        &settings_path,
        &echobird_dir().join("claudecode.json"),
        |key| std::env::var(key).ok(),
    )
}

#[cfg(test)]
fn apply_claudecode_at(
    model_info: &ModelInfo,
    settings_path: &std::path::Path,
    relay_path: &std::path::Path,
) -> ApplyResult {
    apply_claudecode_with_environment(model_info, settings_path, relay_path, |_| None)
}

fn apply_claudecode_with_environment(
    model_info: &ModelInfo,
    settings_path: &Path,
    relay_path: &Path,
    inherited: impl Fn(&str) -> Option<String>,
) -> ApplyResult {
    let _mutation = match CONFIG_MUTATIONS.lock() {
        Ok(guard) => guard,
        Err(_) => return config_error("Claude Code's previous configuration update was interrupted. Restart EchoBird before trying again."),
    };
    // Frontend collapses the chosen protocol's URL into base_url (same
    // convention as apply_claudedesktop). Accept either field.
    let anthropic_url = model_info
        .anthropic_url
        .as_deref()
        .or(model_info.base_url.as_deref())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(|u| {
            // Trim a trailing '/' and a trailing '/v1' so relay mode (where
            // Claude Code's SDK appends '/v1/messages' to ANTHROPIC_BASE_URL)
            // can't produce a doubled '/v1/v1/messages'. normalize already does
            // this for base_url; do it here too so the anthropic_url branch is
            // equally safe. Bridge mode is immune either way (the proxy
            // recomposes the path), but this keeps both modes consistent.
            let u = u.trim_end_matches('/');
            u.strip_suffix("/v1")
                .unwrap_or(u)
                .trim_end_matches('/')
                .to_string()
        });
    let anthropic_url = match anthropic_url {
        Some(u) => u,
        None => {
            return ApplyResult {
                success: false,
                message: "Base URL is empty. Pick a model first.".to_string(),
            }
        }
    };

    // Local providers (loopback) legitimately have no key — substitute a
    // sentinel so the relay always has something to forward as Bearer.
    let raw_api_key = model_info.api_key.as_deref().unwrap_or("");
    let is_local_provider =
        anthropic_url.contains("127.0.0.1") || anthropic_url.contains("localhost");
    let api_key = if raw_api_key.is_empty() {
        if is_local_provider {
            "local-no-auth".to_string()
        } else {
            return ApplyResult {
                success: false,
                message: "API Key is empty, cannot apply Claude Code config.".to_string(),
            };
        }
    } else {
        raw_api_key.to_string()
    };

    let real_model_id = model_info
        .model
        .as_deref()
        .or(model_info.name.as_deref())
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();

    let relay_mode = model_info.relay_mode.unwrap_or(false);
    let proxy_base = format!("http://127.0.0.1:{}/claudecode", anthropic_proxy::port());

    let (previous_raw, mut config) = match read_object(settings_path) {
        Ok(value) => value,
        Err(message) => return config_error(message),
    };
    if config.get("env").is_some_and(|env| !env.is_object()) {
        return config_error(
            "Claude Code's env configuration isn't an object. Nothing was changed.",
        );
    }
    if let Some(key) = inherited_provider(inherited, config.get("env").and_then(Value::as_object)) {
        return config_error(format!(
            "Claude Code's {key} is enabled in EchoBird's environment. Disable it and restart EchoBird before applying a model."
        ));
    }
    let (previous_relay_raw, previous_relay) = match read_object(relay_path) {
        Ok(value) => value,
        Err(message) => return config_error(message),
    };
    let mut profiles = match saved_settings(&previous_relay) {
        Ok(value) => value,
        Err(message) => return config_error(message),
    };
    let legacy_relay = profiles.is_empty();
    let previous_settings = profiles
        .iter()
        .position(|saved| saved.path == settings_path)
        .map(|index| profiles.remove(index));
    let env_existed = config.get("env").is_some();
    config
        .as_object_mut()
        .unwrap()
        .entry("env")
        .or_insert_with(|| serde_json::json!({}));
    let env = config
        .get_mut("env")
        .and_then(|v| v.as_object_mut())
        .expect("env is an object");
    if previous_relay_raw.is_none() && uses_local_bridge(env) {
        return config_error("Claude Code still points to EchoBird, but its saved model configuration is missing. Repair its settings before applying a model.");
    }
    if !legacy_relay && previous_settings.is_none() && uses_local_bridge(env) {
        return config_error("Claude Code's original settings for this configuration directory are missing. Nothing was changed.");
    }
    let mut saved = previous_settings.unwrap_or_else(|| {
        let mut original_env = env.clone();
        if legacy_relay {
            restore_legacy_env(&mut original_env, &previous_relay, settings_path);
        }
        ManagedSettings {
            version: 1,
            path: settings_path.to_owned(),
            settings_existed: previous_raw.is_some(),
            env_existed,
            original_env: managed_env(&original_env),
            applied_env: managed_env(env),
            relay: if legacy_relay {
                relay_fields(&previous_relay)
            } else {
                Map::new()
            },
        }
    });
    let previous_profile_relay = Value::Object(saved.relay.clone());
    for key in managed_env_keys() {
        if env.get(key) != saved.applied_env.get(key) {
            match env.get(key) {
                Some(value) => {
                    saved.original_env.insert(key.to_owned(), value.clone());
                }
                None => {
                    saved.original_env.remove(key);
                }
            }
        }
    }
    if previous_raw.is_none() {
        saved.settings_existed = false;
        saved.env_existed = false;
    } else if !env_existed {
        saved.env_existed = false;
    }

    for key in CLOUD_PROVIDER_VARS {
        if env.get(key).is_some_and(|value| match value {
            Value::String(value) => cloud_switch_enabled(value),
            Value::Bool(value) => *value,
            Value::Number(value) => value.as_u64() == Some(1),
            _ => false,
        }) {
            env.remove(key);
        }
    }

    // Shared knobs (match the long-standing generic claudecode mapping).
    env.insert(
        "API_TIMEOUT_MS".to_string(),
        serde_json::Value::String("3000000".to_string()),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
        serde_json::Value::String("1".to_string()),
    );

    // Gateways may not return server classifier verdicts. Preserve explicit
    // user values and track our default separately so official restore can
    // remove it, including after repeated applies or relay/bridge switches.
    let auto_mode_inserted = !env.contains_key(AUTO_MODE_SERVER);
    let auto_mode_managed = auto_mode_inserted
        || (env.get(AUTO_MODE_SERVER).and_then(|v| v.as_str()) == Some("0")
            && previous_profile_relay["autoModeServerSettingsPath"]
                .as_str()
                .is_some_and(|path| Path::new(path) == settings_path));
    env.entry(AUTO_MODE_SERVER.to_string())
        .or_insert_with(|| serde_json::json!("0"));

    // Both modes authenticate via ANTHROPIC_AUTH_TOKEN (Bearer), so always drop
    // any ANTHROPIC_API_KEY (x-api-key) the user may have left in settings.json —
    // a stale real key would otherwise conflict. We *remove* the key rather than
    // write an empty string (the old generic mapping could only set "", not
    // delete) so settings.json stays clean.
    env.remove("ANTHROPIC_API_KEY");

    if relay_mode {
        env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            serde_json::Value::String(anthropic_url.clone()),
        );
        env.insert(
            "ANTHROPIC_AUTH_TOKEN".to_string(),
            serde_json::Value::String(api_key.clone()),
        );
        let one_m = model_info.one_m_context.unwrap_or(false);
        for var in CLAUDECODE_MODEL_VARS {
            if var == "ANTHROPIC_DEFAULT_FABLE_MODEL" && !is_fable_model(&real_model_id) {
                let previous = Some(&previous_profile_relay).filter(|relay| {
                    relay["fableTierManaged"] != false
                        && relay["autoModeServerSettingsPath"]
                            .as_str()
                            .is_some_and(|path| std::path::Path::new(path) == settings_path)
                });
                let stale = env
                    .get(var)
                    .and_then(|value| value.as_str())
                    .is_some_and(|model| {
                        let model = model.strip_suffix("[1m]").unwrap_or(model);
                        previous.as_ref().is_some_and(|relay| {
                            relay["actualModel"].as_str().is_some_and(|previous| {
                                let previous = previous.strip_suffix("[1m]").unwrap_or(previous);
                                model == previous && !is_fable_model(previous)
                            })
                        })
                    });
                if stale {
                    env.remove(var);
                }
                continue;
            }
            env.insert(
                var.to_string(),
                serde_json::Value::String(claudecode_env_model_id(&real_model_id, var, one_m)),
            );
        }
    } else {
        env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            serde_json::Value::String(proxy_base),
        );
        env.insert(
            "ANTHROPIC_AUTH_TOKEN".to_string(),
            serde_json::Value::String("echobird-local-proxy".to_string()),
        );
        for var in CLAUDECODE_MODEL_VARS {
            env.remove(var);
        }
    }

    saved.applied_env = managed_env(env);
    saved.relay.clear();

    // Relay side-channel — real upstream, read fresh by anthropic_proxy on
    // every /claudecode/v1/messages request and by read_claudecode for the UI.
    let relay = serde_json::json!({
        "baseUrl": anthropic_url,
        "apiKey": api_key,
        "actualModel": real_model_id,
        "modelName": model_info.name.as_deref().unwrap_or(real_model_id.as_str()),
        "relayMode": relay_mode,
        "fableTierManaged": relay_mode && is_fable_model(&real_model_id),
        "autoModeServerSettingsPath": auto_mode_managed.then(|| settings_path.to_string_lossy()),
        "managedSettings": saved,
        "otherSettings": profiles,
    });
    let result = (|| {
        let mut transaction = native_account_files::Transaction::new();
        if native_account_files::read_optional(relay_path)? != previous_relay_raw {
            return Err("Claude Code's model configuration changed. Try again.".into());
        }
        transaction.replace(relay_path, &native_account_files::json(&relay)?)?;
        #[cfg(test)]
        regression_tests::after_mutation_stage();
        if native_account_files::read_optional(settings_path)? != previous_raw {
            return Err("Claude Code's settings changed. Try again.".into());
        }
        transaction.replace_private_file(settings_path, &native_account_files::json(&config)?)?;
        transaction.commit();
        Ok::<_, String>(())
    })();
    if let Err(error) = result {
        return config_error(format!(
            "Failed to apply Claude Code configuration: {error}"
        ));
    }

    ApplyResult {
        success: true,
        message:
            "Claude Code configured. Restart Claude Code (or /exit and reopen) for the change to take effect."
                .to_string(),
    }
}

pub(super) fn read_claudecode() -> Option<ModelInfo> {
    let settings_path = crate::services::claude_code_accounts::config_dir()
        .ok()?
        .join("settings.json");
    read_claudecode_at(&settings_path, &echobird_dir().join("claudecode.json"))
}

fn read_claudecode_at(settings_path: &Path, relay_path: &Path) -> Option<ModelInfo> {
    // Source of truth is the relay file (written in both modes), NOT
    // settings.json — mirrors read_claudedesktop. Surfaces the real upstream
    // model so the App Desktop card shows it after a rescan.
    let mut relay = read_json_file(relay_path)?;
    let profiles = saved_settings(&relay).ok()?;
    if !profiles.is_empty() {
        let profile = profiles.iter().find(|saved| saved.path == settings_path)?;
        let (_, settings) = read_object(settings_path).ok()?;
        let env = settings.get("env")?.as_object()?;
        for key in ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN"] {
            if env.get(key) != profile.applied_env.get(key) {
                return None;
            }
        }
        // Direct relays live in the client's own settings. Bridge clients all
        // use the one current top-level relay, regardless of their last pick.
        if profile.relay.get("relayMode") == Some(&Value::Bool(true)) {
            relay = Value::Object(profile.relay.clone());
        }
    }

    let anthropic_url = relay
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let api_key = relay
        .get("apiKey")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let model = relay
        .get("actualModel")
        .and_then(|v| v.as_str())
        // Legacy tolerance: relay files written by ≤v5.3.8 (which had a 1M
        // toggle) may carry a `[1m]` suffix on actualModel. Strip it so the
        // surfaced active-model id matches the user's stored modelId —
        // otherwise the App Desktop card's "currently applied" highlight
        // fails to match after a rescan. Nothing writes the suffix any more.
        .map(|s| s.strip_suffix("[1m]").unwrap_or(s))
        .filter(|s| !s.is_empty())
        .map(String::from);
    let name = relay
        .get("modelName")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    Some(ModelInfo {
        name,
        model,
        base_url: None,
        api_key,
        anthropic_url: Some(anthropic_url),
        protocol: Some("anthropic".to_string()),
        display_model: None,
        relay_mode: None,
        web_search: None,
        one_m_context: None,
    })
}

/// Surgically remove the env keys we own from ~/.claude/settings.json and
/// drop the relay file. Unlike the generic restore (which deletes the whole
/// config file), this preserves allowedTools / hooks / anything else the user
/// keeps in settings.json — deleting it wholesale would wipe their Claude Code
/// setup, not just our model config.
pub(super) fn restore_claudecode_to_official() -> ApplyResult {
    let config_dir = match crate::services::claude_code_accounts::config_dir() {
        Ok(dir) => dir,
        Err(message) => {
            return ApplyResult {
                success: false,
                message,
            }
        }
    };
    restore_claudecode_at(
        &config_dir.join("settings.json"),
        &echobird_dir().join("claudecode.json"),
    )
}

fn restore_claudecode_at(
    settings_path: &std::path::Path,
    relay_path: &std::path::Path,
) -> ApplyResult {
    let _mutation = match CONFIG_MUTATIONS.lock() {
        Ok(guard) => guard,
        Err(_) => return config_error("Claude Code's previous configuration update was interrupted. Restart EchoBird before trying again."),
    };
    let result = (|| {
        let (relay_raw, relay) = read_object(relay_path)?;
        let Some(relay_raw) = relay_raw else {
            let (_, config) = read_object(settings_path)?;
            if config
                .get("env")
                .and_then(Value::as_object)
                .is_some_and(uses_local_bridge)
            {
                return Err("Claude Code still points to EchoBird, but its saved model configuration is missing. Restore was stopped to preserve its settings.".into());
            }
            return Ok(());
        };
        let mut profiles = saved_settings(&relay)?;
        let managed = !profiles.is_empty();
        let saved = profiles
            .iter()
            .position(|saved| saved.path == settings_path)
            .map(|index| profiles.remove(index));
        if managed && saved.is_none() {
            return Err("Claude Code's saved model belongs to another configuration directory. Nothing was changed.".into());
        }
        let (settings_raw, mut config) = read_object(settings_path)?;
        if config.get("env").is_some_and(|env| !env.is_object()) {
            return Err(
                "Claude Code's env configuration isn't an object. Nothing was changed.".into(),
            );
        }
        if let Some(env) = config.get_mut("env").and_then(Value::as_object_mut) {
            if let Some(saved) = &saved {
                for key in managed_env_keys() {
                    if env.get(key) == saved.applied_env.get(key) {
                        match saved.original_env.get(key) {
                            Some(value) => {
                                env.insert(key.to_string(), value.clone());
                            }
                            None => {
                                env.remove(key);
                            }
                        }
                    }
                }
                if !saved.env_existed && env.is_empty() {
                    config.as_object_mut().unwrap().remove("env");
                }
            } else {
                restore_legacy_env(env, &relay, settings_path);
            }
        }
        let next_settings = if settings_raw.is_none()
            || (saved.as_ref().is_some_and(|saved| !saved.settings_existed)
                && config.as_object().unwrap().is_empty())
        {
            None
        } else {
            Some(native_account_files::json(&config)?)
        };
        let next_relay = if let Some(mut remaining) = profiles.pop() {
            let mut relay = Value::Object(std::mem::take(&mut remaining.relay));
            relay["managedSettings"] = serde_json::to_value(remaining)
                .map_err(|_| "Cannot save Claude Code's remaining model configuration")?;
            relay["otherSettings"] = serde_json::to_value(profiles)
                .map_err(|_| "Cannot save Claude Code's remaining model configuration")?;
            Some(native_account_files::json(&relay)?)
        } else {
            None
        };
        if native_account_files::read_optional(settings_path)? != settings_raw
            || native_account_files::read_optional(relay_path)?.as_deref()
                != Some(relay_raw.as_slice())
        {
            return Err("Claude Code's configuration changed. Try again.".into());
        }
        if settings_raw != next_settings {
            replace_config(settings_path, next_settings.as_deref())?;
        }
        #[cfg(test)]
        regression_tests::after_mutation_stage();
        if let Err(error) = replace_config(relay_path, next_relay.as_deref()) {
            if settings_raw != next_settings {
                replace_config(settings_path, settings_raw.as_deref()).map_err(|rollback| {
                    format!("Cannot restore Claude Code's model configuration: {error}. Settings rollback failed: {rollback}")
                })?;
            }
            return Err(format!(
                "Cannot restore Claude Code's model configuration: {error}"
            ));
        }
        Ok::<_, String>(())
    })();
    if let Err(message) = result {
        return config_error(message);
    }

    ApplyResult {
        success: true,
        message: "Claude Code restored to official provider. Restart Claude Code for the change to take effect."
            .to_string(),
    }
}

fn replace_config(path: &Path, bytes: Option<&[u8]>) -> Result<(), String> {
    match bytes {
        Some(bytes) => native_account_files::write_private_file(path, bytes),
        None => fs::remove_file(path).map_err(|error| error.to_string()),
    }
}

fn uses_local_bridge(env: &Map<String, Value>) -> bool {
    env.get("ANTHROPIC_AUTH_TOKEN").and_then(Value::as_str) == Some("echobird-local-proxy")
        && env
            .get("ANTHROPIC_BASE_URL")
            .and_then(Value::as_str)
            .is_some_and(|url| url.starts_with("http://127.0.0.1:") && url.ends_with("/claudecode"))
}

fn restore_legacy_env(env: &mut Map<String, Value>, relay: &Value, settings_path: &Path) {
    // Older relays did not retain original values. Only remove fields whose
    // current values still identify the last applied configuration.
    let connected = if relay["relayMode"] == true {
        env.get("ANTHROPIC_BASE_URL") == relay.get("baseUrl")
            && env.get("ANTHROPIC_AUTH_TOKEN") == relay.get("apiKey")
            && relay["baseUrl"].is_string()
            && relay["apiKey"].is_string()
    } else {
        uses_local_bridge(env)
    };
    if !connected {
        return;
    }
    env.remove("ANTHROPIC_BASE_URL");
    env.remove("ANTHROPIC_AUTH_TOKEN");
    for key in CLAUDECODE_MODEL_VARS
        .into_iter()
        .chain(["ANTHROPIC_SMALL_FAST_MODEL"])
    {
        if env.get(key).and_then(Value::as_str).is_some_and(|value| {
            relay["actualModel"].as_str().is_some_and(|model| {
                value.strip_suffix("[1m]").unwrap_or(value)
                    == model.strip_suffix("[1m]").unwrap_or(model)
            })
        }) {
            env.remove(key);
        }
    }
    for (key, expected) in [
        ("API_TIMEOUT_MS", "3000000"),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
    ] {
        if env.get(key).and_then(Value::as_str) == Some(expected) {
            env.remove(key);
        }
    }
    if env.get(AUTO_MODE_SERVER).and_then(Value::as_str) == Some("0")
        && relay["autoModeServerSettingsPath"]
            .as_str()
            .is_some_and(|path| Path::new(path) == settings_path)
    {
        env.remove(AUTO_MODE_SERVER);
    }
}

#[cfg(test)]
#[path = "claudecode_regression_tests.rs"]
mod regression_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_and_restores_within_the_selected_config_directory() {
        let dir = std::env::temp_dir().join(format!("claude-config-{}", uuid::Uuid::new_v4()));
        let relay = dir.join("relay.json");
        let unchanged = dir.join("unrelated/settings.json");
        let original = serde_json::json!({"permissions":{"allow":["Read"]}, "env":{"KEEP":"yes"}});
        write_json_file(&unchanged, &original).unwrap();
        for name in [".claude", "custom-profile"] {
            let path = dir.join(name).join("settings.json");
            write_json_file(&path, &original).unwrap();
            for mode in [false, true] {
                assert!(
                    apply_claudecode_at(&claudecode_model_info(Some(mode)), &path, &relay).success
                );
                assert!(read_json_file(&path).unwrap()["env"]["ANTHROPIC_BASE_URL"].is_string());
                assert_eq!(
                    read_json_file(&path).unwrap()["env"]["CLAUDE_CODE_AUTO_MODE_SERVER"],
                    "0"
                );
                assert!(restore_claudecode_at(&path, &relay).success);
                assert_eq!(read_json_file(&path).unwrap(), original);
                assert_eq!(read_json_file(&unchanged).unwrap(), original);
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn auto_mode_default_survives_repeated_model_switches_until_official_restore() {
        let dir = std::env::temp_dir().join(format!("claude-auto-mode-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.json");
        let relay = dir.join("relay.json");
        let original = serde_json::json!({"env":{"KEEP":"yes"}});
        write_json_file(&path, &original).unwrap();
        for mode in [false, true, false] {
            assert!(apply_claudecode_at(&claudecode_model_info(Some(mode)), &path, &relay).success);
            assert_eq!(
                read_json_file(&path).unwrap()["env"]["CLAUDE_CODE_AUTO_MODE_SERVER"],
                "0"
            );
        }
        assert!(restore_claudecode_at(&path, &relay).success);
        assert_eq!(read_json_file(&path).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn auto_mode_preserves_user_values_and_manual_edits() {
        let dir = std::env::temp_dir().join(format!("claude-auto-mode-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.json");
        let relay = dir.join("relay.json");
        for mode in [false, true] {
            for value in ["0", "1"] {
                let original =
                    serde_json::json!({"env":{"CLAUDE_CODE_AUTO_MODE_SERVER":value,"KEEP":"yes"}});
                write_json_file(&path, &original).unwrap();
                assert!(
                    apply_claudecode_at(&claudecode_model_info(Some(mode)), &path, &relay).success
                );
                assert_eq!(
                    read_json_file(&path).unwrap()["env"]["CLAUDE_CODE_AUTO_MODE_SERVER"],
                    value
                );
                assert!(restore_claudecode_at(&path, &relay).success);
                assert_eq!(read_json_file(&path).unwrap(), original);
            }
            for reapply in [false, true] {
                write_json_file(&path, &serde_json::json!({"env":{"KEEP":"yes"}})).unwrap();
                assert!(
                    apply_claudecode_at(&claudecode_model_info(Some(mode)), &path, &relay).success
                );
                let mut edited = read_json_file(&path).unwrap();
                edited["env"]["CLAUDE_CODE_AUTO_MODE_SERVER"] = serde_json::json!("1");
                write_json_file(&path, &edited).unwrap();
                if reapply {
                    assert!(
                        apply_claudecode_at(&claudecode_model_info(Some(!mode)), &path, &relay)
                            .success
                    );
                }
                assert!(restore_claudecode_at(&path, &relay).success);
                assert_eq!(
                    read_json_file(&path).unwrap(),
                    serde_json::json!({"env":{"KEEP":"yes","CLAUDE_CODE_AUTO_MODE_SERVER":"1"}})
                );
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn auto_mode_ownership_does_not_cross_config_directories() {
        let dir = std::env::temp_dir().join(format!("claude-auto-mode-{}", uuid::Uuid::new_v4()));
        let first = dir.join("first/settings.json");
        let second = dir.join("second/settings.json");
        let relay = dir.join("relay.json");
        let original = serde_json::json!({"env":{"CLAUDE_CODE_AUTO_MODE_SERVER":"0"}});
        write_json_file(&second, &original).unwrap();
        assert!(apply_claudecode_at(&claudecode_model_info(Some(false)), &first, &relay).success);
        assert!(apply_claudecode_at(&claudecode_model_info(Some(true)), &second, &relay).success);
        assert!(restore_claudecode_at(&second, &relay).success);
        assert_eq!(read_json_file(&second).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_relay_write_does_not_leave_an_untracked_auto_mode_default() {
        let dir = std::env::temp_dir().join(format!("claude-auto-mode-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.json");
        let relay = dir.join("relay.json");
        fs::create_dir_all(&relay).unwrap();
        for mode in [false, true] {
            write_json_file(&path, &serde_json::json!({"env":{"KEEP":"yes"}})).unwrap();
            assert!(
                !apply_claudecode_at(&claudecode_model_info(Some(mode)), &path, &relay).success
            );
            let config = read_json_file(&path).unwrap();
            assert!(config["env"].get(AUTO_MODE_SERVER).is_none());
            assert_eq!(config["env"]["KEEP"], "yes");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    fn claudecode_model_info(relay_mode: Option<bool>) -> ModelInfo {
        ModelInfo {
            name: Some("MiMo v2.5 Pro".to_string()),
            model: Some("mimo-v2.5-pro".to_string()),
            base_url: Some("https://api.example.com/v1".to_string()),
            api_key: Some("test-key".to_string()),
            anthropic_url: None,
            protocol: Some("anthropic".to_string()),
            display_model: None,
            relay_mode,
            web_search: None,
            one_m_context: None,
        }
    }

    #[test]
    fn relay_models_only_set_the_fable_tier_for_fable() {
        let dir = std::env::temp_dir().join(format!("claude-fable-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.json");
        let relay = dir.join("relay.json");
        for model in [
            "gpt-6.1-sol",
            "deepseek-flash",
            "mimo-v2.5-pro",
            "claude-opus-5-5",
            "claude-fable-5-1",
            "vendor/claude-fable-5-1",
        ] {
            for one_m in [false, true] {
                write_json_file(&path, &serde_json::json!({"env":{"KEEP":"yes"}})).unwrap();
                let mut info = claudecode_model_info(Some(true));
                info.model = Some(model.into());
                info.one_m_context = Some(one_m);
                assert!(apply_claudecode_at(&info, &path, &relay).success);
                let settings = read_json_file(&path).unwrap();
                let env = &settings["env"];
                for var in CLAUDECODE_MODEL_VARS {
                    if var == "ANTHROPIC_DEFAULT_FABLE_MODEL"
                        && !model
                            .rsplit('/')
                            .next()
                            .unwrap()
                            .starts_with("claude-fable-")
                    {
                        assert!(env.get(var).is_none(), "{model} must not be a Fable tier");
                    } else {
                        assert_eq!(env[var], claudecode_env_model_id(model, var, one_m));
                    }
                }
                assert_eq!(env["KEEP"], "yes");
                info.relay_mode = Some(false);
                assert!(apply_claudecode_at(&info, &path, &relay).success);
                let bridged = read_json_file(&path).unwrap();
                for var in CLAUDECODE_MODEL_VARS {
                    assert!(bridged["env"].get(var).is_none());
                }
                assert_eq!(bridged["env"]["KEEP"], "yes");
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn relay_fable_cleanup_preserves_independent_user_tiers() {
        let dir = std::env::temp_dir().join(format!("claude-fable-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.json");
        let relay = dir.join("relay.json");
        for (previous, existing, expected) in [
            ("gpt-6.1-sol", "gpt-6.1-sol", None),
            ("gpt-6.1-sol", "gpt-6.1-sol[1m]", None),
            (
                "gpt-6.1-sol",
                "deepseek-flash[1m]",
                Some("deepseek-flash[1m]"),
            ),
            ("gpt-6.1-sol", "claude-fable-5-1", Some("claude-fable-5-1")),
            (
                "claude-fable-5-1",
                "claude-fable-5-1[1m]",
                Some("claude-fable-5-1[1m]"),
            ),
        ] {
            write_json_file(
                &path,
                &serde_json::json!({"env":{"ANTHROPIC_DEFAULT_FABLE_MODEL":existing,"KEEP":"yes"}}),
            )
            .unwrap();
            write_json_file(
                &relay,
                &serde_json::json!({"actualModel":previous,"autoModeServerSettingsPath":path.to_string_lossy()}),
            )
            .unwrap();
            let mut info = claudecode_model_info(Some(true));
            info.model = Some("deepseek-flash".into());
            assert!(apply_claudecode_at(&info, &path, &relay).success);
            let settings = read_json_file(&path).unwrap();
            assert_eq!(
                settings["env"]
                    .get("ANTHROPIC_DEFAULT_FABLE_MODEL")
                    .and_then(|v| v.as_str()),
                expected,
                "previous={previous}, existing={existing}"
            );
            assert_eq!(settings["env"]["KEEP"], "yes");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn relay_fable_value_equal_to_selected_model_needs_ownership_to_be_removed() {
        let dir = std::env::temp_dir().join(format!("claude-fable-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.json");
        let other_path = dir.join("other/settings.json");
        let relay = dir.join("relay.json");
        for existing in ["deepseek-flash", "deepseek-flash[1m]"] {
            for other_owner in [false, true] {
                write_json_file(
                    &path,
                    &serde_json::json!({"env":{"ANTHROPIC_DEFAULT_FABLE_MODEL":existing,"KEEP":"yes"}}),
                )
                .unwrap();
                if other_owner {
                    write_json_file(
                        &relay,
                        &serde_json::json!({"actualModel":"deepseek-flash","autoModeServerSettingsPath":other_path.to_string_lossy()}),
                    )
                    .unwrap();
                } else if relay.exists() {
                    fs::remove_file(&relay).unwrap();
                }
                let mut info = claudecode_model_info(Some(true));
                info.model = Some("deepseek-flash".into());
                for _ in 0..2 {
                    assert!(apply_claudecode_at(&info, &path, &relay).success);
                    let settings = read_json_file(&path).unwrap();
                    assert_eq!(settings["env"]["ANTHROPIC_DEFAULT_FABLE_MODEL"], existing);
                    assert_eq!(settings["env"]["KEEP"], "yes");
                }
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn claudecode_normalize_never_decorates_model_id() {
        // The model id must reach the upstream verbatim in every mode. Claude
        // decorations like the `[1m]` suffix are gone entirely: in bridge mode
        // Claude Code's own built-in claude-* ids already budget the full
        // window, and the rewritten upstream id must be exactly what the
        // third-party provider advertises.
        for relay_mode in [None, Some(false), Some(true)] {
            let info =
                normalize_model_info_for_tool("claudecode", claudecode_model_info(relay_mode));
            assert_eq!(info.model.as_deref(), Some("mimo-v2.5-pro"));
        }
    }

    #[test]
    fn claudecode_normalize_strips_trailing_v1_from_base_url() {
        // Relay mode appends `/v1/messages` client-side, so a `/v1` left on the
        // base URL would double into `/v1/v1/messages`.
        let info = normalize_model_info_for_tool("claudecode", claudecode_model_info(None));
        assert_eq!(info.base_url.as_deref(), Some("https://api.example.com"));
    }

    // API Router (relay) full write-in pins every model tier to the real
    // upstream model so Claude Code uses the third-party model end-to-end.
    // Two requirements locked here, per https://code.claude.com/docs/en/model-config:
    //   * CLAUDE_CODE_SUBAGENT_MODEL must be written — otherwise subagents
    //     fall back to claude-* ids and bypass the router (breaks "全量").
    //   * ANTHROPIC_SMALL_FAST_MODEL must NOT be written — Claude Code
    //     deprecated it in favor of ANTHROPIC_DEFAULT_HAIKU_MODEL (still pinned).
    #[test]
    fn claudecode_model_vars_pin_subagent_and_drop_small_fast() {
        assert!(
            CLAUDECODE_MODEL_VARS.contains(&"CLAUDE_CODE_SUBAGENT_MODEL"),
            "relay mode must pin CLAUDE_CODE_SUBAGENT_MODEL so subagents use the third-party model"
        );
        assert!(
            !CLAUDECODE_MODEL_VARS.contains(&"ANTHROPIC_SMALL_FAST_MODEL"),
            "ANTHROPIC_SMALL_FAST_MODEL is deprecated (favor ANTHROPIC_DEFAULT_HAIKU_MODEL, also pinned) — stop writing it"
        );
    }

    // ── claudecode_env_model_id: [1m] opt-in, 1M-tier-only ──
    // The 1M-context toggle is relay-only and Claude-Code-only. When opted in,
    // `[1m]` is appended to the 1M-capable tier (MODEL / SONNET / OPUS / FABLE) so CC
    // budgets the 1M window; HAIKU + SUBAGENT stay bare (no 1M concept). CC
    // strips the suffix before sending upstream, so the provider sees the bare id.
    #[test]
    fn claudecode_env_model_id_appends_1m_for_1m_tier_when_opt_in() {
        for &var in CLAUDECODE_MODEL_VARS_1M {
            assert_eq!(
                claudecode_env_model_id("mimo-v2.5-pro", var, true),
                "mimo-v2.5-pro[1m]",
                "opt-in 1M must decorate the 1M-capable tier var {}",
                var
            );
        }
    }

    #[test]
    fn claudecode_env_model_id_leaves_haiku_and_subagent_bare_even_when_opt_in() {
        for var in [
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "CLAUDE_CODE_SUBAGENT_MODEL",
        ] {
            assert_eq!(
                claudecode_env_model_id("mimo-v2.5-pro", var, true),
                "mimo-v2.5-pro",
                "opt-in must NOT decorate the non-1M tier var {}",
                var
            );
        }
    }

    #[test]
    fn claudecode_env_model_id_bare_when_opt_out() {
        // Opt-out (or unset) → every var stays bare, regardless of tier.
        for var in CLAUDECODE_MODEL_VARS {
            assert_eq!(
                claudecode_env_model_id("mimo-v2.5-pro", var, false),
                "mimo-v2.5-pro",
                "opt-out must leave {} bare",
                var
            );
        }
    }
}
