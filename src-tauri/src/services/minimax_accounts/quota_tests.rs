use super::*;
use crate::services::minimax_accounts::{
    account,
    test_support::{json, jwt, MockServer},
    Region,
};

fn credential() -> Credentials {
    Credentials {
        region: Region::Global,
        access_token: jwt("user", "old"),
        refresh_token: "refresh".into(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        user_id: "profile".into(),
        email: "a@fixture.invalid".into(),
        name: None,
    }
}

#[test]
fn membership_tiers_use_server_names_and_never_guess_from_combo_ids() {
    for (tier, expected) in [
        (json!("Go"), "M Plan Go"),
        (json!("Explore"), "M Plan Explore"),
        (json!("Build"), "M Plan Build"),
        (json!("123"), "M Plan"),
        (json!(123), "M Plan"),
    ] {
        let mut summary = account(&credential()).unwrap();
        membership(
            &json!({"has_token_plan":true,"token_plan_tier":tier}),
            &mut summary,
        );
        assert_eq!(summary.plan.as_deref(), Some(expected));
    }
    let mut summary = account(&credential()).unwrap();
    membership(&json!({"has_token_plan":false}), &mut summary);
    assert_eq!(summary.plan.as_deref(), Some("Free"));
}

#[tokio::test]
async fn explicit_membership_query_reports_credits_plan_windows_and_signature() {
    let server=MockServer::start(|request| async move {
        assert_eq!(request.headers["authorization"],format!("Bearer {}",jwt("user","old")));
        if request.path.contains("coding_plan") {
            assert_eq!(request.headers["x-group-id"],"group");
            json(200,json!({"base_resp":{"status_code":0},"model_remains":[{"model_name":"general","current_interval_status":1,"current_interval_remaining_percent":75,"start_time":1_800_000_000,"end_time":1_800_018_000,"current_weekly_status":1,"current_weekly_remaining_percent":50,"weekly_start_time":1_800_000_000,"weekly_end_time":1_800_604_800}]}))
        } else {
            assert!(request.headers.contains_key("yy"));assert!(request.headers.contains_key("x-signature"));
            assert!(request.query.contains("user_id=profile"));
            if request.path.ends_with("get_user_extra_info") { json(200,json!({"workspaces":[{"workspace_type":0,"workspace_id":7,"has_token_plan":true,"op_group_id":"group"}]})) }
            else { assert_eq!(serde_json::from_str::<Value>(&request.body).unwrap()["workspace_id"],7);json(200,json!({"data":{"has_token_plan":true,"token_plan_tier":"Build","token_plan_expires_at":1_900_000_000,"opcredit_balance":"123.50"}})) }
        }
    }).await;
    let mut account = account(&credential()).unwrap();
    refresh_at(
        &super::super::oauth::client().unwrap(),
        &server.url,
        &server.url,
        &server.url,
        &credential(),
        &mut account,
    )
    .await
    .unwrap();
    assert_eq!(account.plan.as_deref(), Some("M Plan Build"));
    assert_eq!(account.quota.as_ref().unwrap().credits, Some(123.5));
    let windows = &account.quota.unwrap().windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].used_percent, 25.0);
    assert_eq!(windows[1].span_seconds, Some(604800));
}

#[test]
fn unknown_membership_is_not_a_free_plan_or_zero_balance_and_unavailable_windows_are_omitted() {
    let mut summary = account(&credential()).unwrap();
    membership(&json!({}), &mut summary);
    assert!(summary.plan.is_none());
    assert!(summary.quota.is_none());
    assert!(windows(&json!({"model_remains":[{"model_name":"general","current_interval_status":3,"current_interval_remaining_percent":100},{"model_name":"missing"}]})).is_empty());
    let disabled =
        windows(&json!({"model_remains":[{"model_name":"general","current_interval_status":2}]}));
    assert_eq!(disabled[0].used_percent, 100.0);
}

