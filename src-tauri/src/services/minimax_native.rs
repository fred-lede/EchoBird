mod desktop;

use super::{
    minimax_accounts::{self, Account, Credentials, Region},
    native_account_files::{self as files, DirectoryLock, Transaction},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

fn native_region(region: Region) -> &'static str {
    match region {
        Region::Cn => "cn",
        Region::Global => "en",
    }
}

pub(crate) async fn apply(account_id: &str, tool_id: &str) -> Result<Account, String> {
    if !matches!(tool_id, "minimaxcode" | "minimaxdesktop") {
        return Err("accountError.invalidAccount".into());
    }
    let executable =
        super::tool_manager::get_tool_exe_path(tool_id).ok_or("accountError.unavailable")?;
    let session = minimax_accounts::credentials_for_native(account_id).await?;
    let credentials = &session.credentials;
    let home = if tool_id == "minimaxdesktop" {
        if super::tool_config_manager::minimaxcode::desktop_region(Path::new(&executable))
            != Some(native_region(credentials.region))
        {
            return Err("accountError.invalidAccount".into());
        }
        desktop::data_dir(Path::new(&executable), credentials.region)?
    } else {
        let dir = files::absolute_path(&super::tool_config_manager::minimaxcode::data_dir())?;
        if !["MINIMAX_DATA_DIR", "MAVIS_DATA_DIR"]
            .iter()
            .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
        {
            desktop::ensure_default_root(&dirs::home_dir().ok_or("accountError.home")?, &dir)?;
        }
        dir
    };
    if tool_id == "minimaxdesktop" {
        super::process_manager::stop_desktop_for_config(tool_id).await;
        if super::process_manager::desktop_tool_is_running(tool_id).await? {
            return Err("accountError.closeClient".into());
        }
    }
    let _lock = DirectoryLock::acquire(&home.join("config.yaml"))?;
    let auth_home = home
        .join("auth/prod")
        .join(native_region(credentials.region))
        .join("mcode-public");
    let _auth_lock = DirectoryLock::acquire(&auth_home.join("auth.lock"))?;
    let transaction = write_cli(&home, credentials)?;
    let account = minimax_accounts::mark_active(account_id).await?;
    transaction.commit();
    Ok(account)
}

