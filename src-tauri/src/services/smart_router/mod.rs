mod runtime;
mod server;

pub(crate) use server::forget_account_authentication;

#[cfg(test)]
pub(crate) use server::route_live_account_probe;

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Mutex, OnceLock};

use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::models::model::{ModelConfig, ModelScope, ModelType};
use crate::services::model_manager;
use crate::utils::platform::echobird_dir;

pub const SMART_ROUTER_PORT: u16 = 53683;
pub const SMART_ROUTER_INTERNAL_ID: &str = "smart-router";
pub const SMART_ROUTER_MODEL_ID: &str = "auto";

static CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);
static PORT: AtomicU16 = AtomicU16::new(SMART_ROUTER_PORT);
static SERVER: tokio::sync::Mutex<Option<runtime::ServerControl>> =
    tokio::sync::Mutex::const_new(None);

pub fn port() -> u16 {
    PORT.load(Ordering::Relaxed)
}

pub(crate) fn is_router_url(raw: &str) -> bool {
    is_loopback_url_on_port(raw, port()) || is_loopback_url_on_port(raw, SMART_ROUTER_PORT)
}

fn is_loopback_url_on_port(raw: &str, port: u16) -> bool {
    url::Url::parse(raw.trim()).ok().is_some_and(|url| {
        matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            && url.port_or_known_default() == Some(port)
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConfig {
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    candidate_ids: Vec<String>,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    defaults_initialization_pending: bool,
}

impl Default for StoredConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            version: default_version(),
            candidate_ids: Vec::new(),
            api_key: String::new(),
            defaults_initialization_pending: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicConfig {
    pub enabled: bool,
    pub candidate_ids: Vec<String>,
    pub usable_candidate_count: usize,
    pub base_url: String,
    pub model_id: String,
    pub port: u16,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicActivity {
    pub candidate_id: Option<String>,
    pub active: bool,
    pub sequence: u64,
    pub updated_at_ms: u64,
}

fn default_version() -> u32 {
    1
}

fn default_enabled() -> bool {
    true
}

fn config_path() -> PathBuf {
    echobird_dir().join("config").join("smart-router.json")
}

fn config_lock() -> &'static Mutex<()> {
    CONFIG_LOCK.get_or_init(|| Mutex::new(()))
}

fn generate_api_key() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("eb-local-{}", hex::encode(bytes))
}

fn save_config_unlocked(config: &StoredConfig) -> Result<(), String> {
    save_config_at(&config_path(), config)
}

fn save_config_at(path: &Path, config: &StoredConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("create smart router config directory failed: {e}"))?;
    }
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("serialize smart router config failed: {e}"))?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(|e| format!("write {} failed: {e}", path.display()))
}

fn load_config_unlocked() -> Result<StoredConfig, String> {
    let path = config_path();
    let mut config = if path.exists() {
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("read {} failed: {e}", path.display()))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("parse {} failed: {e}", path.display()))?
    } else {
        let mut config = StoredConfig::default();
        if let Some(models) = model_manager::prepare_initial_router_defaults()? {
            config.candidate_ids = models.into_iter().map(|model| model.internal_id).collect();
            config.defaults_initialization_pending = true;
            save_config_unlocked(&config)?;
        }
        config
    };

    if config.api_key.is_empty() {
        config.api_key = model_manager::encrypt_key_for_storage(&generate_api_key());
        save_config_unlocked(&config)?;
    } else if !config.api_key.starts_with("enc:v1:") {
        config.api_key = model_manager::encrypt_key_for_storage(&config.api_key);
        save_config_unlocked(&config)?;
    }

    if config.defaults_initialization_pending {
        model_manager::persist_initial_router_defaults()?;
        config.defaults_initialization_pending = false;
        save_config_unlocked(&config)?;
    }

    Ok(config)
}

fn load_config() -> Result<StoredConfig, String> {
    let _guard = config_lock()
        .lock()
        .map_err(|_| "smart router config lock poisoned".to_string())?;
    load_config_unlocked()
}

fn public_config(config: StoredConfig) -> PublicConfig {
    let port = port();
    let usable_candidate_count = usable_candidate_count(&config.candidate_ids);
    PublicConfig {
        enabled: config.enabled,
        candidate_ids: config.candidate_ids,
        usable_candidate_count,
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model_id: SMART_ROUTER_MODEL_ID.to_string(),
        port,
        running: RUNNING.load(Ordering::Relaxed),
    }
}