#[test]
fn credit_summary_keeps_total_separate_from_paid_and_granted_values_and_preserves_zero() {
    let mut summary = account(&credential()).unwrap();
    membership(
        &json!({"data":{"op_credit_summary":{
            "total_remaining_amount":"1,250.5","purchased_remaining_amount":"250.5","free_remaining_amount":"1,000"
        }}}),
        &mut summary,
    );
    let quota = summary.quota.unwrap();
    assert_eq!(quota.credits, Some(1250.5));
    assert_eq!(quota.purchased_credits, Some(250.5));
    assert_eq!(quota.granted_credits, Some(1000.0));
    let mut summary = account(&credential()).unwrap();
    membership(
        &json!({"op_credit_summary":{
            "total_remaining_amount":3,"purchased_credit":3,"free_credit":0
        }}),
        &mut summary,
    );
    assert_eq!(summary.quota.as_ref().unwrap().granted_credits, Some(0.0));
    membership(
        &json!({"op_credit_summary":{
            "total_remaining_amount":5,"purchased_remaining_amount":null,"free_remaining_amount":null,
            "purchased_credit":2,"free_credit":3
        }}),
        &mut summary,
    );
    let quota = summary.quota.as_ref().unwrap();
    assert_eq!(quota.purchased_credits, Some(2.0));
    assert_eq!(quota.granted_credits, Some(3.0));
    membership(
        &json!({"op_credit_summary":{
            "total_remaining_amount":3,"purchased_credit":3,"free_credit":"unknown"
        }}),
        &mut summary,
    );
    assert_eq!(summary.quota.unwrap().granted_credits, None);
    let old: Quota = serde_json::from_value(json!({"credits":23,"windows":[]})).unwrap();
    assert_eq!(old.credits, Some(23.0));
    assert!(old.purchased_credits.is_none() && old.granted_credits.is_none());
    assert!(old.purchased_total.is_none() && old.granted_total.is_none());
}

fn credit_row(kind: u8, remaining: f64, expires_at: i64) -> Value {
    json!({"credit_type":kind,"granted_amount":remaining,"remaining_amount":remaining,
        "consumed_amount":0,"granted_at_ms":1_700_000_000_000i64,"expire_at_ms":expires_at*1000})
}

#[test]
fn only_complete_matching_balances_with_one_actual_expiry_produce_a_category_date() {
    let expiry = chrono::Utc::now().timestamp() + 3600;
    let details = vec![
        detail(&credit_row(2, 0.1, expiry)).unwrap(),
        detail(&credit_row(2, 0.2, expiry)).unwrap(),
    ];
    assert_eq!(category_expiry(&details, 2, Some(0.3)), Some(expiry));
    assert_eq!(category_expiry(&details, 2, Some(0.4)), None);
    assert_eq!(category_expiry(&details, 2, Some(0.0)), None);
    let mixed = vec![
        detail(&credit_row(2, 1.0, expiry)).unwrap(),
        detail(&credit_row(2, 2.0, expiry + 3600)).unwrap(),
    ];
    assert_eq!(category_expiry(&mixed, 2, Some(3.0)), None);
    let mut unknown = credit_row(2, 3.0, expiry);
    unknown["expire_at_ms"] = json!(0);
    assert_eq!(
        category_expiry(&[detail(&unknown).unwrap()], 2, Some(3.0)),
        None
    );
    unknown["expire_at_ms"] = json!(expiry * 1000);
    unknown["granted_at_ms"] = json!("invalid");
    assert_eq!(
        category_expiry(&[detail(&unknown).unwrap()], 2, Some(3.0)),
        None
    );
    assert_eq!(
        category_expiry(
            &[detail(&credit_row(2, 3.0, 1_700_000_001)).unwrap()],
            2,
            Some(3.0)
        ),
        None
    );
}

