mod common;

use std::sync::Arc;
use std::time::Duration;

use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, EngineError, VolumeSpec, WorkspaceOpts, WorkspaceStatus};
use agent_engine::session::SessionManager;
use serial_test::serial;

const TEST_IMAGE: &str = "nikolaik/python-nodejs:python3.12-nodejs22-slim";
const TIMEOUT_SECS: u64 = 30;

fn make_opts() -> WorkspaceOpts {
    WorkspaceOpts {
        image: TEST_IMAGE.to_string(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".to_string(),
        volumes: vec![
            VolumeSpec::disk("/workspace", "ws", 256),
            VolumeSpec::tmpfs("/tmp", 64),
        ],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}

fn timeout() -> Duration {
    Duration::from_secs(TIMEOUT_SECS)
}

fn unique_name(prefix: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{}", prefix, ts)
}

// Running status and stale-handle error after an externally verified stop.
#[tokio::test]
async fn test_sandbox_status_running_and_error_on_stopped() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-st");
    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();

    // ST-01: exec succeeds, status is Running
    let r = handle
        .exec("bash", &["-c", "echo ok"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    let status = handle.status().await;
    assert_eq!(status, WorkspaceStatus::Running);

    // ST-11: SandboxNotRunning error classification
    stop_externally(&name);
    let result = handle
        .exec("bash", &["-c", "echo dead"], Some(timeout()))
        .await;
    assert!(result.is_err(), "exec on stopped sandbox should fail");
    let err = result.unwrap_err();
    match err {
        EngineError::SandboxNotRunning(_) => {} // expected
        other => panic!("expected SandboxNotRunning, got: {:?}", other),
    }

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

// ST-03, ST-05, ST-06: Session auto-recovers after sandbox stop
// Requires #[serial] because external msb stop interferes with other tests
#[tokio::test]
#[serial]
async fn test_session_auto_recovery_after_sandbox_stop() {
    common::cleanup_test_sandboxes().await;
    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;
    let mgr = SessionManager::new(engine, 4, make_opts());

    let sb_name = unique_name("test-rec");
    let session = mgr.get_or_create(&sb_name).await.unwrap();
    let r = session.exec("echo before-stop", timeout()).await.unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("before-stop"));

    // Stop externally
    stop_externally(&sb_name);

    // The existing Session queue must recover too, before a new lookup.
    let recovered = session
        .exec("echo existing-session", timeout())
        .await
        .unwrap();
    assert_eq!(recovered.exit_code, 0);
    assert_eq!(recovered.stdout.trim(), "existing-session");

    // A later lookup must reuse the recovered workspace.
    let session2 = mgr.get_or_create(&sb_name).await;
    assert!(session2.is_ok(), "should recover: {:?}", session2.err());
    let r2 = session2
        .unwrap()
        .exec("echo after-recovery", timeout())
        .await
        .unwrap();
    assert_eq!(r2.exit_code, 0);
    assert!(r2.stdout.contains("after-recovery"));

    mgr.remove(&sb_name).await.unwrap();
}

fn stop_externally(name: &str) {
    let output = std::process::Command::new(common::msb_binary())
        .args(["stop", "--force", name])
        .output()
        .expect("run msb stop");
    assert!(
        output.status.success(),
        "external stop failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
