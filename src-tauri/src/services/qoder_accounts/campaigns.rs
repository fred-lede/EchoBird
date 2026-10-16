use super::{quota::Failure, DailyReward, Edition, RewardStatus};
use reqwest::{Client, RequestBuilder, Url};
use serde_json::Value;

const CAMPAIGNS_PATH: &str = "/sash/api/v1/me/campaigns";

pub(super) struct RequestIdentity {
    pub headers: reqwest::header::HeaderMap,
}

impl RequestIdentity {
    pub(super) async fn load(
        edition: Edition,
        account_uid: &str,
        machine_id: &str,
    ) -> Result<Self, String> {
        Self::from_native(
            crate::services::process_manager::qoder_campaign_identity(edition.tool(), account_uid)
                .await?,
        )
        .and_then(|identity| identity.bind_machine(machine_id))
    }

    fn bind_machine(mut self, machine_id: &str) -> Result<Self, String> {
        if machine_id.trim().is_empty()
            || machine_id.len() > 512
            || !machine_id.bytes().all(|b| (32..=126).contains(&b))
        {
            return Err("accountError.invalidAccount".into());
        }
        self.headers.insert(
            "cosy-machineid",
            machine_id
                .parse()
                .map_err(|_| "accountError.invalidAccount")?,
        );
        Ok(self)
    }

    fn from_native(
        identity: crate::services::process_manager::QoderCampaignIdentity,
    ) -> Result<Self, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in [
            ("cosy-machinetoken", Some(identity.machine_token)),
            ("cosy-machinetype", Some(identity.machine_type)),
            ("cosy-machinecode", Some(identity.machine_code)),
            ("cosy-version", identity.version),
        ] {
            if let Some(value) = value {
                headers.insert(name, value.parse().map_err(|_| "accountError.format")?);
            }
        }
        let platform = match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            other => other,
        };
        let os = format!("{}_{platform}", std::env::consts::ARCH);
        headers.insert(
            "cosy-machineos",
            os.parse().map_err(|_| "accountError.format")?,
        );
        let hostname = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME"));
        if let Ok(hostname) = hostname {
            if let Ok(value) = hostname.parse() {
                headers.insert("cosy-machinehostname", value);
            }
        }
        Ok(Self { headers })
    }
}

pub(super) struct Eligibility {
    pub reward: DailyReward,
    pub campaign_id: Option<String>,
}

pub(super) fn next_reset(now: i64) -> i64 {
    (now - 7_200).div_euclid(86_400) * 86_400 + 93_600
}

fn unavailable(now: i64) -> Eligibility {
    Eligibility {
        reward: DailyReward {
            status: RewardStatus::Unavailable,
            checked_at: now,
            reset_at: next_reset(now),
            verified: true,
        },
        campaign_id: None,
    }
}

fn daily_link(value: &Value, edition: Edition) -> bool {
    let host = match edition {
        Edition::Qoder => "docs.qoder.com",
        Edition::QoderCn => "docs.qoder.cn",
    };
    value["placements"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|placement| {
            ["zh", "en"].iter().any(|locale| {
                placement["content"][locale]["detailUrl"]
                    .as_str()
                    .and_then(|link| Url::parse(link).ok())
                    .is_some_and(|url| {
                        url.scheme() == "https"
                            && url.host_str() == Some(host)
                            && url.port().is_none()
                            && url.username().is_empty()
                            && url.password().is_none()
                            && url.path().ends_with("/events/100credits")
                    })
            })
        })
}

fn daily_campaign(value: &Value, edition: Edition) -> bool {
    daily_link(value, edition)
        && value["actionType"] == "CLAIM_BENEFIT"
        && value["benefit"]["kind"] == "CREDITS"
        && value["benefit"]["amount"].as_f64() == Some(100.0)
        && value["benefit"]["modelScope"]["modelSeries"]["key"] == "ALL_MODELS"
        && value["benefit"]["validity"]["mode"] == "RELATIVE_DAYS"
        && value["benefit"]["validity"]["days"].as_i64() == Some(30)
}

