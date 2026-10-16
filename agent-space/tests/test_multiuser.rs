mod common;

use std::sync::Arc;
use std::time::Duration;

use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, VolumeSpec, WorkspaceOpts};
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

// M-01, M-02, M-03: Users cannot see each other's files (isolated sandboxes)
// Requires #[serial] because creating 2 sandboxes concurrently + cleanup can timeout
#[tokio::test]
#[serial]
async fn test_multiuser_isolated_sandboxes_cannot_see_other_files() {
    common::cleanup_test_sandboxes().await;
    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;
    let mgr = SessionManager::new(engine, 6, make_opts());

    let name_a = unique_name("test-mu-a");
    let name_b = unique_name("test-mu-b");

    let sa = mgr.get_or_create(&name_a).await.unwrap();
    let sb = mgr.get_or_create(&name_b).await.unwrap();

    // M-01: User A writes file
    sa.exec("echo AAA > /workspace/a.txt", timeout())
        .await
        .unwrap();

    // M-02: User B writes file
    sb.exec("echo BBB > /workspace/b.txt", timeout())
        .await
        .unwrap();

    // M-03: User isolation — different sandboxes
    let r = sa.exec("cat /workspace/b.txt", timeout()).await.unwrap();
    assert_ne!(
        r.stdout.trim(),
        "BBB",
        "user A should not see user B's file: {}",
        r.stdout
    );
    let r = sb.exec("cat /workspace/a.txt", timeout()).await.unwrap();
    assert_ne!(
        r.stdout.trim(),
        "AAA",
        "user B should not see user A's file: {}",
        r.stdout
    );

    let _ = std::process::Command::new(common::msb_binary())
        .args(["stop", "--force", &name_a])
        .output();
    let _ = mgr.remove(&name_a).await;
    let _ = std::process::Command::new(common::msb_binary())
        .args(["stop", "--force", &name_b])
        .output();
    let _ = mgr.remove(&name_b).await;
}
