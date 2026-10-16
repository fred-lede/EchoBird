//! Shared model configuration for Codex CLI and ChatGPT Desktop.

use super::{
    echobird_dir, ensure_parent, toml_delete_top, toml_read_table_value, toml_read_top,
    toml_write_table_value, toml_write_table_value_raw, toml_write_top, toml_write_top_raw,
    write_json_file, ApplyResult, ModelInfo,
};
use crate::services::codex_accounts;
use crate::services::codex_catalog;
use std::fs;
use std::path::Path;

/// Canonical Codex config identity. Every apply_codex run reuses this
/// provider section so model switches do not accumulate stale sections.
const CODEX_PROVIDER: &str = "OpenAI";
const DEEPSEEK_DESKTOP_EFFORTS: &str = r#"["low", "medium", "high", "xhigh", "ultra", "max"]"#;

// ─── Per-model capability registry ───
//
// Some tool configs must reflect a model's real context window and input
// modalities rather than a one-size-fits-all default. Codex's
// `model_context_window` / `model_auto_compact_token_limit` and ZCode's
// `modalities.input` are the two places a wrong default silently caps or
// mis-advertises a third-party model. The values below are sourced from the
// provider's official model specs so apply_codex / apply_zcode stay
// data-driven — no model-id branching inside the apply logic.

const DEFAULT_CODEX_CONTEXT_WINDOW: u64 = 1_000_000;

/// Look up a model's real context window (in tokens). Returns
/// `DEFAULT_CODEX_CONTEXT_WINDOW` for models the registry does not list —
/// the historic Codex default — so unknown models keep working as before.
fn model_context_window_for(model_id: &str) -> u64 {
    match model_id {
        "deepseek-flash" | "deepseek-v4-pro" => 1_048_576,
        "MiniMax-M3" => 1_000_000,
        "MiniMax-M2.7" => 204_800,
        _ => DEFAULT_CODEX_CONTEXT_WINDOW,
    }
}

/// Derive the auto-compact token limit (90% of the context window) that
/// Codex writes as `model_auto_compact_token_limit`. Keeping it proportional
/// to the real window stops Codex from trying to compact at 900k on a model
/// whose entire window is only 204,800 tokens.
fn codex_compact_limit_for(context_window: u64) -> u64 {
    context_window * 9 / 10
}

fn codex_web_search_mode(base_url: &str, enabled: Option<bool>) -> &'static str {
    if crate::services::smart_router::is_router_url(base_url) {
        return "disabled";
    }
    if enabled.unwrap_or_else(|| {
        !codex_catalog::url_matches_domain(base_url, "deepseek.com")
            && !codex_catalog::url_matches_domain(base_url, "xiaomimimo.com")
    }) {
        "live"
    } else {
        "disabled"
    }
}

fn codex_responses_base_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if let Ok(mut url) = url::Url::parse(base_url) {
        if matches!(url.host_str(), Some("open.bigmodel.cn" | "api.z.ai"))
            && matches!(url.path(), "/api/coding/paas/v4" | "/api/paas/v4")
        {
            url.set_path("/api/v1");
            return url.to_string();
        }
    }
    base_url.to_string()
}

// Codex CLI and ChatGPT desktop share ~/.codex/config.toml.

