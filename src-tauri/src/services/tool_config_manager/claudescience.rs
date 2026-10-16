use super::{echobird_dir, read_json_file, ApplyResult, ModelInfo};
use crate::services::native_account_files::{self, DirectoryLock, Transaction};
use aes_gcm::{aead::Aead, aead::Payload, Aes256Gcm, KeyInit, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

const LOGIN_FILE: &str = "claudescience-login.json";
const RELAY_FILE: &str = "claudescience.json";
pub(super) const RESTART_FILE: &str = "claudescience-restart";

fn result(value: Result<(), String>, success: &str) -> ApplyResult {
    match value {
        Ok(()) => ApplyResult {
            success: true,
            message: success.into(),
        },
        Err(message) => ApplyResult {
            success: false,
            message,
        },
    }
}

fn data_dir() -> Result<std::path::PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".claude-science"))
        .ok_or_else(|| "Cannot locate the Claude Science data directory.".into())
}

fn fingerprint(bytes: &[u8]) -> String {
    STANDARD.encode(Sha256::digest(bytes))
}

fn encrypted_login(data: &Path) -> Result<Vec<u8>, String> {
    let key_file = fs::read_to_string(data.join("encryption.key")).map_err(|_| {
        "Claude Science encryption key is unavailable. Start Claude Science once.".to_string()
    })?;
    let encoded = key_file
        .lines()
        .find_map(|line| line.trim().strip_prefix("OAUTH_ENCRYPTION_KEY="))
        .ok_or_else(|| "Claude Science encryption key has an unknown format.".to_string())?;
    let secret = STANDARD
        .decode(encoded.trim().trim_matches(['\'', '"']))
        .map_err(|_| "Claude Science encryption key has an unknown format.".to_string())?;
    if secret.len() < 16 {
        return Err("Claude Science encryption key is incomplete.".into());
    }

    type HmacSha256 = Hmac<Sha256>;
    let mut extract = <HmacSha256 as Mac>::new_from_slice(&[]).map_err(|e| e.to_string())?;
    extract.update(&secret);
    let prk = extract.finalize().into_bytes();
    let mut expand = <HmacSha256 as Mac>::new_from_slice(&prk).map_err(|e| e.to_string())?;
    expand.update(b"operon:aes-256-gcm:oauth\x01");
    let key = expand.finalize().into_bytes();

    let expires = (chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339();
    let payload = json!({
        "access_token": format!("sk-ant-oat01-{}", uuid::Uuid::new_v4().simple()),
        "refresh_token": format!("sk-ant-ort01-{}", uuid::Uuid::new_v4().simple()),
        "api_key": null,
        "token_expires_at": expires,
        "refresh_token_expires_at": expires,
        "provider": "claude_ai",
        "scopes": "user:inference user:file_upload user:profile user:mcp_servers user:plugins",
        "email": "local@claude-science.invalid",
        "account_uuid": uuid::Uuid::new_v4().to_string(),
        "subscription_type": "max",
        "rate_limit_tier": "max",
        "seat_tier": "max",
        "org_uuid": uuid::Uuid::new_v4().to_string(),
        "billing_type": "subscription",
        "has_extra_usage_enabled": false
    });
    let plaintext = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let mut iv = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut iv);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: &plaintext,
                aad: b"v2:oauth",
            },
        )
        .map_err(|_| "Failed to create Claude Science login.".to_string())?;
    let mut bytes = iv.to_vec();
    bytes.extend(ciphertext);
    Ok(format!("v2:{}", STANDARD.encode(bytes)).into_bytes())
}

