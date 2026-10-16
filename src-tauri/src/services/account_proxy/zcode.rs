use std::{
    collections::{BTreeMap, HashSet},
    sync::OnceLock,
};

use axum::{http::StatusCode, response::Response};
use serde_json::{json, Value};

use super::{protocol, providers, ProxyError};
use crate::models::model::AccountProvider;

const ORIGIN: &str = "https://zcode.z.ai";
const APP_VERSION: &str = "3.14.3";
const START_MODELS: [&str; 3] = ["GLM-5.3-Flash", "GLM-5.2", "GLM-5-Turbo"];

fn platform() -> String {
    let os = if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    format!("{os}-{arch}")
}

fn os_version() -> String {
    platform().replace('-', " ")
}

fn origin(credentials: &Value) -> &str {
    #[cfg(test)]
    if let Some(base) = credentials["baseUrl"].as_str() {
        return base.trim_end_matches('/');
    }
    let _ = credentials;
    ORIGIN
}

fn token(credentials: &Value) -> Result<&str, ProxyError> {
    credentials["jwtToken"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| ProxyError::new(StatusCode::UNAUTHORIZED, "ZCode account needs sign-in"))
}

fn device(credentials: &Value) -> Result<&str, ProxyError> {
    credentials["deviceMid"]
        .as_str()
        .filter(|device| !device.is_empty())
        .ok_or_else(|| {
            ProxyError::new(StatusCode::UNAUTHORIZED, "ZCode device identity is missing")
        })
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}

fn timestamp(value: &Value) -> Option<f64> {
    number(value).map(|value| {
        if value > 1_000_000_000_000.0 {
            value / 1000.0
        } else {
            value
        }
    })
}

fn available_models(data: &Value) -> Result<Vec<String>, ProxyError> {
    let plans = data["plans"]
        .as_array()
        .ok_or_else(|| ProxyError::upstream("ZCode balance has no plans"))?;
    let balances = data["balances"]
        .as_array()
        .ok_or_else(|| ProxyError::upstream("ZCode balance has no buckets"))?;
    let now =
        timestamp(&data["server_time"]).unwrap_or_else(|| chrono::Utc::now().timestamp() as f64);
    let mut models = BTreeMap::new();
    for plan in plans {
        if !plan["status"]
            .as_str()
            .is_some_and(|status| status.eq_ignore_ascii_case("active"))
            || timestamp(&plan["ends_at"]).is_some_and(|end| end <= now)
        {
            continue;
        }
        for bucket in balances {
            let same_plan =
                if plan["user_plan_id"].is_string() && bucket["user_plan_id"].is_string() {
                    plan["user_plan_id"] == bucket["user_plan_id"]
                } else {
                    plan["plan_id"].is_string() && plan["plan_id"] == bucket["plan_id"]
                };
            if !same_plan || timestamp(&bucket["expires_at"]).is_some_and(|end| end <= now) {
                continue;
            }
            let remaining = number(&bucket["remaining_units"])
                .or_else(|| Some(number(&bucket["total_units"])? - number(&bucket["used_units"])?));
            if remaining.is_some_and(|remaining| remaining <= 0.0) {
                continue;
            }
            let mut names: Vec<_> = bucket["capabilities"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(|value| {
                    let value = value.trim();
                    value
                        .strip_prefix("model:")
                        .or_else(|| (!value.contains(':')).then_some(value))
                })
                .collect();
            if names.is_empty() {
                names.extend(bucket["show_name"].as_str());
            }
            for name in names {
                let name = name.trim();
                if !name.is_empty() {
                    models
                        .entry(name.to_ascii_lowercase())
                        .or_insert_with(|| name.to_string());
                }
            }
        }
    }
    Ok(models.into_values().collect())
}

async fn balance(credentials: &Value, client: &reqwest::Client) -> Result<Vec<String>, ProxyError> {
    let response = client
        .get(format!(
            "{}/api/v1/zcode-plan/billing/balance",
            origin(credentials)
        ))
        .query(&[("app_version", APP_VERSION)])
        .bearer_auth(token(credentials)?)
        .header("x-device-mid", device(credentials)?)
        .send()
        .await
        .map_err(ProxyError::upstream)?;
    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let value = response.json::<Value>().await.unwrap_or(Value::Null);
        return Err(providers::upstream_error(status, &headers, &value));
    }
    let value = response
        .json::<Value>()
        .await
        .map_err(ProxyError::upstream)?;
    if value["success"] == false
        || value["code"]
            .as_i64()
            .is_some_and(|code| code != 0 && code != 200)
    {
        return Err(ProxyError::upstream("ZCode balance request failed"));
    }
    available_models(&value["data"])
}

