mod common;

use agent_engine::config::LifecycleConfig;
use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, EngineError, VolumeSpec, WorkspaceOpts};
use agent_engine::session::SessionManager;
use agent_engine::state::StateStore;
use agent_engine::workspace_service::WorkspaceService;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn opts() -> WorkspaceOpts {
    WorkspaceOpts {
        image: "nikolaik/python-nodejs:python3.12-nodejs22-slim".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![VolumeSpec::disk("/workspace", "ws", 256)],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}

#[tokio::test]
async fn real_resource_summary_is_scoped_and_never_keeps_vm_alive() {
    common::cleanup_test_sandboxes().await;
    let dir = std::env::temp_dir().join(format!("resource-vm-{}", uuid::Uuid::new_v4()));
    StateStore::init(&dir).unwrap();
    let state = Arc::new(StateStore::open(&dir).unwrap());
    let engine = Arc::new(MsbEngine::new());
    let sessions = Arc::new(SessionManager::new(engine.clone(), 1, opts()));
    let service = WorkspaceService::new(
        state.clone(),
        sessions.clone(),
        "test-resources",
        "http://localhost",
    );
    let created = service.create_user("alice").unwrap();
    let user = state.get("alice").unwrap();
    assert!(matches!(
        engine.get_metrics(&user.sandbox_name).await,
        Err(EngineError::NotFound(_))
    ));
    let fresh = service.get_user("alice").await.unwrap().resources.unwrap();
    assert_eq!(fresh.runtime.value.as_deref(), Some("notfound"));
    assert_eq!(fresh.allocation.cpu_cores.source, "default");
    assert_eq!(
        fresh.allocation.workspace_capacity_bytes.value,
        Some(256 * 1024 * 1024)
    );
    assert_eq!(fresh.disk.status, "unavailable");
    assert_eq!(sessions.session_count().await, 0);
    let session = sessions.get_or_create(&user.sandbox_name).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let request_at = session.last_request().await;
    let work_at = session.last_work().await;
    let first = service.get_user("alice").await.unwrap().resources.unwrap();
    eprintln!(
        "running summary: {}",
        serde_json::to_string(&first).unwrap()
    );
    assert_eq!(first.runtime.value.as_deref(), Some("running"));
    assert_eq!(first.disk.status, "current", "{first:?}");
    assert_eq!(first.memory.status, "current", "{first:?}");
    assert!(first.memory.value.unwrap() > 0);
    assert_eq!(first.allocation.cpu_cores.source, "sandbox");
    assert_eq!(
        first.allocation.memory_limit_bytes.value,
        Some(256 * 1024 * 1024)
    );
    assert_eq!(first.allocation.workspace_capacity_bytes.source, "volume");
    let cached = service.get_user("alice").await.unwrap().resources.unwrap();
    assert_eq!(first.disk.observed_at, cached.disk.observed_at);
    assert_eq!(session.last_request().await, request_at);
    assert_eq!(session.last_work().await, work_at);

    let result = session
        .exec(
            "dd if=/dev/zero of=/root/resource-root-test bs=1M count=8 conv=fsync",
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let root_write = service.get_user("alice").await.unwrap().resources.unwrap();
    assert_eq!(
        root_write.disk.value.as_ref().unwrap().used_bytes,
        first.disk.value.as_ref().unwrap().used_bytes
    );
    let result = session
        .exec(
            "dd if=/dev/zero of=/workspace/resource-data bs=1M count=8 conv=fsync",
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let written = service.get_user("alice").await.unwrap().resources.unwrap();
    assert!(
        written.disk.value.as_ref().unwrap().used_bytes
            >= first.disk.value.as_ref().unwrap().used_bytes + 8 * 1024 * 1024
    );
    assert!(
        written.disk.value.as_ref().unwrap().available_bytes
            < first.disk.value.as_ref().unwrap().available_bytes
    );

    // A mistaken root-disk bind over /workspace must fail instead of counting root storage.
    let result = session
        .exec("mount --bind /root /workspace", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let wrong_mount = service.get_user("alice").await.unwrap().resources.unwrap();
    assert_eq!(wrong_mount.disk.status, "historical");
    assert_eq!(
        wrong_mount.disk.reason.as_deref(),
        Some("workspace_stat_failed")
    );
    assert_eq!(wrong_mount.disk.observed_at, written.disk.observed_at);
    assert_eq!(wrong_mount.memory.status, "current");
    let result = session
        .exec("umount /workspace", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);

    // A second manager with changed defaults must still report actual existing resources.
    let mut changed = opts();
    changed.cpus = 4;
    changed.memory_mib = 1024;
    changed.volumes[0].size_mib = 1024;
    let other = WorkspaceService::new(
        state.clone(),
        Arc::new(SessionManager::new(engine.clone(), 1, changed)),
        "test-resources",
        "http://localhost",
    );
    let actual = other.get_user("alice").await.unwrap().resources.unwrap();
    assert_eq!(actual.allocation.cpu_cores.value, Some(1));
    assert_eq!(
        actual.allocation.workspace_capacity_bytes.value,
        Some(256 * 1024 * 1024)
    );

    microsandbox::Sandbox::get(&user.sandbox_name)
        .await
        .unwrap()
        .pause()
        .await
        .unwrap();
    let paused = service.get_user("alice").await.unwrap().resources.unwrap();
    assert_eq!(paused.runtime.value.as_deref(), Some("paused"));
    assert_eq!(paused.memory.status, "unavailable");
    assert_eq!(paused.disk.status, "historical");
    assert_eq!(
        microsandbox::Sandbox::get(&user.sandbox_name)
            .await
            .unwrap()
            .status_snapshot(),
        microsandbox::sandbox::SandboxStatus::Paused
    );
    microsandbox::Sandbox::get(&user.sandbox_name)
        .await
        .unwrap()
        .resume()
        .await
        .unwrap();

    // Keep querying across the idle deadline; queries must not reset activity or hold the stop gate.
    let request_at = session.last_request().await;
    session
        .set_last_work(Instant::now() - Duration::from_secs(3))
        .await;
    let idle_at = session.last_work().await;
    let config = LifecycleConfig {
        session_timeout_secs: 3600,
        idle_timeout_secs: 3,
        check_interval_secs: 1,
    };
    let (names, _) = state.registered_resources();
    let polling = async {
        for _ in 0..12 {
            service.get_user("alice").await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    let stopping = async {
        sessions.check_and_reclaim(&config, &names).await.unwrap();
    };
    tokio::join!(polling, stopping);
    assert_eq!(session.last_request().await, request_at);
    assert_eq!(session.last_work().await, idle_at);
    let stopped = service.get_user("alice").await.unwrap().resources.unwrap();
    assert_eq!(stopped.runtime.value.as_deref(), Some("stopped"));
    assert_eq!(stopped.memory.status, "unavailable");
    assert_eq!(stopped.disk.status, "historical");
    let no_cache = other.get_user("alice").await.unwrap().resources.unwrap();
    assert_eq!(no_cache.disk.status, "historical");
    let restarted = WorkspaceService::new(
        state.clone(),
        sessions.clone(),
        "test-resources",
        "http://localhost",
    );
    assert_eq!(
        restarted
            .get_user("alice")
            .await
            .unwrap()
            .resources
            .unwrap()
            .disk
            .status,
        "unavailable"
    );

    service
        .delete_user("alice", &created.agent_space_id)
        .await
        .unwrap();
    for _ in 0..100 {
        if state.get("alice").is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(state.get("alice").is_err());
    let new = service.create_user("alice").unwrap();
    assert_ne!(new.agent_space_id, created.agent_space_id);
    let rebuilt = service.get_user("alice").await.unwrap().resources.unwrap();
    assert_eq!(rebuilt.agent_space_id, new.agent_space_id);
    assert_eq!(rebuilt.disk.status, "unavailable");
    assert!(rebuilt.disk.value.is_none());
    service
        .delete_user("alice", &new.agent_space_id)
        .await
        .unwrap();
    service.retry_deletions().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
