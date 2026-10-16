mod common;

use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, VolumeSpec, WorkspaceOpts};
use agent_engine::session::SessionManager;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn existing_tmpfs_grows_preserving_files_and_open_handles_and_reapplies_after_restart() {
    common::cleanup_test_sandboxes().await;
    let engine = Arc::new(MsbEngine::new());
    let name = "test-tmpfs-growth";
    let old = WorkspaceOpts {
        image: "nikolaik/python-nodejs:python3.12-nodejs22-slim".into(),
        cpus: 1,
        memory_mib: 768,
        shell: "/bin/bash".into(),
        volumes: vec![
            VolumeSpec::disk("/workspace", "ws", 128),
            VolumeSpec::tmpfs("/tmp", 128),
        ],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    };
    let handle = engine.create_sandbox(name, &old).await.unwrap();
    let setup = handle.exec("bash", &["-c", r#"
set -e
printf original > /tmp/growth-marker
printf persistent > /workspace/growth-marker
chmod 640 /tmp/growth-marker
bash -c 'exec 8<>/tmp/growth-marker; echo ready >/tmp/growth-ready; exec sleep 120' >/tmp/growth-log 2>&1 &
echo $! > /tmp/growth-pid
for i in $(seq 1 50); do test -f /tmp/growth-ready && exit 0; sleep .1; done
exit 1
"#], Some(Duration::from_secs(10))).await.unwrap();
    assert_eq!(setup.exit_code, 0, "{}", setup.stderr);
    let inspect = r#"set -e; stat -c '%d:%i:%a:%u:%g' /tmp /tmp/growth-marker; kill -0 $(cat /tmp/growth-pid); cat /proc/$(cat /tmp/growth-pid)/fd/8"#;
    let before = handle
        .exec("bash", &["-c", inspect], Some(Duration::from_secs(10)))
        .await
        .unwrap();
    handle.grow_tmpfs("/tmp", 512).await.unwrap();
    handle.grow_tmpfs("/tmp", 64).await.unwrap();
    assert!(handle.grow_tmpfs("/workspace", 512).await.is_err());
    assert!(handle.grow_tmpfs("/tmp", 8192).await.is_err());
    let after = handle
        .exec("bash", &["-c", inspect], Some(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(after.exit_code, 0, "{}", after.stderr);
    assert_eq!(before.stdout, after.stdout);
    let mut desired = old.clone();
    desired.volumes[1].size_mib = 512;
    let manager = SessionManager::new(engine.clone(), 1, desired.clone());
    manager
        .adopt_running(&HashSet::from([name.to_string()]))
        .await
        .unwrap();
    let session = manager.get_or_create(name).await.unwrap();
    let capacity = "python3 -c 'import os; s=os.statvfs(\"/tmp\"); print(s.f_blocks*s.f_frsize)'";
    assert_eq!(
        session
            .exec(capacity, Duration::from_secs(10))
            .await
            .unwrap()
            .stdout
            .trim(),
        "536870912"
    );
    manager.stop_all().await;
    let manager = SessionManager::new(engine.clone(), 1, desired);
    let session = manager.get_or_create(name).await.unwrap();
    let result = session
        .exec(
            &format!("{capacity}; cat /workspace/growth-marker; test ! -e /tmp/growth-marker"),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert_eq!(result.stdout.trim(), "536870912\npersistent");
    manager.stop_all().await;
    common::cleanup_sandbox(&engine, name, &old).await;
}
