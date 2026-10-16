//! Update existing client and relay URLs when a local listener changes port.

use super::{claudedesktop, mimodesktop};
use crate::services::{codex_runtime, local_proxy, tool_manager};
use std::collections::HashSet;
use std::path::PathBuf;

pub(crate) fn migrate_local_proxy_port(old: u16, new: u16) -> Result<(), String> {
    let (mut paths, errors) = tool_manager::model_config_paths();
    if let Some(home) = dirs::home_dir() {
        // Split settings/providers and JSONC alternatives to configFile.
        for relative in [
            ".claude/settings.json",
            ".echobird/claudecode.json",
            ".echobird/claudedesktop.json",
            ".pi/agent/models.json",
            ".omp/agent/models.yml",
            ".omp/agent/models.yaml",
            ".dsh/.credentials.yaml",
            ".config/opencode/opencode.json",
            ".config/opencode/opencode.jsonc",
            ".config/mimocode/mimocode.json",
            ".config/mimocode/mimocode.jsonc",
            ".config/kilo/kilo.json",
            ".config/kilo/kilo.jsonc",
            ".config/openscience/openscience.json",
            ".config/openscience/openscience.jsonc",
        ] {
            paths.push(home.join(relative));
        }
    }
    if let Some(dir) = codex_runtime::default_codex_dir() {
        paths.push(dir.join("config.toml"));
    }
    if let Some(layout) = claudedesktop::resolve_claudedesktop_paths() {
        paths.push(
            layout
                .lib_dir
                .join(format!("{}.json", claudedesktop::CLAUDE_DESKTOP_PROFILE_ID)),
        );
    }
    if let Ok(dir) = mimodesktop::config_dir() {
        paths.extend(mimodesktop::CONFIG_NAMES.iter().map(|name| dir.join(name)));
    }
    let paths_result = migrate_paths(paths, errors, old, new);
    let muse_result = super::musecode::migrate_proxy_port(old, new);
    match (paths_result, muse_result) {
        (Err(paths), Err(muse)) => Err(format!("{paths}; {muse}")),
        (Err(error), _) | (_, Err(error)) => Err(error),
        _ => Ok(()),
    }
}

fn migrate_paths(
    paths: Vec<PathBuf>,
    mut errors: Vec<String>,
    old: u16,
    new: u16,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for path in paths {
        if seen.insert(path.clone()) {
            if let Err(error) = local_proxy::migrate_file(&path, old, new) {
                errors.push(error);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zcode_path_failure_keeps_migration_pending_without_blocking_other_clients() {
        let root =
            std::env::temp_dir().join(format!("echobird-zcode-port-{}", uuid::Uuid::new_v4()));
        let settings = root.join(".zcode/v2/setting.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, "broken").unwrap();
        let error = crate::services::zcode_paths::resolve_at(&root, None)
            .err()
            .unwrap();
        let other_client = root.join("other.json");
        std::fs::write(&other_client, r#"{"baseUrl":"http://localhost:14565/v1"}"#).unwrap();
        let failed_client = root.join("blocked.json");
        std::fs::create_dir(&failed_client).unwrap();
        let combined = migrate_paths(
            vec![other_client.clone(), failed_client],
            vec![error.clone()],
            14565,
            14566,
        )
        .unwrap_err();
        assert!(combined.contains(&error));
        assert!(combined.contains("blocked.json"));
        assert!(std::fs::read_to_string(&other_client)
            .unwrap()
            .contains("localhost:14566/v1"));
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "broken");

        std::fs::write(&settings, "{}").unwrap();
        let paths = crate::services::zcode_paths::resolve_at(&root, None).unwrap();
        let provider = paths.config_dir.join("provider_config.json");
        std::fs::write(&provider, r#"{"baseUrl":"http://localhost:14565/v1"}"#).unwrap();
        assert!(migrate_paths(vec![other_client, provider.clone()], vec![], 14565, 14566).is_ok());
        assert!(std::fs::read_to_string(provider)
            .unwrap()
            .contains("localhost:14566/v1"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