pub fn get_public_config() -> Result<PublicConfig, String> {
    load_config().map(public_config)
}

pub fn get_public_activity() -> PublicActivity {
    server::public_activity()
}

pub fn set_candidate_ids(candidate_ids: Vec<String>) -> Result<PublicConfig, String> {
    let user_models = model_manager::get_user_models();
    let _guard = config_lock()
        .lock()
        .map_err(|_| "smart router config lock poisoned".to_string())?;
    let mut config = load_config_unlocked()?;
    config.candidate_ids = prepare_candidate_ids(
        candidate_ids,
        &config.candidate_ids,
        &user_models,
        &crate::services::local_llm::get_server_info_sync(),
    )?;
    save_config_unlocked(&config)?;
    server::retain_candidate_memory(&config.candidate_ids);
    Ok(public_config(config))
}

fn prepare_candidate_ids(
    candidate_ids: Vec<String>,
    existing_ids: &[String],
    user_models: &[ModelConfig],
    local_server: &crate::services::local_llm::LocalServerInfo,
) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let candidate_ids: Vec<String> = candidate_ids
        .into_iter()
        .filter(|id| id != SMART_ROUTER_INTERNAL_ID)
        .filter(|id| seen.insert(id.clone()))
        .collect();
    if candidate_ids.len() > server::MAX_ROUTE_CANDIDATES {
        return Err(format!(
            "Smart Router supports up to {} candidate models",
            server::MAX_ROUTE_CANDIDATES
        ));
    }

    for id in &candidate_ids {
        if existing_ids.contains(id) {
            continue;
        }
        let routable = if id == "local-server" {
            local_server_is_routable(local_server)
        } else {
            user_models
                .iter()
                .any(|model| model.internal_id == *id && model_is_routable(model))
        };
        if !routable {
            return Err(format!(
                "Smart Router cannot add an unavailable model: {id}"
            ));
        }
    }

    Ok(candidate_ids)
}

pub fn get_candidate_models() -> Vec<ModelConfig> {
    let candidate_ids: HashSet<String> = candidate_ids().into_iter().collect();
    let mut models: Vec<ModelConfig> = model_manager::get_user_models()
        .into_iter()
        .filter(|model| candidate_ids.contains(&model.internal_id))
        .collect();
    if candidate_ids.contains("local-server") {
        let local_server = crate::services::local_llm::get_server_info_sync();
        if local_server_is_routable(&local_server) {
            models.push(ModelConfig {
                internal_id: "local-server".to_string(),
                name: local_server.model_name.clone(),
                model_id: Some(local_server.model_name),
                base_url: format!("http://127.0.0.1:{}/v1", local_server.port),
                api_key: local_server.api_key,
                account_source: None,
                anthropic_url: Some(format!("http://127.0.0.1:{}/anthropic", local_server.port)),
                model_type: Some(ModelType::Local),
                openai_tested: None,
                anthropic_tested: None,
                openai_latency: None,
                anthropic_latency: None,
                scope: ModelScope::ModelCenter,
            });
        }
    }
    models
}

fn model_is_routable(model: &ModelConfig) -> bool {
    model.internal_id != SMART_ROUTER_INTERNAL_ID
        && model
            .model_id
            .as_deref()
            .is_some_and(|model_id| !model_id.trim().is_empty())
        && (model
            .account_source
            .as_ref()
            .is_some_and(|s| !s.account_id.is_empty())
            || (!model.base_url.trim().is_empty()
                && !is_router_url(&model.base_url)
                && (!model_manager::decrypt_key_for_use(&model.api_key)
                    .trim()
                    .is_empty()
                    || crate::services::free_model_directory::is_keyless_model(
                        &model.base_url,
                        model.model_id.as_deref().unwrap_or_default(),
                    ))))
}

fn local_server_is_routable(server: &crate::services::local_llm::LocalServerInfo) -> bool {
    server.running
        && server.port != 0
        && !server.model_name.trim().is_empty()
        && !server.api_key.is_empty()
}

fn router_owns_model(model: &ModelConfig) -> bool {
    model.scope == ModelScope::SmartRouter
}

