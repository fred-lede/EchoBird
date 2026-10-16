//! Native Muse settings and EchoBird's selected upstream model.

use super::{ApplyResult, ModelInfo};
use crate::services::{musecode_proxy, tool_manager};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io::Write, path::Path, path::PathBuf, sync::Mutex};

static CONFIG_LOCK: Mutex<()> = Mutex::new(());
const OWNED_FIELDS: &[&str] = &[
    "schema_version",
    "provider",
    "model",
    "endpoint_transport.base_url",
    "endpoint_transport.auth",
];

#[derive(Serialize, Deserialize)]
struct Backup {
    settings_path: PathBuf,
    settings_existed: bool,
    endpoint_transport_existed: bool,
    original: BTreeMap<String, Value>,
    applied: BTreeMap<String, Value>,
}

fn config_dir_at(home: &Path, xdg: Option<&Path>) -> PathBuf {
    xdg.filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".config"))
        .join("muse")
}

pub(crate) fn settings_path() -> PathBuf {
    config_dir_at(
        &dirs::home_dir().unwrap_or_default(),
        std::env::var_os("XDG_CONFIG_HOME")
            .as_deref()
            .map(Path::new),
    )
    .join("settings.json")
}

fn state_dir() -> PathBuf {
    crate::utils::platform::echobird_dir()
}

fn result(outcome: Result<(), String>, message: &str) -> ApplyResult {
    ApplyResult {
        success: outcome.is_ok(),
        message: outcome.err().unwrap_or_else(|| message.to_string()),
    }
}

pub(super) fn apply(model_info: &ModelInfo) -> ApplyResult {
    let outcome = CONFIG_LOCK
        .lock()
        .map_err(|_| "Muse configuration is busy".to_string())
        .and_then(|_guard| {
            let model_info = validated_model(model_info)?;
            let port = musecode_proxy::ensure_available()?;
            apply_at(&settings_path(), &state_dir(), &model_info, port)
        });
    result(outcome, "Muse Code configured.")
}

fn validated_model(model_info: &ModelInfo) -> Result<ModelInfo, String> {
    let mut model = model_info.clone();
    model.model = Some(
        model
            .model
            .as_deref()
            .or(model.name.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("Model ID is empty")?
            .to_string(),
    );
    if model.protocol.as_deref().is_some_and(|p| p != "openai") {
        return Err("Muse Code requires an OpenAI Chat Completions model".into());
    }
    let base_url = model
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("Base URL is empty")?;
    let url = url::Url::parse(base_url).map_err(|_| "Invalid model Base URL")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Invalid model Base URL".into());
    }
    let is_local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if model.api_key.as_deref().unwrap_or("").is_empty() && !is_local {
        return Err("API Key is empty".into());
    }
    model.base_url = Some(base_url.trim_end_matches('/').to_string());
    model.protocol = Some("openai".into());
    Ok(model)
}

fn read_settings(path: &Path) -> Result<(Value, bool), String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((json!({}), false));
        }
        Err(error) => return Err(format!("Read {}: {error}", path.display())),
    };
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| format!("Invalid Muse settings JSON: {}", path.display()))?;
    if !value.is_object()
        || value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || value
            .get("endpoint_transport")
            .is_some_and(|transport| !transport.is_object())
    {
        return Err("Unsupported Muse settings schema; EchoBird left it unchanged".into());
    }
    Ok((value, true))
}

fn owned_fields(settings: &Value) -> BTreeMap<String, Value> {
    OWNED_FIELDS
        .iter()
        .filter_map(|key| {
            tool_manager::get_nested_value(settings, key).map(|value| (key.to_string(), value))
        })
        .collect()
}