#[tokio::test]
async fn explicit_refresh_reads_all_active_credit_pages_and_uses_each_category_date() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let expiry = chrono::Utc::now().timestamp() + 3600;
    let pages = Arc::new(AtomicUsize::new(0));
    let seen = pages.clone();
    let server=MockServer::start(move|request| {
        let seen=seen.clone();
        async move {
            if request.path.ends_with("get_user_extra_info") {return json(200,json!({"workspaces":[{"workspace_type":0,"workspace_id":7}]}));}
            if request.path.ends_with("get_membership_info") {return json(200,json!({"has_token_plan":false,"op_credit_summary":{"total_remaining_amount":50,"purchased_remaining_amount":10,"free_remaining_amount":40}}));}
            assert_eq!(request.path,"/minimax-cloud/api/v1/credit/details");
            assert!(request.body.is_empty());
            let query:std::collections::BTreeMap<_,_>=url::form_urlencoded::parse(request.query.as_bytes()).into_owned().collect();
            assert_eq!(query["page_size"],"20");assert_eq!(query["include_expired"],"false");assert_eq!(query["is_desktop"],"1");
            seen.fetch_add(1,Ordering::SeqCst);
            match query["page_number"].as_str(){
                "1"=>json(200,json!({"total_count":2,"details":[credit_row(1,10.0,expiry)]})),
                "2"=>json(200,json!({"total_count":2,"details":[credit_row(2,40.0,expiry+3600)]})),
                _=>panic!("Unexpected credit details page"),
            }
        }
    }).await;
    let mut summary = account(&credential()).unwrap();
    refresh_at(
        &super::super::oauth::client().unwrap(),
        &server.url,
        &server.url,
        &server.url,
        &credential(),
        &mut summary,
    )
    .await
    .unwrap();
    let quota = summary.quota.unwrap();
    assert_eq!(quota.credits, Some(50.0));
    assert_eq!(quota.purchased_credits, Some(10.0));
    assert_eq!(quota.granted_credits, Some(40.0));
    assert_eq!(quota.purchased_total, Some(10.0));
    assert_eq!(quota.granted_total, Some(40.0));
    assert_eq!(quota.purchased_expires_at, Some(expiry));
    assert_eq!(quota.granted_expires_at, Some(expiry + 3600));
    assert_eq!(pages.load(Ordering::SeqCst), 2);
    assert!(quota.windows.is_empty());
}

#[tokio::test]
async fn category_totals_use_unexpired_grants_and_invalidate_stale_denominators() {
    let expiry = chrono::Utc::now().timestamp() + 3600;
    let mut exhausted = credit_row(1, 0.0, expiry);
    exhausted["granted_amount"] = json!(100);
    exhausted["consumed_amount"] = json!(100);
    let mut available = credit_row(2, 20.0, expiry);
    available["granted_amount"] = json!(40);
    available["consumed_amount"] = json!(20);
    let mut expired = credit_row(2, 500.0, 1_700_000_001);
    expired["granted_amount"] = json!(900);
    expired["consumed_amount"] = json!(400);
    let mut inconsistent = available.clone();
    inconsistent["remaining_amount"] = json!(30);
    let mut bad_amount = available.clone();
    bad_amount["granted_amount"] = json!(10);
    let mut unknown_date = available.clone();
    unknown_date["expire_at_ms"] = Value::Null;
    for (details, balance, purchased, granted) in [
        (
            json!({"total_count":3,"details":[exhausted,available,expired]}),
            20,
            Some(100.0),
            Some(40.0),
        ),
        (json!({"total_count":0}), 0, Some(0.0), Some(0.0)),
        (json!({"total_count":1}), 20, None, None),
        (
            json!({"total_count":2,"details":[inconsistent.clone()]}),
            20,
            None,
            None,
        ),
        (
            json!({"total_count":1,"details":[inconsistent]}),
            20,
            Some(0.0),
            None,
        ),
        (
            json!({"total_count":1,"details":[bad_amount]}),
            20,
            Some(0.0),
            None,
        ),
        (
            json!({"total_count":1,"details":[unknown_date]}),
            20,
            Some(0.0),
            None,
        ),
    ] {
        let server = MockServer::start(move |request| {
            let details = details.clone();
            async move {
                match request.path.as_str() {
                    "/matrix/api/v1/user/get_user_extra_info" => json(200, json!({"workspaces":[{"workspace_type":0,"workspace_id":7}]})),
                    "/matrix/api/v1/commerce/get_membership_info" => json(200, json!({"has_token_plan":false,
                        "op_credit_summary":{"total_remaining_amount":balance,"purchased_remaining_amount":0,"free_remaining_amount":balance}})),
                    "/minimax-cloud/api/v1/credit/details" => json(200, details),
                    _ => panic!("Unexpected quota query"),
                }
            }
        }).await;
        let mut summary = account(&credential()).unwrap();
        summary.quota = Some(Quota {
            purchased_total: Some(700.0),
            granted_total: Some(800.0),
            ..Quota::default()
        });
        refresh_at(
            &super::super::oauth::client().unwrap(),
            &server.url,
            &server.url,
            &server.url,
            &credential(),
            &mut summary,
        )
        .await
        .unwrap();
        let quota = summary.quota.unwrap();
        assert_eq!(quota.credits, Some(f64::from(balance)));
        assert_eq!(quota.purchased_credits, Some(0.0));
        assert_eq!(quota.granted_credits, Some(f64::from(balance)));
        assert_eq!(quota.purchased_total, purchased);
        assert_eq!(quota.granted_total, granted);
    }
}