/// Apply our 10 canonical Codex fields surgically — overwrite if
/// present, insert if missing — and return the rewritten content.
/// Preserves everything else in the file (`[projects.*]` trust grants,
/// `[tui.*]` NUX progress, `[plugins.*]` state, comments, hand-edited
/// top-level keys).
///
/// This handles cases where manual edits or another tool rewrote keys we own
/// (`model_provider`, `model`, `wire_api`, `requires_openai_auth`, etc.)
/// to point at a different provider. Without this, a v4.8.x `apply_codex`
/// that only flipped `base_url` would leave the rest of the sibling
/// tool's edits in place and Codex would behave wrong (wrong model id,
/// wrong wire protocol, wrong reasoning effort).
///
/// `codex_base_url` and `model` are always the real upstream values.
/// Codex CLI and ChatGPT connect to the provider's Responses endpoint
/// directly; EchoBird no longer runs a Responses-to-Chat bridge.
///
/// `context_window` is the real token limit of the selected model. Codex
/// writes it as `model_context_window` and derives
/// `model_auto_compact_token_limit` as 90% of it, so a model whose window
/// is smaller than the historic 1M default is not over-claimed.
fn write_codex_canonical_fields(
    content: &str,
    codex_base_url: &str,
    model: &str,
    context_window: u64,
    web_search: Option<bool>,
) -> String {
    // Preserve the input's trailing-newline convention. `toml_write_*`
    // helpers go through `content.lines().collect().join("\n")` which
    // strips trailing newlines; without re-adding it, a canonical-input
    // round-trip would always show as a one-byte diff and trigger
    // pointless rewrites on repeated model applications.
    let trailing_nl = content.ends_with('\n');
    let mut c = content.to_string();

    // Top-level string keys.
    c = toml_write_top(&c, "model_provider", CODEX_PROVIDER);
    c = toml_write_top(&c, "model", model);
    let template = codex_catalog::template_for_model(codex_base_url, model);
    let reasoning = template
        .as_ref()
        .filter(|_| !codex_catalog::url_matches_domain(codex_base_url, "xiaomimimo.com"))
        .and_then(|t| t["default_reasoning_level"].as_str())
        .unwrap_or("high");
    c = toml_write_top(&c, "model_reasoning_effort", reasoning);
    // Evict legacy keys we no longer own. Older EchoBird versions wrote
    // `review_model = "gpt-5.5"`; we stopped writing it (Codex no longer
    // consumes it). But our TOML helpers are update-or-insert — they
    // never delete — so a stale line from an old version survives every
    // switch and self-heal. Under a Responses direct-connect session
    // (model = the real upstream id, e.g. "glm-5.2"; base_url = the real
    // upstream) Codex would still send `gpt-5.5` on its review pass to a
    // gateway that only knows the real id → 4xx. Strip it on every write
    // so the canonical set we write below is the full top-level truth.
    c = toml_delete_top(&c, "review_model");
    // Evict a stale `model_catalog_json` before conditionally re-adding the
    // catalog for the newly selected vendor.
    c = toml_delete_top(&c, "model_catalog_json");
    // Remove the deprecated flag from configs written by older EchoBird
    // versions. Codex no longer supports opting into response storage.
    c = toml_delete_top(&c, "disable_response_storage");
    // Top-level raw (int).
    c = toml_write_top_raw(&c, "model_context_window", &context_window.to_string());
    c = toml_write_top_raw(
        &c,
        "model_auto_compact_token_limit",
        &codex_compact_limit_for(context_window).to_string(),
    );
    c = toml_write_top(
        &c,
        "web_search",
        codex_web_search_mode(codex_base_url, web_search),
    );

    // MiMo's official Codex configuration requires this top-level capability
    // flag for model_reasoning_effort to take effect. Remove the pair first so
    // switching away from MiMo cannot leak its model-specific settings.
    c = toml_delete_top(&c, "model_supports_reasoning_summaries");
    c = toml_delete_top(&c, "model_reasoning_summary");
    if codex_catalog::url_matches_domain(codex_base_url, "xiaomimimo.com") {
        c = toml_write_top_raw(&c, "model_supports_reasoning_summaries", "true");
        c = toml_write_top(&c, "model_reasoning_summary", "none");
    }

    // DeepSeek's desktop allowlist exposes max; the catalog still limits
    // the selected model to its supported low/high/max reasoning levels.
    if codex_catalog::url_matches_domain(codex_base_url, "deepseek.com") {
        c = toml_write_table_value_raw(
            &c,
            "desktop",
            "enabled-reasoning-efforts",
            DEEPSEEK_DESKTOP_EFFORTS,
        );
    } else {
        let previous_provider = toml_read_top(content, "model_provider");
        let previous_url = toml_read_table_value(
            content,
            &format!("model_providers.{previous_provider}"),
            "base_url",
        );
        if codex_catalog::url_matches_domain(&previous_url, "deepseek.com")
            && toml_read_table_value(content, "desktop", "enabled-reasoning-efforts")
                == DEEPSEEK_DESKTOP_EFFORTS
        {
            // Remove only our exact list when leaving DeepSeek, keeping
            // custom effort lists and every other desktop setting.
            let mut in_desktop = false;
            c = c
                .lines()
                .filter(|line| {
                    let t = line.trim();
                    if t.starts_with('[') && t.ends_with(']') {
                        in_desktop = t == "[desktop]";
                    }
                    !in_desktop
                        || t.split_once('=')
                            .map_or(true, |(key, _)| key.trim() != "enabled-reasoning-efforts")
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
    }

    // [model_providers.OpenAI] string keys.
    let table = format!("model_providers.{}", CODEX_PROVIDER);
    c = toml_write_table_value(&c, &table, "name", CODEX_PROVIDER);
    c = toml_write_table_value(&c, &table, "base_url", codex_base_url);
    c = toml_write_table_value(&c, &table, "wire_api", "responses");
    // [model_providers.OpenAI] raw (bool).
    c = toml_write_table_value_raw(&c, &table, "requires_openai_auth", "true");

    if trailing_nl && !c.ends_with('\n') {
        c.push('\n');
    }
    c
}

/// Whether `content` (a config.toml) references OUR canonical catalog path in
/// `model_catalog_json`. Used by `apply_codex` to decide when to delete the
/// stale `~/.codex/models.json` file after leaving catalog mode. The check is
/// exact-path, not substring: a user's own catalog pointed at via a different
/// path (e.g. MiniMax docs' `~/.codex/model-catalogs/custom-catalog.json`)
/// must NOT trigger deletion of our file. Accepts both the absolute
/// forward-slash form we write and the `~/.codex/models.json` shorthand the
/// vendor docs use.
fn codex_catalog_referenced(content: &str, our_path: &str) -> bool {
    let referenced = toml_read_top(content, "model_catalog_json");
    !referenced.is_empty() && (referenced == our_path || referenced == "~/.codex/models.json")
}

pub(crate) fn apply_codex(tool_id: &str, model_info: &ModelInfo) -> ApplyResult {
    let codex_dir = codex_accounts::codex_home().unwrap_or_default();
    let state_dir = echobird_dir();
    apply_codex_at(tool_id, model_info, &codex_dir, &state_dir)
}

pub(crate) fn apply_codex_at(
    tool_id: &str,
    model_info: &ModelInfo,
    codex_dir: &Path,
    state_dir: &Path,
) -> ApplyResult {
    let config_path = codex_dir.join("config.toml");
    let auth_path = codex_dir.join("auth.json");

    let model_id = model_info
        .model
        .as_deref()
        .or(model_info.name.as_deref())
        .unwrap_or("");
    if model_id.is_empty() {
        return ApplyResult {
            success: false,
            message: "Model ID is empty".to_string(),
        };
    }

    let base_url = codex_responses_base_url(
        model_info
            .base_url
            .as_deref()
            .unwrap_or("https://api.openai.com/v1"),
    );

    // For local-LLM endpoints (127.0.0.1 / localhost), llama-server
    // ignores the API key entirely. Codex CLI, on the other hand, refuses to
    // start when OPENAI_API_KEY is empty. Substitute a non-empty dummy so
    // users don't have to invent a fake key in the Model Center just to use
    // their own local model.
    let raw_api_key = model_info.api_key.as_deref().unwrap_or("");
    let is_local_provider = base_url.contains("127.0.0.1") || base_url.contains("localhost");
    let api_key = if raw_api_key.is_empty() {
        if is_local_provider {
            "local-no-auth"
        } else {
            return ApplyResult {
                success: false,
                message: "API Key is empty, cannot apply Codex config".to_string(),
            };
        }
    } else {
        raw_api_key
    };

    // Establish every rollback point before touching Codex's live config. A
    // failed account snapshot must stop the apply before auth.json changes.
    let auth_backup_path = state_dir.join("codex-auth.bak.json");
    if auth_path.exists() && !auth_backup_path.exists() {
        let existing_auth = match fs::read(&auth_path) {
            Ok(value) => value,
            Err(error) => {
                return ApplyResult {
                    success: false,
                    message: format!("Codex auth backup read error: {error}"),
                };
            }
        };
        ensure_parent(&auth_backup_path);
        if let Err(error) = fs::write(&auth_backup_path, existing_auth) {
            return ApplyResult {
                success: false,
                message: format!("Codex auth backup write error: {error}"),
            };
        }
    }
    if codex_accounts::has_effective_oauth_snapshot(&auth_path, &auth_backup_path) {
        if let Err(error) = codex_accounts::save_effective_snapshot_at(
            &auth_path,
            &auth_backup_path,
            &state_dir.join("codex-accounts"),
        ) {
            return ApplyResult {
                success: false,
                message: format!("Codex account backup error: {error}"),
            };
        }
    }

    // Resolve the real context window for the selected model so Codex writes
    // `model_context_window` / `model_auto_compact_token_limit` matching the
    // model's actual token budget rather than the historic 1M default.
    let catalog_template = codex_catalog::template_for_model(&base_url, model_id);
    let context_window = catalog_template
        .as_ref()
        .and_then(|template| template["context_window"].as_u64())
        .unwrap_or_else(|| model_context_window_for(model_id));

    ensure_parent(&config_path);

    // Canonicalize every field we own. Overwrite-in-place
    // if present, insert if missing. Manual edits or other tools may have
    // rewritten our keys to point at a different provider — we restore
    // canonical shape end-to-end, not just `base_url`. Codex's own
    // runtime state (`[projects.*]` trust, `[tui.*]` NUX, `[plugins.*]`)
    // and any unrelated user-edited top-level keys stay untouched.
    let existing = fs::read_to_string(&config_path).unwrap_or_default();
    let mut new_content = write_codex_canonical_fields(
        &existing,
        &base_url,
        model_id,
        context_window,
        model_info.web_search,
    );

    // Model catalog — documented direct third-party providers
    // need `model_catalog_json` so Codex knows the real model's context window,
    // reasoning levels, and tool capabilities. Vendors without a bundled
    // catalog keep Codex's default behavior.
    // The stale line is evicted by `write_codex_canonical_fields` on every
    // canonicalize, so switching to a non-catalog vendor cannot leave a
    // dangling pointer.
    if let Some(template) = catalog_template {
        let catalog_path = codex_catalog::models_json_path(codex_dir);
        // Stamp the SELECTED model onto the vendor capability template and
        // write a single-entry catalog. Unknown model versions remain usable
        // with conservative capabilities; vendor-documented image models are
        // opted in by `build_catalog`.
        let catalog = codex_catalog::build_catalog(
            &template,
            model_id,
            model_info.name.as_deref().unwrap_or(model_id),
            context_window,
        );
        // Abort before changing config/auth if the required capability
        // catalog cannot be written.
        if let Err(error) = write_json_file(&catalog_path, &catalog) {
            return ApplyResult {
                success: false,
                message: format!("Codex model catalog error: {error}"),
            };
        }
        new_content = toml_write_top(
            &new_content,
            "model_catalog_json",
            &catalog_path.to_string_lossy(),
        );
    } else {
        // Leaving a bundled vendor for a non-bundled vendor:
        // the canonical write evicted the `model_catalog_json` line, so Codex
        // no longer reads the file. Delete the stale file at OUR canonical
        // path so the switch is disk-clean too — but ONLY when the previous
        // config actually referenced OUR canonical path. A user's own catalog
        // pointed at a different path (e.g. MiniMax docs' custom-catalog.json)
        // must not trigger deletion of our file, and a config that never
        // mentioned a catalog must leave whatever's on disk alone.
        let catalog_path = codex_catalog::models_json_path(codex_dir);
        if codex_catalog_referenced(&existing, &catalog_path.to_string_lossy())
            && catalog_path.exists()
        {
            let _ = fs::remove_file(&catalog_path);
        }
    }

    // Only write if content actually changed — avoids touching mtime
    // for no-op applies and avoids unnecessary fs traffic.
    if new_content != existing {
        if let Err(e) = fs::write(&config_path, &new_content) {
            return ApplyResult {
                success: false,
                message: format!("Codex config error: {}", e),
            };
        }
    }

    // Write the api-key auth.json that Codex v0.130+ expects.
    let auth_payload = serde_json::json!({ "OPENAI_API_KEY": api_key });
    ensure_parent(&auth_path);
    if let Err(e) = fs::write(
        &auth_path,
        serde_json::to_string_pretty(&auth_payload).unwrap_or_default(),
    ) {
        return ApplyResult {
            success: false,
            message: format!("Codex auth.json error: {}", e),
        };
    }

    // Remove the relay file used by pre-direct versions. Current selection is
    // fully represented by config.toml + auth.json now.
    let legacy_relay_path = state_dir.join("codex.json");
    if legacy_relay_path.exists() {
        let _ = fs::remove_file(legacy_relay_path);
    }

    let display = if tool_id == "chatgptdesktop" {
        "ChatGPT"
    } else {
        "Codex CLI"
    };
    ApplyResult {
        success: true,
        message: format!(
            "Model \"{}\" configured for {}.",
            model_info.name.as_deref().unwrap_or(model_id),
            display
        ),
    }
}

pub(super) fn read_codex() -> Option<ModelInfo> {
    let codex_dir = codex_accounts::codex_home().ok()?;
    let content = fs::read_to_string(codex_dir.join("config.toml")).ok()?;

    let model_from_toml = toml_read_top(&content, "model");
    if model_from_toml.is_empty() {
        return None;
    }

    let provider_id = toml_read_top(&content, "model_provider");
    let base_url = if provider_id.is_empty() {
        None
    } else {
        let value = toml_read_table_value(
            &content,
            &format!("model_providers.{}", provider_id),
            "base_url",
        );
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    };

    let model = model_from_toml;

    // API key now lives in ~/.codex/auth.json (preferred_auth_method=apikey).
    // Fall back to the legacy env_key path for configs written before this change.
    let api_key = read_codex_auth_key(&codex_dir).or_else(|| {
        let env_key = if provider_id.is_empty() {
            String::new()
        } else {
            toml_read_table_value(
                &content,
                &format!("model_providers.{}", provider_id),
                "env_key",
            )
        };
        if env_key.is_empty() {
            None
        } else {
            std::env::var(&env_key).ok()
        }
    });

    Some(ModelInfo {
        name: Some(model.clone()),
        model: Some(model),
        base_url,
        api_key,
        anthropic_url: None,
        protocol: Some("openai".to_string()),
        display_model: None,
        relay_mode: None,
        web_search: None,
        one_m_context: None,
    })
}

fn read_codex_auth_key(codex_dir: &Path) -> Option<String> {
    let content = fs::read_to_string(codex_dir.join("auth.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    v.get("OPENAI_API_KEY")
        .and_then(|x| x.as_str())
        .map(String::from)
}

pub(super) fn restore_codex_to_official(tool_id: &str, config_path: &Path) -> ApplyResult {
    // Full-file overwrite. ChatGPT's model picker shows the
    // `model_provider` id VERBATIM, so the built-in lowercase id
    // "openai" rendered as a lowercase "openai" chip — inconsistent with
    // the third-party path, which uses a capitalized "OpenAI" provider.
    // Point at a capitalized "OpenAI" provider so the chip casing
    // matches everywhere.
    //
    // We deliberately do NOT set `base_url`. Codex's `to_api_provider`
    // resolves an unset base_url from the AUTH MODE — chatgpt.com/
    // backend-api/codex for a ChatGPT login, api.openai.com otherwise —
    // which is exactly the built-in openai behavior that keeps
    // ChatGPT-account users working. `requires_openai_auth = true`
    // reproduces the built-in's login-screen / auth.json handling. (A
    // table keyed lowercase "openai" would be silently dropped: Codex's
    // merge does `entry(key).or_insert`, and the built-in already owns
    // that key — only a new "OpenAI" key takes effect.)
    //
    // We also write no `model` line — pinning one (we used to pin
    // "gpt-4o") breaks ChatGPT-account users because OpenAI rejects
    // gpt-4o for that auth path. Without it Codex selects an
    // auth-appropriate default (gpt-5-codex for ChatGPT, otherwise its
    // built-in default). Codex regenerates everything else
    // (projects/marketplaces/tui state) on next launch.
    let content = "model_provider = \"OpenAI\"\n\
                   \n\
                   [model_providers.OpenAI]\n\
                   name = \"OpenAI\"\n\
                   wire_api = \"responses\"\n\
                   requires_openai_auth = true\n";

    ensure_parent(config_path);
    match fs::write(config_path, content) {
        Ok(_) => {
            // Restore auth.json from our backup if we have one (OAuth
            // tokens, prior api-key from before the third-party detour).
            //
            // We do NOT delete auth.json when no backup exists. The old
            // behavior was "fall through to Codex's own login flow",
            // but deleting auth.json out from under a running Codex
            // process produces a worse failure mode: the in-memory
            // React state and the on-disk auth become inconsistent,
            // which surfaces as a "fake account"
            // displayed in the Codex sidebar and silently partitions
            // any chats the user created during the third-party
            // session into an inaccessible namespace. Leaving the
            // third-party apikey in place at worst causes a 401 on
            // next Codex request, which Codex handles loudly via its
            // own re-login UI — better than silent data loss. Users
            // who actually want to log out should use Codex's own
            // logout button.
            let auth_path = config_path
                .parent()
                .unwrap_or(Path::new(""))
                .join("auth.json");
            let auth_backup_path = echobird_dir().join("codex-auth.bak.json");
            if let Err(error) = codex_accounts::restore_legacy_oauth(&auth_path, &auth_backup_path)
            {
                return ApplyResult {
                    success: false,
                    message: format!("Failed to restore Codex account: {error}"),
                };
            }

            let relay_path = echobird_dir().join("codex.json");
            if relay_path.exists() {
                let _ = fs::remove_file(&relay_path);
            }
            ApplyResult {
                success: true,
                message: format!(
                    "{} restored to OpenAI official provider.",
                    if tool_id == "chatgptdesktop" {
                        "ChatGPT"
                    } else {
                        "Codex CLI"
                    }
                ),
            }
        }
        Err(e) => ApplyResult {
            success: false,
            message: format!("Failed to restore Codex config: {}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_search_choice_survives_repeated_apply_for_both_clients() {
        let fixture =
            std::env::temp_dir().join(format!("codex-web-search-{}", uuid::Uuid::new_v4()));
        let codex_dir = fixture.join("codex");
        let state_dir = fixture.join("state");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("config.toml"),
            "# keep\n[projects.fixture]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();
        for tool in ["codex", "chatgptdesktop"] {
            for enabled in [false, false, true, false] {
                let info: ModelInfo = serde_json::from_value(serde_json::json!({
                    "model": "fixture-model",
                    "baseUrl": "https://provider.example/v1",
                    "apiKey": "fixture-key",
                    "webSearch": enabled
                }))
                .unwrap();
                let result = apply_codex_at(tool, &info, &codex_dir, &state_dir);
                assert!(result.success, "{}", result.message);
                let content = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
                assert_eq!(
                    toml_read_top(&content, "web_search"),
                    if enabled { "live" } else { "disabled" }
                );
                assert_eq!(content.matches("web_search =").count(), 1);
                assert!(content.contains("# keep"));
                assert_eq!(
                    toml_read_table_value(&content, "projects.fixture", "trust_level"),
                    "trusted"
                );
                assert_eq!(
                    toml_read_table_value(&content, "model_providers.OpenAI", "wire_api"),
                    "responses"
                );
                assert_eq!(
                    toml_read_table_value(&content, "model_providers.OpenAI", "base_url"),
                    "https://provider.example/v1"
                );
            }
        }
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn explicit_web_search_choice_overrides_provider_defaults() {
        for base_url in [
            "https://api.deepseek.com",
            "https://api.xiaomimimo.com/v1",
            "https://provider.example/v1",
        ] {
            for (enabled, expected) in [(true, "live"), (false, "disabled")] {
                let out = write_codex_canonical_fields(
                    "",
                    base_url,
                    "fixture",
                    DEFAULT_CODEX_CONTEXT_WINDOW,
                    Some(enabled),
                );
                assert_eq!(toml_read_top(&out, "web_search"), expected);
            }
        }
    }

    #[test]
    fn auto_router_disables_hosted_search_without_changing_other_local_providers() {
        for enabled in [None, Some(true), Some(false)] {
            let out = write_codex_canonical_fields(
                "",
                "http://127.0.0.1:53683/v1",
                "auto",
                DEFAULT_CODEX_CONTEXT_WINDOW,
                enabled,
            );
            assert_eq!(toml_read_top(&out, "web_search"), "disabled");
        }
        assert_eq!(
            codex_web_search_mode("http://127.0.0.1:1234/v1", Some(true)),
            "live"
        );
    }

    #[test]
    fn write_codex_canonical_fields_evicts_stale_review_model_on_direct_connect() {
        // Regression: an older EchoBird version wrote `review_model =
        // "gpt-5.5"`. Our write helpers never delete, so it survived every
        // switch. Under a Responses direct-connect session (real model id
        // + real upstream base_url) Codex would still send `gpt-5.5` on its
        // review pass to a gateway that only knows the real id → 4xx. The
        // canonical write must strip the stale line so `model` is the only
        // model key the upstream ever sees.
        let stale = "model_provider = \"OpenAI\"\n\
                     model = \"gpt-5.5\"\n\
                     review_model = \"gpt-5.5\"\n\
                     [model_providers.OpenAI]\n\
                     name = \"OpenAI\"\n";
        let out = write_codex_canonical_fields(
            stale,
            "https://ark.cn-beijing.volces.com/api/coding/v1",
            "glm-5.2",
            DEFAULT_CODEX_CONTEXT_WINDOW,
            None,
        );
        assert!(
            !out.contains("review_model"),
            "stale review_model survived: {out}"
        );
        assert!(out.contains("model = \"glm-5.2\""));
        assert!(out.contains("base_url = \"https://ark.cn-beijing.volces.com/api/coding/v1\""));
    }

    #[test]
    fn write_codex_canonical_fields_evicts_review_model_for_every_provider() {
        // A stale review_model must be removed regardless of the selected
        // direct provider.
        let stale = "model = \"gpt-5.5\"\n\
                     review_model = \"gpt-5.5\"\n\
                     [model_providers.OpenAI]\n";
        let out = write_codex_canonical_fields(
            stale,
            "https://provider.example/v1",
            "provider-model",
            DEFAULT_CODEX_CONTEXT_WINDOW,
            None,
        );
        assert!(!out.contains("review_model"));
    }

    #[test]
    fn write_codex_canonical_fields_evicts_stale_model_catalog_json() {
        // Switching to a non-catalog vendor must not retain another vendor's
        // catalog path.
        let stale = "model_provider = \"OpenAI\"\n\
                     model = \"gpt-5.5\"\n\
                     model_catalog_json = \"C:/Users/x/.codex/models.json\"\n\
                     [model_providers.OpenAI]\n\
                     name = \"OpenAI\"\n";
        let out = write_codex_canonical_fields(
            stale,
            "https://provider.example/v1",
            "provider-model",
            DEFAULT_CODEX_CONTEXT_WINDOW,
            None,
        );
        assert!(
            !out.contains("model_catalog_json"),
            "stale model_catalog_json survived: {out}"
        );
    }

    #[test]
    fn codex_catalog_referenced_matches_only_our_canonical_path() {
        // Our absolute forward-slash form.
        let ours = "C:/Users/x/.codex/models.json";
        let abs = "model_provider = \"OpenAI\"\n\
                   model_catalog_json = \"C:/Users/x/.codex/models.json\"\n\
                   [model_providers.OpenAI]\n";
        assert!(codex_catalog_referenced(abs, ours));
        // Vendor-doc tilde shorthand resolves to the same file.
        let tilde = "model_catalog_json = \"~/.codex/models.json\"\n";
        assert!(codex_catalog_referenced(tilde, ours));
        // A user's own catalog pointed at a DIFFERENT path must NOT match.
        let other_path = "model_catalog_json = \"~/.codex/model-catalogs/custom-catalog.json\"\n";
        assert!(!codex_catalog_referenced(other_path, ours));
        // Config with no catalog line must not match.
        let no_line = "model = \"gpt-5.5\"\n[model_providers.OpenAI]\n";
        assert!(!codex_catalog_referenced(no_line, ours));
    }

    // ── Per-model context window + compaction (Codex) ──
    // apply_codex must write the real model_context_window and a proportional
    // model_auto_compact_token_limit (90% of the window) so a model whose
    // window is smaller than the historic 1M default is not over-claimed.

    #[test]
    fn model_context_window_for_known_model() {
        assert_eq!(model_context_window_for("MiniMax-M2.7"), 204_800);
    }

    #[test]
    fn deepseek_context_matches_official_catalog_in_config_and_generated_catalog() {
        let template = serde_json::from_str(codex_catalog::DEEPSEEK_TEMPLATE).unwrap();
        for model in ["deepseek-flash", "deepseek-v4-pro"] {
            let context = model_context_window_for(model);
            assert_eq!(context, 1_048_576);
            let config =
                write_codex_canonical_fields("", "https://api.deepseek.com", model, context, None);
            assert_eq!(toml_read_top(&config, "model_context_window"), "1048576");
            assert_eq!(
                toml_read_top(&config, "model_auto_compact_token_limit"),
                "943718"
            );
            let catalog = codex_catalog::build_catalog(&template, model, "Custom name", context);
            assert_eq!(catalog["models"][0]["context_window"], 1_048_576);
            assert_eq!(catalog["models"][0]["max_context_window"], 1_048_576);
            assert_eq!(catalog["models"][0]["display_name"], "Custom name");
        }
    }

    #[test]
    fn deepseek_desktop_efforts_preserve_settings_and_do_not_leak_on_switch() {
        let mut config = "# keep\nshow_raw_agent_reasoning = false\n\
                          [desktop]\ncustom-setting = true\n\
                          [projects.fixture]\ntrust_level = \"trusted\"\n"
            .to_string();
        for model in ["deepseek-flash", "deepseek-v4-pro", "deepseek-v4-pro"] {
            config = write_codex_canonical_fields(
                &config,
                "https://api.deepseek.com",
                model,
                model_context_window_for(model),
                None,
            );
            assert_eq!(
                toml_read_table_value(&config, "desktop", "enabled-reasoning-efforts"),
                DEEPSEEK_DESKTOP_EFFORTS
            );
            assert_eq!(config.matches("[desktop]").count(), 1);
            assert_eq!(config.matches("enabled-reasoning-efforts =").count(), 1);
        }
        let reapplied = write_codex_canonical_fields(
            &config,
            "https://api.deepseek.com",
            "deepseek-v4-pro",
            model_context_window_for("deepseek-v4-pro"),
            None,
        );
        assert_eq!(reapplied, config);

        for base_url in [
            "https://api.xiaomimimo.com/v1",
            "https://provider.example/v1",
        ] {
            let other = write_codex_canonical_fields(
                &config,
                base_url,
                "other-model",
                DEFAULT_CODEX_CONTEXT_WINDOW,
                None,
            );
            assert!(!other.contains("enabled-reasoning-efforts"));
            assert!(other.contains("# keep"));
            assert_eq!(toml_read_top(&other, "show_raw_agent_reasoning"), "false");
            assert_eq!(
                toml_read_table_value(&other, "desktop", "custom-setting"),
                "true"
            );
            assert_eq!(
                toml_read_table_value(&other, "projects.fixture", "trust_level"),
                "trusted"
            );
        }
    }

    #[test]
    fn other_providers_preserve_custom_desktop_efforts() {
        for previous_url in ["https://api.deepseek.com", "https://provider.example/v1"] {
            let config = format!(
                "model_provider = \"custom\"\n\
                 [desktop]\nenabled-reasoning-efforts = [\"none\", \"high\"]\n\
                 [model_providers.custom]\nbase_url = \"{previous_url}\"\n"
            );
            let other = write_codex_canonical_fields(
                &config,
                "https://api.xiaomimimo.com/v1",
                "mimo-v2.5",
                DEFAULT_CODEX_CONTEXT_WINDOW,
                None,
            );
            assert_eq!(
                toml_read_table_value(&other, "desktop", "enabled-reasoning-efforts"),
                r#"["none", "high"]"#
            );
        }
    }

    #[test]
    fn vendor_application_writes_isolated_catalogs_and_preserves_switching_state() {
        let fixture =
            std::env::temp_dir().join(format!("echobird-codex-catalog-{}", uuid::Uuid::new_v4()));
        let codex_dir = fixture.join("codex");
        let state_dir = fixture.join("state");
        fs::create_dir_all(&codex_dir).unwrap();
        let config_path = codex_dir.join("config.toml");
        let catalog_path = codex_dir.join("models.json");
        let original_auth = r#"{"OPENAI_API_KEY":"fixture-before"}"#;
        fs::write(codex_dir.join("auth.json"), original_auth).unwrap();
        fs::write(
            &config_path,
            "# keep\n[projects.fixture]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();

        for tool in ["codex", "chatgptdesktop"] {
            for (base_url, model, expected_base, context, effort) in [
                (
                    "https://api.deepseek.com",
                    "deepseek-flash",
                    "https://api.deepseek.com",
                    1_048_576,
                    "high",
                ),
                (
                    "https://api.xiaomimimo.com/v1",
                    "mimo-v2.6-pro",
                    "https://api.xiaomimimo.com/v1",
                    1_048_576,
                    "high",
                ),
                (
                    "https://token-plan-cn.xiaomimimo.com/v1",
                    "mimo-v2.5-pro",
                    "https://token-plan-cn.xiaomimimo.com/v1",
                    1_048_576,
                    "high",
                ),
                (
                    "https://api.minimax.cn/v1",
                    "MiniMax-M3.1-Flash-Preview",
                    "https://api.minimax.cn/v1",
                    524_288,
                    "max",
                ),
                (
                    "https://api.minimax.io/v1",
                    "MiniMax-M3.1-Flash-Preview",
                    "https://api.minimax.io/v1",
                    524_288,
                    "max",
                ),
                (
                    "https://open.bigmodel.cn/api/coding/paas/v4",
                    "glm-5.3",
                    "https://open.bigmodel.cn/api/v1",
                    1_048_576,
                    "max",
                ),
                (
                    "https://api.z.ai/api/coding/paas/v4",
                    "glm-5.3",
                    "https://api.z.ai/api/v1",
                    1_048_576,
                    "max",
                ),
                (
                    "https://open.bigmodel.cn/api/v1",
                    "glm-5-turbo",
                    "https://open.bigmodel.cn/api/v1",
                    204_800,
                    "max",
                ),
                (
                    "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1",
                    "qwen3.8-max",
                    "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1",
                    983_616,
                    "xhigh",
                ),
                (
                    "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
                    "qwen3.8-flash",
                    "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
                    983_616,
                    "xhigh",
                ),
                (
                    "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
                    "deepseek-v4-pro",
                    "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
                    163_840,
                    "medium",
                ),
                (
                    "https://workspace.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
                    "qwen3.8-max",
                    "https://workspace.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
                    983_616,
                    "xhigh",
                ),
            ] {
                let info: ModelInfo = serde_json::from_value(serde_json::json!({
                    "name": "Custom name", "model": model, "baseUrl": base_url,
                    "apiKey": "fixture-key", "webSearch": false
                }))
                .unwrap();
                let result = apply_codex_at(tool, &info, &codex_dir, &state_dir);
                assert!(result.success, "{}", result.message);
                let config = fs::read_to_string(&config_path).unwrap();
                assert_eq!(
                    toml_read_table_value(&config, "model_providers.OpenAI", "base_url"),
                    expected_base
                );
                assert_eq!(
                    toml_read_table_value(&config, "model_providers.OpenAI", "wire_api"),
                    "responses"
                );
                assert_eq!(
                    toml_read_top(&config, "model_context_window"),
                    context.to_string()
                );
                assert_eq!(
                    toml_read_top(&config, "model_auto_compact_token_limit"),
                    codex_compact_limit_for(context).to_string()
                );
                assert_eq!(toml_read_top(&config, "model_reasoning_effort"), effort);
                assert_eq!(
                    toml_read_top(&config, "model_catalog_json"),
                    catalog_path.to_string_lossy().replace('\\', "/")
                );
                assert_eq!(
                    toml_read_table_value(&config, "projects.fixture", "trust_level"),
                    "trusted"
                );
                assert!(config.contains("# keep"));
                let catalog: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(&catalog_path).unwrap()).unwrap();
                assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
                assert_eq!(catalog["models"][0]["slug"], model);
                assert_eq!(catalog["models"][0]["display_name"], "Custom name");
                assert_eq!(catalog["models"][0]["context_window"], context);
                let before = config;
                assert!(apply_codex_at(tool, &info, &codex_dir, &state_dir).success);
                assert_eq!(fs::read_to_string(&config_path).unwrap(), before);
            }
        }
        assert_eq!(
            fs::read_to_string(state_dir.join("codex-auth.bak.json")).unwrap(),
            original_auth
        );
        let unknown: ModelInfo = serde_json::from_value(serde_json::json!({
            "model": "future", "baseUrl": "https://reseller.example/v1", "apiKey": "fixture-key"
        }))
        .unwrap();
        assert!(apply_codex_at("codex", &unknown, &codex_dir, &state_dir).success);
        assert!(!catalog_path.exists());
        assert!(toml_read_top(
            &fs::read_to_string(&config_path).unwrap(),
            "model_catalog_json"
        )
        .is_empty());

        let config_before = fs::read(&config_path).unwrap();
        let auth_before = fs::read(codex_dir.join("auth.json")).unwrap();
        let mut missing_key = unknown;
        missing_key.api_key = None;
        assert!(!apply_codex_at("codex", &missing_key, &codex_dir, &state_dir).success);
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(fs::read(codex_dir.join("auth.json")).unwrap(), auth_before);
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn catalog_write_failure_preserves_config_and_auth() {
        let fixture =
            std::env::temp_dir().join(format!("echobird-codex-write-{}", uuid::Uuid::new_v4()));
        let codex_dir = fixture.join("codex");
        let state_dir = fixture.join("state");
        let catalog_path = codex_dir.join("models.json");
        fs::create_dir_all(&catalog_path).unwrap();
        let config_path = codex_dir.join("config.toml");
        let auth_path = codex_dir.join("auth.json");
        let original_config = "model = \"before\"\n[projects.fixture]\ntrust_level = \"trusted\"\n";
        let original_auth = r#"{"OPENAI_API_KEY":"fixture-before"}"#;
        fs::write(&config_path, original_config).unwrap();
        fs::write(&auth_path, original_auth).unwrap();
        let info: ModelInfo = serde_json::from_value(serde_json::json!({
            "model": "mimo-v2.6-pro", "baseUrl": "https://api.xiaomimimo.com/v1",
            "apiKey": "fixture-new"
        }))
        .unwrap();

        let result = apply_codex_at("codex", &info, &codex_dir, &state_dir);
        assert!(!result.success);
        assert!(result.message.contains("Codex model catalog error"));
        assert_eq!(fs::read_to_string(&config_path).unwrap(), original_config);
        assert_eq!(fs::read_to_string(&auth_path).unwrap(), original_auth);

        fs::remove_dir(&catalog_path).unwrap();
        assert!(apply_codex_at("codex", &info, &codex_dir, &state_dir).success);
        assert!(catalog_path.is_file());
        assert_eq!(
            toml_read_top(&fs::read_to_string(&config_path).unwrap(), "model"),
            "mimo-v2.6-pro"
        );
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn responses_endpoint_normalization_only_changes_documented_glm_routes() {
        for base_url in [
            "https://open.bigmodel.cn/api/v1",
            "https://api.z.ai/api/v1",
            "https://reseller.example/api/coding/paas/v4",
            "https://open.bigmodel.cn/custom/v1",
        ] {
            assert_eq!(codex_responses_base_url(base_url), base_url);
        }
        assert_eq!(
            codex_responses_base_url("https://open.bigmodel.cn/api/paas/v4/"),
            "https://open.bigmodel.cn/api/v1"
        );
        assert_eq!(
            codex_responses_base_url("https://api.z.ai/api/coding/paas/v4/"),
            "https://api.z.ai/api/v1"
        );
    }

    #[test]
    fn model_context_window_for_unknown_model_defaults_to_1m() {
        assert_eq!(
            model_context_window_for("glm-5.2"),
            DEFAULT_CODEX_CONTEXT_WINDOW
        );
    }

    #[test]
    fn codex_compact_limit_is_90_percent_of_window() {
        assert_eq!(codex_compact_limit_for(1_000_000), 900_000);
        assert_eq!(codex_compact_limit_for(204_800), 184_320);
    }

    #[test]
    fn write_codex_canonical_fields_evicts_deprecated_response_storage_flag() {
        let stale = "disable_response_storage = true\nmodel = \"gpt-5.5\"\n";
        let out = write_codex_canonical_fields(
            stale,
            "https://provider.example/v1",
            "provider-model",
            DEFAULT_CODEX_CONTEXT_WINDOW,
            None,
        );
        assert!(
            !out.contains("disable_response_storage"),
            "deprecated flag survived: {out}"
        );
    }

    #[test]
    fn write_codex_canonical_fields_writes_model_context_window() {
        // A 204,800-token model must get model_context_window = 204800 and
        // model_auto_compact_token_limit = 184320 (90%), not the historic
        // 1,000,000 / 900,000 defaults.
        let out = write_codex_canonical_fields(
            "model = \"gpt-5.5\"\n[model_providers.OpenAI]\n",
            "http://127.0.0.1:53682/v1",
            "gpt-5.5",
            204_800,
            None,
        );
        assert!(out.contains("model_context_window = 204800"), "got: {out}");
        assert!(
            out.contains("model_auto_compact_token_limit = 184320"),
            "got: {out}"
        );
        assert!(out.contains("web_search = \"live\""), "got: {out}");
        assert!(
            !out.contains("model_context_window = 1000000"),
            "got: {out}"
        );
        assert!(
            !out.contains("model_auto_compact_token_limit = 900000"),
            "got: {out}"
        );
    }

    #[test]
    fn official_deepseek_and_mimo_configs_disable_web_search() {
        for base_url in [
            "https://api.deepseek.com",
            "https://api.xiaomimimo.com/v1",
            "https://token-plan-cn.xiaomimimo.com/v1",
        ] {
            let out = write_codex_canonical_fields(
                "",
                base_url,
                "provider-model",
                DEFAULT_CODEX_CONTEXT_WINDOW,
                None,
            );
            assert!(out.contains("web_search = \"disabled\""), "got: {out}");
        }
    }

    #[test]
    fn mimo_reasoning_flags_do_not_leak_to_other_providers() {
        let mimo = write_codex_canonical_fields(
            "",
            "https://api.xiaomimimo.com/v1",
            "mimo-v2.5-pro",
            DEFAULT_CODEX_CONTEXT_WINDOW,
            None,
        );
        assert!(mimo.contains("model_supports_reasoning_summaries = true"));
        assert!(mimo.contains("model_reasoning_summary = \"none\""));

        let other = write_codex_canonical_fields(
            &mimo,
            "https://provider.example/v1",
            "provider-model",
            DEFAULT_CODEX_CONTEXT_WINDOW,
            None,
        );
        assert!(!other.contains("model_supports_reasoning_summaries"));
        assert!(!other.contains("model_reasoning_summary"));
        assert!(other.contains("web_search = \"live\""));
    }
}
