mod app;
pub(crate) mod desktop;

pub(crate) use app::active_user_id as active_app_user_id;

use super::qoder_accounts::{self, Account, Edition};
use std::path::Path;

fn tool_matches(edition: Edition, tool_id: &str) -> bool {
    matches!(
        (edition, tool_id),
        (Edition::Qoder, "qoder") | (Edition::QoderCn, "qodercn")
    )
}

pub(crate) fn desktop_edition(executable: &Path) -> Option<Edition> {
    desktop::edition(&std::fs::canonicalize(executable).ok()?)
}
pub(crate) async fn apply(
    edition: Edition,
    account_id: &str,
    tool_id: &str,
) -> Result<Account, String> {
    if !tool_matches(edition, tool_id) {
        return Err("accountError.invalidAccount".into());
    }
    let executable =
        super::tool_manager::get_tool_exe_path(tool_id).ok_or("accountError.unavailable")?;
    if super::process_manager::desktop_tool_is_running(tool_id).await? {
        return Err("accountError.closeClient".into());
    }
    let session = qoder_accounts::credentials_for_native(edition, account_id).await?;
    let credentials = &session.credentials;
    let executable = std::fs::canonicalize(executable).map_err(|_| "accountError.unavailable")?;
    let account = match desktop::identity(&executable) {
        Some((e, desktop::ProductKind::Ide)) if e == edition => {
            let transaction = desktop::apply(credentials, &executable).await?;
            let account = qoder_accounts::mark_active(edition, account_id).await?;
            transaction.commit();
            account
        }
        Some((e, desktop::ProductKind::App)) if e == edition => {
            let transaction = app::apply(credentials, &executable).await?;
            let account = qoder_accounts::mark_active(edition, account_id).await?;
            transaction.commit();
            account
        }
        _ => return Err("accountError.invalidAccount".into()),
    };
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_binding_accepts_only_its_desktop_edition() {
        assert!(tool_matches(Edition::Qoder, "qoder"));
        assert!(tool_matches(Edition::QoderCn, "qodercn"));
        assert!(!tool_matches(Edition::Qoder, "qodercn"));
        assert!(!tool_matches(Edition::QoderCn, "qoder"));
        assert!(!tool_matches(Edition::Qoder, "unknown"));
        assert!(!tool_matches(Edition::QoderCn, "unknown"));
    }

    #[tokio::test]
    async fn mismatched_or_unsupported_tools_fail_before_resolving_native_configuration() {
        for (edition, tool_id) in [
            (Edition::Qoder, "qodercn"),
            (Edition::QoderCn, "qoder"),
            (Edition::Qoder, "unknown"),
            (Edition::QoderCn, "unknown"),
        ] {
            assert!(matches!(
                apply(edition, "fixture-account", tool_id).await,
                Err(error) if error == "accountError.invalidAccount"
            ));
        }
    }
}