fn parse(value: &Value, edition: Edition, now: i64) -> Result<Eligibility, Failure> {
    if value["showCampaign"] == false {
        return Ok(unavailable(now));
    }
    let campaigns = value
        .as_array()
        .or_else(|| value["campaigns"].as_array())
        .or_else(|| value["data"]["campaigns"].as_array())
        .or_else(|| value["data"].as_array())
        .ok_or(Failure::Format)?;
    let mut selected = None;
    for campaign in campaigns.iter().filter(|row| daily_campaign(row, edition)) {
        let start = campaign["startAt"].as_i64().ok_or(Failure::Format)?;
        let end = campaign["endAt"].as_i64().ok_or(Failure::Format)?;
        if start <= 0 || end <= start {
            return Err(Failure::Format);
        }
        if now < start || now >= end {
            continue;
        }
        let id = campaign["campaignId"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .ok_or(Failure::Format)?;
        if selected.is_some() {
            return Err(Failure::Format);
        }
        let status = match campaign["claimStatus"].as_str() {
            Some("CLAIMABLE") => RewardStatus::Claimable,
            Some("CLAIMED") => RewardStatus::Claimed,
            _ => RewardStatus::Unavailable,
        };
        selected = Some(Eligibility {
            reward: DailyReward {
                status,
                checked_at: now,
                reset_at: end.min(next_reset(now)),
                verified: true,
            },
            campaign_id: (status == RewardStatus::Claimable).then(|| id.to_string()),
        });
    }
    Ok(selected.unwrap_or_else(|| unavailable(now)))
}

async fn response(request: RequestBuilder) -> Result<Value, Failure> {
    let response = request.send().await.map_err(|_| Failure::Network)?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(Failure::Expired);
    }
    if !response.status().is_success() {
        return Err(Failure::Network);
    }
    response.json().await.map_err(|_| Failure::Format)
}

pub(super) async fn fetch(
    client: &Client,
    base: &str,
    token: &str,
    edition: Edition,
    identity: &RequestIdentity,
) -> Result<Eligibility, Failure> {
    let value = response(
        client
            .get(format!("{}{CAMPAIGNS_PATH}", base.trim_end_matches('/')))
            .bearer_auth(token)
            .header("Cosy-ClientType", "10")
            .header("Accept", "application/json")
            .header("User-Agent", "Qoder")
            .headers(identity.headers.clone()),
    )
    .await?;
    parse(&value, edition, chrono::Utc::now().timestamp())
}

fn claim_result(value: &Value) -> Result<(), Failure> {
    let value = value["data"].as_object().map_or(value, |_| &value["data"]);
    if value.is_object() && value["status"] == "CLAIMED" {
        Ok(())
    } else {
        Err(Failure::Format)
    }
}

pub(super) async fn claim(
    client: &Client,
    base: &str,
    token: &str,
    campaign_id: &str,
    identity: &RequestIdentity,
) -> Result<(), Failure> {
    if campaign_id.trim().is_empty() {
        return Err(Failure::Format);
    }
    let mut url = Url::parse(&format!("{}{CAMPAIGNS_PATH}", base.trim_end_matches('/')))
        .map_err(|_| Failure::Format)?;
    url.path_segments_mut()
        .map_err(|_| Failure::Format)?
        .push(campaign_id)
        .push("claim");
    claim_result(
        &response(
            client
                .post(url)
                .bearer_auth(token)
                .header("Cosy-ClientType", "10")
                .header("Accept", "application/json")
                .header("User-Agent", "Qoder")
                .headers(identity.headers.clone())
                .json(&serde_json::json!({})),
        )
        .await?,
    )
}

