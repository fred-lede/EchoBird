//! Kilo CLI providers and Kilo Desktop's separate new-chat model preferences.

use super::{echobird_dir, model_input_modalities_for, write_json_file, ApplyResult, ModelInfo};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

const PROVIDER: &str = "echobird";
const CONFIG_NAMES: [&str; 3] = ["config.json", "kilo.json", "kilo.jsonc"];

fn kilo_config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("kilo")
}

fn desktop_data_dir() -> PathBuf {
    // Electron uses Application Support on macOS; XDG_CONFIG_HOME on Linux.
    dirs::config_dir().unwrap_or_default().join("Kilo Desktop")
}

fn desktop_config_dir() -> PathBuf {
    desktop_data_dir().join("plugins/kilo-server/runtime/config/kilo")
}

fn desktop_preferences_path() -> PathBuf {
    desktop_data_dir().join("plugins/kilo-ui/model-preferences.json")
}

fn write_path(dir: &Path) -> PathBuf {
    let jsonc = dir.join("kilo.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        dir.join("kilo.json")
    }
}

fn load_object(path: &Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Cannot read Kilo config {}: {e}", path.display()))?;
    jsonc_parser::parse_to_serde_value::<Value>(&content, &Default::default())
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| format!("Cannot parse Kilo config: {}", path.display()))
}

fn merge(target: &mut Value, source: Value) {
    match (target, source) {
        (Value::Object(target), Value::Object(source)) => {
            for (key, value) in source {
                merge(target.entry(key).or_insert(Value::Null), value);
            }
        }
        (target, source) => *target = source,
    }
}

fn merged_config(dirs: &[&Path]) -> Result<Value, String> {
    let mut result = json!({});
    for dir in dirs {
        for name in CONFIG_NAMES {
            merge(&mut result, load_object(&dir.join(name))?);
        }
    }
    Ok(result)
}