#[tokio::test]
async fn confirmed_free_membership_removes_cached_plan_windows_and_subscription_date() {
    let server = MockServer::start(|request| async move {
        match request.path.as_str() {
            "/matrix/api/v1/user/get_user_extra_info" => json(200, json!({"workspaces":[{"workspace_type":0,"workspace_id":7}]})),
            "/matrix/api/v1/commerce/get_membership_info" => json(200, json!({
                "has_token_plan":false,"token_plan_expires_at":1_900_000_000,
                "op_credit_summary":{"total_remaining_amount":30,"purchased_remaining_amount":10,"free_remaining_amount":20}
            })),
            "/minimax-cloud/api/v1/credit/details" => json(200, json!({"total_count":0,"details":[]})),
            _ => panic!("Free membership must not request plan remains"),
        }
    }).await;
    let mut summary = account(&credential()).unwrap();
    summary.plan = Some("M Plan Go".into());
    summary.subscription_end_at = Some(1_900_000_000);
    summary.quota = Some(Quota {
        credits: Some(8.0),
        windows: vec![QuotaWindow {
            name: "5 hours".into(),
            used_percent: 20.0,
            resets_at: Some(1_900_000_000),
            span_seconds: Some(18_000),
        }],
        ..Quota::default()
    });
    refresh_at(
        &super::super::oauth::client().unwrap(),
        &server.url,
        &server.url,
        &server.url,
        &credential(),
        &mut summary,
    )
    .await
    .unwrap();
    assert_eq!(summary.plan.as_deref(), Some("Free"));
    assert!(summary.subscription_end_at.is_none());
    let quota = summary.quota.unwrap();
    assert!(quota.windows.is_empty());
    assert_eq!(quota.credits, Some(30.0));
    assert_eq!(quota.purchased_credits, Some(10.0));
    assert_eq!(quota.granted_credits, Some(20.0));
    assert!(quota.purchased_expires_at.is_none() && quota.granted_expires_at.is_none());
}

#[tokio::test]
async fn only_an_empty_wallet_accepts_missing_or_null_credit_details() {
    for (body, valid) in [
        (json!({"total_count":0}), true),
        (json!({"total_count":0,"details":null}), true),
        (json!({"total_count":1}), false),
        (json!({"total_count":1,"details":null}), false),
        (json!({"total_count":0,"details":{}}), false),
        (json!({"total_count":0,"details":"unknown"}), false),
        (
            json!({"total_count":0,"details":[credit_row(2,1.0,1_900_000_000)]}),
            false,
        ),
    ] {
        let server = MockServer::start(move |_| {
            let body = body.clone();
            async move { json(200, body) }
        })
        .await;
        let result = credit_details_at(
            &super::super::oauth::client().unwrap(),
            &server.url,
            &credential(),
        )
        .await;
        assert_eq!(result.is_ok(), valid);
        if valid {
            let details = result.unwrap();
            assert_eq!(details.pages, 1);
            assert_eq!(details.total_count, 0);
            assert!(details.items.is_empty());
            assert_eq!(category_expiry(&details.items, 2, Some(100.0)), None);
        }
    }
}