fn usable_candidate_count(candidate_ids: &[String]) -> usize {
    usable_candidate_count_from(
        candidate_ids,
        &model_manager::get_user_models(),
        &crate::services::local_llm::get_server_info_sync(),
    )
}

fn usable_candidate_count_from(
    candidate_ids: &[String],
    user_models: &[ModelConfig],
    local_server: &crate::services::local_llm::LocalServerInfo,
) -> usize {
    candidate_ids
        .iter()
        .filter(|candidate_id| {
            (candidate_id.as_str() == "local-server" && local_server_is_routable(local_server))
                || user_models.iter().any(|model| {
                    model.internal_id == candidate_id.as_str() && model_is_routable(model)
                })
        })
        .count()
}

pub fn remove_candidate(internal_id: &str) -> Result<PublicConfig, String> {
    let configured_ids = candidate_ids();
    let is_configured = configured_ids.iter().any(|id| id == internal_id);
    let stored_model = model_manager::get_user_models()
        .into_iter()
        .find(|model| model.internal_id == internal_id);
    let delete_owned_model = is_configured
        && internal_id != "local-server"
        && stored_model.as_ref().is_some_and(router_owns_model);

    let remaining = configured_ids
        .into_iter()
        .filter(|id| id != internal_id)
        .collect();
    let config = set_candidate_ids(remaining)?;
    if delete_owned_model && !model_manager::delete_model(internal_id) {
        return Err(format!("Smart Router candidate not found: {internal_id}"));
    }
    Ok(config)
}

pub(crate) fn forget_candidate_memory(internal_id: &str) {
    server::forget_candidate_memory(internal_id);
}

pub(crate) fn candidate_ids() -> Vec<String> {
    load_config()
        .map(|config| config.candidate_ids)
        .unwrap_or_else(|e| {
            log::error!("[SmartRouter] Failed to load candidates: {e}");
            Vec::new()
        })
}

pub(crate) fn api_key_for_use() -> Result<String, String> {
    request_config_for_use().map(|config| config.api_key)
}

pub(super) struct RequestConfig {
    pub api_key: String,
    pub candidate_ids: Vec<String>,
}

fn request_config_from_loader(
    load: impl FnOnce() -> Result<StoredConfig, String>,
) -> Result<RequestConfig, String> {
    let config = load()?;
    let api_key = model_manager::decrypt_key_for_use(&config.api_key);
    if api_key.is_empty() {
        Err("smart router API key could not be decrypted".to_string())
    } else {
        Ok(RequestConfig {
            api_key,
            candidate_ids: config.candidate_ids,
        })
    }
}

pub(super) fn request_config_for_use() -> Result<RequestConfig, String> {
    request_config_from_loader(load_config)
}

pub fn model_config() -> Option<ModelConfig> {
    let port = port();
    let config = load_config().ok()?;
    if usable_candidate_count(&config.candidate_ids) == 0 {
        return None;
    }

    Some(ModelConfig {
        internal_id: SMART_ROUTER_INTERNAL_ID.to_string(),
        name: "Auto Router".to_string(),
        model_id: Some(SMART_ROUTER_MODEL_ID.to_string()),
        base_url: format!("http://127.0.0.1:{port}/v1"),
        api_key: config.api_key,
        account_source: None,
        anthropic_url: Some(format!("http://127.0.0.1:{port}")),
        model_type: Some(ModelType::Local),
        openai_tested: None,
        anthropic_tested: None,
        openai_latency: None,
        anthropic_latency: None,
        scope: ModelScope::ModelCenter,
    })
}

pub fn spawn_proxy_task() {
    if let Err(error) = tauri::async_runtime::block_on(async {
        let mut server = SERVER.lock().await;
        PORT.store(
            super::local_proxy::saved_port("smart-router", SMART_ROUTER_PORT)?,
            Ordering::Relaxed,
        );
        if load_config()?.enabled {
            *server = Some(start_server()?);
        }
        Ok::<_, String>(())
    }) {
        log::error!("[SmartRouter] {error}");
    }
}

fn start_server() -> Result<runtime::ServerControl, String> {
    let listener = super::local_proxy::bind("smart-router", SMART_ROUTER_PORT)?;
    PORT.store(
        listener.local_addr().expect("bound listener").port(),
        Ordering::Relaxed,
    );
    server::start(listener)
}

