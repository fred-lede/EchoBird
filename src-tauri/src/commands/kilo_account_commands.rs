use crate::services::kilo_accounts::{self, Account, LoginStart};

#[tauri::command]
pub async fn list_kilo_accounts() -> Result<Vec<Account>, String> {
    kilo_accounts::list_accounts().await
}

#[tauri::command]
pub async fn start_kilo_login() -> Result<LoginStart, String> {
    kilo_accounts::start_login().await
}

#[tauri::command]
pub async fn poll_kilo_login(login_id: String) -> Result<Option<Account>, String> {
    kilo_accounts::poll_login(&login_id).await
}

#[tauri::command]
pub fn cancel_kilo_login(login_id: String) -> Result<(), String> {
    kilo_accounts::cancel_login(&login_id)
}

#[tauri::command]
pub async fn switch_kilo_account(account_id: String, tool_id: String) -> Result<Account, String> {
    kilo_accounts::switch_account(&account_id, &tool_id).await
}

#[tauri::command]
pub async fn refresh_kilo_account(account_id: String) -> Result<Account, String> {
    kilo_accounts::refresh(&account_id).await
}

#[tauri::command]
pub async fn delete_kilo_account(account_id: String) -> Result<(), String> {
    kilo_accounts::delete(&account_id).await
}

#[tauri::command]
pub async fn capture_kilo_account() -> Result<Account, String> {
    kilo_accounts::capture_current_account().await
}
