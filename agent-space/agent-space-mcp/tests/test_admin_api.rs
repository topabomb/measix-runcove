use std::sync::Arc;

use agent_engine::engine::mock::{
    MockEngine, TEST_FAIL_WORKSPACE_STOP, TEST_NEXT_STATUS, TEST_STOPPED_WORKSPACES,
};
use agent_engine::engine::{AgentEngine, WorkspaceOpts};
use agent_engine::session::SessionManager;
use agent_engine::state::StateStore;
use agent_engine::workspace_service::WorkspaceService;
use agent_space_mcp::admin_api::{self, AdminState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

#[tokio::test]
#[serial_test::serial]
async fn disable_cancels_execution_before_waiting_for_another_cleanup() {
    use agent_engine::engine::mock::{TEST_EXEC_HOLD, TEST_HOLD_STOP_WORKSPACE};
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(MockEngine::new());
    let (_, sessions, service) = app(engine, dir.path()).await;
    let alice = service.create_user("alice").unwrap();
    let bob = service.create_user("bob").unwrap();
    let alice_name = service.state().get("alice").unwrap().sandbox_name;
    let bob_name = service.state().get("bob").unwrap().sandbox_name;
    sessions.get_or_create(&alice_name).await.unwrap();
    let bob_session = sessions.get_or_create(&bob_name).await.unwrap();
    TEST_EXEC_HOLD.store(true, Ordering::SeqCst);
    let exec = tokio::spawn(async move { bob_session.exec("held", Duration::from_secs(30)).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while TEST_EXEC_HOLD.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    *TEST_HOLD_STOP_WORKSPACE.lock().unwrap() = Some(alice_name);
    let outcome = {
        let first_stop = service.set_enabled("alice", &alice.agent_space_id, false);
        tokio::pin!(first_stop);
        assert!(futures::poll!(&mut first_stop).is_pending());
        let second_stop = service.set_enabled("bob", &bob.agent_space_id, false);
        tokio::pin!(second_stop);
        assert!(futures::poll!(&mut second_stop).is_pending());
        tokio::time::timeout(Duration::from_secs(1), exec).await
    };
    *TEST_HOLD_STOP_WORKSPACE.lock().unwrap() = None;
    assert!(outcome
        .expect("disabled execution waited behind another user's cleanup")
        .unwrap()
        .is_err());
}

#[tokio::test]
#[serial_test::serial]
async fn pending_stop_retry_does_not_revoke_a_reenabled_account() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(MockEngine::new());
    let (_, _, service) = app(engine.clone(), dir.path()).await;
    let first = service.create_user("alice").unwrap();
    let second = service.create_user("bob").unwrap();
    let alice = service
        .state()
        .set_enabled("alice", &first.agent_space_id, false)
        .unwrap();
    let bob = service
        .state()
        .set_enabled("bob", &second.agent_space_id, false)
        .unwrap();
    let mut sandboxes = engine.sandboxes.lock().await;
    sandboxes.extend([alice.sandbox_name.clone(), bob.sandbox_name.clone()]);
    TEST_STOPPED_WORKSPACES.lock().unwrap().clear();
    let retry = service.retry_pending_stops();
    tokio::pin!(retry);
    // Freeze the first engine lookup after retry has captured both pending users.
    assert!(futures::poll!(&mut retry).is_pending());
    // Model another stop completing and the administrator restoring Bob while
    // this pass is still waiting on Alice. Its snapshot is now stale.
    service
        .state()
        .clear_stop_pending("bob", &bob.agent_space_id)
        .unwrap();
    service
        .state()
        .set_enabled("bob", &bob.agent_space_id, true)
        .unwrap();
    drop(sandboxes);
    retry.await.unwrap();
    assert_eq!(
        service.state().get("bob").unwrap().status,
        agent_engine::state::UserStatus::Active
    );
    assert!(
        !TEST_STOPPED_WORKSPACES
            .lock()
            .unwrap()
            .contains(&bob.sandbox_name),
        "stale pending-stop snapshot revoked an enabled account"
    );
}

fn opts() -> WorkspaceOpts {
    WorkspaceOpts {
        image: "mock".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}

async fn app(
    engine: Arc<MockEngine>,
    dir: &std::path::Path,
) -> (axum::Router, Arc<SessionManager>, Arc<WorkspaceService>) {
    StateStore::init(dir).unwrap();
    let state = Arc::new(StateStore::open(dir).unwrap());
    let sessions = Arc::new(SessionManager::new(engine, 4, opts()));
    let service = Arc::new(WorkspaceService::new(
        state,
        sessions.clone(),
        "smcp",
        "http://127.0.0.1",
    ));
    (
        admin_api::router(AdminState {
            service: service.clone(),
            admin_token: "admin-secret".into(),
        }),
        sessions,
        service,
    )
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            body.map(|value| value.to_string()).unwrap_or_default(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, value)
}

#[tokio::test]
async fn resource_observation_failure_is_partial_and_admin_only() {
    let dir = std::env::temp_dir().join(format!(
        "admin-resources-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let engine = Arc::new(MockEngine::new());
    let (app, sessions, service) = app(engine.clone(), &dir).await;
    let created = service.create_user("alice").unwrap();
    let (status, _) = send(&app, "GET", "/admin/v1/users/alice", &created.token, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, response) = send(&app, "GET", "/admin/v1/users/alice", "admin-secret", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response["resources"]["agentSpaceId"],
        created.agent_space_id
    );
    // This mock does not implement resource inspection: detail still succeeds.
    assert_eq!(response["resources"]["runtime"]["status"], "error");
    assert_eq!(response["resources"]["disk"]["status"], "error");
    assert!(response["resources"]["disk"]["value"].is_null());
    assert!(response["resources"].get("cpu").is_none());
    assert_eq!(sessions.session_count().await, 0);
    assert_eq!(
        engine
            .create_count
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    let (status, list) = send(&app, "GET", "/admin/v1/users", "admin-secret", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(list["users"][0].get("resources").is_none());
    let (status, disabled) = send(
        &app,
        "PATCH",
        "/admin/v1/users/alice",
        "admin-secret",
        Some(json!({"agentSpaceId": created.agent_space_id, "enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(disabled.get("resources").is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn admin_create_disable_delete_and_recreate_are_space_scoped() {
    let dir = std::env::temp_dir().join(format!("admin-api-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Arc::new(MockEngine::new());
    let (app, sessions, _) = app(engine.clone(), &dir).await;

    let (status, ready) = send(&app, "GET", "/admin/v1/status", "admin-secret", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["ready"], true);
    assert_eq!(ready["activeSpaces"], 0);
    let (status, _) = send(
        &app,
        "POST",
        "/admin/v1/users",
        "wrong",
        Some(json!({"username":"alice"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, created) = send(
        &app,
        "POST",
        "/admin/v1/users",
        "admin-secret",
        Some(json!({"username":"alice"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let space = created["agentSpaceId"].as_str().unwrap().to_string();
    let token = created["token"].as_str().unwrap();
    assert_eq!(created["mcpUrl"], "http://127.0.0.1/u/alice/mcp");
    assert!(!serde_json::to_string(&created)
        .unwrap()
        .contains("secretDigest"));
    let (status, viewed) = send(&app, "GET", "/admin/v1/users/alice", "admin-secret", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(viewed["mcpUrl"], "http://127.0.0.1/u/alice/mcp");
    assert_eq!(viewed["stopPending"], false);
    assert!(viewed.get("token").is_none());
    assert_eq!(sessions.session_count().await, 0);

    let (status, _) = send(
        &app,
        "POST",
        "/admin/v1/users",
        "admin-secret",
        Some(json!({"username":"alice"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    engine.create_sandbox("smcp-other", &opts()).await.unwrap();
    let session = SessionManager::new(engine.clone(), 4, opts());
    session.get_or_create(&format!("smcp-{space}")).await.ok();

    let (status, _) = send(
        &app,
        "PATCH",
        "/admin/v1/users/alice",
        "admin-secret",
        Some(json!({"agentSpaceId":"spc_wrong","enabled":false})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/admin/v1/users/alice?agentSpaceId={space}"),
        "admin-secret",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !engine
                .list_running()
                .await
                .unwrap()
                .iter()
                .any(|name| name.contains(&space))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!engine
        .list_running()
        .await
        .unwrap()
        .iter()
        .any(|name| name.contains(&space)));
    assert!(engine
        .list_running()
        .await
        .unwrap()
        .contains(&"smcp-other".to_string()));
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/admin/v1/users/alice?agentSpaceId={space}"),
        "admin-secret",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, "GET", "/admin/v1/users/alice", "admin-secret", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, recreated) = send(
        &app,
        "POST",
        "/admin/v1/users",
        "admin-secret",
        Some(json!({"username":"alice"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(recreated["agentSpaceId"].as_str().unwrap(), space);
    assert_ne!(recreated["token"].as_str().unwrap(), token);
    let publish_session = sessions
        .get_or_create(&format!(
            "smcp-{}",
            recreated["agentSpaceId"].as_str().unwrap()
        ))
        .await
        .unwrap();
    let (_, ready) = send(&app, "GET", "/admin/v1/status", "admin-secret", None).await;
    assert_eq!(ready["activeSpaces"], 1);
    publish_session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                (recreated["agentSpaceId"].as_str().unwrap()).to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let (status, viewed) = send(&app, "GET", "/admin/v1/users/alice", "admin-secret", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(viewed["publications"][0]["type"], "port");
    assert_eq!(viewed["publications"][0]["name"], "test");
    let item = publish_session.publication("test").unwrap();
    assert!(!viewed.to_string().contains(&item.password));
    assert!(!viewed.to_string().contains(&item.access_username));
    let (status, missing) =
        send(&app, "GET", "/admin/v1/users/missing", "admin-secret", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["error"], "not_found");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn disable_stop_failure_stays_pending_and_volume_only_delete_succeeds() {
    let dir = std::env::temp_dir().join(format!("admin-pending-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Arc::new(MockEngine::new());
    let (router, sessions, service) = app(engine.clone(), &dir).await;
    let (_, created) = send(
        &router,
        "POST",
        "/admin/v1/users",
        "admin-secret",
        Some(json!({"username":"alice"})),
    )
    .await;
    let space = created["agentSpaceId"].as_str().unwrap();
    sessions
        .get_or_create(&format!("smcp-{space}"))
        .await
        .unwrap();
    TEST_NEXT_STATUS
        .lock()
        .unwrap()
        .replace(agent_engine::engine::WorkspaceStatus::Running);
    TEST_FAIL_WORKSPACE_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, body) = send(
        &router,
        "PATCH",
        "/admin/v1/users/alice",
        "admin-secret",
        Some(json!({"agentSpaceId": space, "enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["stop"], "pending");
    let (_, viewed) = send(
        &router,
        "GET",
        "/admin/v1/users/alice",
        "admin-secret",
        None,
    )
    .await;
    assert_eq!(viewed["stopPending"], true);
    let (status, conflict) = send(
        &router,
        "PATCH",
        "/admin/v1/users/alice",
        "admin-secret",
        Some(json!({"agentSpaceId": space, "enabled": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["error"], "stop_pending");
    TEST_FAIL_WORKSPACE_STOP.store(false, std::sync::atomic::Ordering::SeqCst);
    service.retry_pending_stops().await.unwrap();
    let (status, body) = send(
        &router,
        "GET",
        "/admin/v1/users/alice",
        "admin-secret",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stopPending"], false);
    assert!(body.get("cleanupError").is_none());

    let dir_only = std::env::temp_dir().join(format!("admin-volume-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir_only);
    let volume_engine = Arc::new(MockEngine::new());
    let (volume_router, _, volume_service) = app(volume_engine.clone(), &dir_only).await;
    let (_, created) = send(
        &volume_router,
        "POST",
        "/admin/v1/users",
        "admin-secret",
        Some(json!({"username":"cara"})),
    )
    .await;
    let space = created["agentSpaceId"].as_str().unwrap();
    volume_engine
        .volumes
        .lock()
        .await
        .push(format!("smcp-{space}-ws"));
    volume_engine
        .fail_volume_removal
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, _) = send(
        &volume_router,
        "DELETE",
        &format!("/admin/v1/users/cara?agentSpaceId={space}"),
        "admin-secret",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _) = send(
        &volume_router,
        "DELETE",
        &format!("/admin/v1/users/cara?agentSpaceId={space}"),
        "admin-secret",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    volume_engine
        .fail_volume_removal
        .store(false, std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !volume_engine
                .list_volumes()
                .await
                .unwrap()
                .contains(&format!("smcp-{space}-ws"))
                && volume_service.state().get("cara").is_err()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!volume_engine
        .list_volumes()
        .await
        .unwrap()
        .contains(&format!("smcp-{space}-ws")));
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir_only);
}

#[tokio::test]
async fn accepted_delete_closes_publications_and_old_session_before_cleanup_finishes() {
    let dir = std::env::temp_dir().join(format!("admin-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (_, sessions, service) = app(Arc::new(MockEngine::new()), &dir).await;
    let created = service.create_user("alice").unwrap();
    let session = sessions
        .get_or_create(&format!("smcp-{}", created.agent_space_id))
        .await
        .unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                created.agent_space_id.to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();

    assert!(!service
        .delete_user("alice", &created.agent_space_id)
        .await
        .unwrap());
    assert!(session.publication("test").is_none());
    assert!(session
        .exec("must-not-run", std::time::Duration::from_secs(1))
        .await
        .is_err());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while service.state().get("alice").is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn disabling_detached_vm_stops_it_without_an_in_memory_session() {
    let dir = std::env::temp_dir().join(format!("admin-detached-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let engine = Arc::new(MockEngine::new());
    let (router, sessions, service) = app(engine.clone(), &dir).await;
    let (_, created) = send(
        &router,
        "POST",
        "/admin/v1/users",
        "admin-secret",
        Some(json!({"username":"detached"})),
    )
    .await;
    let space = created["agentSpaceId"].as_str().unwrap();
    let name = format!("smcp-{space}");
    engine.create_sandbox(&name, &opts()).await.unwrap();
    assert_eq!(sessions.session_count().await, 0);
    let (status, _) = send(
        &router,
        "PATCH",
        "/admin/v1/users/detached",
        "admin-secret",
        Some(json!({"agentSpaceId":space,"enabled":false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(TEST_STOPPED_WORKSPACES.lock().unwrap().contains(&name));
    assert!(service.state().pending_stops().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn dav_token_admin_contract_is_independent_and_never_starts_a_vm() {
    let dir = tempfile::tempdir().unwrap();
    StateStore::init(dir.path()).unwrap();
    let state = Arc::new(StateStore::open(dir.path()).unwrap());
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 4, opts()));
    let service =
        WorkspaceService::new(state.clone(), sessions.clone(), "smcp", "http://127.0.0.1");
    let enabled = Arc::new(
        service
            .clone()
            .with_dav_base_url(Some("https://files.example")),
    );
    let router = admin_api::router(AdminState {
        service: enabled,
        admin_token: "admin-secret".into(),
    });
    let disabled = admin_api::router(AdminState {
        service: Arc::new(service),
        admin_token: "admin-secret".into(),
    });
    let (user, mcp) = state.create_user("alice", "smcp", &["ws"]).unwrap();
    let payload = json!({"agentSpaceId":user.agent_space_id});
    let endpoint = "/admin/v1/users/alice/dav-token";
    let revoke = format!("{endpoint}?agentSpaceId={}", user.agent_space_id);
    assert_eq!(
        send(&router, "POST", endpoint, &mcp.token, Some(payload.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &disabled,
            "POST",
            endpoint,
            "admin-secret",
            Some(payload.clone())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        send(
            &router,
            "POST",
            "/admin/v1/users/missing/dav-token",
            "admin-secret",
            Some(payload.clone())
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &router,
            "POST",
            endpoint,
            "admin-secret",
            Some(json!({"agentSpaceId":"spc_stale"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, before) = send(
        &router,
        "GET",
        "/admin/v1/users/alice",
        "admin-secret",
        None,
    )
    .await;
    assert_eq!(before["hasDavCredential"], false);
    assert_eq!(before["davUrl"], "https://files.example/u/alice/dav/");
    let (status, issued) = send(
        &router,
        "POST",
        endpoint,
        "admin-secret",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(issued["username"], "alice");
    assert_eq!(issued["agentSpaceId"], user.agent_space_id);
    assert_eq!(issued["davUrl"], before["davUrl"]);
    let token = issued["token"].as_str().unwrap();
    assert!(state.authenticate_dav(token).is_ok());
    assert!(state.authenticate(token).is_err());
    assert!(state.authenticate(&mcp.token).is_ok());
    assert_eq!(
        send(&router, "POST", endpoint, token, Some(payload.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, view) = send(
        &router,
        "GET",
        "/admin/v1/users/alice",
        "admin-secret",
        None,
    )
    .await;
    assert_eq!(view["hasDavCredential"], true);
    assert!(view.get("credential").is_none() && view.get("davCredential").is_none());
    assert!(!view.to_string().contains("secretDigest"));
    let (_, second) = send(
        &router,
        "POST",
        endpoint,
        "admin-secret",
        Some(payload.clone()),
    )
    .await;
    assert!(state.authenticate_dav(token).is_err());
    assert!(state
        .authenticate_dav(second["token"].as_str().unwrap())
        .is_ok());
    assert_eq!(
        send(&disabled, "DELETE", &revoke, "admin-secret", None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&disabled, "DELETE", &revoke, "admin-secret", None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(state.authenticate(&mcp.token).is_ok());
    assert!(state
        .authenticate_dav(second["token"].as_str().unwrap())
        .is_err());
    let (_, hidden) = send(
        &disabled,
        "GET",
        "/admin/v1/users/alice",
        "admin-secret",
        None,
    )
    .await;
    assert!(hidden.get("davUrl").is_none());
    state
        .set_enabled("alice", &user.agent_space_id, false)
        .unwrap();
    assert_eq!(
        send(&router, "POST", endpoint, "admin-secret", Some(payload))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(sessions.active_space_count(), 0);
}

#[tokio::test]
async fn custom_dav_admin_input_retry_and_generation_contract() {
    let dir = tempfile::tempdir().unwrap();
    StateStore::init(dir.path()).unwrap();
    let state = Arc::new(StateStore::open(dir.path()).unwrap());
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 4, opts()));
    let service = Arc::new(
        WorkspaceService::new(state.clone(), sessions.clone(), "smcp", "http://127.0.0.1")
            .with_dav_base_url(Some("https://files.example")),
    );
    let router = admin_api::router(AdminState {
        service,
        admin_token: "admin-secret".into(),
    });
    let (user, mcp) = state.create_user("alice", "smcp", &["ws"]).unwrap();
    let endpoint = "/admin/v1/users/alice/dav-token";
    let token = "integration-0123456789abcdef0123456789abcdef"; // gitleaks:allow -- synthetic test credential
    let payload = json!({"agentSpaceId":user.agent_space_id, "token":token});
    assert_eq!(
        send(&router, "POST", endpoint, &mcp.token, Some(payload.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, first) = send(
        &router,
        "POST",
        endpoint,
        "admin-secret",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["token"], token);
    assert_eq!(first["davUrl"], "https://files.example/u/alice/dav/");
    let mut changes = state.subscribe_changes();
    let (_, retry) = send(&router, "POST", endpoint, "admin-secret", Some(payload)).await;
    assert_eq!(first, retry);
    assert!(!changes.has_changed().unwrap());
    for invalid in [
        json!(null),
        json!(42),
        json!(true),
        json!([]),
        json!(""),
        json!("short"),
        json!("a".repeat(257)),
        json!("x ".repeat(32)),
    ] {
        let (status, error) = send(
            &router,
            "POST",
            endpoint,
            "admin-secret",
            Some(json!({"agentSpaceId":user.agent_space_id,"token":invalid})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!error.to_string().contains(token));
        assert!(!changes.has_changed().unwrap());
    }
    assert_eq!(
        send(
            &router,
            "POST",
            endpoint,
            "admin-secret",
            Some(json!({"agentSpaceId":user.agent_space_id,"tokne":token}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(state.authenticate_dav(token).is_ok());
    let (_, generated) = send(
        &router,
        "POST",
        endpoint,
        "admin-secret",
        Some(json!({"agentSpaceId":user.agent_space_id})),
    )
    .await;
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    assert_ne!(first["keyId"], generated["keyId"]);
    assert!(state.authenticate_dav(token).is_err());
    assert!(state
        .authenticate_dav(generated["token"].as_str().unwrap())
        .is_ok());
    let (_, view) = send(
        &router,
        "GET",
        "/admin/v1/users/alice",
        "admin-secret",
        None,
    )
    .await;
    assert!(!view.to_string().contains(token));
    assert!(view.get("token").is_none());
    assert_eq!(sessions.active_space_count(), 0);
}

#[tokio::test]
async fn request_entry_links_change_without_mutating_identity_or_canonical_links() {
    use agent_space_mcp::origins::{origin_boundary, OriginPolicy};
    let dir = tempfile::tempdir().unwrap();
    StateStore::init(dir.path()).unwrap();
    let store = Arc::new(StateStore::open(dir.path()).unwrap());
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 4, opts()));
    let service = Arc::new(
        WorkspaceService::new(store.clone(), sessions.clone(), "smcp", "")
            .with_dav_base_url(Some(""))
            .with_dav_public_port(18933),
    );
    let created = service.create_user("alice").unwrap();
    let original_key = store.get("alice").unwrap().credential.unwrap();
    let router = admin_api::router(AdminState {
        service: service.clone(),
        admin_token: "admin-secret".into(),
    })
    .layer(axum::middleware::from_fn_with_state(
        OriginPolicy::new(None).unwrap(),
        origin_boundary,
    ));
    for host in [
        "127.0.0.1:18932",
        "localhost:18932",
        "192.168.1.249:18932",
        "192.168.31.236:18932",
        "[::1]:18932",
    ] {
        for token in ["admin-secret", "wrong-token"] {
            let request = Request::builder()
                .uri("/admin/v1/users")
                .header("host", host)
                .header("authorization", format!("Bearer {token}"))
                .header("x-forwarded-host", "stale.example")
                .body(Body::empty())
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            if token == "wrong-token" {
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                continue;
            }
            assert_eq!(response.status(), StatusCode::OK);
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 65536)
                    .await
                    .unwrap(),
            )
            .unwrap();
            let user = &body["users"][0];
            assert_eq!(user["mcpUrl"], format!("http://{host}/u/alice/mcp"));
            let mut dav = url::Url::parse(&format!("http://{host}")).unwrap();
            dav.set_port(Some(18933)).unwrap();
            assert_eq!(
                user["davUrl"],
                format!("{}/u/alice/dav/", dav.origin().ascii_serialization())
            );
            assert_eq!(user["agentSpaceId"], created.agent_space_id);
        }
    }
    let current = store.get("alice").unwrap();
    assert_eq!(current.credential.unwrap().key_id, original_key.key_id);
    assert_eq!(
        store.authenticate(&created.token).unwrap().username,
        "alice"
    );
    assert_eq!(sessions.active_space_count(), 0);
    let fixed = WorkspaceService::new(store, sessions, "smcp", "https://workspace.example/")
        .with_dav_base_url(Some("https://files.example/"));
    let view = fixed
        .for_request(&url::Url::parse("http://localhost:18932").unwrap())
        .list_users();
    assert_eq!(view[0].mcp_url, "https://workspace.example/u/alice/mcp");
    assert_eq!(
        view[0].dav_url.as_deref(),
        Some("https://files.example/u/alice/dav/")
    );
}