fn save_enabled(enabled: bool) -> Result<(), String> {
    let _guard = config_lock()
        .lock()
        .map_err(|_| "smart router config lock poisoned".to_string())?;
    let mut config = load_config_unlocked()?;
    config.enabled = enabled;
    save_config_unlocked(&config)
}

pub async fn set_enabled(enabled: bool) -> Result<PublicConfig, String> {
    let mut server = SERVER.lock().await;
    if enabled {
        if !RUNNING.load(Ordering::Relaxed) {
            if let Some(previous) = server.take() {
                previous.stop().await;
            }
            let started = start_server()?;
            if let Err(error) = save_enabled(true) {
                started.stop().await;
                return Err(error);
            }
            *server = Some(started);
        } else {
            save_enabled(true)?;
        }
    } else {
        save_enabled(false)?;
        if let Some(previous) = server.take() {
            previous.stop().await;
        }
    }
    get_public_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_snapshot_follows_config_updates_without_changing_inflight_ids() {
        let path = std::env::temp_dir().join(format!(
            "echobird-router-request-{}.json",
            uuid::Uuid::new_v4()
        ));
        let load = || {
            let bytes = fs::read(&path).map_err(|error| error.to_string())?;
            serde_json::from_slice::<StoredConfig>(&bytes).map_err(|error| error.to_string())
        };
        let mut stored = StoredConfig {
            api_key: "fixture-first-key".into(),
            candidate_ids: vec!["first".into(), "second".into()],
            ..StoredConfig::default()
        };
        save_config_at(&path, &stored).unwrap();
        let accepted = request_config_from_loader(load).unwrap();

        stored.api_key = "fixture-next-key".into();
        stored.candidate_ids.reverse();
        save_config_at(&path, &stored).unwrap();
        let next = request_config_from_loader(load).unwrap();
        assert_eq!(next.api_key, "fixture-next-key");
        assert_eq!(next.candidate_ids, vec!["second", "first"]);
        assert_eq!(accepted.api_key, "fixture-first-key");
        assert_eq!(accepted.candidate_ids, vec!["first", "second"]);

        fs::write(&path, "invalid config").unwrap();
        assert!(request_config_from_loader(load).is_err());
        fs::remove_file(&path).unwrap();
        assert!(request_config_from_loader(load).is_err());
    }

    #[test]
    fn request_snapshot_rejects_unusable_entry_keys() {
        for api_key in ["", "enc:v1:zz"] {
            let result = request_config_from_loader(|| {
                Ok(StoredConfig {
                    api_key: api_key.into(),
                    candidate_ids: vec!["first".into()],
                    ..StoredConfig::default()
                })
            });
            assert_eq!(
                result.err().as_deref(),
                Some("smart router API key could not be decrypted")
            );
        }
        assert_eq!(
            request_config_from_loader(|| Err("unreadable config".into()))
                .err()
                .as_deref(),
            Some("unreadable config")
        );
    }

    #[test]
    fn config_initialization_journal_survives_atomic_replacement() {
        let path = std::env::temp_dir().join(format!(
            "echobird-router-journal-{}.json",
            uuid::Uuid::new_v4()
        ));
        let mut config = StoredConfig {
            candidate_ids: vec!["first".into(), "second".into()],
            defaults_initialization_pending: true,
            ..StoredConfig::default()
        };
        save_config_at(&path, &config).unwrap();
        let pending: StoredConfig = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(pending.defaults_initialization_pending);
        assert!(pending.api_key.is_empty());
        config.api_key = "enc:v1:local-key".into();
        config.defaults_initialization_pending = false;
        save_config_at(&path, &config).unwrap();
        let completed: StoredConfig = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(!completed.defaults_initialization_pending);
        assert_eq!(completed.candidate_ids, pending.candidate_ids);
        assert_eq!(completed.api_key, config.api_key);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_config_replacement_preserves_storage_and_cleans_staging_file() {
        let directory =
            std::env::temp_dir().join(format!("echobird-router-journal-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("smart-router.json");
        fs::create_dir(&path).unwrap();
        let previous = path.join("previous");
        fs::write(&previous, "keep existing storage").unwrap();
        assert!(save_config_at(&path, &StoredConfig::default()).is_err());
        assert_eq!(
            fs::read_to_string(&previous).unwrap(),
            "keep existing storage"
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_file(previous).unwrap();
        fs::remove_dir(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn enabled_defaults_on_and_persists_without_changing_candidates() {
        let mut config: StoredConfig =
            serde_json::from_str(r#"{"candidateIds":["first","second"],"apiKey":"key"}"#).unwrap();
        assert!(config.enabled);
        assert!(!config.defaults_initialization_pending);
        config.enabled = false;
        let restored: StoredConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert!(!restored.enabled);
        assert_eq!(restored.candidate_ids, vec!["first", "second"]);
        assert_eq!(restored.api_key, "key");
    }

    #[test]
    fn loopback_detection_uses_the_actual_port() {
        for host in ["127.0.0.1", "localhost", "[::1]"] {
            assert!(is_loopback_url_on_port(
                &format!("http://{host}:41234/v1"),
                41234
            ));
        }
        assert!(!is_loopback_url_on_port("http://127.0.0.1:41235/v1", 41234));
        assert!(!is_loopback_url_on_port(
            "https://example.com:41234/v1",
            41234
        ));
        assert!(is_router_url(&format!("http://127.0.0.1:{}/v1", port())));
    }

    #[test]
    fn generated_api_key_is_prefixed_and_random_sized() {
        let key = generate_api_key();
        assert!(key.starts_with("eb-local-"));
        assert_eq!(key.len(), "eb-local-".len() + 64);
    }

    #[test]
    fn candidate_requires_model_url_and_key() {
        let mut model = ModelConfig {
            internal_id: "candidate".to_string(),
            name: "Candidate".to_string(),
            model_id: Some("model-id".to_string()),
            base_url: "https://example.com/v1".to_string(),
            api_key: "key".to_string(),
            account_source: None,
            anthropic_url: None,
            model_type: Some(ModelType::Cloud),
            openai_tested: None,
            anthropic_tested: None,
            openai_latency: None,
            anthropic_latency: None,
            scope: ModelScope::SmartRouter,
        };
        assert!(model_is_routable(&model));
        model.name.clear();
        assert!(model_is_routable(&model));

        model.model_id = Some(String::new());
        assert!(!model_is_routable(&model));
        model.model_id = Some("model-id".to_string());
        model.base_url.clear();
        model.anthropic_url = Some("https://example.com/anthropic".to_string());
        assert!(!model_is_routable(&model));
        model.base_url = "https://example.com/v1".to_string();
        model.api_key.clear();
        assert!(!model_is_routable(&model));
        model.api_key = "   ".to_string();
        assert!(!model_is_routable(&model));
    }

    #[test]
    fn new_unroutable_candidates_are_rejected_before_changing_the_list() {
        let local = crate::services::local_llm::LocalServerInfo::default();
        for model in [
            serde_json::json!({"internalId":"new","name":"Anthropic only","modelId":"fixture",
                "baseUrl":"","anthropicUrl":"https://example.test/anthropic","apiKey":"fixture-key"}),
            serde_json::json!({"internalId":"new","name":"Missing key","modelId":"fixture",
                "baseUrl":"https://example.test/v1","apiKey":""}),
            serde_json::json!({"internalId":"new","name":"Missing model","modelId":"",
                "baseUrl":"https://example.test/v1","apiKey":"fixture-key"}),
        ] {
            let model: ModelConfig = serde_json::from_value(model).unwrap();
            assert!(prepare_candidate_ids(vec!["new".into()], &[], &[model], &local).is_err());
        }
        for id in ["missing", "local-server"] {
            assert!(prepare_candidate_ids(vec![id.into()], &[], &[], &local).is_err());
        }
    }

    #[test]
    fn existing_unavailable_candidates_can_be_reordered_and_removed() {
        let legacy: ModelConfig = serde_json::from_value(serde_json::json!({
            "internalId":"legacy","name":"Legacy","modelId":"fixture","baseUrl":"",
            "anthropicUrl":"https://example.test/anthropic","apiKey":""
        }))
        .unwrap();
        let local = crate::services::local_llm::LocalServerInfo::default();
        let existing = vec!["legacy".into(), "missing".into(), "local-server".into()];
        let reordered = vec!["local-server".into(), "missing".into(), "legacy".into()];
        assert_eq!(
            prepare_candidate_ids(reordered.clone(), &existing, &[legacy], &local).unwrap(),
            reordered
        );
        assert_eq!(
            prepare_candidate_ids(vec!["missing".into()], &existing, &[], &local).unwrap(),
            vec!["missing"]
        );
        assert!(prepare_candidate_ids(Vec::new(), &existing, &[], &local)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn available_account_keyless_and_local_candidates_can_still_be_added() {
        let account: ModelConfig = serde_json::from_value(serde_json::json!({
            "internalId":"account","name":"Account","modelId":"fixture","baseUrl":"",
            "accountSource":{"provider":"codex","accountId":"fixture-account"}
        }))
        .unwrap();
        let keyless: ModelConfig = serde_json::from_value(serde_json::json!({
            "internalId":"keyless","name":"Keyless","modelId":"kilo-auto/free",
            "baseUrl":"https://api.kilo.ai/api/gateway","apiKey":""
        }))
        .unwrap();
        let local = crate::services::local_llm::LocalServerInfo {
            running: true,
            port: 53682,
            model_name: "fixture-local".into(),
            api_key: "fixture-key".into(),
            ..Default::default()
        };
        let requested = vec!["account".into(), "keyless".into(), "local-server".into()];
        assert_eq!(
            prepare_candidate_ids(requested.clone(), &[], &[account, keyless], &local).unwrap(),
            requested
        );
    }

    #[test]
    fn keyless_candidates_count_only_for_their_catalog_model_and_endpoint() {
        let mut model: ModelConfig = serde_json::from_value(serde_json::json!({
            "internalId": "keyless",
            "name": "Keyless",
            "modelId": "kilo-auto/free",
            "baseUrl": "https://api.kilo.ai/api/gateway",
            "apiKey": ""
        }))
        .unwrap();
        let ids = vec![model.internal_id.clone()];
        let local_server = crate::services::local_llm::LocalServerInfo::default();
        model.base_url = "https://api.kilo.ai/api/gateway".into();
        model.model_id = Some("kilo-auto/free".into());
        assert!(model_is_routable(&model));
        assert_eq!(
            usable_candidate_count_from(&ids, &[model.clone()], &local_server),
            1
        );
        for (base_url, model_id) in [
            ("https://example.com/v1", "kilo-auto/free"),
            (
                "https://api.kilo.ai/api/gateway",
                "anthropic/claude-sonnet-4",
            ),
            ("https://opencode.ai/zen/v1", "claude-opus-5"),
            ("https://opencode.ai/zen/v1", "space-bunny-free"),
            ("https://opencode.ai/zen/v1", ""),
        ] {
            model.base_url = base_url.into();
            model.model_id = Some(model_id.into());
            assert!(!model_is_routable(&model));
            assert_eq!(
                usable_candidate_count_from(&ids, &[model.clone()], &local_server),
                0
            );
        }
    }

    #[test]
    fn router_owns_only_router_scoped_models() {
        let mut model = ModelConfig {
            internal_id: "candidate".to_string(),
            name: "Candidate".to_string(),
            model_id: Some("model-id".to_string()),
            base_url: "https://example.com/v1".to_string(),
            api_key: "key".to_string(),
            account_source: None,
            anthropic_url: None,
            model_type: Some(ModelType::Cloud),
            openai_tested: None,
            anthropic_tested: None,
            openai_latency: None,
            anthropic_latency: None,
            scope: ModelScope::ModelCenter,
        };

        assert!(!router_owns_model(&model));
        model.scope = ModelScope::SmartRouter;
        assert!(router_owns_model(&model));
    }

    #[test]
    fn stopped_local_server_is_not_usable() {
        let server = crate::services::local_llm::LocalServerInfo {
            running: false,
            port: 53682,
            model_name: "local-model".to_string(),
            pid: None,
            api_key: "local-key".to_string(),
            runtime: "llama-server".to_string(),
        };
        assert!(!local_server_is_routable(&server));

        let running = crate::services::local_llm::LocalServerInfo {
            running: true,
            ..server
        };
        assert!(local_server_is_routable(&running));
    }
}
