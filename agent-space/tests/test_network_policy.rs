mod common;

use std::sync::Arc;
use std::time::Duration;

use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, NetworkAccess, VolumeSpec, WorkspaceOpts};

const TEST_IMAGE: &str = "nikolaik/python-nodejs:python3.12-nodejs22-slim";

fn opts_with(access: NetworkAccess) -> WorkspaceOpts {
    WorkspaceOpts {
        image: TEST_IMAGE.to_string(),
        cpus: 1,
        memory_mib: 512,
        shell: "/bin/bash".to_string(),
        volumes: vec![
            VolumeSpec::disk("/workspace", "ws", 256),
            VolumeSpec::tmpfs("/tmp", 64),
        ],
        network_access: access,
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}

fn unique_name(prefix: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{}", prefix, ts)
}

async fn exec_str(handle: &Arc<dyn agent_engine::engine::AgentHandle>, cmd: &str) -> String {
    match handle
        .exec("bash", &["-c", cmd], Some(Duration::from_secs(20)))
        .await
    {
        Ok(r) => r.stdout,
        Err(e) => format!("<ERR> {e}"),
    }
}

// ─── NP-01: public-only — public internet reachable, host blocked ───────────
#[tokio::test]
async fn test_np01_public_only() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-np-public");
    let handle = engine
        .create_sandbox(&name, &opts_with(NetworkAccess::PublicOnly))
        .await
        .unwrap();

    // Public internet should be reachable (200 from pypi.org)
    let public = exec_str(
        &handle,
        "curl -sS -m 8 -o /dev/null -w '%{http_code}' https://pypi.org/ || echo 'FAILED'",
    )
    .await;
    assert_eq!(
        public.trim(),
        "200",
        "NP-01: public internet should be reachable, got: {}",
        public
    );

    // Host/gateway should be blocked
    let host = exec_str(&handle, "curl -sS -m 5 -o /dev/null -w '%{http_code}' http://host.microsandbox.internal/ 2>&1 || echo 'BLOCKED'").await;
    assert!(
        host.contains("BLOCKED") || host.contains("000") || host.contains("Could not connect"),
        "NP-01: host should be blocked, got: {}",
        host
    );

    common::cleanup_sandbox(&engine, &name, &opts_with(NetworkAccess::PublicOnly)).await;
}

// ─── NP-02: none — no egress at all ─────────────────────────────────────────
#[tokio::test]
async fn test_np02_no_egress() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-np-none");
    let handle = engine
        .create_sandbox(&name, &opts_with(NetworkAccess::None))
        .await
        .unwrap();

    // Even public internet should be blocked with policy "none"
    let public = exec_str(
        &handle,
        "curl -sS -m 8 -o /dev/null -w '%{http_code}' https://pypi.org/ 2>&1 || echo 'BLOCKED'",
    )
    .await;
    assert!(
        public.contains("BLOCKED")
            || public.contains("000")
            || public.contains("Could not connect"),
        "NP-02: egress should be fully blocked with none, got: {}",
        public
    );

    common::cleanup_sandbox(&engine, &name, &opts_with(NetworkAccess::None)).await;
}

#[tokio::test]
async fn test_np03_connection_limits_are_persisted() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-np-limits");
    let mut opts = opts_with(NetworkAccess::Unrestricted);
    opts.max_tcp_connections = Some(128);
    opts.max_udp_connections = Some(64);
    engine.create_sandbox(&name, &opts).await.unwrap();

    let config = microsandbox::Sandbox::get(&name)
        .await
        .unwrap()
        .config()
        .unwrap();
    assert_eq!(config.spec.network.max_tcp_connections, Some(128));
    assert_eq!(config.spec.network.max_udp_connections, Some(64));

    common::cleanup_sandbox(&engine, &name, &opts).await;
}