#[cfg(test)]
pub(super) fn fixture_identity(machine_id: &str) -> RequestIdentity {
    RequestIdentity::from_native(crate::services::process_manager::QoderCampaignIdentity {
        machine_token: "fixture-sdk-token".into(),
        machine_type: "fixture-sdk-type".into(),
        machine_code: "fixture-sdk-code".into(),
        version: Some("0.4.4".into()),
    })
    .and_then(|identity| identity.bind_machine(machine_id))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        extract::Request,
        http::StatusCode,
        response::Response,
        routing::any,
        Router,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn campaign(edition: Edition, now: i64) -> Value {
        let host = match edition {
            Edition::Qoder => "docs.qoder.com",
            Edition::QoderCn => "docs.qoder.cn",
        };
        json!({
            "campaignId":"daily-server-id", "campaignKey":"dynamic-daily-key",
            "actionType":"CLAIM_BENEFIT", "claimStatus":"CLAIMABLE",
            "startAt":now - 60, "endAt":next_reset(now) - 60,
            "benefit":{"kind":"CREDITS","amount":100,
                "modelScope":{"modelSeries":{"key":"ALL_MODELS"}},
                "validity":{"mode":"RELATIVE_DAYS","days":30}},
            "placements":[{"content":{"en":{"detailUrl":format!("https://{host}/en/events/100credits")}}}]
        })
    }

    fn eligibility(value: Value, edition: Edition, now: i64) -> Eligibility {
        parse(&value, edition, now).ok().unwrap()
    }

    #[test]
    fn official_daily_links_and_dynamic_ids_work_in_each_region() {
        let now = 1_791_857_000;
        for edition in [Edition::Qoder, Edition::QoderCn] {
            let mut daily = campaign(edition, now);
            daily["campaignId"] = json!("next-day-server-id");
            daily["campaignKey"] = json!("different-daily-key");
            for env in [
                json!([daily.clone()]),
                json!({"campaigns":[daily.clone()]}),
                json!({"data":{"campaigns":[daily.clone()]}}),
                json!({"data":[daily.clone()]}),
            ] {
                let result = eligibility(env, edition, now);
                assert!(result.reward.status == RewardStatus::Claimable);
                assert_eq!(result.reward.checked_at, now);
                assert_eq!(result.reward.reset_at, next_reset(now) - 60);
                assert_eq!(result.campaign_id.as_deref(), Some("next-day-server-id"));
            }
        }
    }

    #[test]
    fn unrelated_promotions_and_unverified_eligibility_never_supply_a_claim_id() {
        let now = 1_791_857_000;
        let daily = campaign(Edition::QoderCn, now);
        let mut cases = vec![];
        for (path, value) in [
            ("/actionType", json!("VIEW_DETAILS")),
            ("/benefit/kind", json!("REDEMPTION_CODE")),
            ("/benefit/amount", json!(500)),
            ("/benefit/modelScope/modelSeries/key", json!("QWEN_SERIES")),
            ("/benefit/validity/mode", json!("FIXED_END")),
            ("/benefit/validity/days", json!(7)),
            ("/claimStatus", json!("PENDING")),
            ("/startAt", json!(now + 1)),
            ("/endAt", json!(now)),
            (
                "/placements/0/content/en/detailUrl",
                json!("https://docs.qoder.com/events/100credits"),
            ),
            (
                "/placements/0/content/en/detailUrl",
                json!("https://docs.qoder.cn.evil.test/events/100credits"),
            ),
            (
                "/placements/0/content/en/detailUrl",
                json!("http://docs.qoder.cn/events/100credits"),
            ),
            (
                "/placements/0/content/en/detailUrl",
                json!("https://docs.qoder.cn/events/first-subscription"),
            ),
        ] {
            let mut other = daily.clone();
            *other.pointer_mut(path).unwrap() = value;
            cases.push(json!({"campaigns":[other]}));
        }
        cases.extend([
            json!({"campaigns":[]}),
            json!({"showCampaign":false,"campaigns":[daily.clone()]}),
        ]);
        for env in cases {
            let result = eligibility(env, Edition::QoderCn, now);
            assert!(result.reward.status == RewardStatus::Unavailable);
            assert!(result.campaign_id.is_none());
        }
        let mut claimed = daily;
        claimed["claimStatus"] = json!("CLAIMED");
        let result = eligibility(json!({"campaigns":[claimed]}), Edition::QoderCn, now);
        assert!(result.reward.status == RewardStatus::Claimed);
        assert!(result.campaign_id.is_none());
    }

    #[test]
    fn reset_uses_the_server_deadline_or_next_ten_oclock_and_rejects_ambiguity() {
        let boundary = 1_791_856_800;
        assert_eq!(next_reset(boundary - 1), boundary);
        assert_eq!(next_reset(boundary), boundary + 86_400);
        let now = boundary + 60;
        let mut daily = campaign(Edition::QoderCn, now);
        daily["endAt"] = json!(next_reset(now) + 60);
        assert_eq!(
            eligibility(json!({"campaigns":[daily.clone()]}), Edition::QoderCn, now)
                .reward
                .reset_at,
            next_reset(now)
        );
        assert!(matches!(
            parse(
                &json!({"campaigns":[daily.clone(),daily.clone()]}),
                Edition::QoderCn,
                now
            ),
            Err(Failure::Format)
        ));
        for (path, value) in [
            ("/campaignId", json!(" ")),
            ("/startAt", json!("invalid")),
            ("/endAt", json!(0)),
        ] {
            let mut invalid = daily.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(matches!(
                parse(&json!({"campaigns":[invalid]}), Edition::QoderCn, now),
                Err(Failure::Format)
            ));
        }
        assert!(matches!(
            parse(&json!({}), Edition::QoderCn, now),
            Err(Failure::Format)
        ));
    }

    #[test]
    fn only_an_explicit_claimed_receipt_confirms_credits() {
        for value in [
            json!({"status":"CLAIMED"}),
            json!({"data":{"status":"CLAIMED"}}),
        ] {
            assert!(claim_result(&value).is_ok());
        }
        for value in [
            json!({}),
            json!({"code":0}),
            json!({"errorCode":"GRANT_RESULT_UNKNOWN"}),
            json!({"status":"PENDING"}),
            json!({"status":"CLAIMED","data":{}}),
            json!(null),
            json!([]),
        ] {
            assert!(matches!(claim_result(&value), Err(Failure::Format)));
        }
    }

    #[tokio::test]
    async fn wire_requests_preserve_device_auth_encoded_ids_and_never_retry_a_post() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let app = Router::new().fallback(any(move |request: Request| {
            let recorded = recorded.clone();
            async move {
                let (parts, body) = request.into_parts();
                assert_eq!(parts.headers["authorization"], "Bearer fixture-device");
                assert_eq!(parts.headers["cosy-clienttype"], "10");
                assert_eq!(parts.headers["user-agent"], "Qoder");
                assert_eq!(parts.headers["cosy-machinetoken"], "fixture-sdk-token");
                assert_eq!(parts.headers["cosy-machineid"], "fixture-machine");
                assert_eq!(parts.headers["cosy-machinetype"], "fixture-sdk-type");
                assert_eq!(parts.headers["cosy-machinecode"], "fixture-sdk-code");
                assert_eq!(parts.headers["cosy-version"], "0.4.4");
                let body = to_bytes(body, 1024).await.unwrap();
                if parts.method == axum::http::Method::POST {
                    assert_eq!(body.as_ref(), b"{}");
                    assert_eq!(parts.headers["content-type"], "application/json");
                } else {
                    assert!(body.is_empty());
                }
                recorded
                    .lock()
                    .unwrap()
                    .push((parts.method.to_string(), parts.uri.path().to_string()));
                if parts.method == axum::http::Method::GET {
                    let now = chrono::Utc::now().timestamp();
                    let mut daily = campaign(Edition::QoderCn, now);
                    daily["endAt"] = json!(now + 86_400);
                    return axum::Json(json!({"campaigns":[daily]})).into_response();
                }
                let (status, value) = if parts.uri.path().contains("failure") {
                    (StatusCode::SERVICE_UNAVAILABLE, json!({"errorCode":"BUSY"}))
                } else if parts.uri.path().contains("expired") {
                    (StatusCode::UNAUTHORIZED, json!({}))
                } else if parts.uri.path().contains("empty") {
                    return Response::builder()
                        .status(StatusCode::NO_CONTENT)
                        .body(Body::empty())
                        .unwrap();
                } else if parts.uri.path().contains("wrapped") {
                    (StatusCode::OK, json!({"data":{"status":"CLAIMED"}}))
                } else {
                    (StatusCode::OK, json!({"status":"CLAIMED"}))
                };
                (status, axum::Json(value)).into_response()
            }
        }));
        use axum::response::IntoResponse;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client::new();
        let identity = fixture_identity("fixture-machine");
        let eligible = fetch(
            &client,
            &base,
            "fixture-device",
            Edition::QoderCn,
            &identity,
        )
        .await
        .ok()
        .unwrap();
        assert!(eligible.reward.status == RewardStatus::Claimable);
        assert_eq!(eligible.campaign_id.as_deref(), Some("daily-server-id"));
        for id in ["server/id?x#y", "wrapped"] {
            assert!(claim(&client, &base, "fixture-device", id, &identity)
                .await
                .is_ok());
        }
        assert!(matches!(
            claim(&client, &base, "fixture-device", "failure", &identity).await,
            Err(Failure::Network)
        ));
        assert!(matches!(
            claim(&client, &base, "fixture-device", "expired", &identity).await,
            Err(Failure::Expired)
        ));
        assert!(matches!(
            claim(&client, &base, "fixture-device", "empty", &identity).await,
            Err(Failure::Format)
        ));
        assert_eq!(
            requests.lock().unwrap().as_slice(),
            &[
                ("GET".into(), CAMPAIGNS_PATH.into()),
                (
                    "POST".into(),
                    format!("{CAMPAIGNS_PATH}/server%2Fid%3Fx%23y/claim")
                ),
                ("POST".into(), format!("{CAMPAIGNS_PATH}/wrapped/claim")),
                ("POST".into(), format!("{CAMPAIGNS_PATH}/failure/claim")),
                ("POST".into(), format!("{CAMPAIGNS_PATH}/expired/claim")),
                ("POST".into(), format!("{CAMPAIGNS_PATH}/empty/claim")),
            ]
        );
        server.abort();
    }

    #[tokio::test]
    async fn http_failures_and_malformed_receipts_are_classified_without_retries() {
        use axum::response::IntoResponse;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let recorded = count.clone();
        let app = Router::new().fallback(any(move |request: Request| {
            let recorded = recorded.clone();
            async move {
                recorded.fetch_add(1, Ordering::SeqCst);
                let path = request.uri().path();
                if path.contains("unauthorized") {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                if path.contains("forbidden") {
                    return StatusCode::FORBIDDEN.into_response();
                }
                if path.contains("network") {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                if path.contains("malformed") {
                    return (StatusCode::OK, "not-json").into_response();
                }
                axum::Json(json!({"code":0})).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client::new();
        let identity = fixture_identity("fixture-machine");
        for (path, kind) in [
            ("unauthorized", "expired"),
            ("forbidden", "expired"),
            ("network", "network"),
            ("malformed", "format"),
            ("unconfirmed", "format"),
        ] {
            let get = fetch(
                &client,
                &format!("{base}/{path}"),
                "fixture-device",
                Edition::QoderCn,
                &identity,
            )
            .await;
            let post = claim(&client, &base, "fixture-device", path, &identity).await;
            assert!(match kind {
                "expired" =>
                    matches!(get, Err(Failure::Expired)) && matches!(post, Err(Failure::Expired)),
                "network" =>
                    matches!(get, Err(Failure::Network)) && matches!(post, Err(Failure::Network)),
                _ => matches!(get, Err(Failure::Format)) && matches!(post, Err(Failure::Format)),
            });
        }
        assert!(matches!(
            claim(&client, &base, "fixture-device", " ", &identity).await,
            Err(Failure::Format)
        ));
        assert_eq!(count.load(Ordering::SeqCst), 10);
        server.abort();
    }
}
