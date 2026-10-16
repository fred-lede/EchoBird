use crate::services::cline_accounts::{self, Account, LoginStart};

#[tauri::command]
pub async fn list_cline_accounts() -> Result<Vec<Account>, String> {
    cline_accounts::list_saved_accounts().await
}
#[tauri::command]
pub async fn start_cline_login() -> Result<LoginStart, String> {
    cline_accounts::start_login().await
}
#[tauri::command]
pub async fn poll_cline_login(login_id: String) -> Result<Option<Account>, String> {
    cline_accounts::poll_login(&login_id).await
}
#[tauri::command]
pub fn cancel_cline_login(login_id: String) -> Result<(), String> {
    cline_accounts::cancel_login(&login_id)
}
#[tauri::command]
pub async fn refresh_cline_account(account_id: String) -> Result<Account, String> {
    cline_accounts::refresh_account(&account_id).await
}
#[tauri::command]
pub async fn delete_cline_account(account_id: String) -> Result<(), String> {
    cline_accounts::delete_account(&account_id).await
}
#[tauri::command]
pub async fn switch_cline_account(account_id: String, tool_id: String) -> Result<Account, String> {
    crate::services::cline_native::apply(&account_id, &tool_id).await
}
