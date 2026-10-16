use crate::models::model::{AccountProvider, AccountSource};
use crate::services::account_model_sources::{self, SavedAccountOption};

#[tauri::command]
pub async fn list_proxy_accounts(
    provider: AccountProvider,
) -> Result<Vec<SavedAccountOption>, String> {
    account_model_sources::list_accounts(provider).await
}

#[tauri::command]
pub async fn get_account_proxy_models(source: AccountSource) -> Result<Vec<String>, String> {
    account_model_sources::list_models(&source).await
}