fn configured_start_models(config: &Value) -> Vec<String> {
    let plan = "account:zai-start-plan";
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |id: &str| {
        let id = id.trim();
        if !id.is_empty() && seen.insert(id.to_ascii_lowercase()) {
            models.push(id.to_string());
        }
    };
    for rule in config["config"]["providerConfigRules"]["providerRules"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|rule| rule["providerId"] == plan)
    {
        for id in rule["builtinModelIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            add(id);
        }
    }
    let mut disabled = HashSet::new();
    for rule in config["config"]["modelConfigRules"]["builtinProviderModelRules"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|rule| rule["providerId"] == plan)
    {
        if let Some(id) = rule["modelId"].as_str() {
            if rule["config"]["enabled"] == false {
                disabled.insert(id.to_ascii_lowercase());
            } else {
                add(id);
            }
        }
    }
    models.retain(|id| !disabled.contains(&id.to_ascii_lowercase()));
    models
}

async fn start_models(credentials: &Value, client: &reqwest::Client) -> Vec<String> {
    let remote = async {
        let platform = platform();
        let config_response = client
            .get(format!("{}/api/v1/client/configs", origin(credentials)))
            .query(&[
                ("app_version", APP_VERSION),
                ("platform", platform.as_str()),
            ])
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .ok()?;
        if !config_response.status().is_success() {
            return None;
        }
        let response: Value = config_response.json().await.ok()?;
        let url = response["data"]["configs"]["builtin_provider_config_json"].as_str()?;
        let parsed = reqwest::Url::parse(url).ok()?;
        #[cfg(not(test))]
        if parsed.scheme() != "https" || parsed.host_str() != Some("cdn-zcode.z.ai") {
            return None;
        }
        let provider_response = client
            .get(parsed)
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .ok()?;
        if !provider_response.status().is_success() {
            return None;
        }
        let config: Value = provider_response.json().await.ok()?;
        Some(configured_start_models(&config))
    }
    .await;
    remote.unwrap_or_else(|| START_MODELS.iter().map(|id| (*id).to_string()).collect())
}