#[tokio::test]
async fn details_failures_omit_dates_without_losing_classification_or_hiding_primary_errors() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    for case in [
        "duplicate",
        "empty",
        "zero-missing",
        "zero-null",
        "invalid",
        "http",
    ] {
        let expiry = chrono::Utc::now().timestamp() + 3600;
        let pages = Arc::new(AtomicUsize::new(0));
        let seen = pages.clone();
        let server=MockServer::start(move|request|{let seen=seen.clone();async move{
            if request.path.ends_with("get_user_extra_info"){return json(200,json!({"workspaces":[{"workspace_type":0,"workspace_id":7}]}));}
            if request.path.ends_with("get_membership_info"){return json(200,json!({"has_token_plan":false,"op_credit_summary":{"total_remaining_amount":2,"purchased_remaining_amount":1,"free_remaining_amount":1}}));}
            seen.fetch_add(1,Ordering::SeqCst);
            match case {
                "duplicate"=>json(200,json!({"total_count":2,"details":[credit_row(2,1.0,expiry)]})),
                "empty"=>json(200,json!({"total_count":1,"details":[]})),
                "zero-missing"=>json(200,json!({"total_count":0})),
                "zero-null"=>json(200,json!({"total_count":0,"details":null})),
                "invalid"=>json(200,json!({"total_count":1,"details":[{"credit_type":7}]})),
                _=>json(500,json!({})),
            }
        }}).await;
        let mut summary = account(&credential()).unwrap();
        summary.quota = Some(Quota {
            credits: Some(80.0),
            purchased_credits: Some(40.0),
            granted_credits: Some(40.0),
            purchased_total: Some(80.0),
            granted_total: Some(80.0),
            purchased_expires_at: Some(expiry),
            granted_expires_at: Some(expiry),
            ..Quota::default()
        });
        refresh_at(
            &super::super::oauth::client().unwrap(),
            &server.url,
            &server.url,
            &server.url,
            &credential(),
            &mut summary,
        )
        .await
        .unwrap();
        let quota = summary.quota.unwrap();
        assert_eq!(quota.credits, Some(2.0));
        assert_eq!(quota.purchased_credits, Some(1.0));
        assert_eq!(quota.granted_credits, Some(1.0));
        assert!(quota.purchased_total.is_none() && quota.granted_total.is_none());
        assert!(quota.purchased_expires_at.is_none() && quota.granted_expires_at.is_none());
        assert_eq!(
            pages.load(Ordering::SeqCst),
            if case == "duplicate" { 2 } else { 1 }
        );
    }
    let server = MockServer::start(|_| async { json(500, json!({})) }).await;
    let mut summary = account(&credential()).unwrap();
    assert!(refresh_at(
        &super::super::oauth::client().unwrap(),
        &server.url,
        &server.url,
        &server.url,
        &credential(),
        &mut summary
    )
    .await
    .is_err());
}