fn apply_at(
    settings_path: &Path,
    state_dir: &Path,
    model_info: &ModelInfo,
    port: u16,
) -> Result<(), String> {
    let (mut settings, settings_existed) = read_settings(settings_path)?;
    let backup_path = state_dir.join("musecode-settings-backup.json");
    let mut backup = match fs::read(&backup_path) {
        Ok(bytes) => {
            serde_json::from_slice::<Backup>(&bytes).map_err(|_| "Invalid Muse settings backup")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Backup {
            settings_path: settings_path.to_path_buf(),
            settings_existed,
            endpoint_transport_existed: settings.get("endpoint_transport").is_some(),
            original: owned_fields(&settings),
            applied: BTreeMap::new(),
        },
        Err(error) => return Err(format!("Read Muse settings backup: {error}")),
    };
    if backup.settings_path != settings_path {
        return Err(
            "Muse settings location changed; restore the previous configuration first".into(),
        );
    }
    for (key, value) in [
        ("schema_version", json!(1)),
        ("provider", json!("meta")),
        ("model", json!(model_info.model)),
        (
            "endpoint_transport.base_url",
            json!(format!("http://127.0.0.1:{port}/v1")),
        ),
        ("endpoint_transport.auth", json!("none")),
    ] {
        tool_manager::set_nested_value(&mut settings, key, value);
    }
    backup.applied = owned_fields(&settings);
    commit_files(vec![
        (backup_path, Some(json_bytes(&backup)?)),
        (
            state_dir.join("musecode.json"),
            Some(json_bytes(model_info)?),
        ),
        (settings_path.to_path_buf(), Some(json_bytes(&settings)?)),
    ])
}

pub(super) fn read() -> Option<ModelInfo> {
    read_at(
        &settings_path(),
        &state_dir().join("musecode.json"),
        musecode_proxy::port().ok()?,
    )
}

#[cfg(not(windows))]
pub(crate) fn managed_config_home() -> Option<PathBuf> {
    read()?;
    let path = settings_path();
    let config_home = path.parent()?.parent()?;
    Some(if config_home.is_absolute() {
        config_home.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(config_home)
    })
}

fn read_at(settings_path: &Path, relay_path: &Path, port: u16) -> Option<ModelInfo> {
    let (settings, _) = read_settings(settings_path).ok()?;
    let relay: ModelInfo = serde_json::from_slice(&fs::read(relay_path).ok()?).ok()?;
    let relay = validated_model(&relay).ok()?;
    (settings["model"].as_str() == relay.model.as_deref()
        && settings["provider"] == "meta"
        && settings["endpoint_transport"]["auth"] == "none"
        && settings["endpoint_transport"]["base_url"] == format!("http://127.0.0.1:{port}/v1"))
    .then_some(relay)
}

pub(super) fn restore() -> ApplyResult {
    let outcome = CONFIG_LOCK
        .lock()
        .map_err(|_| "Muse configuration is busy".to_string())
        .and_then(|_guard| restore_at(&settings_path(), &state_dir()));
    result(outcome, "Muse Code restored.")
}

fn restore_at(settings_path: &Path, state_dir: &Path) -> Result<(), String> {
    let backup_path = state_dir.join("musecode-settings-backup.json");
    let backup = match fs::read(&backup_path) {
        Ok(bytes) => {
            serde_json::from_slice::<Backup>(&bytes).map_err(|_| "Invalid Muse settings backup")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Read Muse settings backup: {error}")),
    };
    if backup.settings_path != settings_path {
        return Err("Muse settings location changed; EchoBird left it unchanged".into());
    }
    let (mut settings, exists) = read_settings(settings_path)?;
    if exists {
        let endpoint_owned = ["endpoint_transport.base_url", "endpoint_transport.auth"]
            .iter()
            .all(|key| {
                tool_manager::get_nested_value(&settings, key) == backup.applied.get(*key).cloned()
            });
        for key in OWNED_FIELDS {
            // A user's edits after applying take precedence over our saved state.
            // Endpoint and auth are one choice: never restore credentials to a changed URL.
            if key.starts_with("endpoint_transport.") && !endpoint_owned {
                continue;
            }
            if tool_manager::get_nested_value(&settings, key) != backup.applied.get(*key).cloned() {
                continue;
            }
            if let Some(value) = backup.original.get(*key) {
                tool_manager::set_nested_value(&mut settings, key, value.clone());
            } else {
                tool_manager::delete_nested_value(&mut settings, key);
            }
        }
        if !backup.endpoint_transport_existed
            && settings["endpoint_transport"]
                .as_object()
                .is_some_and(|transport| transport.is_empty())
        {
            settings
                .as_object_mut()
                .unwrap()
                .remove("endpoint_transport");
        }
        if !backup.settings_existed
            && !settings.as_object().unwrap().is_empty()
            && settings.get("schema_version").is_none()
        {
            settings["schema_version"] = json!(1);
        }
    }
    let native = if exists && (backup.settings_existed || !settings.as_object().unwrap().is_empty())
    {
        Some(json_bytes(&settings)?)
    } else {
        None
    };
    commit_files(vec![
        (settings_path.to_path_buf(), native),
        (state_dir.join("musecode.json"), None),
        (backup_path, None),
    ])
}

fn json_bytes(value: &impl Serialize) -> Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(value).map_err(|error| error.to_string())
}

fn stage_file(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    fs::create_dir_all(path.parent().ok_or("Missing configuration directory")?)
        .map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let outcome = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        Ok(temporary.clone())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(temporary);
    }
    outcome
}

