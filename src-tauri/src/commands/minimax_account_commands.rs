use crate::services::minimax_accounts::{self, Account, LoginStart, Region};

#[tauri::command]
pub async fn list_minimax_accounts() -> Result<Vec<Account>, String> {
    minimax_accounts::list_saved_accounts().await
}
#[tauri::command]
pub async fn start_minimax_login(region: Region) -> Result<LoginStart, String> {
    minimax_accounts::start_login(region).await
}
#[tauri::command]
pub async fn poll_minimax_login(login_id: String) -> Result<Option<Account>, String> {
    minimax_accounts::poll_login(&login_id).await
}
#[tauri::command]
pub fn cancel_minimax_login(login_id: String) -> Result<(), String> {
    minimax_accounts::cancel_login(&login_id)
}
#[tauri::command]
pub async fn delete_minimax_account(account_id: String) -> Result<(), String> {
    minimax_accounts::delete_account(&account_id).await
}
#[tauri::command]
pub async fn claim_minimax_daily_credits(account_id: String) -> Result<Account, String> {
    minimax_accounts::claim_daily_credits(&account_id).await
}
#[tauri::command]
pub async fn refresh_minimax_account(account_id: String) -> Result<Account, String> {
    minimax_accounts::refresh_account(&account_id).await
}