fn select_desktop_model(prefs: &mut Value, provider: &str, model: &str) -> Result<(), String> {
    let selected = json!({"providerID": provider, "modelID": model});
    if let Some(selections) = prefs.get("modelSelections") {
        if !selections.is_object() {
            return Err("Kilo modelSelections must be an object".into());
        }
    } else {
        prefs["modelSelections"] = json!({});
    }
    for value in prefs["modelSelections"]
        .as_object_mut()
        .into_iter()
        .flat_map(|v| v.values_mut())
    {
        *value = selected.clone();
    }
    prefs["preferred"] = json!({"providerID": provider, "modelID": model, "variant": ""});
    let mut recent = prefs
        .get("recentModels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    recent.retain(|v| v != &selected);
    recent.insert(0, selected);
    recent.truncate(5);
    prefs["recentModels"] = json!(recent);
    Ok(())
}

fn apply_at(dir: &Path, preferences: Option<&Path>, info: &ModelInfo) -> Result<(), String> {
    let model = info.model.as_deref().or(info.name.as_deref()).unwrap_or("");
    if model.trim().is_empty() {
        return Err("Model ID is empty, cannot apply config".into());
    }
    let path = write_path(dir);
    let mut config = load_object(&path)?;
    let mut prefs = preferences.map(load_object).transpose()?;
    if let Some(provider) = config.get("provider") {
        if !provider.is_object() {
            return Err("Kilo provider config must be an object".into());
        }
    } else {
        config["provider"] = json!({});
    }
    config["provider"][PROVIDER] = json!({
        "npm": "@ai-sdk/openai-compatible", "name": "EchoBird",
        "options": {
            "baseURL": info.base_url.as_deref().unwrap_or("https://api.openai.com/v1").trim_end_matches('/'),
            "apiKey": info.api_key.as_deref().unwrap_or("")
        },
        "models": {model: {
            "name": info.name.as_deref().unwrap_or(model),
            "modalities": {"input": model_input_modalities_for(model), "output": ["text"]}
        }}
    });
    if let Some(prefs) = prefs.as_mut() {
        select_desktop_model(prefs, PROVIDER, model)?;
    } else {
        config["model"] = json!(format!("{PROVIDER}/{model}"));
        config["small_model"] = config["model"].clone();
    }
    write_json_file(&path, &config)?;
    if let (Some(path), Some(prefs)) = (preferences, prefs) {
        write_json_file(path, &prefs)?;
    }
    Ok(())
}

fn result(value: Result<(), String>, message: &str) -> ApplyResult {
    match value {
        Ok(()) => ApplyResult {
            success: true,
            message: message.into(),
        },
        Err(message) => ApplyResult {
            success: false,
            message,
        },
    }
}

pub(super) fn apply_kilo(info: &ModelInfo) -> ApplyResult {
    result(
        apply_at(&kilo_config_dir(), None, info),
        "Kilo CLI model configured. Restart kilo to apply.",
    )
}

pub(super) fn apply_kilo_desktop(info: &ModelInfo) -> ApplyResult {
    result(apply_at(&desktop_config_dir(), Some(&desktop_preferences_path()), info),
        "Kilo Desktop default model configured. Reopen the app; existing chats keep their own model.")
}

fn model_info(config: &Value, provider_id: &str, model: &str) -> Option<ModelInfo> {
    let provider = config.get("provider")?.get(provider_id)?;
    let entry = provider.get("models")?.get(model)?;
    serde_json::from_value(json!({
        "model": model, "name": entry.get("name").and_then(Value::as_str).unwrap_or(model),
        "baseUrl": provider.pointer("/options/baseURL"), "apiKey": provider.pointer("/options/apiKey"),
        "protocol": "openai"
    })).ok()
}

pub(super) fn read_kilo() -> Option<ModelInfo> {
    let config = merged_config(&[&kilo_config_dir()]).ok()?;
    let (provider, model) = config.get("model")?.as_str()?.split_once('/')?;
    model_info(&config, provider, model)
}

pub(super) fn read_kilo_desktop() -> Option<ModelInfo> {
    let config = merged_config(&[&kilo_config_dir(), &desktop_config_dir()]).ok()?;
    let prefs = load_object(&desktop_preferences_path()).ok()?;
    if let Some(preferred) = prefs.get("preferred") {
        return model_info(
            &config,
            preferred["providerID"].as_str()?,
            preferred["modelID"].as_str()?,
        );
    }
    let (provider, model) = config.get("model")?.as_str()?.split_once('/')?;
    model_info(&config, provider, model)
}

/// Only the CLI launcher uses --model; Desktop has its own preference store.
pub fn kilo_echobird_model() -> Option<String> {
    let config = merged_config(&[&kilo_config_dir()]).ok()?;
    echobird_model_from(&config)
}

fn echobird_model_from(config: &Value) -> Option<String> {
    config
        .get("model")?
        .as_str()?
        .strip_prefix("echobird/")
        .map(str::to_owned)
}

fn restore_at(dir: &Path, preferences: Option<&Path>) -> Result<(), String> {
    let mut configs = Vec::new();
    for name in CONFIG_NAMES {
        let path = dir.join(name);
        if path.exists() {
            configs.push((path.clone(), load_object(&path)?));
        }
    }
    let mut prefs = preferences
        .filter(|p| p.exists())
        .map(load_object)
        .transpose()?;
    if let Some(prefs) = prefs.as_mut() {
        if prefs
            .get("preferred")
            .is_some_and(|p| p["providerID"] == PROVIDER)
        {
            prefs.as_object_mut().unwrap().remove("preferred");
        }
        if let Some(selections) = prefs
            .get_mut("modelSelections")
            .and_then(Value::as_object_mut)
        {
            selections.retain(|_, v| v["providerID"] != PROVIDER);
        }
        if let Some(recent) = prefs.get_mut("recentModels").and_then(Value::as_array_mut) {
            recent.retain(|v| v["providerID"] != PROVIDER);
        }
    }
    for (path, mut config) in configs {
        let before = config.clone();
        if let Some(provider) = config.get_mut("provider").and_then(Value::as_object_mut) {
            provider.remove(PROVIDER);
        }
        for key in ["model", "small_model"] {
            if config
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("echobird/"))
            {
                config.as_object_mut().unwrap().remove(key);
            }
        }
        if before != config {
            write_json_file(&path, &config)?;
        }
    }
    if let (Some(path), Some(prefs)) = (preferences, prefs) {
        write_json_file(path, &prefs)?;
    }
    Ok(())
}

pub(super) fn restore_kilo_to_official() -> ApplyResult {
    let relay = echobird_dir().join("kilo.json");
    if relay.exists() {
        let _ = fs::remove_file(relay);
    }
    result(
        restore_at(&kilo_config_dir(), None),
        "Kilo CLI EchoBird provider removed.",
    )
}

pub(super) fn restore_kilo_desktop_to_official() -> ApplyResult {
    result(
        restore_at(&desktop_config_dir(), Some(&desktop_preferences_path())),
        "Kilo Desktop EchoBird provider removed.",
    )
}

/// Called only by an explicit account apply, after Desktop has stopped.
pub(crate) fn kilo_apply_account_selection(
    desktop: bool,
    model: &str,
    apply_auth: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if desktop {
        account_selection_at(
            &desktop_config_dir(),
            Some(&desktop_preferences_path()),
            model,
            apply_auth,
        )
    } else {
        account_selection_at(&kilo_config_dir(), None, model, apply_auth)
    }
}

