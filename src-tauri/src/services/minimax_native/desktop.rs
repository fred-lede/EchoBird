use crate::services::minimax_accounts::Region;
use std::path::{Path, PathBuf};

pub(super) fn data_dir(executable: &Path, region: Region) -> Result<PathBuf, String> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        let expected = if region == Region::Cn { "cn" } else { "en" };
        if crate::services::tool_config_manager::minimaxcode::desktop_region(executable)
            != Some(expected)
        {
            return Err("accountError.invalidAccount".into());
        }
        let home = dirs::home_dir().ok_or("accountError.home")?;
        #[cfg(windows)]
        let config_root = dirs::config_dir().ok_or("accountError.home")?;
        #[cfg(target_os = "macos")]
        let config_root = home.join("Library/Application Support");
        resolve(
            &home,
            &config_root,
            region,
            std::env::var_os("MINIMAX_DATA_DIR")
                .filter(|value| !value.to_string_lossy().trim().is_empty())
                .map(PathBuf::from),
            std::env::var_os("MAVIS_DATA_DIR")
                .filter(|value| !value.to_string_lossy().trim().is_empty())
                .map(PathBuf::from),
        )
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (executable, region);
        Err("accountError.unavailable".into())
    }
}

fn directory_has_data(path: &Path) -> Result<bool, String> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("accountError.read".into()),
    };
    let entries: Vec<_> = entries
        .collect::<Result<_, _>>()
        .map_err(|_| "accountError.read")?;
    if entries.len() == 1 && entries[0].file_name() == "workspace" {
        return directory_has_data(&entries[0].path());
    }
    Ok(!entries.is_empty())
}

pub(super) fn ensure_default_root(home: &Path, primary: &Path) -> Result<(), String> {
    if !directory_has_data(primary)? && directory_has_data(&home.join(".mavis"))? {
        return Err("accountError.initializeClient".into());
    }
    Ok(())
}

#[cfg(any(windows, target_os = "macos", test))]
fn absolute(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("accountError.home".into());
    }
    crate::services::native_account_files::absolute_path(&path)
}

#[cfg(any(windows, target_os = "macos", test))]
fn resolve(
    home: &Path,
    config_root: &Path,
    region: Region,
    primary_env: Option<PathBuf>,
    legacy_env: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(path) = primary_env.or(legacy_env) {
        return absolute(path);
    }
    let (app, config_name) = match region {
        Region::Cn => ("MiniMax", "minimax-agent-cn-config.json"),
        Region::Global => ("MiniMax Agent", "minimax-agent-config.json"),
    };
    let path = config_root.join(app).join(config_name);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| "accountError.format")?;
            if !value.is_object()
                || value
                    .get("config")
                    .is_some_and(|config| !config.is_object())
            {
                return Err("accountError.format".into());
            }
            if let Some(parent) = value["config"].get("localRuntimeDataParentDir") {
                if !parent.is_null() {
                    let parent = parent.as_str().ok_or("accountError.format")?.trim();
                    if !parent.is_empty() {
                        return absolute(Path::new(parent).join(".minimax"));
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("accountError.read".into()),
    }
    let primary = home.join(".minimax");
    ensure_default_root(home, &primary)?;
    absolute(primary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::minimax_accounts::test_support::Fixture;
    fn settings(root: &Path, region: Region, value: &serde_json::Value) {
        let (app, name) = if region == Region::Cn {
            ("MiniMax", "minimax-agent-cn-config.json")
        } else {
            ("MiniMax Agent", "minimax-agent-config.json")
        };
        std::fs::create_dir_all(root.join(app)).unwrap();
        std::fs::write(root.join(app).join(name), value.to_string()).unwrap();
    }
    #[test]
    fn regional_custom_parents_and_environment_precedence_are_respected_without_writes() {
        let fixture = Fixture::new();
        let root = fixture.0.join("settings");
        let home = fixture.0.join("home");
        let cn = fixture.0.join("cn-parent");
        let global = fixture.0.join("global-parent");
        settings(
            &root,
            Region::Cn,
            &serde_json::json!({"config":{"localRuntimeDataParentDir":cn}}),
        );
        settings(
            &root,
            Region::Global,
            &serde_json::json!({"config":{"localRuntimeDataParentDir":global}}),
        );
        assert_eq!(
            resolve(&home, &root, Region::Cn, None, None).unwrap(),
            cn.join(".minimax")
        );
        assert_eq!(
            resolve(&home, &root, Region::Global, None, None).unwrap(),
            global.join(".minimax")
        );
        let primary = fixture.0.join("override");
        let legacy = fixture.0.join("legacy");
        assert_eq!(
            resolve(
                &home,
                &root,
                Region::Cn,
                Some(primary.clone()),
                Some(legacy.clone())
            )
            .unwrap(),
            primary
        );
        assert_eq!(
            resolve(&home, &root, Region::Cn, None, Some(legacy.clone())).unwrap(),
            legacy
        );
        assert!(!home.exists());
        assert!(!cn.exists());
        assert!(!global.exists());
    }
    #[test]
    fn legacy_data_waits_for_native_migration_and_malformed_settings_are_not_ignored() {
        let fixture = Fixture::new();
        let root = fixture.0.join("settings");
        let home = fixture.0.join("home");
        std::fs::create_dir_all(home.join(".mavis")).unwrap();
        std::fs::write(home.join(".mavis/old-data"), b"existing").unwrap();
        assert_eq!(
            resolve(&home, &root, Region::Cn, None, None).unwrap_err(),
            "accountError.initializeClient"
        );
        assert!(!home.join(".minimax").exists());
        assert!(home.join(".mavis/old-data").exists());
        std::fs::create_dir_all(home.join(".minimax")).unwrap();
        std::fs::write(home.join(".minimax/config.yaml"), b"theme: dark").unwrap();
        assert_eq!(
            resolve(&home, &root, Region::Cn, None, None).unwrap(),
            home.join(".minimax")
        );
        settings(
            &root,
            Region::Cn,
            &serde_json::json!({"config":{"localRuntimeDataParentDir":42}}),
        );
        assert_eq!(
            resolve(&home, &root, Region::Cn, None, None).unwrap_err(),
            "accountError.format"
        );
        assert!(resolve(
            &home,
            &root,
            Region::Cn,
            Some(PathBuf::from("relative")),
            None
        )
        .is_err());
    }
}