#[tokio::test]
#[ignore = "Read-only credit metadata probe using an explicitly selected saved account"]
async fn saved_minimax_credit_breakdown_probe() -> Result<(), String> {
    let account_id = std::env::var("ECHOBIRD_MINIMAX_CREDIT_PROBE_ACCOUNT_ID")
        .map_err(|_| "Select a saved account explicitly")?;
    let saved = super::super::load(&super::super::store_path()?)?
        .remove(&account_id)
        .ok_or("Selected saved account not found")?;
    let raw = crate::services::model_manager::decrypt_key_for_use(&saved.credential);
    let credential: Credentials =
        serde_json::from_str(&raw).map_err(|_| "Stored credential decryption failed")?;
    if super::super::id(&credential)? != saved.account.id
        || credential.region != saved.account.region
    {
        return Err("Selected account identity mismatch".into());
    }
    let client = super::super::oauth::client()?;
    let mut summary = account(&credential)?;
    let extra = call(
        &client,
        credential.region.agent_host(),
        "/matrix/api/v1/user/get_user_extra_info",
        &credential,
        Some(json!({})),
    )
    .await?;
    let workspace = extra
        .get("workspaces")
        .or_else(|| extra["data"].get("workspaces"))
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| number(&row["workspace_type"]) == Some(0.0))
        });
    if let Some(workspace) = workspace {
        membership(workspace, &mut summary);
    }
    let body = workspace
        .and_then(|row| row.get("workspace_id"))
        .map(|id| json!({"workspace_id":id}))
        .unwrap_or(json!({}));
    let info = call(
        &client,
        credential.region.agent_host(),
        "/matrix/api/v1/commerce/get_membership_info",
        &credential,
        Some(body),
    )
    .await?;
    membership(&info, &mut summary);
    println!(
        "{}",
        json!({"summary":summary.quota.as_ref().map(|quota|json!({"total_remaining_amount":quota.credits,"purchased_remaining_amount":quota.purchased_credits,"free_remaining_amount":quota.granted_credits}))})
    );
    fn value_type(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }
    fn summary_metadata(value: &Value) -> Value {
        let data = value.get("data").unwrap_or(value);
        let field = |key: &str| value.get(key).or_else(|| data.get(key));
        let credit = field("op_credit_summary").unwrap_or(&Value::Null);
        let fields: serde_json::Map<String, Value> = [
            "total_remaining_amount",
            "purchased_remaining_amount",
            "purchased_credit",
            "free_remaining_amount",
            "free_credit",
        ]
        .into_iter()
        .map(|key| {
            let value = &credit[key];
            (
                key.into(),
                json!({"type":value_type(value),"number":credit_number(value)}),
            )
        })
        .collect();
        let has_plan = field("has_token_plan").unwrap_or(&Value::Null);
        json!({
            "has_token_plan":{"type":value_type(has_plan),"value":has_plan.as_bool()},
            "op_credit_summary":{"type":value_type(credit),"fields":fields},
        })
    }
    println!(
        "{}",
        json!({"legacy_host_summary_metadata":summary_metadata(&info)})
    );
    match call(
        &client,
        credential.region.llm_host(),
        "/matrix/api/v1/user/get_user_extra_info",
        &credential,
        Some(json!({})),
    )
    .await
    {
        Ok(extra) => {
            let workspace = extra
                .get("workspaces")
                .or_else(|| extra["data"].get("workspaces"))
                .and_then(Value::as_array)
                .and_then(|rows| {
                    rows.iter()
                        .find(|row| number(&row["workspace_type"]) == Some(0.0))
                });
            let body = workspace
                .and_then(|row| row.get("workspace_id"))
                .map(|id| json!({"workspace_id":id}))
                .unwrap_or(json!({}));
            match call(
                &client,
                credential.region.llm_host(),
                "/matrix/api/v1/commerce/get_membership_info",
                &credential,
                Some(body),
            )
            .await
            {
                Ok(info) => println!(
                    "{}",
                    json!({"desktop_host_summary_metadata":{
                        "workspace":workspace.map(summary_metadata),
                        "membership":summary_metadata(&info),
                    }})
                ),
                Err(error) => println!("{}", json!({"desktop_host_summary_error":error})),
            }
        }
        Err(error) => println!("{}", json!({"desktop_host_summary_error":error})),
    }
    fn details_metadata(value: &Value) -> Value {
        let fields: serde_json::Map<String, Value> = value
            .as_object()
            .into_iter()
            .flatten()
            .map(|(key, value)| (key.clone(), json!(value_type(value))))
            .collect();
        let first: serde_json::Map<String, Value> = [
            "credit_type",
            "granted_amount",
            "remaining_amount",
            "consumed_amount",
            "granted_at_ms",
            "expire_at_ms",
        ]
        .into_iter()
        .filter_map(|key| {
            let field = value["details"].as_array()?.first()?.get(key)?;
            Some((
                key.into(),
                json!({"type":value_type(field),"number":credit_number(field)}),
            ))
        })
        .collect();
        json!({
            "type":value_type(value), "field_types":fields,
            "details_type":value_type(&value["details"]),
            "details_count":value["details"].as_array().map(Vec::len),
            "total_count_type":value_type(&value["total_count"]),
            "total_count":value["total_count"].as_u64(),
            "first_detail":first,
        })
    }
    let first_page = signed_desktop(
        &client,
        credential.region.llm_host(),
        "/minimax-cloud/api/v1/credit/details?page_number=1&page_size=20&include_expired=false",
        &credential,
        None,
        env!("CARGO_PKG_VERSION"),
    )?
    .send()
    .await
    .map_err(|_| "Credit metadata query failed")?;
    let status = first_page.status().as_u16();
    let first_page: Value = first_page
        .json()
        .await
        .map_err(|_| "Credit metadata response is not JSON")?;
    println!(
        "{}",
        json!({"credit_details_first_page_metadata":{
            "http_status":status,
            "top":details_metadata(&first_page),
            "data":details_metadata(&first_page["data"]),
        }})
    );
    let details = credit_details_at(&client, credential.region.llm_host(), &credential).await?;
    println!(
        "{}",
        json!({"category_totals":{
            "purchased_total":category_total(&details.items,1,summary.quota.as_ref().and_then(|quota|quota.purchased_credits)),
            "granted_total":category_total(&details.items,2,summary.quota.as_ref().and_then(|quota|quota.granted_credits)),
        }})
    );
    println!(
        "{}",
        serde_json::to_value(details).map_err(|_| "Credit metadata serialization failed")?
    );
    Ok(())
}
