use super::{providers, ProxyError};
use crate::models::model::AccountProvider;
use axum::http::StatusCode;
use serde_json::Value;
use std::collections::BTreeSet;

const BASE: &str = "https://api.cline.bot";

pub(super) async fn list_models(
    credentials: Value,
    client: &reqwest::Client,
) -> Result<Vec<String>, ProxyError> {
    let token = credentials["accessToken"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            ProxyError::new(
                StatusCode::UNAUTHORIZED,
                "Cline account has no access token",
            )
        })?;
    let base = BASE;
    #[cfg(test)]
    let base = credentials["baseUrl"].as_str().unwrap_or(base);
    let get = |path: &str| {
        client
            .get(format!("{base}{path}"))
            .bearer_auth(token)
            .header("HTTP-Referer", "https://cline.bot")
            .header("X-Title", "EchoBird")
    };
    let (catalog, recommended) = tokio::try_join!(
        read(get("/api/v1/ai/cline/models")),
        read(get("/api/v1/ai/cline/recommended-models"))
    )?;
    let rows = catalog["data"]
        .as_array()
        .ok_or_else(|| ProxyError::new(StatusCode::BAD_GATEWAY, "Invalid Cline model directory"))?;
    let mut ids = BTreeSet::new();
    let excluded: BTreeSet<_> = rows
        .iter()
        .filter(|row| !chat_model(row))
        .filter_map(|row| row["id"].as_str())
        .collect();
    for row in rows {
        add(&mut ids, row);
    }
    for bucket in ["recommended", "free", "clinePass"] {
        if let Some(rows) = recommended[bucket].as_array() {
            for row in rows {
                add(&mut ids, row);
            }
        }
    }
    ids.retain(|id| !excluded.contains(id.as_str()));
    if ids.is_empty() {
        return Err(ProxyError::new(
            StatusCode::BAD_GATEWAY,
            "Cline returned no chat models",
        ));
    }
    let free_ids = rows
        .iter()
        .chain(
            ["recommended", "clinePass"]
                .into_iter()
                .flat_map(|bucket| recommended[bucket].as_array().into_iter().flatten()),
        )
        .filter(|row| row["free"] == true || row["free"] == "true")
        .chain(recommended["free"].as_array().into_iter().flatten())
        .filter(|row| chat_model(row))
        .filter_map(|row| row["id"].as_str())
        .filter(|id| ids.contains(*id))
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    providers::remember_free_model_ids(AccountProvider::Cline, &credentials, free_ids);
    Ok(ids.into_iter().collect())
}
fn add(ids: &mut BTreeSet<String>, row: &Value) {
    if !chat_model(row) {
        return;
    }
    if let Some(id) = row["id"]
        .as_str()
        .filter(|id| !id.trim().is_empty() && !id.starts_with("cline-cloud/"))
    {
        ids.insert(id.into());
    }
}
fn chat_model(row: &Value) -> bool {
    row["disabled"] != true
        && row["supported_in_api"] != false
        && !row["architecture"]["output_modalities"]
            .as_array()
            .is_some_and(|modalities| {
                !modalities.iter().any(|value| value == "text")
                    || modalities.iter().any(|value| value == "image")
            })
}
async fn read(request: reqwest::RequestBuilder) -> Result<Value, ProxyError> {
    let response = request.send().await.map_err(ProxyError::upstream)?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.json::<Value>().await;
    if !status.is_success() {
        return Err(providers::upstream_error(
            status,
            &headers,
            &body.unwrap_or(Value::Null),
        ));
    }
    body.map_err(ProxyError::upstream)
}

#[cfg(test)]
mod tests;
