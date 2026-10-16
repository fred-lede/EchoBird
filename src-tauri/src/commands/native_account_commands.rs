use crate::services::{minimax_accounts, minimax_native, qoder_accounts, qoder_native};

#[tauri::command]
pub async fn switch_minimax_account(
    account_id: String,
    tool_id: String,
) -> Result<minimax_accounts::Account, String> {
    minimax_native::apply(&account_id, &tool_id).await
}

#[tauri::command]
pub async fn switch_qoder_account(
    edition: qoder_accounts::Edition,
    account_id: String,
    tool_id: String,
) -> Result<qoder_accounts::Account, String> {
    qoder_native::apply(edition, &account_id, &tool_id).await
}
