mod common;
use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, ManagedLaunch, VolumeSpec, WorkspaceOpts};
use agent_engine::session::SessionManager;
use agent_engine::state::{StateError, StateStore};
use agent_engine::workspace_service::WorkspaceService;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

async fn get_page(session: &agent_engine::session::Session, port: u16) -> String {
    let mut tcp = session.connect_guest_tcp(port).await.unwrap();
    tcp.writer
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut body = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), tcp.reader.read_to_end(&mut body))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&body).into_owned()
}

#[tokio::test]
async fn dynamic_user_directory_and_stopped_vm_deletion_use_real_resources() {
    common::cleanup_test_sandboxes().await;
    let dir = std::env::temp_dir().join(format!("dynamic-vm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    StateStore::init(&dir).unwrap();
    let state = Arc::new(StateStore::open(&dir).unwrap());
    let (user, _token) = state.create_user("alice", "test-hosted", &["ws"]).unwrap();
    let engine = Arc::new(MsbEngine::new());
    let sessions = Arc::new(SessionManager::new(engine.clone(), 1, opts()));
    let service = WorkspaceService::new(
        state.clone(),
        sessions.clone(),
        "test-hosted",
        "http://127.0.0.1",
    );
    let session = sessions.get_session(&user.sandbox_name).await.unwrap();
    session
        .exec(
            "mkdir -p /workspace/report && printf '<h1>one</h1>' > /workspace/report/index.html",
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let binary = common::static_helper_binary();
    let pid = session
        .start_managed(ManagedLaunch::StaticHttp {
            host_binary: binary.clone(),
            root: "/workspace/report".into(),
            file: false,
        })
        .await
        .unwrap();
    let port = pid.port;
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_test").to_string(),
                agent_engine::publication::PublishTarget::Directory("/workspace/report".into()),
                port,
                Some(pid),
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let first = get_page(&session, session.publication("test").unwrap().port).await;
    assert!(first.contains("one"), "{first}");
    session
        .exec(
            "printf '<h1>two</h1>' > /workspace/report/index.html",
            Duration::from_secs(20),
        )
        .await
        .unwrap();
    let second = get_page(&session, port).await;
    assert!(second.contains("two"), "{second}");
    session.unpublish("test").await;
    assert!(session.publication("test").is_none());
    sessions.stop_workspace(&user.sandbox_name).await.unwrap();
    assert_eq!(
        session.workspace_status().await,
        agent_engine::engine::WorkspaceStatus::Stopped
    );
    assert!(!service
        .delete_user(&user.username, &user.agent_space_id)
        .await
        .unwrap());
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if matches!(state.get(&user.username), Err(StateError::NotFound(_))) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("stopped VM deletion completes");
    assert!(!engine
        .list_workspaces()
        .await
        .unwrap()
        .contains(&user.sandbox_name));
    let remaining_volumes = engine.list_volumes().await.unwrap();
    for volume in &user.owned_volume_names {
        assert!(!remaining_volumes.contains(volume), "{volume} remains");
    }
    let _ = std::fs::remove_dir_all(dir);
}
