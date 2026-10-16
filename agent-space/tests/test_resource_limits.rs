mod common;

use std::sync::Arc;
use std::time::Duration;

use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, EngineError, VolumeSpec, WorkspaceOpts};
use agent_engine::session::SessionManager;

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

// RL-01, RL-07: CPU and memory match config
#[tokio::test]
async fn test_cpu_and_memory_limits() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-rl-cpu");
    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();

    // RL-01: CPU limit
    let r = handle
        .exec("bash", &["-c", "nproc"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout.trim(), "1", "expected 1 CPU, got: {}", r.stdout);

    // RL-07: Memory match config
    let r = handle
        .exec(
            "bash",
            &["-c", "grep MemTotal /proc/meminfo | awk '{print $2}'"],
            Some(timeout()),
        )
        .await
        .unwrap();
    let mem_kb: u64 = r.stdout.trim().parse().unwrap();
    let mem_mb = mem_kb / 1024;
    assert!(
        (200..=350).contains(&mem_mb),
        "memory {} MB not in expected range 200-350 (configured 256)",
        mem_mb
    );

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

// RL-02: Memory limit — OOM
#[tokio::test]
async fn test_memory_oom() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-rl-oom");
    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();

    let r = handle
        .exec(
            "bash",
            &[
                "-c",
                r#"
python3 -c "
data = []
try:
    for i in range(480):
        data.append(bytearray(1024 * 1024))
    print('ALLOCATED_ALL')
except MemoryError:
    print('MEMORY_ERROR')
"
status=$?
printf "CHILD_STATUS=%s\n" "$status"
"#,
            ],
            Some(Duration::from_secs(20)),
        )
        .await;

    let result = r.expect("OOM test must report the child exit status");
    assert!(
        result.stdout.contains("MEMORY_ERROR") || result.stdout.contains("CHILD_STATUS=137"),
        "expected MemoryError or SIGKILL, exit={} stdout={} stderr={}",
        result.exit_code,
        result.stdout,
        result.stderr
    );

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

// RL-03, RL-04: Disk limit and reclaim
#[tokio::test]
async fn test_disk_limit_and_reclaim() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-rl-disk");
    let opts = WorkspaceOpts {
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
    };
    let handle = engine.create_sandbox(&name, &opts).await.unwrap();

    // RL-03: Writing >256MB should fail (ext4 min ~256 MiB)
    let r = handle
        .exec(
            "bash",
            &[
                "-c",
                r#"
rm -f /workspace/disk_test.bin
if ! dd if=/dev/zero of=/workspace/disk_test.bin bs=1M count=280 2>/dev/null; then
  echo "DISK_FULL"
fi
rm -f /workspace/disk_test.bin
"#,
            ],
            Some(Duration::from_secs(60)),
        )
        .await;

    let result = r.expect("disk pressure must report a command result");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stdout.contains("DISK_FULL"),
        "expected ENOSPC, got {}",
        result.stdout
    );

    // RL-04: Delete file then write succeeds
    let filled = handle
        .exec(
            "bash",
            &[
                "-c",
                r#"
dd if=/dev/zero of=/workspace/fill.bin bs=1M count=50 2>/dev/null && echo "FILLED"
"#,
            ],
            Some(Duration::from_secs(30)),
        )
        .await;

    assert_eq!(filled.unwrap().exit_code, 0);
    let r = handle
        .exec(
            "bash",
            &["-c", "rm -f /workspace/fill.bin && echo DELETED"],
            Some(timeout()),
        )
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("DELETED"));

    let r = handle
        .exec(
            "bash",
            &[
                "-c",
                "echo reclaimed > /workspace/reclaim.txt && cat /workspace/reclaim.txt",
            ],
            Some(timeout()),
        )
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("reclaimed"));

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

// RL-05: tmpfs limit
#[tokio::test]
async fn test_tmpfs_limit() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-rl-tmp");
    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();

    let r = handle
        .exec(
            "bash",
            &[
                "-c",
                r#"
rm -f /tmp/big.bin
if ! dd if=/dev/zero of=/tmp/big.bin bs=1M count=100 2>/dev/null; then
  echo "TMPFS_FULL"
else
  echo "WRITTEN_ALL"
fi
rm -f /tmp/big.bin
"#,
            ],
            Some(Duration::from_secs(20)),
        )
        .await;

    let result = r.expect("tmpfs quota check must execute successfully");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stdout.contains("TMPFS_FULL"),
        "64 MiB tmpfs accepted 100 MiB: {}",
        result.stdout
    );

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

// RL-06: Max sandboxes limit
#[tokio::test]
async fn test_max_sandboxes() {
    common::cleanup_test_sandboxes().await;
    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;
    let mgr = SessionManager::new(engine, 2, make_opts());

    let name_a = unique_name("test-rl-a");
    let name_b = unique_name("test-rl-b");
    let name_c = unique_name("test-rl-c");

    let _ = mgr.get_or_create(&name_a).await.unwrap();
    let _ = mgr.get_or_create(&name_b).await.unwrap();

    let result = mgr.get_or_create(&name_c).await;
    assert!(matches!(
        result,
        Err(EngineError::WorkspaceCapacityExhausted)
    ));

    let _ = mgr.remove(&name_a).await;
    let _ = mgr.remove(&name_b).await;
}

// RL-08: /.msb runtime mount is size-capped (previously an unquota'd
// virtiofs that could fill the host disk). Since 0.6.9 it has a hard cap;
// assert the mount is small (quota present) so we catch a regression.
#[tokio::test]
async fn test_msb_runtime_quota() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-rl-msb");
    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();

    let r = handle
        .exec(
            "bash",
            &[
                "-c",
                r#"
df -k /.msb 2>/dev/null | tail -1 | awk '{print $2}'
"#,
            ],
            Some(Duration::from_secs(20)),
        )
        .await;

    let result = r.expect("runtime mount quota check must execute");
    assert_eq!(result.exit_code, 0);
    let kb: u64 = result
        .stdout
        .trim()
        .parse()
        .expect("runtime mount capacity");
    assert!(
        (1..1024 * 1024).contains(&kb),
        "runtime mount must have a positive capacity below 1 GiB: {kb} KiB"
    );

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}