fn account_selection_at(
    dir: &Path,
    preferences: Option<&Path>,
    model: &str,
    apply_auth: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let read = |path: &Path| match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("accountError.read".to_owned()),
    };
    let paths = CONFIG_NAMES
        .map(|name| dir.join(name))
        .into_iter()
        .chain(preferences.map(Path::to_path_buf));
    let before = paths
        .map(|path| read(&path).map(|bytes| (path, bytes)))
        .collect::<Result<Vec<_>, _>>()?;
    let result = (|| {
        restore_at(dir, preferences)?;
        if let Some(path) = preferences {
            let mut prefs = load_object(path)?;
            select_desktop_model(&mut prefs, "kilo", model)?;
            write_json_file(path, &prefs)?;
        } else {
            let path = write_path(dir);
            let mut config = load_object(&path)?;
            config["model"] = json!(format!("kilo/{model}"));
            write_json_file(&path, &config)?;
        }
        // The final credential write is atomic, so an error leaves the old auth intact.
        apply_auth()
    })();
    if result.is_err() {
        let mut failed = false;
        for (path, bytes) in before {
            if read(&path).is_ok_and(|current| current == bytes) {
                continue;
            }
            let restored = match bytes {
                Some(bytes) => fs::write(path, bytes),
                None => fs::remove_file(path),
            };
            failed |= restored.is_err();
        }
        if failed {
            return Err("accountError.write".into());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_account_default_removes_the_cli_echo_bird_launch_override() {
        let config = json!({"model":"echobird/vendor/model"});
        assert_eq!(
            echobird_model_from(&config).as_deref(),
            Some("vendor/model")
        );
        let config = json!({"model":"kilo/kilo-auto/free"});
        assert!(echobird_model_from(&config).is_none());
        assert!(echobird_model_from(&json!({})).is_none());
    }

    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!("echobird-kilo-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn account_apply_restores_exact_files_when_the_final_auth_write_fails() {
        for desktop in [false, true] {
            let dir = temp();
            let config = dir.join("kilo.jsonc");
            let original = b"// Keep native formatting.\n{\"model\":\"echobird/old\",\"provider\":{\"echobird\":{\"keep\":false},\"custom\":{\"keep\":true}},\"unknown\":[1,2]}\n";
            fs::write(&config, original).unwrap();
            let untouched = dir.join("config.json");
            let untouched_bytes = b"{\"untouched\":true}\n";
            fs::write(&untouched, untouched_bytes).unwrap();
            let old_time =
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(86_400);
            fs::OpenOptions::new()
                .write(true)
                .open(&untouched)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old_time))
                .unwrap();
            let untouched_time = fs::metadata(&untouched).unwrap().modified().unwrap();
            let preferences = dir.join("preferences.json");
            let original_preferences = b"{\"preferred\":{\"providerID\":\"echobird\",\"modelID\":\"old\"},\"unknown\":\"preserve\"}\n";
            fs::write(&preferences, original_preferences).unwrap();
            let old_auth = b"{\"kilo\":{\"access\":\"old-token\"},\"other\":{\"keep\":true}}\n";
            // The existing filename is valid, but the atomic temporary filename exceeds 255 bytes.
            let blocked_auth = dir.join(format!("{}.json", "a".repeat(230)));
            fs::write(&blocked_auth, old_auth).unwrap();
            let attempted = std::cell::Cell::new(false);
            assert!(account_selection_at(
                &dir,
                desktop.then_some(preferences.as_path()),
                "new-model",
                || {
                    assert!(load_object(&config).unwrap()["provider"]
                        .get(PROVIDER)
                        .is_none());
                    attempted.set(true);
                    crate::services::cursor_auth::write(
                        &blocked_auth,
                        &json!({"kilo":{"access":"new-token"}}),
                    )
                },
            )
            .is_err());
            assert!(attempted.get());
            assert_eq!(fs::read(&config).unwrap(), original);
            assert_eq!(fs::read(&preferences).unwrap(), original_preferences);
            assert_eq!(fs::read(&blocked_auth).unwrap(), old_auth);
            assert_eq!(fs::read(&untouched).unwrap(), untouched_bytes);
            assert_eq!(
                fs::metadata(&untouched).unwrap().modified().unwrap(),
                untouched_time
            );
            assert!(!dir.join("kilo.json").exists());

            let auth = dir.join("auth.json");
            fs::write(&auth, old_auth).unwrap();
            account_selection_at(
                &dir,
                desktop.then_some(preferences.as_path()),
                "new-model",
                || {
                    crate::services::cursor_auth::write(
                        &auth,
                        &json!({"kilo":{"access":"new-token"},"other":{"keep":true}}),
                    )
                },
            )
            .unwrap();
            let config = load_object(&config).unwrap();
            assert!(config["provider"].get(PROVIDER).is_none());
            assert_eq!(config["provider"]["custom"]["keep"], true);
            assert_eq!(config["unknown"], json!([1, 2]));
            if desktop {
                let prefs = load_object(&preferences).unwrap();
                assert_eq!(prefs["preferred"]["providerID"], "kilo");
                assert_eq!(prefs["preferred"]["modelID"], "new-model");
                assert_eq!(prefs["unknown"], "preserve");
                assert!(config.get("model").is_none());
            } else {
                assert_eq!(config["model"], "kilo/new-model");
                assert_eq!(fs::read(&preferences).unwrap(), original_preferences);
            }
            assert_eq!(load_object(&auth).unwrap()["kilo"]["access"], "new-token");
            assert_eq!(load_object(&auth).unwrap()["other"]["keep"], true);
            assert_eq!(
                fs::metadata(&untouched).unwrap().modified().unwrap(),
                untouched_time
            );
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn failed_account_apply_removes_only_files_created_by_that_apply() {
        for desktop in [false, true] {
            let root = temp();
            let dir = root.join("missing-config");
            let preferences = root.join("missing-preferences.json");
            let auth = root.join(format!("{}.json", "a".repeat(230)));
            let original_auth = b"{\"kilo\":{\"access\":\"old-token\"}}\n";
            fs::write(&auth, original_auth).unwrap();
            let attempted = std::cell::Cell::new(false);
            assert!(account_selection_at(
                &dir,
                desktop.then_some(preferences.as_path()),
                "new-model",
                || {
                    assert!(if desktop {
                        preferences.exists()
                    } else {
                        dir.join("kilo.json").exists()
                    });
                    attempted.set(true);
                    crate::services::cursor_auth::write(
                        &auth,
                        &json!({"kilo":{"access":"new-token"}}),
                    )
                },
            )
            .is_err());
            assert!(attempted.get());
            assert!(CONFIG_NAMES.iter().all(|name| !dir.join(name).exists()));
            assert!(!preferences.exists());
            assert_eq!(fs::read(&auth).unwrap(), original_auth);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn desktop_selection_preserves_other_provider_and_effort_settings() {
        let dir = temp();
        let prefs_path = dir.join("preferences.json");
        let original =
            json!({"provider":{"custom":{"options":{"apiKey":"fixture"}}},"model":"custom/old"});
        write_json_file(&dir.join("kilo.jsonc"), &original).unwrap();
        write_json_file(&prefs_path, &json!({"modelSelections":{"build":{"providerID":"custom","modelID":"old"}},"variantSelections":{"custom/old":"high"},"autoFreeModelsEnabled":false})).unwrap();
        let info: ModelInfo = serde_json::from_value(json!({"model":"vendor/model","name":"Test","baseUrl":"http://localhost:123/v1","apiKey":"fixture"})).unwrap();
        apply_at(&dir, Some(&prefs_path), &info).unwrap();
        let config = load_object(&dir.join("kilo.jsonc")).unwrap();
        assert_eq!(config["provider"]["custom"], original["provider"]["custom"]);
        assert_eq!(config["model"], "custom/old");
        assert!(model_info(&config, PROVIDER, "vendor/model").is_some());
        let prefs = load_object(&prefs_path).unwrap();
        assert_eq!(prefs["preferred"]["modelID"], "vendor/model");
        assert_eq!(prefs["modelSelections"]["build"]["providerID"], PROVIDER);
        assert_eq!(prefs["variantSelections"]["custom/old"], "high");
        assert_eq!(prefs["autoFreeModelsEnabled"], false);
        restore_at(&dir, Some(&prefs_path)).unwrap();
        assert_eq!(load_object(&dir.join("kilo.jsonc")).unwrap(), original);
        assert!(load_object(&prefs_path).unwrap().get("preferred").is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_config_is_not_overwritten_and_jsonc_accepts_trailing_commas() {
        let dir = temp();
        let path = dir.join("kilo.jsonc");
        fs::write(&path, "// comment\n{\"provider\": {\"other\": {}},}").unwrap();
        assert!(load_object(&path).is_ok());
        fs::write(&path, "{broken").unwrap();
        let info: ModelInfo = serde_json::from_value(json!({"model":"test"})).unwrap();
        assert!(apply_at(&dir, None, &info).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{broken");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn desktop_apply_and_restore_preserve_an_existing_schema() {
        let dir = temp();
        let path = dir.join("kilo.json");
        let original =
            json!({"$schema":"https://app.kilo.ai/config.json","provider":{"custom":{}}});
        write_json_file(&path, &original).unwrap();
        let info: ModelInfo = serde_json::from_value(json!({"model":"test"})).unwrap();
        apply_at(&dir, Some(&dir.join("preferences.json")), &info).unwrap();
        restore_at(&dir, Some(&dir.join("preferences.json"))).unwrap();
        assert_eq!(load_object(&path).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }
}