fn apply_at(model: &ModelInfo, data: &Path, echo: &Path) -> Result<(), String> {
    let base_url = model
        .anthropic_url
        .as_deref()
        .or(model.base_url.as_deref())
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or("Select an Anthropic-compatible model first.")?;
    let api_key = model
        .api_key
        .as_deref()
        .filter(|key| !key.is_empty())
        .or_else(|| {
            (base_url.contains("127.0.0.1") || base_url.contains("localhost"))
                .then_some("local-no-auth")
        })
        .ok_or("This model needs an API key.")?;
    let model_id = model
        .model
        .as_deref()
        .filter(|id| !id.is_empty())
        .or(model.name.as_deref().filter(|name| !name.is_empty()))
        .ok_or("Model ID is empty.")?;
    let credential = data.join(".oauth-tokens").join("local-dev.enc");
    let native_config = data.join("config.toml");
    let metadata = echo.join(LOGIN_FILE);
    let relay = echo.join(RELAY_FILE);
    let restart = echo.join(RESTART_FILE);
    let _lock = DirectoryLock::acquire(&metadata)?;
    let current = native_account_files::read_optional(&credential)?;
    let current_config = native_account_files::read_optional(&native_config)?;
    let (previous, previous_config) = match native_account_files::read_optional(&metadata)? {
        Some(bytes) => {
            let state: Value = serde_json::from_slice(&bytes)
                .map_err(|_| "Claude Science login backup is invalid.".to_string())?;
            if current.as_deref().map(fingerprint).as_deref() != state["managedHash"].as_str() {
                return Err("Claude Science login changed outside EchoBird. Restore it manually before switching models.".into());
            }
            if let Some(hash) = state["managedConfigHash"].as_str() {
                if current_config.as_deref().map(fingerprint).as_deref() != Some(hash) {
                    return Err("Claude Science model settings changed outside EchoBird. Restore them manually before switching models.".into());
                }
            }
            let previous = state["original"]
                .as_str()
                .map(|value| {
                    STANDARD
                        .decode(value)
                        .map_err(|_| "Claude Science login backup is invalid.".to_string())
                })
                .transpose()?;
            let previous_config = state["managedConfigHash"]
                .as_str()
                .map(|_| {
                    state["originalConfig"]
                        .as_str()
                        .map(|value| {
                            STANDARD.decode(value).map_err(|_| {
                                "Claude Science model settings backup is invalid.".to_string()
                            })
                        })
                        .transpose()
                })
                .transpose()?
                .unwrap_or(current_config.clone());
            (previous, previous_config)
        }
        None => (current, current_config.clone()),
    };
    let relay_config = json!({ "baseUrl": base_url, "apiKey": api_key, "modelId": model_id,
        "modelName": model.name.as_deref().unwrap_or(model_id), "actualModel": model_id, "protocol": "anthropic" });
    if metadata.exists()
        && read_json_file(&relay).as_ref() == Some(&relay_config)
        && current_config.as_deref().is_some_and(|bytes| {
            std::str::from_utf8(bytes)
                .is_ok_and(|text| super::toml_read_top(text, "default_model") == model_id)
        })
    {
        return Ok(());
    }
    let fake = encrypted_login(data)?;
    let config_text = std::str::from_utf8(current_config.as_deref().unwrap_or_default())
        .map_err(|_| "Claude Science model settings are not UTF-8.".to_string())?;
    let config = super::toml_write_top(config_text, "default_model", model_id);
    let state = json!({ "managedHash": fingerprint(&fake), "original": previous.as_deref().map(|bytes| STANDARD.encode(bytes)),
        "managedConfigHash": fingerprint(config.as_bytes()), "originalConfig": previous_config.as_deref().map(|bytes| STANDARD.encode(bytes)) });
    let mut transaction = Transaction::new();
    transaction.replace(
        &relay,
        &serde_json::to_vec_pretty(&relay_config).map_err(|e| e.to_string())?,
    )?;
    transaction.replace(&credential, &fake)?;
    transaction.replace(&native_config, config.as_bytes())?;
    transaction.replace(
        &metadata,
        &serde_json::to_vec(&state).map_err(|e| e.to_string())?,
    )?;
    transaction.replace(&restart, b"")?;
    transaction.commit();
    Ok(())
}

pub(super) fn apply(model: &ModelInfo) -> ApplyResult {
    result(
        data_dir().and_then(|data| apply_at(model, &data, &echobird_dir())),
        "Claude Science model configured. Restart it from EchoBird to apply.",
    )
}

