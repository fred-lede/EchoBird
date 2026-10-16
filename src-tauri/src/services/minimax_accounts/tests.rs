use super::test_support::{json, jwt, Fixture, MockServer};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn credential(region: Region) -> Credentials {
    Credentials {
        region,
        access_token: jwt("user", "old"),
        refresh_token: "old-refresh".into(),
        expires_at: chrono::Utc::now().timestamp() + 30,
        user_id: "user".into(),
        email: "user@fixture.invalid".into(),
        name: None,
    }
}

#[test]
fn passive_saved_listing_reads_metadata_only_and_preserves_regions_and_unknown_quota() {
    let fixture = Fixture::new();
    let path = fixture.path();
    assert!(list_at(&path).unwrap().is_empty());
    assert!(!path.exists());
    let cn = account(&credential(Region::Cn)).unwrap();
    let global = account(&credential(Region::Global)).unwrap();
    assert_ne!(cn.id, global.id);
    let store = Store::from([
        (
            cn.id.clone(),
            Saved {
                account: cn,
                credential: "not decrypted".into(),
            },
        ),
        (
            global.id.clone(),
            Saved {
                account: global,
                credential: "not decrypted".into(),
            },
        ),
    ]);
    super::super::cursor_auth::write(&path, &store).unwrap();
    let rows = list_at(&path).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|row| row.plan.is_none() && row.quota.is_none()));
}

#[tokio::test]
async fn quota_refresh_does_not_restore_a_stale_active_account_selection() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut first = credential(Region::Cn);
    first.expires_at = chrono::Utc::now().timestamp() + 3600;
    let first = capture_at(&path, &first).unwrap();
    let mut second = credential(Region::Cn);
    second.expires_at = chrono::Utc::now().timestamp() + 3600;
    second.user_id = "second-user".into();
    second.email = "second@fixture.invalid".into();
    let second = capture_at(&path, &second).unwrap();
    mark_active_at(&path, &first.id).unwrap();
    let disk = path.clone();
    let selected = second.id.clone();
    let server = MockServer::start(move |request| {
        let disk = disk.clone();
        let selected = selected.clone();
        async move {
            if request.path.ends_with("get_user_extra_info") {
                json(200, json!({"workspaces":[{"workspace_type":0,"workspace_id":1,"has_token_plan":false}]}))
            } else {
                mark_active_at(&disk, &selected).unwrap();
                json(200, json!({"data":{"has_token_plan":false,"opcredit_balance":120}}))
            }
        }
    }).await;
    let refreshed = refresh_account_at(&path, &first.id, Some((&server.url, &server.url)))
        .await
        .unwrap();
    assert!(!refreshed.active);
    assert_eq!(refreshed.quota.unwrap().credits, Some(120.0));
    let rows = list_at(&path).unwrap();
    assert_eq!(rows.iter().filter(|row| row.active).count(), 1);
    assert_eq!(rows.iter().find(|row| row.active).unwrap().id, second.id);
}