fn prepare_request(request: &mut Value, credentials: &Value) -> Result<(), ProxyError> {
    static PROMPT: OnceLock<Value> = OnceLock::new();
    let prompt = PROMPT.get_or_init(|| {
        serde_json::from_str(include_str!("zcode_prompt.json"))
            .expect("bundled ZCode prompt must be valid JSON")
    });
    let text = |key: &str| prompt[key].as_str().unwrap_or_default();
    let cached =
        |text: String| json!({"type":"text","text":text,"cache_control":{"type":"ephemeral"}});
    let model = request["model"].as_str().unwrap_or_default();
    let site = if credentials["site"] == "bigmodel" {
        "bigmodel-api"
    } else {
        "zai-api"
    };
    let powered = prompt["environment"]["poweredByLine"]
        .as_str()
        .unwrap_or_default()
        .replace("{provider}", site)
        .replace("{model}", model);
    let environment = format!(
        "{}\n{}\n- {}: {}\n- {}: {}\n{powered}",
        prompt["environment"]["heading"]
            .as_str()
            .unwrap_or_default(),
        prompt["environment"]["invokedLine"]
            .as_str()
            .unwrap_or_default(),
        prompt["environment"]["platformLabel"]
            .as_str()
            .unwrap_or_default(),
        platform().split('-').next().unwrap_or("linux"),
        prompt["environment"]["osVersionLabel"]
            .as_str()
            .unwrap_or_default(),
        os_version(),
    );
    let mut system = vec![
        cached(text("prefix").to_string()),
        cached(format!("{}\n\n{}", text("stable"), text("desktop"))),
        cached(format!(
            "\n\n{}\n\n{}\n\n{}",
            text("beforeEnvironment"),
            environment,
            text("afterEnvironment")
        )),
    ];
    if let Some(own) = request["system"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
    {
        system.push(json!({"type":"text","text":own}));
    } else if let Some(own) = request["system"].as_array() {
        system.extend(
            own.iter()
                .filter(|entry| {
                    entry["type"] == "text"
                        && entry["text"].as_str().is_some_and(|text| !text.is_empty())
                })
                .map(|entry| json!({"type":"text","text":entry["text"]})),
        );
    }
    request["system"] = Value::Array(system);

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let reminder = format!(
        "<system-reminder>{}\n{}\n{}\n\n{}</system-reminder>",
        prompt["context"]["intro"].as_str().unwrap_or_default(),
        prompt["context"]["currentDateHeading"]
            .as_str()
            .unwrap_or_default(),
        prompt["context"]["currentDateLine"]
            .as_str()
            .unwrap_or_default()
            .replace("{date}", &date),
        prompt["context"]["outro"].as_str().unwrap_or_default(),
    );
    let messages = request["messages"]
        .as_array_mut()
        .ok_or_else(|| ProxyError::new(StatusCode::BAD_REQUEST, "Missing messages"))?;
    messages.insert(
        0,
        json!({"role":"user","content":[{"type":"text","text":reminder}]}),
    );
    for message in messages.iter_mut() {
        if let Some(blocks) = message["content"].as_array_mut() {
            for block in blocks {
                if let Some(object) = block.as_object_mut() {
                    object.remove("cache_control");
                }
            }
        }
    }
    if let Some(last) = messages
        .iter_mut()
        .rev()
        .find(|message| message["role"] != "system")
    {
        if let Some(content) = last["content"].as_str() {
            last["content"] = json!([cached(content.to_string())]);
        } else if let Some(block) = last["content"]
            .as_array_mut()
            .and_then(|blocks| blocks.last_mut())
        {
            if let Some(object) = block.as_object_mut() {
                object.insert("cache_control".into(), json!({"type":"ephemeral"}));
            }
        }
    }
    if let Some(tools) = request["tools"].as_array_mut() {
        for tool in tools {
            if let Some(object) = tool.as_object_mut() {
                object.remove("cache_control");
            }
        }
    }
    request["metadata"] = json!({"user_id": json!({"device_id":device(credentials)?,"account_uuid":"","session_id":""}).to_string()});
    Ok(())
}

pub(super) async fn list_models(
    credentials: Value,
    client: &reqwest::Client,
) -> Result<Vec<String>, ProxyError> {
    token(&credentials)?;
    device(&credentials)?;
    let mut models = start_models(&credentials, client).await;
    match balance(&credentials, client).await {
        Ok(extra) => {
            for model in extra {
                if !models
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&model))
                {
                    models.push(model);
                }
            }
        }
        Err(error) if error.status == StatusCode::UNAUTHORIZED => return Err(error),
        Err(_) => {}
    }
    providers::remember_free_model_ids(AccountProvider::Zcode, &credentials, models.clone());
    Ok(models)
}