/// Stage every write before changing native settings; roll back an install failure.
fn commit_files(changes: Vec<(PathBuf, Option<Vec<u8>>)>) -> Result<(), String> {
    commit_files_with(changes, |path, temporary, existed| match temporary {
        Some(temporary) => fs::rename(temporary, path),
        None if existed => fs::remove_file(path),
        None => Ok(()),
    })
}

fn commit_files_with(
    changes: Vec<(PathBuf, Option<Vec<u8>>)>,
    mut install: impl FnMut(&Path, Option<&Path>, bool) -> std::io::Result<()>,
) -> Result<(), String> {
    let mut originals = Vec::new();
    for (path, _) in &changes {
        originals.push(match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("Read {}: {error}", path.display())),
        });
    }
    let mut staged = Vec::new();
    let outcome = (|| {
        for (path, bytes) in &changes {
            staged.push(
                bytes
                    .as_ref()
                    .map(|bytes| stage_file(path, bytes))
                    .transpose()?,
            );
        }
        for (index, (path, _)) in changes.iter().enumerate() {
            if let Err(error) = install(path, staged[index].as_deref(), originals[index].is_some())
            {
                let mut failures = Vec::new();
                for rollback in (0..index).rev() {
                    let rollback_path = &changes[rollback].0;
                    let restored = match &originals[rollback] {
                        Some(original) => stage_file(rollback_path, original).and_then(|temp| {
                            fs::rename(&temp, rollback_path).map_err(|error| {
                                let _ = fs::remove_file(temp);
                                error.to_string()
                            })
                        }),
                        None => fs::remove_file(rollback_path)
                            .or_else(|error| {
                                if error.kind() == std::io::ErrorKind::NotFound {
                                    Ok(())
                                } else {
                                    Err(error)
                                }
                            })
                            .map_err(|error| error.to_string()),
                    };
                    if let Err(error) = restored {
                        failures.push(error);
                    }
                }
                return Err(format!(
                    "Write {}: {error}{}",
                    path.display(),
                    if failures.is_empty() {
                        String::new()
                    } else {
                        format!("; rollback failed: {}", failures.join("; "))
                    }
                ));
            }
        }
        Ok(())
    })();
    for temporary in staged.into_iter().flatten() {
        let _ = fs::remove_file(temporary);
    }
    outcome
}

