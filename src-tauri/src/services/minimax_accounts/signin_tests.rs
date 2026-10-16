use super::test_support::{json, Fixture, MockServer};
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn credential(region: Region) -> Credentials {
    Credentials {
        region,
        access_token: "fixture-access".into(),
        refresh_token: "fixture-refresh".into(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        user_id: "fixture-user".into(),
        email: "fixture@example.invalid".into(),
        name: None,
    }
}

fn panel(status: u8) -> Value {
    json!({"scene":2,"days":(1..=7).map(|day| json!({
        "day_no":day,"points":if day==4 || day==7 {1000} else {400},
        "status":if day==4 {status} else {1},"is_today":day==4,
    })).collect::<Vec<_>>()})
}

fn receipt() -> Value {
    json!({"claim_id":"fixture-claim","claim_result":1,"day_no":4,
        "points":1000,"expire_at_ms":1_900_000_000_000i64,"panel":panel(3)})
}

#[tokio::test]
async fn concurrent_claims_send_one_post_and_keep_confirmation_when_quota_fails() {
    for region in [Region::Cn, Region::Global] {
        let fixture = Fixture::new();
        let path = fixture.path();
        let credential = credential(region);
        let account = capture_at(&path, &credential).unwrap();
        let mut store = load(&path).unwrap();
        let saved = store.get_mut(&account.id).unwrap();
        saved.account.plan = Some("M Plan Go".into());
        saved.account.quota = Some(Quota {
            credits: Some(123.0),
            windows: Vec::new(),
            ..Quota::default()
        });
        super::super::cursor_auth::write(&path, &store).unwrap();
        let claimed = Arc::new(AtomicBool::new(false));
        let posts = Arc::new(AtomicUsize::new(0));
        let statuses = Arc::new(AtomicUsize::new(0));
        let quotas = Arc::new(AtomicUsize::new(0));
        let (did_claim, post_count, status_count, quota_count) = (
            claimed.clone(),
            posts.clone(),
            statuses.clone(),
            quotas.clone(),
        );
        let disk = path.clone();
        let target = account.id.clone();
        let server = MockServer::start(move |request| {
            let (claimed, posts, statuses, quotas) = (
                did_claim.clone(),
                post_count.clone(),
                status_count.clone(),
                quota_count.clone(),
            );
            let disk = disk.clone();
            let target = target.clone();
            async move {
                if request.path.ends_with("/status") {
                    statuses.fetch_add(1, Ordering::SeqCst);
                    json(
                        200,
                        json!({"base_resp":{"status_code":0},"data":panel(
                            if claimed.load(Ordering::SeqCst) {3} else {2}
                        )}),
                    )
                } else if request.path.ends_with("/claim") {
                    posts.fetch_add(1, Ordering::SeqCst);
                    assert!(!claimed.swap(true, Ordering::SeqCst));
                    json(200, json!({"base_resp":{"status_code":0},"data":receipt()}))
                } else {
                    assert!(
                        request.path.ends_with("get_user_extra_info")
                            || request.path.ends_with("get_membership_info")
                    );
                    quotas.fetch_add(1, Ordering::SeqCst);
                    let saved = load(&disk).unwrap().remove(&target).unwrap().account;
                    assert_eq!(saved.daily_reward.unwrap().status, RewardStatus::Claimed);
                    assert_eq!(saved.quota.unwrap().credits, Some(123.0));
                    json(500, json!({}))
                }
            }
        })
        .await;
        let (first, second) = tokio::join!(
            claim_daily_at(&path, &account.id, Some((&server.url, &server.url))),
            claim_daily_at(&path, &account.id, Some((&server.url, &server.url))),
        );
        for account in [first.unwrap(), second.unwrap()] {
            assert_eq!(account.daily_reward.unwrap().status, RewardStatus::Claimed);
            assert_eq!(account.plan.as_deref(), Some("M Plan Go"));
            assert_eq!(account.quota.unwrap().credits, Some(123.0));
        }
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        assert_eq!(statuses.load(Ordering::SeqCst), 2);
        assert_eq!(quotas.load(Ordering::SeqCst), 2);
        assert_eq!(current(&path, &account.id).unwrap(), credential);
        let disk = std::fs::read_to_string(&path).unwrap();
        assert!(!disk.contains("fixture-refresh"));
        assert!(!disk.contains("fixture-access"));
    }
}

#[tokio::test]
async fn incomplete_receipt_is_not_replayed_and_next_status_reconciles_the_claim() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let account = capture_at(&path, &credential(Region::Cn)).unwrap();
    let claimed = Arc::new(AtomicBool::new(false));
    let posts = Arc::new(AtomicUsize::new(0));
    let (did_claim, post_count) = (claimed.clone(), posts.clone());
    let server = MockServer::start(move |request| {
        let claimed = did_claim.clone();
        let posts = post_count.clone();
        async move {
            if request.path.ends_with("/status") {
                json(
                    200,
                    json!({"base_resp":{"status_code":0},"data":panel(
                        if claimed.load(Ordering::SeqCst) {3} else {2}
                    )}),
                )
            } else {
                assert!(request.path.ends_with("/claim"));
                posts.fetch_add(1, Ordering::SeqCst);
                claimed.store(true, Ordering::SeqCst);
                let mut data = receipt();
                data.as_object_mut().unwrap().remove("claim_id");
                json(200, json!({"base_resp":{"status_code":0},"data":data}))
            }
        }
    })
    .await;
    assert_eq!(
        claim_daily_at(&path, &account.id, Some((&server.url, &server.url)))
            .await
            .unwrap_err(),
        "accountError.format"
    );
    let saved = list_at(&path).unwrap().remove(0);
    assert_eq!(saved.daily_reward.unwrap().status, RewardStatus::Claimable);
    assert!(saved.quota.is_none());
    let reconciled = claim_daily_at(&path, &account.id, Some((&server.url, &server.url)))
        .await
        .unwrap();
    assert_eq!(
        reconciled.daily_reward.unwrap().status,
        RewardStatus::Claimed
    );
    assert_eq!(posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn late_status_cannot_overwrite_reauthenticated_or_deleted_accounts() {
    for delete in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.path();
        let mut replacement = credential(Region::Global);
        let account = capture_at(&path, &replacement).unwrap();
        replacement.access_token = "replacement-access".into();
        replacement.refresh_token = "replacement-refresh".into();
        replacement.expires_at += 600;
        let disk = path.clone();
        let target = account.id.clone();
        let incoming = replacement.clone();
        let server = MockServer::start(move |request| {
            let disk = disk.clone();
            let target = target.clone();
            let incoming = incoming.clone();
            async move {
                assert!(request.path.ends_with("/status"));
                let _guard = STORE_LOCK.lock().await;
                if delete {
                    delete_at(&disk, &target).unwrap();
                } else {
                    capture_at(&disk, &incoming).unwrap();
                }
                json(200, json!({"base_resp":{"status_code":0},"data":panel(2)}))
            }
        })
        .await;
        assert_eq!(
            claim_daily_at(&path, &account.id, Some((&server.url, &server.url)))
                .await
                .unwrap_err(),
            "accountError.invalidAccount"
        );
        if delete {
            assert!(list_at(&path).unwrap().is_empty());
        } else {
            assert_eq!(current(&path, &account.id).unwrap(), replacement);
            assert!(list_at(&path).unwrap()[0].daily_reward.is_none());
        }
    }
}

#[tokio::test]
async fn confirmed_receipt_keeps_rotated_tokens_and_cannot_resurrect_deleted_accounts() {
    for delete in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.path();
        let mut replacement = credential(Region::Cn);
        let account = capture_at(&path, &replacement).unwrap();
        replacement.access_token = "replacement-access".into();
        replacement.refresh_token = "replacement-refresh".into();
        replacement.expires_at += 600;
        let disk = path.clone();
        let target = account.id.clone();
        let incoming = replacement.clone();
        let server = MockServer::start(move |request| {
            let disk = disk.clone();
            let target = target.clone();
            let incoming = incoming.clone();
            async move {
                if request.path.ends_with("/status") {
                    return json(200, json!({"base_resp":{"status_code":0},"data":panel(2)}));
                }
                if !request.path.ends_with("/claim") {
                    assert!(
                        request.path.ends_with("get_user_extra_info")
                            || request.path.ends_with("get_membership_info")
                    );
                    return json(500, json!({}));
                }
                let _guard = STORE_LOCK.lock().await;
                if delete {
                    delete_at(&disk, &target).unwrap();
                } else {
                    capture_at(&disk, &incoming).unwrap();
                }
                json(200, json!({"base_resp":{"status_code":0},"data":receipt()}))
            }
        })
        .await;
        let result = claim_daily_at(&path, &account.id, Some((&server.url, &server.url))).await;
        if delete {
            assert_eq!(result.unwrap_err(), "accountError.invalidAccount");
            assert!(list_at(&path).unwrap().is_empty());
        } else {
            let confirmed = result.unwrap();
            assert_eq!(
                confirmed.daily_reward.unwrap().status,
                RewardStatus::Claimed
            );
            assert_eq!(confirmed.expires_at, Some(replacement.expires_at));
            assert_eq!(current(&path, &account.id).unwrap(), replacement);
            assert_eq!(
                unprotect(&load(&path).unwrap()[&account.id]).unwrap(),
                replacement
            );
            assert_eq!(
                list_at(&path).unwrap()[0]
                    .daily_reward
                    .as_ref()
                    .unwrap()
                    .status,
                RewardStatus::Claimed
            );
        }
    }
}

#[tokio::test]
async fn missing_corrupt_or_region_mismatched_accounts_fail_before_status_requests() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let server = MockServer::start(move |_| {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            json(500, json!({}))
        }
    })
    .await;
    assert_eq!(
        claim_daily_at(&path, "missing", Some((&server.url, &server.url)))
            .await
            .unwrap_err(),
        "accountError.invalidAccount"
    );
    let account = capture_at(&path, &credential(Region::Cn)).unwrap();
    let original = load(&path).unwrap();
    for (corrupt, expected) in [
        (true, "accountError.keychain"),
        (false, "accountError.invalidAccount"),
    ] {
        let mut store = original.clone();
        let saved = store.get_mut(&account.id).unwrap();
        if corrupt {
            saved.credential = "invalid ciphertext".into();
        } else {
            saved.account.region = Region::Global;
        }
        super::super::cursor_auth::write(&path, &store).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            claim_daily_at(&path, &account.id, Some((&server.url, &server.url)))
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    assert_eq!(requests.load(Ordering::SeqCst), 0);
}