fn write_cli(home: &Path, credentials: &Credentials) -> Result<Transaction, String> {
    let region = native_region(credentials.region);
    let auth_home = home.join("auth");
    let namespace = auth_home.join("prod").join(region).join("mcode-public");
    let credential_path = namespace.join("auth.json");
    let state_path = namespace.join("auth-state.json");
    let config_path = home.join("config.yaml");
    let preference_path = home.join("preferences/mcode-region.json");
    let mut credential = files::object(&credential_path)?;
    let previous_state = files::object(&state_path)?;
    if credential.get("records").is_some_and(|v| !v.is_object()) {
        return Err("accountError.format".into());
    }
    let account_key = URL_SAFE_NO_PAD.encode(Sha256::digest(
        format!("{}\0mcode-public", auth_home.display()).as_bytes(),
    ));
    let key = format!("com.minimax.mcode.oauth.prod.{region}\0{account_key}");
    let generation = credential["records"][&key]["generation"]
        .as_u64()
        .unwrap_or(0)
        .max(previous_state["generation"].as_u64().unwrap_or(0))
        .checked_add(1)
        .filter(|g| *g < 9_007_199_254_740_992)
        .ok_or("accountError.format")?;
    let expires = credentials
        .expires_at
        .checked_mul(1000)
        .ok_or("accountError.format")?;
    let mut record = json!({"schemaVersion":1,"accessToken":credentials.access_token,
        "refreshToken":credentials.refresh_token,"tokenType":"Bearer","clientId":"mcode-public",
        "scopes":["agent.default"],"audience":"agent-backend","expiresAtMs":expires,
        "generation":generation,"loginEpoch":uuid::Uuid::new_v4().to_string()});
    if let Ok(claims) = super::cursor_auth::claims(&credentials.access_token) {
        for (source, target) in [("sub", "subject"), ("account_id", "accountId")] {
            if let Some(value) = claims[source].as_str().filter(|s| !s.is_empty()) {
                record[target] = json!(value);
            }
        }
    }
    credential["schemaVersion"] = json!(1);
    if credential.get("records").is_none() {
        credential["records"] = json!({});
    }
    credential["records"][key] = record;
    let state = json!({"schemaVersion":2,"status":"authenticated","storeKind":"file",
        "clientId":"mcode-public","scopes":["agent.default"],"audience":"agent-backend",
        "buildEnv":"prod","region":region,"expiresAtMs":expires,"generation":generation});
    let mut preference = files::object(&preference_path)?;
    if preference.get("regions").is_some_and(|v| !v.is_object()) {
        return Err("accountError.format".into());
    }
    preference["version"] = json!(1);
    if preference.get("regions").is_none() {
        preference["regions"] = json!({});
    }
    preference["regions"]["prod"] = json!(region);
    preference["updatedAtMs"] = json!(chrono::Utc::now().timestamp_millis());
    let mut config: Value = match files::read_optional(&config_path)? {
        Some(bytes) => serde_yaml_ng::from_slice(&bytes).map_err(|_| "accountError.format")?,
        None => json!({}),
    };
    if config.is_null() {
        config = json!({});
    }
    if !config.is_object()
        || config
            .get("custom_provider")
            .is_some_and(|v| !v.is_object())
    {
        return Err("accountError.format".into());
    }
    super::tool_config_manager::minimaxcode::select_official(&mut config);
    if let Some(providers) = config["custom_provider"].as_object_mut() {
        providers.remove("echobird");
    }
    let mut transaction = Transaction::new();
    for (path, bytes) in [
        (credential_path, files::json(&credential)?),
        (state_path, files::json(&state)?),
        (preference_path, files::json(&preference)?),
        (
            config_path,
            serde_yaml_ng::to_string(&config)
                .map_err(|_| "accountError.format")?
                .into_bytes(),
        ),
    ] {
        transaction.replace(&path, &bytes)?;
    }
    Ok(transaction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn fixture(region: Region) -> Credentials {
        Credentials {
            region,
            access_token: "synthetic-access".into(),
            refresh_token: "synthetic-refresh".into(),
            expires_at: 2_000_000_000,
            user_id: "fixture-user".into(),
            email: "fixture@example.test".into(),
            name: None,
        }
    }
    fn home() -> PathBuf {
        std::env::temp_dir().join(format!("echobird-minimax-native-{}", uuid::Uuid::new_v4()))
    }
    #[test]
    fn cli_regions_select_native_namespace_and_preserve_custom_configuration() {
        let home = home();
        files::write(&home.join("config.yaml"),b"theme: dark\ncustom_provider:\n  user: {enabled: true}\n  echobird: {}\ndefaultModel: custom_provider:echobird/test\n").unwrap();
        files::write(
            &home.join("preferences/mcode-region.json"),
            b"{\"version\":1,\"regions\":{\"test\":\"cn\"}}",
        )
        .unwrap();
        for region in [Region::Cn, Region::Global] {
            write_cli(&home, &fixture(region)).unwrap().commit();
        }
        for region in ["cn", "en"] {
            let directory = home.join("auth/prod").join(region).join("mcode-public");
            let state = files::object(&directory.join("auth-state.json")).unwrap();
            assert_eq!(state["status"], "authenticated");
            assert_eq!(state["region"], region);
            assert_eq!(state["schemaVersion"], 2);
            assert_eq!(state["expiresAtMs"], 2_000_000_000_000i64);
            assert!(state.get("accessToken").is_none());
            let record = files::object(&directory.join("auth.json")).unwrap();
            assert_eq!(record["records"].as_object().unwrap().len(), 1);
            assert_eq!(
                record["records"]
                    .as_object()
                    .unwrap()
                    .values()
                    .next()
                    .unwrap()["accessToken"],
                "synthetic-access"
            );
        }
        let preference = files::object(&home.join("preferences/mcode-region.json")).unwrap();
        assert_eq!(preference["regions"], json!({"test":"cn","prod":"en"}));
        let config: Value =
            serde_yaml_ng::from_slice(&std::fs::read(home.join("config.yaml")).unwrap()).unwrap();
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["custom_provider"]["user"]["enabled"], true);
        assert!(config["custom_provider"].get("echobird").is_none());
        assert!(config.get("defaultModel").is_none());
        assert_eq!(config["minimaxModelSource"], "token_plan");
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn malformed_native_configuration_is_preserved_and_auth_not_written() {
        let home = home();
        files::write(&home.join("config.yaml"), b"invalid: [").unwrap();
        assert!(write_cli(&home, &fixture(Region::Cn)).is_err());
        assert!(!home.join("auth/prod/cn/mcode-public/auth.json").exists());
        assert_eq!(
            std::fs::read(home.join("config.yaml")).unwrap(),
            b"invalid: ["
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn native_credential_key_uses_the_resolved_lexical_data_root() {
        let root = home();
        let normalized = files::absolute_path(&root.join("data/../.minimax")).unwrap();
        write_cli(&normalized, &fixture(Region::Cn))
            .unwrap()
            .commit();
        let auth_home = root.join(".minimax").join("auth");
        let key = format!(
            "com.minimax.mcode.oauth.prod.cn\0{}",
            URL_SAFE_NO_PAD.encode(Sha256::digest(
                format!("{}\0mcode-public", auth_home.display()).as_bytes()
            ))
        );
        let saved = files::object(&auth_home.join("prod/cn/mcode-public/auth.json")).unwrap();
        assert_eq!(saved["records"][key]["accessToken"], "synthetic-access");
        std::fs::remove_dir_all(root).unwrap();
    }
}
