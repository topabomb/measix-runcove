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
            VolumeSpec::disk("/workspace", "test-ws", 256),
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

// L-01, L-05: Create sandbox, exec command, volume persists across stop/start
#[tokio::test]
#[serial]
async fn test_lifecycle_create_exec_volume_persists_on_restart() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-lc");

    // L-01: Create sandbox, exec command
    let handle = engine
        .create_sandbox(&name, &make_opts())
        .await
        .expect("create should succeed");
    assert!(handle.name().contains("test-lc"));
    let result = handle
        .exec("bash", &["-c", "echo hello"], Some(timeout()))
        .await
        .expect("exec should succeed");
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("hello") || result.stderr.contains("hello"));

    // L-05: Volume persistence across stop/start
    handle
        .exec(
            "bash",
            &["-c", "touch /workspace/persist.txt"],
            Some(timeout()),
        )
        .await
        .expect("touch");
    handle.stop().await.expect("stop");
    let handle2 = engine.start_sandbox(&name).await.expect("reconnect");
    let result = handle2
        .exec("bash", &["-c", "ls /workspace/"], Some(timeout()))
        .await
        .expect("ls");
    assert!(
        result.stdout.contains("persist.txt"),
        "file should persist: {}",
        result.stdout
    );
    handle2.kill().await.ok();
    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