pub(crate) fn migrate_proxy_port(old: u16, new: u16) -> Result<(), String> {
    let path = state_dir().join("musecode-settings-backup.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let mut backup: Backup = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let field = "endpoint_transport.base_url";
    if backup.applied.get(field) == Some(&json!(format!("http://127.0.0.1:{old}/v1"))) {
        backup
            .applied
            .insert(field.into(), json!(format!("http://127.0.0.1:{new}/v1")));
        commit_files(vec![(path, Some(json_bytes(&backup)?))])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("echobird-muse-config-{}", uuid::Uuid::new_v4()));
        let native = root.join("native/settings.json");
        fs::create_dir_all(native.parent().unwrap()).unwrap();
        let state = root.join("state");
        (root, native, state)
    }

    fn model(id: &str) -> ModelInfo {
        serde_json::from_value(json!({
            "model": id, "baseUrl":"https://api.example/v1", "apiKey":"fixture-key",
            "name":"Fixture", "protocol":"openai"
        }))
        .unwrap()
    }

    #[test]
    fn native_paths_follow_xdg_on_every_platform() {
        let home = Path::new("fixture-home");
        assert_eq!(config_dir_at(home, None), home.join(".config/muse"));
        assert_eq!(
            config_dir_at(home, Some(Path::new("fixture-xdg"))),
            PathBuf::from("fixture-xdg/muse")
        );
        assert_eq!(
            config_dir_at(home, Some(Path::new(""))),
            home.join(".config/muse")
        );
    }

    #[test]
    fn model_switch_and_restore_preserve_unknown_settings_and_user_edits() {
        let (root, native, state) = fixture();
        let original = json!({"schema_version":1,"model":"official","theme":"dark","endpoint_transport":{"custom":true}});
        fs::write(&native, original.to_string()).unwrap();
        apply_at(&native, &state, &model("first"), 53684).unwrap();
        apply_at(&native, &state, &model("second"), 53684).unwrap();
        let mut current: Value = serde_json::from_slice(&fs::read(&native).unwrap()).unwrap();
        assert_eq!(current["model"], "second");
        assert_eq!(current["endpoint_transport"]["auth"], "none");
        assert!(!current.to_string().contains("fixture-key"));
        current["theme"] = json!("light");
        current["model"] = json!("user-choice");
        current["endpoint_transport"]["extra"] = json!(9);
        fs::write(&native, current.to_string()).unwrap();
        restore_at(&native, &state).unwrap();
        let restored: Value = serde_json::from_slice(&fs::read(&native).unwrap()).unwrap();
        assert_eq!(
            restored,
            json!({"schema_version":1,"model":"user-choice","theme":"light","endpoint_transport":{"custom":true,"extra":9}})
        );
        assert!(!state.join("musecode.json").exists());
        assert!(!state.join("musecode-settings-backup.json").exists());
        assert!(!native.with_file_name("auth.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fresh_config_restore_removes_only_our_files() {
        let (root, native, state) = fixture();
        apply_at(&native, &state, &model("first"), 53684).unwrap();
        restore_at(&native, &state).unwrap();
        assert!(!native.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fresh_settings_with_user_additions_keep_the_required_schema_on_restore() {
        let (root, native, state) = fixture();
        apply_at(&native, &state, &model("first"), 53684).unwrap();
        let mut current: Value = serde_json::from_slice(&fs::read(&native).unwrap()).unwrap();
        current["theme"] = json!("light");
        fs::write(&native, current.to_string()).unwrap();
        restore_at(&native, &state).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&native).unwrap()).unwrap(),
            json!({"schema_version":1,"theme":"light"})
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_model_read_is_passive_and_requires_the_native_bridge_selection() {
        let (root, native, state) = fixture();
        let relay = state.join("musecode.json");
        assert!(read_at(&native, &relay, 53684).is_none());
        assert!(!state.exists());
        apply_at(&native, &state, &model("first"), 53684).unwrap();
        let before = fs::read(&native).unwrap();
        assert_eq!(
            read_at(&native, &relay, 53684).unwrap().model.as_deref(),
            Some("first")
        );
        assert_eq!(fs::read(&native).unwrap(), before);
        let mut current: Value = serde_json::from_slice(&before).unwrap();
        current["model"] = json!("user-choice");
        fs::write(&native, current.to_string()).unwrap();
        assert!(read_at(&native, &relay, 53684).is_none());
        current["model"] = json!("first");
        current["endpoint_transport"]["base_url"] = json!("https://other.example/v1");
        fs::write(&native, current.to_string()).unwrap();
        assert!(read_at(&native, &relay, 53684).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_preserves_a_changed_endpoint_and_its_auth_as_a_pair() {
        let (root, native, state) = fixture();
        fs::write(&native, json!({"schema_version":1,"endpoint_transport":{"base_url":"https://official.example/v1","auth":"bearer"}}).to_string()).unwrap();
        apply_at(&native, &state, &model("first"), 53684).unwrap();
        let mut current: Value = serde_json::from_slice(&fs::read(&native).unwrap()).unwrap();
        current["endpoint_transport"]["base_url"] = json!("https://user.example/v1");
        fs::write(&native, current.to_string()).unwrap();
        restore_at(&native, &state).unwrap();
        let restored: Value = serde_json::from_slice(&fs::read(&native).unwrap()).unwrap();
        assert_eq!(
            restored["endpoint_transport"],
            json!({"base_url":"https://user.example/v1","auth":"none"})
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_native_schema_and_staging_failures_leave_configuration_unchanged() {
        let (root, native, state) = fixture();
        for invalid in [
            "{",
            "[]",
            "{}",
            "{\"schema_version\":2}",
            "{\"schema_version\":1,\"endpoint_transport\":null}",
        ] {
            fs::write(&native, invalid).unwrap();
            assert!(apply_at(&native, &state, &model("first"), 53684).is_err());
            assert_eq!(fs::read_to_string(&native).unwrap(), invalid);
            assert!(!state.exists());
        }
        fs::write(&native, "{\"schema_version\":1}").unwrap();
        fs::write(&state, "directory-blocker").unwrap();
        assert!(apply_at(&native, &state, &model("first"), 53684).is_err());
        assert_eq!(
            fs::read_to_string(&native).unwrap(),
            "{\"schema_version\":1}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_second_file_install_rolls_back_the_first() {
        let (root, native, _) = fixture();
        let second = root.join("second.json");
        fs::write(&native, "original").unwrap();
        let mut installs = 0;
        assert!(commit_files_with(
            vec![
                (native.clone(), Some(b"changed".to_vec())),
                (second.clone(), Some(b"value".to_vec()))
            ],
            |path, temporary, _| {
                installs += 1;
                if installs == 2 {
                    return Err(std::io::Error::other("fixture install failure"));
                }
                fs::rename(temporary.unwrap(), path)
            }
        )
        .is_err());
        assert_eq!(installs, 2);
        assert_eq!(fs::read_to_string(&native).unwrap(), "original");
        assert!(!second.exists());
        assert_eq!(fs::read_dir(native.parent().unwrap()).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