fn restore_at(data: &Path, echo: &Path) -> Result<(), String> {
    let credential = data.join(".oauth-tokens").join("local-dev.enc");
    let native_config = data.join("config.toml");
    let metadata = echo.join(LOGIN_FILE);
    let relay = echo.join(RELAY_FILE);
    let restart = echo.join(RESTART_FILE);
    let _lock = DirectoryLock::acquire(&metadata)?;
    let changed = metadata.exists() || relay.exists();
    if let Some(state) = native_account_files::read_optional(&metadata)? {
        let state: Value = serde_json::from_slice(&state)
            .map_err(|_| "Claude Science login backup is invalid.".to_string())?;
        let current = native_account_files::read_optional(&credential)?;
        if current.as_deref().map(fingerprint).as_deref() != state["managedHash"].as_str() {
            return Err(
                "Claude Science login changed outside EchoBird. No credentials were removed."
                    .into(),
            );
        }
        if let Some(hash) = state["managedConfigHash"].as_str() {
            let current = native_account_files::read_optional(&native_config)?;
            if current.as_deref().map(fingerprint).as_deref() != Some(hash) {
                return Err("Claude Science model settings changed outside EchoBird. No credentials were removed.".into());
            }
        }
        if let Some(original) = state["original"].as_str() {
            native_account_files::write(
                &credential,
                &STANDARD
                    .decode(original)
                    .map_err(|_| "Claude Science login backup is invalid.".to_string())?,
            )?;
        } else {
            fs::remove_file(&credential).map_err(|e| e.to_string())?;
        }
        if state["managedConfigHash"].as_str().is_some() {
            if let Some(original) = state["originalConfig"].as_str() {
                native_account_files::write(
                    &native_config,
                    &STANDARD.decode(original).map_err(|_| {
                        "Claude Science model settings backup is invalid.".to_string()
                    })?,
                )?;
            } else {
                fs::remove_file(&native_config).map_err(|e| e.to_string())?;
            }
        }
        fs::remove_file(&metadata).map_err(|e| e.to_string())?;
    } else if relay.exists() {
        return Err("Claude Science login backup is missing. No credentials were removed.".into());
    }
    if relay.exists() {
        fs::remove_file(&relay).map_err(|e| e.to_string())?;
    }
    if changed {
        native_account_files::write(&restart, b"")?;
    }
    Ok(())
}

pub(super) fn restore() -> ApplyResult {
    result(
        data_dir().and_then(|data| restore_at(&data, &echobird_dir())),
        "Claude Science official login restored.",
    )
}