#[tokio::test]
#[serial]
async fn recreating_vm_reuses_registered_disk_at_its_original_capacity() {
    let engine = MsbEngine::new();
    let name = unique_name("test-lc-reuse");
    let volume_name = format!("{name}-test-ws");
    let mut old_opts = make_opts();
    old_opts.volumes[0].size_mib = 384;

    let old = engine.create_sandbox(&name, &old_opts).await.unwrap();
    let seed = old
        .exec(
            "bash",
            &["-c", "printf preserved >/workspace/reuse-marker"],
            Some(timeout()),
        )
        .await
        .unwrap();
    assert_eq!(seed.exit_code, 0);
    old.stop().await.unwrap();
    engine.remove_sandbox(&name).await.unwrap();

    let opts = make_opts();
    let recreated = engine.create_sandbox(&name, &opts).await.unwrap();
    let marker = recreated
        .exec("cat", &["/workspace/reuse-marker"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(marker.stdout.trim(), "preserved");
    let inspection = engine
        .inspect_resources(&name, std::slice::from_ref(&volume_name), &opts)
        .await
        .unwrap();
    assert!(inspection.workspace_disk_verified);
    assert_eq!(
        inspection.allocation.workspace_capacity_bytes.value,
        Some(384 * 1024 * 1024)
    );
    let sandbox = microsandbox::Sandbox::get(&name).await.unwrap();
    assert!(sandbox.config().unwrap().spec.mounts.iter().any(|mount| {
        matches!(mount, microsandbox::sandbox::VolumeMount::Named { name, guest, .. }
            if name == &volume_name && guest == "/workspace")
    }));

    recreated.stop().await.unwrap();
    engine.remove_sandbox(&name).await.unwrap();
    engine.remove_volume(&volume_name).await.unwrap();
    assert!(matches!(
        microsandbox::Volume::get(&volume_name).await,
        Err(microsandbox::MicrosandboxError::VolumeNotFound(_))
    ));
}

#[tokio::test]
#[serial]
async fn restarting_vm_rejects_a_directory_replacing_the_registered_disk() {
    let engine = MsbEngine::new();
    let name = unique_name("test-lc-volume-kind");
    let volume_name = format!("{name}-test-ws");

    let sandbox = engine.create_sandbox(&name, &make_opts()).await.unwrap();
    sandbox.stop().await.unwrap();
    engine.remove_volume(&volume_name).await.unwrap();
    microsandbox::Volume::builder(&volume_name)
        .directory()
        .create()
        .await
        .unwrap();

    let start = engine.start_sandbox(&name).await;
    let error = start.err().expect("typed disk must not accept a directory");
    assert!(
        error
            .to_string()
            .contains("but this sandbox requested disk"),
        "{error}"
    );

    engine.remove_sandbox(&name).await.unwrap();
    engine.remove_volume(&volume_name).await.unwrap();
}

// L-02: Overlay filesystem persists across stop/start (apt installs survive)
#[tokio::test]
#[serial]
async fn test_overlay_persists_across_stop_start() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-overlay");

    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();
    handle
        .exec(
            "bash",
            &["-c", "echo overlay-test > /etc/test-overlay.txt"],
            Some(timeout()),
        )
        .await
        .unwrap();
    handle.stop().await.unwrap();

    let handle2 = engine.start_sandbox(&name).await.unwrap();
    let r = handle2
        .exec(
            "bash",
            &["-c", "cat /etc/test-overlay.txt"],
            Some(timeout()),
        )
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.contains("overlay-test"),
        "overlay file should persist: {}",
        r.stdout
    );
    handle2.kill().await.ok();
    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

// L-04, L-06: Kill sandbox, remove and recreate with new name
#[tokio::test]
#[serial]
async fn test_lifecycle_kill_remove_and_recreate() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-rm");

    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();
    handle
        .exec("bash", &["-c", "echo first"], Some(timeout()))
        .await
        .unwrap();

    // L-06: Kill sandbox
    let kill_result = handle.kill().await;
    assert!(
        kill_result.is_ok(),
        "kill should succeed: {:?}",
        kill_result.err()
    );

    // L-04: Remove and recreate
    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
    let name2 = unique_name("test-rm2");
    let handle2 = engine.create_sandbox(&name2, &make_opts()).await;
    assert!(
        handle2.is_ok(),
        "create new sandbox should succeed: {:?}",
        handle2.err()
    );
    let handle2 = handle2.unwrap();
    let r = handle2
        .exec("bash", &["-c", "echo second"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("second"));
    handle2.kill().await.unwrap();

    common::cleanup_sandbox(&engine, &name2, &make_opts()).await;
}

// Regression: the existing client Session, root disk and data volume survive
// repeated idle reclamation; a running detached VM can be adopted by a new host.
#[tokio::test]
#[serial]
async fn test_existing_session_wakes_and_host_adopts_running_vm() {
    let engine = Arc::new(MsbEngine::new());
    let name = unique_name("test-lifecycle-audit");
    let mgr = SessionManager::new(engine.clone(), 1, make_opts());
    let session = mgr.get_or_create(&name).await.unwrap();
    session.exec("echo root-marker > /etc/lifecycle-marker; echo data-marker > /workspace/lifecycle-marker", timeout()).await.unwrap();
    let config = agent_engine::config::LifecycleConfig {
        session_timeout_secs: 1,
        idle_timeout_secs: 2,
        check_interval_secs: 1,
    };
    let valid = [name.clone()].into_iter().collect();
    for _ in 0..3 {
        let old = std::time::Instant::now() - Duration::from_secs(5);
        session.set_last_request(old).await;
        session.set_last_work(old).await;
        mgr.check_and_reclaim(&config, &valid).await.unwrap();
        assert_eq!(
            session.workspace_status().await,
            agent_engine::engine::WorkspaceStatus::Stopped
        );
        let output = session
            .exec(
                "cat /etc/lifecycle-marker /workspace/lifecycle-marker",
                timeout(),
            )
            .await
            .unwrap();
        assert!(output.stdout.contains("root-marker"));
        assert!(output.stdout.contains("data-marker"));
    }
    let new_host = SessionManager::new(engine.clone(), 1, make_opts());
    new_host.adopt_running(&valid).await.unwrap();
    let adopted = new_host.get_session(&name).await.unwrap();
    assert!(adopted
        .exec("cat /workspace/lifecycle-marker", timeout())
        .await
        .unwrap()
        .stdout
        .contains("data-marker"));
    new_host.stop_all().await;
    mgr.remove(&name).await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_new_manager_reopens_stopped_workspace_without_recreating_data() {
    let engine = Arc::new(MsbEngine::new());
    let name = unique_name("test-host-restart");
    let first = SessionManager::new(engine.clone(), 1, make_opts());
    first
        .get_or_create(&name)
        .await
        .unwrap()
        .exec("echo retained > /workspace/host-restart", timeout())
        .await
        .unwrap();
    first.stop_all().await;
    let second = SessionManager::new(engine.clone(), 1, make_opts());
    let session = second.get_session(&name).await.unwrap();
    let result = session
        .exec("cat /workspace/host-restart", timeout())
        .await
        .unwrap();
    assert!(result.stdout.contains("retained"));
    second.remove(&name).await.unwrap();
}
