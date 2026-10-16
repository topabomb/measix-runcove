use agent_engine::engine::mock::MockEngine;
use agent_engine::engine::WorkspaceOpts;
use agent_engine::session::SessionManager;
use agent_engine::state::StateStore;
use agent_engine::workspace_service::WorkspaceService;
use std::sync::Arc;

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

#[tokio::test]
async fn restart_adopts_registered_dynamic_user_only() {
    let dir = std::env::temp_dir().join(format!("dynamic-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    StateStore::init(&dir).unwrap();
    let state = Arc::new(StateStore::open(&dir).unwrap());
    let engine = Arc::new(MockEngine::new());
    let first = Arc::new(SessionManager::new(engine.clone(), 2, opts()));
    let service = WorkspaceService::new(state.clone(), first.clone(), "smcp", "http://127.0.0.1");
    let created = service.create_user("alice").unwrap();
    let user = state.authenticate(&created.token).unwrap();
    first.get_or_create(&user.sandbox_name).await.unwrap();
    engine.sandboxes.lock().await.push("foreign-sandbox".into());
    drop(first);
    drop(service);
    drop(state);
    let reopened = Arc::new(StateStore::open(&dir).unwrap());
    let restarted = Arc::new(SessionManager::new(engine.clone(), 2, opts()));
    let (registered, volumes) = reopened.registered_resources();
    restarted.adopt_running(&registered).await.unwrap();
    let adopted = restarted.get_session(&user.sandbox_name).await.unwrap();
    assert!(adopted.workspace_status().await == agent_engine::engine::WorkspaceStatus::Running);
    let foreign = restarted.get_session("foreign-sandbox").await.unwrap();
    assert!(foreign.workspace_status().await != agent_engine::engine::WorkspaceStatus::Running);
    let unknown = restarted
        .report_unregistered_resources(&registered, &volumes)
        .await
        .unwrap();
    assert!(
        unknown.contains(&"foreign-sandbox".to_string()),
        "{unknown:?}"
    );
    assert!(!unknown.contains(&user.sandbox_name), "{unknown:?}");
    assert!(engine.sandboxes.lock().await.contains(&user.sandbox_name));
    assert!(engine
        .sandboxes
        .lock()
        .await
        .contains(&"foreign-sandbox".to_string()));
    let _ = std::fs::remove_dir_all(dir);
}