#[tokio::test]
async fn concurrent_expiry_refresh_and_stale_rejection_consume_each_refresh_token_once() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let old = credential(Region::Cn);
    let summary = capture_at(&path, &old).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server=MockServer::start(move |request| { let count=count.clone(); async move {
        assert!(request.body.contains("refresh_token=old-refresh")); count.fetch_add(1,Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        json(200,json!({"access_token":jwt("user","new"),"refresh_token":"new-refresh","expires_in":3600}))
    }}).await;
    let (first, second) = tokio::join!(
        credentials_at(&path, &summary.id, false, None, Some(&server.url)),
        credentials_at(&path, &summary.id, false, None, Some(&server.url))
    );
    assert_eq!(first.unwrap().refresh_token, "new-refresh");
    assert_eq!(second.unwrap().refresh_token, "new-refresh");
    let next = credentials_at(
        &path,
        &summary.id,
        true,
        Some(&old.access_token),
        Some(&server.url),
    )
    .await
    .unwrap();
    assert_eq!(next.access_token, jwt("user", "new"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(!stored.contains("new-refresh"));
    assert!(stored.contains("enc:v1:"));
    assert_eq!(
        current(&path, &summary.id).unwrap().refresh_token,
        "new-refresh"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn rotated_token_stays_usable_if_its_save_fails() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut old = credential(Region::Cn);
    old.expires_at = chrono::Utc::now().timestamp() + 7200;
    let summary = capture_at(&path, &old).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let disk = path.clone();
    let original_permissions = std::fs::metadata(&path).unwrap().permissions();
    let server=MockServer::start(move |_| { let count=count.clone(); let disk=disk.clone(); async move {
        count.fetch_add(1,Ordering::SeqCst);
        let mut permissions=std::fs::metadata(&disk).unwrap().permissions();permissions.set_readonly(true);std::fs::set_permissions(&disk,permissions).unwrap();
        json(200,json!({"access_token":jwt("user","new"),"refresh_token":"new-refresh","expires_in":600}))
    }}).await;
    let result = credentials_at(
        &path,
        &summary.id,
        true,
        Some(&old.access_token),
        Some(&server.url),
    )
    .await;
    std::fs::set_permissions(&path, original_permissions).unwrap();
    assert_eq!(result.unwrap().refresh_token, "new-refresh");
    assert_eq!(
        unprotect(&load(&path).unwrap()[&summary.id])
            .unwrap()
            .refresh_token,
        "old-refresh"
    );
    assert_eq!(
        credentials_at(&path, &summary.id, false, None, Some(&server.url))
            .await
            .unwrap()
            .refresh_token,
        "new-refresh"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let session = native_session_at(&path, &summary.id).await.unwrap();
    let active = mark_active_at(&path, &summary.id).unwrap();
    assert!(active.active);
    assert_eq!(active.expires_at, Some(session.credentials.expires_at));
    assert_eq!(
        unprotect(&load(&path).unwrap()[&summary.id]).unwrap(),
        session.credentials
    );
    assert!(!super::rotated()
        .lock()
        .unwrap()
        .contains_key(&cache_key(&path, &summary.id)));
}

#[cfg(windows)]
#[tokio::test]
async fn successive_unsaved_rotations_remain_bound_to_the_disk_generation() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut old = credential(Region::Cn);
    old.expires_at = chrono::Utc::now().timestamp() + 7200;
    let summary = capture_at(&path, &old).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let disk = path.clone();
    let original = std::fs::metadata(&path).unwrap().permissions();
    let server = MockServer::start(move |request| {
        let count = count.clone();
        let disk = disk.clone();
        async move {
            let round = count.fetch_add(1, Ordering::SeqCst);
            assert!(request.body.contains(if round == 0 {
                "refresh_token=old-refresh"
            } else {
                "refresh_token=first-refresh"
            }));
            let mut permissions = std::fs::metadata(&disk).unwrap().permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(&disk, permissions).unwrap();
            json(
                200,
                json!({"access_token":jwt("user",if round==0 {"first"}else{"second"}),
                "refresh_token":if round==0 {"first-refresh"}else{"second-refresh"},
                "expires_in":if round==0 {600}else{300}}),
            )
        }
    })
    .await;
    let mut rejected = old.access_token.clone();
    for expected in ["first-refresh", "second-refresh"] {
        let result =
            credentials_at(&path, &summary.id, true, Some(&rejected), Some(&server.url)).await;
        std::fs::set_permissions(&path, original.clone()).unwrap();
        let renewed = result.unwrap();
        assert_eq!(renewed.refresh_token, expected);
        rejected = renewed.access_token.clone();
        assert_eq!(current(&path, &summary.id).unwrap(), renewed);
        assert_eq!(unprotect(&load(&path).unwrap()[&summary.id]).unwrap(), old);
    }
    assert_eq!(
        credentials_at(&path, &summary.id, false, None, Some(&server.url))
            .await
            .unwrap()
            .refresh_token,
        "second-refresh"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_fresh_login_replaces_longer_lived_credentials_and_invalidates_old_pending_rotations() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut old = credential(Region::Cn);
    old.expires_at += 7200;
    let summary = capture_at(&path, &old).unwrap();
    let mut rotated = old.clone();
    rotated.access_token = jwt("user", "rotated");
    rotated.refresh_token = "rotated-refresh".into();
    rotated.expires_at += 7200;
    super::rotated().lock().unwrap().insert(
        cache_key(&path, &summary.id),
        PendingRotation {
            previous: old,
            current: rotated,
        },
    );
    let mut login = credential(Region::Cn);
    login.access_token = jwt("user", "login");
    login.refresh_token = "login-refresh".into();
    capture_at(&path, &login).unwrap();
    assert_eq!(current(&path, &summary.id).unwrap(), login);
    assert!(!super::rotated()
        .lock()
        .unwrap()
        .contains_key(&cache_key(&path, &summary.id)));
}

#[tokio::test]
async fn native_session_keeps_the_account_pair_stable_until_the_native_transaction_commits() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut old = credential(Region::Cn);
    old.expires_at += 3600;
    let account = capture_at(&path, &old).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = MockServer::start(move |request| {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            assert!(request.body.contains("refresh_token=old-refresh"));
            json(200,json!({"access_token":jwt("user","new"),"refresh_token":"new-refresh","expires_in":3600}))
        }
    }).await;
    let session = native_session_at(&path, &account.id).await.unwrap();
    let mut refresh = Box::pin(credentials_at(
        &path,
        &account.id,
        true,
        Some(&old.access_token),
        Some(&server.url),
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut refresh)
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let native_path = fixture.0.join("native-auth.json");
    let mut transaction = super::super::native_account_files::Transaction::new();
    transaction
        .replace(
            &native_path,
            &serde_json::to_vec(&session.credentials).unwrap(),
        )
        .unwrap();
    assert_eq!(current(&path, &account.id).unwrap(), session.credentials);
    transaction.commit();
    drop(session);
    let renewed = refresh.await.unwrap();
    assert_eq!(renewed.refresh_token, "new-refresh");
    let native: Credentials = serde_json::from_slice(&std::fs::read(native_path).unwrap()).unwrap();
    assert_eq!(native, old);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refresh_cannot_recreate_an_account_deleted_while_request_was_pending() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let old = credential(Region::Cn);
    let summary = capture_at(&path, &old).unwrap();
    let disk = path.clone();
    let target = summary.id.clone();
    let server=MockServer::start(move |_| {let disk=disk.clone(); let target=target.clone();async move {
        delete_at(&disk,&target).unwrap();
        json(200,json!({"access_token":jwt("user","new"),"refresh_token":"new-refresh","expires_in":3600}))
    }}).await;
    assert_eq!(
        credentials_at(&path, &summary.id, false, None, Some(&server.url))
            .await
            .unwrap_err(),
        "accountError.invalidAccount"
    );
    assert!(list_at(&path).unwrap().is_empty());
}

#[tokio::test]
async fn late_refresh_does_not_overwrite_newer_native_credentials() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let old = credential(Region::Global);
    let summary = capture_at(&path, &old).unwrap();
    let disk = path.clone();
    let incoming = old.clone();
    let server=MockServer::start(move |_| {let disk=disk.clone();let mut incoming=incoming.clone();async move {
        incoming.access_token=jwt("user","native");incoming.refresh_token="native-refresh".into();incoming.expires_at+=7200;
        capture_at(&disk,&incoming).unwrap();
        json(200,json!({"access_token":jwt("user","new"),"refresh_token":"new-refresh","expires_in":3600}))
    }}).await;
    let got = credentials_at(&path, &summary.id, false, None, Some(&server.url))
        .await
        .unwrap();
    assert_eq!(got.refresh_token, "native-refresh");
    assert_eq!(
        current(&path, &summary.id).unwrap().refresh_token,
        "native-refresh"
    );
}