pub(super) async fn route(
    credentials: Value,
    model: &str,
    body: Value,
    client: &reqwest::Client,
) -> Result<Response, ProxyError> {
    protocol::reject_structured_output(&body)?;
    let model = START_MODELS
        .iter()
        .find(|known| known.eq_ignore_ascii_case(model))
        .copied()
        .unwrap_or(model);
    let mut request = providers::anthropic_request(&body, model);
    prepare_request(&mut request, &credentials)?;
    let platform = if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    let response = client
        .post(format!(
            "{}/api/v1/zcode-plan/anthropic/v1/messages",
            origin(&credentials)
        ))
        .bearer_auth(token(&credentials)?)
        .header("anthropic-version", "2023-06-01")
        .header("http-referer", origin(&credentials))
        .header(
            "user-agent",
            format!("ZCode/{APP_VERSION} ai-sdk/anthropic/3.0.81"),
        )
        .header("x-zcode-app-version", APP_VERSION)
        .header("x-title", "Z Code@cli")
        .header("x-release-channel", "production")
        .header("x-client-language", "unknown")
        .header("x-client-timezone", "unknown")
        .header("x-zcode-agent", "glm")
        .header("x-platform", format!("{platform}-{arch}"))
        .header("x-os-category", os)
        .header("x-os-version", os_version())
        .header("x-request-id", uuid::Uuid::new_v4().to_string())
        .header("x-zcode-session-type", "main")
        .header("x-zcode-trace-id", uuid::Uuid::new_v4().to_string())
        .json(&request)
        .send()
        .await
        .map_err(ProxyError::upstream)?;
    if !response.status().is_success() {
        let status = response.status();
        let headers = response.headers().clone();
        let value = response.json::<Value>().await.unwrap_or(Value::Null);
        return Err(providers::upstream_error(status, &headers, &value));
    }
    if response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("application/json"))
    {
        let headers = response.headers().clone();
        let value = response
            .json::<Value>()
            .await
            .map_err(ProxyError::upstream)?;
        let status = if matches!(value["code"].as_i64(), Some(1005 | 1113)) {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::BAD_GATEWAY
        };
        return Err(providers::upstream_error(status, &headers, &value));
    }
    providers::converted(
        response,
        protocol::WireProtocol::Anthropic,
        model,
        body["stream"] == true,
        true,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        body::to_bytes,
        extract::{Request, State},
        http::{header, StatusCode},
        routing::any,
        Router,
    };
    use serde_json::{json, Value};

    use super::{available_models, configured_start_models, list_models, route, start_models};

    #[derive(Clone, Default)]
    struct Seen(Arc<Mutex<Vec<(String, String, Value)>>>);

    async fn upstream(State(seen): State<Seen>, request: Request) -> axum::response::Response {
        let path = request.uri().path().to_string();
        let host = request.headers()[header::HOST]
            .to_str()
            .unwrap()
            .to_string();
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|header| header.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if path.ends_with("/billing/balance") {
            assert_eq!(request.headers()["x-device-mid"], "fixture-device");
        } else if path.ends_with("/anthropic/v1/messages") {
            assert!(request.headers().get("x-device-mid").is_none());
            assert_eq!(request.headers()["x-title"], "Z Code@cli");
        }
        let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
        let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let message = body["messages"][1]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        seen.0.lock().unwrap().push((path.clone(), token, body));
        if path == "/api/v1/client/configs" {
            return super::super::json_response(&json!({"code":0,"data":{"configs":{
                "builtin_provider_config_json":format!("http://{host}/config.json")
            }}}));
        }
        if path == "/config.json" {
            return super::super::json_response(&json!({"config":{"modelConfigRules":{
                "builtinProviderModelRules":[
                    {"providerId":"account:zai-start-plan","modelId":"GLM-5.3-Flash","config":{"enabled":true}},
                    {"providerId":"account:zai-start-plan","modelId":"GLM-5.2","config":{"enabled":true}},
                    {"providerId":"account:zai-start-plan","modelId":"GLM-5-Turbo","config":{"enabled":true}}
                ]
            }}}));
        }
        if path.ends_with("/billing/balance") {
            return super::super::json_response(&json!({"success":true,"data":{
                "server_time":1000,
                "plans":[],"balances":[]
            }}));
        }
        assert!(path.ends_with("/anthropic/v1/messages"));
        if message == "quota" {
            return super::super::json_response(&json!({"code":1005,"msg":"exceed quota limit"}));
        }
        if message == "hello" {
            let events = [
                json!({"type":"message_start","message":{"usage":{"input_tokens":1,"output_tokens":0}}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"accepted"}}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
                json!({"type":"message_stop"}),
            ];
            let stream: String = events
                .iter()
                .map(|event| {
                    format!(
                        "event: {}\ndata: {event}\n\n",
                        event["type"].as_str().unwrap()
                    )
                })
                .collect();
            return axum::response::Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(axum::body::Body::from(stream))
                .unwrap();
        }
        axum::response::Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(
                json!({"error":{"message":"fixture stopped request"}}).to_string(),
            ))
            .unwrap()
    }

    #[test]
    fn start_models_require_live_plan_and_unspent_bucket() {
        let data = json!({
            "server_time": 1000,
            "plans": [
                {"user_plan_id":"live","status":"active","ends_at":2000},
                {"user_plan_id":"expired","status":"active","ends_at":900}
            ],
            "balances": [
                {"user_plan_id":"live","remaining_units":4,"expires_at":2000,"capabilities":["model:GLM-5.3-Flash", "other:tool"]},
                {"user_plan_id":"live","remaining_units":2,"expires_at":2000,"capabilities":["GLM-5.2"]},
                {"user_plan_id":"live","remaining_units":0,"capabilities":["model:GLM-5.2"]},
                {"user_plan_id":"expired","remaining_units":4,"capabilities":["model:GLM-5"]}
            ]
        });
        assert_eq!(
            available_models(&data).unwrap(),
            ["GLM-5.2", "GLM-5.3-Flash"]
        );
    }

    #[test]
    fn configured_models_use_start_plan_and_respect_disabled_entries() {
        let config = json!({"config":{
            "providerConfigRules":{"providerRules":[
                {"providerId":"account:zai-start-plan","builtinModelIds":["GLM-5.2","GLM-5.3"]}
            ]},
            "modelConfigRules":{"builtinProviderModelRules":[
                {"providerId":"account:zai-start-plan","modelId":"GLM-5.2","config":{"enabled":false}},
                {"providerId":"account:zai-start-plan","modelId":"GLM-5-Turbo","config":{"enabled":true}},
                {"providerId":"account:zai-individual-coding-plan","modelId":"GLM-5.3-Flash","config":{"enabled":true}}
            ]}
        }});
        assert_eq!(configured_start_models(&config), ["GLM-5.3", "GLM-5-Turbo"]);
    }

    #[tokio::test]
    async fn unavailable_config_uses_start_plan_fallback() {
        let client = reqwest::Client::new();
        let credentials = json!({"baseUrl":"http://127.0.0.1:9"});
        assert_eq!(
            start_models(&credentials, &client).await,
            super::START_MODELS
        );
    }

    #[tokio::test]
    async fn missing_login_token_does_not_offer_models() {
        let client = reqwest::Client::new();
        let error = list_models(json!({"deviceMid":"fixture-device"}), &client)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_token_lists_start_models_and_never_uses_paid_key() {
        let seen = Seen::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .fallback(any(upstream))
            .with_state(seen.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let credentials = json!({"baseUrl":base,"jwtToken":"fixture-jwt","deviceMid":"fixture-device","accessToken":"paid-token","apiKey":"paid-key"});
        let client = reqwest::Client::new();
        assert_eq!(
            list_models(credentials.clone(), &client).await.unwrap(),
            super::START_MODELS
        );
        let response = route(
            credentials.clone(),
            "glm-5.3-flash",
            json!({"messages":[{"role":"user","content":"hello"}]}),
            &client,
        )
        .await
        .unwrap();
        let answer: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(answer["choices"][0]["message"]["content"], "accepted");
        let error = route(
            credentials.clone(),
            "GLM-5.3-Flash",
            json!({"messages":[{"role":"user","content":"reject"}]}),
            &client,
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        let quota = route(
            credentials,
            "GLM-5.3-Flash",
            json!({"messages":[{"role":"user","content":"quota"}]}),
            &client,
        )
        .await
        .unwrap_err();
        assert_eq!(quota.status, StatusCode::TOO_MANY_REQUESTS);
        let requests = seen.0.lock().unwrap();
        assert_eq!(requests.len(), 6);
        assert!(requests
            .iter()
            .filter(|(path, _, _)| path != "/api/v1/client/configs" && path != "/config.json")
            .all(|(_, auth, _)| auth == "Bearer fixture-jwt"));
        assert_eq!(requests[3].2["model"], "GLM-5.3-Flash");
        assert_eq!(
            requests[3].2["system"][0]["text"],
            "You are ZCode, an interactive coding agent"
        );
        assert_eq!(requests[3].2["messages"][0]["role"], "user");
        assert!(requests[3].2["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("<system-reminder>"));
        assert_eq!(
            requests[3].2["messages"][1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        task.abort();
    }
}
