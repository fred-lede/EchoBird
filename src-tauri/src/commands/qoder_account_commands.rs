use crate::services::qoder_accounts::{self, Account, Edition, LoginStart};

#[tauri::command]
pub async fn list_qoder_accounts(edition: Edition) -> Result<Vec<Account>, String> {
    qoder_accounts::list_saved_accounts(edition).await
}
#[tauri::command]
pub async fn start_qoder_login(edition: Edition) -> Result<LoginStart, String> {
    qoder_accounts::start_login(edition).await
}
#[tauri::command]
pub async fn poll_qoder_login(login_id: String) -> Result<Option<Account>, String> {
    qoder_accounts::poll_login(&login_id).await
}
#[tauri::command]
pub async fn cancel_qoder_login(login_id: String) -> Result<(), String> {
    qoder_accounts::cancel_login(&login_id)
}
#[tauri::command]
pub async fn refresh_qoder_account_quota(
    edition: Edition,
    account_id: String,
) -> Result<Account, String> {
    qoder_accounts::refresh_quota(edition, &account_id).await
}
#[tauri::command]
pub async fn claim_qoder_daily_credits(
    edition: Edition,
    account_id: String,
) -> Result<Account, String> {
    qoder_accounts::claim_daily_credits(edition, &account_id).await
}
#[tauri::command]
pub async fn delete_qoder_account(edition: Edition, account_id: String) -> Result<(), String> {
    qoder_accounts::delete_account(edition, &account_id).await
}