pub(super) fn read() -> Option<ModelInfo> {
    let config = read_json_file(&echobird_dir().join(RELAY_FILE))?;
    Some(ModelInfo {
        name: config["modelName"].as_str().map(str::to_string),
        model: config["modelId"].as_str().map(str::to_string),
        base_url: config["baseUrl"].as_str().map(str::to_string),
        api_key: config["apiKey"].as_str().map(str::to_string),
        anthropic_url: config["baseUrl"].as_str().map(str::to_string),
        protocol: Some("anthropic".into()),
        display_model: None,
        relay_mode: None,
        web_search: None,
        one_m_context: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("echobird-science-{}", uuid::Uuid::new_v4()));
        let data = root.join("data");
        let echo = root.join("echo");
        fs::create_dir_all(&data).unwrap();
        fs::write(
            data.join("encryption.key"),
            format!("OAUTH_ENCRYPTION_KEY={}\n", STANDARD.encode([7u8; 32])),
        )
        .unwrap();
        (root, data, echo)
    }

    fn model() -> ModelInfo {
        ModelInfo {
            name: Some("Test".into()),
            model: Some("model-a".into()),
            base_url: Some("http://127.0.0.1:3000".into()),
            api_key: None,
            anthropic_url: None,
            protocol: Some("anthropic".into()),
            display_model: None,
            relay_mode: None,
            web_search: None,
            one_m_context: None,
        }
    }

    #[test]
    fn switching_and_restoring_keeps_original_login() {
        let (root, data, echo) = fixture();
        let credential = data.join(".oauth-tokens/local-dev.enc");
        native_account_files::write(&credential, b"existing-official-login").unwrap();
        apply_at(&model(), &data, &echo).unwrap();
        assert!(fs::read_to_string(&credential).unwrap().starts_with("v2:"));
        assert_eq!(
            super::super::toml_read_top(
                &fs::read_to_string(data.join("config.toml")).unwrap(),
                "default_model"
            ),
            "model-a"
        );
        let relay: Value =
            serde_json::from_slice(&fs::read(echo.join(RELAY_FILE)).unwrap()).unwrap();
        assert_eq!(relay["actualModel"], "model-a");
        let managed_login = fs::read(&credential).unwrap();
        assert!(echo.join(RESTART_FILE).exists());
        apply_at(&model(), &data, &echo).unwrap();
        assert_eq!(fs::read(&credential).unwrap(), managed_login);
        restore_at(&data, &echo).unwrap();
        assert_eq!(fs::read(&credential).unwrap(), b"existing-official-login");
        assert!(!data.join("config.toml").exists());
        assert!(!echo.join(RELAY_FILE).exists());
        assert!(echo.join(RESTART_FILE).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_login_change_is_not_overwritten_or_deleted() {
        let (root, data, echo) = fixture();
        let credential = data.join(".oauth-tokens/local-dev.enc");
        apply_at(&model(), &data, &echo).unwrap();
        native_account_files::write(&credential, b"new-official-login").unwrap();
        assert!(apply_at(&model(), &data, &echo).is_err());
        assert!(restore_at(&data, &echo).is_err());
        assert_eq!(fs::read(&credential).unwrap(), b"new-official-login");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_removes_only_managed_login_when_no_original_exists() {
        let (root, data, echo) = fixture();
        let credential = data.join(".oauth-tokens/local-dev.enc");
        apply_at(&model(), &data, &echo).unwrap();
        restore_at(&data, &echo).unwrap();
        assert!(!credential.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn display_name_is_used_when_model_id_is_empty() {
        let (root, data, echo) = fixture();
        let mut selected = model();
        selected.model = Some(String::new());
        apply_at(&selected, &data, &echo).unwrap();
        let relay: Value =
            serde_json::from_slice(&fs::read(echo.join(RELAY_FILE)).unwrap()).unwrap();
        assert_eq!(relay["actualModel"], "Test");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restores_existing_model_settings_after_switching() {
        let (root, data, echo) = fixture();
        let original = b"default_model = \"claude-opus-5\"\n[network]\ntimeout = 30\n";
        fs::write(data.join("config.toml"), original).unwrap();
        apply_at(&model(), &data, &echo).unwrap();
        assert_eq!(
            super::super::toml_read_top(
                &fs::read_to_string(data.join("config.toml")).unwrap(),
                "default_model"
            ),
            "model-a"
        );
        restore_at(&data, &echo).unwrap();
        assert_eq!(fs::read(data.join("config.toml")).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn earlier_managed_login_can_gain_model_settings() {
        let (root, data, echo) = fixture();
        fs::create_dir_all(data.join(".oauth-tokens")).unwrap();
        fs::create_dir_all(&echo).unwrap();
        let credential = encrypted_login(&data).unwrap();
        fs::write(data.join(".oauth-tokens/local-dev.enc"), &credential).unwrap();
        fs::write(
            echo.join(LOGIN_FILE),
            serde_json::to_vec(&json!({"managedHash": fingerprint(&credential), "original": null}))
                .unwrap(),
        )
        .unwrap();
        apply_at(&model(), &data, &echo).unwrap();
        assert_eq!(
            super::super::toml_read_top(
                &fs::read_to_string(data.join("config.toml")).unwrap(),
                "default_model"
            ),
            "model-a"
        );
        restore_at(&data, &echo).unwrap();
        assert!(!data.join("config.toml").exists());
        assert!(!data.join(".oauth-tokens/local-dev.enc").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
