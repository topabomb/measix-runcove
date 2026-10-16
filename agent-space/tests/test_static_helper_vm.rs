mod common;
use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, ManagedLaunch, ManagedService, VolumeSpec, WorkspaceOpts};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn directory_helper_runs_after_install_and_sees_file_changes() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = "test-static-helper";
    let opts = WorkspaceOpts {
        image: "nikolaik/python-nodejs:python3.12-nodejs22-slim".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![VolumeSpec::disk("/workspace", "ws", 128)],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    };
    let handle = engine.create_sandbox(name, &opts).await.unwrap();
    handle.exec("bash", &["-lc", "mkdir -p /workspace/report && printf '<h1>one</h1>' > /workspace/report/index.html"], Some(Duration::from_secs(10))).await.unwrap();
    let binary = common::static_helper_binary();
    let pid = handle
        .start_managed(&ManagedLaunch::StaticHttp {
            host_binary: binary.clone(),
            root: "/workspace/report".into(),
            file: false,
        })
        .await
        .unwrap();
    let port = pid.port;
    let mut tcp = handle.connect_guest_tcp(port).await.unwrap();
    tcp.writer
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), tcp.reader.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&buf).contains("one"));
    handle
        .exec(
            "bash",
            &[
                "-lc",
                "printf '<h1>two</h1>' > /workspace/report/index.html",
            ],
            Some(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    let mut tcp = handle.connect_guest_tcp(port).await.unwrap();
    tcp.writer
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    buf.clear();
    tokio::time::timeout(Duration::from_secs(5), tcp.reader.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&buf).contains("two"));
    handle
        .exec(
            "bash",
            &[
                "-lc",
                "cp /opt/agent-space/bin/agent-space-static-http /tmp/user-copy",
            ],
            Some(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    handle.exec("bash", &["-lc", "setsid /tmp/user-copy directory /workspace/report >/tmp/user.ready 2>/tmp/user.err < /dev/null & echo started"], Some(Duration::from_secs(10))).await.unwrap();
    handle.exec("bash", &["-lc", "for d in /proc/[0-9]*; do pid=${d##*/}; IFS= read -r -d '' cmd < $d/cmdline || true; case $cmd in /tmp/user-copy) echo $pid > /tmp/user.pid; exit 0;; esac; done"], Some(Duration::from_secs(5))).await.unwrap();
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let ready = handle
            .exec(
                "bash",
                &["-lc", "cat /tmp/user.pid /tmp/user.ready"],
                Some(Duration::from_secs(5)),
            )
            .await
            .unwrap();
        if ready.stdout.contains("READY") {
            break;
        }
    }
    handle.stop_managed(&pid).await.unwrap();
    let pid = pid.pid;
    let check = format!("kill -0 $(cat /tmp/user.pid) && echo user-alive || echo user-dead; kill -0 {pid} && echo helper-alive || echo helper-dead; echo PID=$(cat /tmp/user.pid) HELPER={pid}");
    let after = handle
        .exec("bash", &["-lc", &check], Some(Duration::from_secs(5)))
        .await
        .unwrap();
    assert!(after.stdout.contains("user-alive"), "{}", after.stdout);
    assert!(after.stdout.contains("helper-dead"), "{}", after.stdout);
    engine.remove_sandbox(name).await.unwrap();
    engine.remove_volume(&format!("{name}-ws")).await.unwrap();
}

#[tokio::test]
async fn restarted_owner_reclaims_only_marked_helper() {
    common::cleanup_test_sandboxes().await;
    let engine = std::sync::Arc::new(MsbEngine::new());
    let name = "test-helper-reclaim";
    let opts = WorkspaceOpts {
        image: "nikolaik/python-nodejs:python3.12-nodejs22-slim".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![VolumeSpec::disk("/workspace", "ws", 128)],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    };
    let handle = engine.create_sandbox(name, &opts).await.unwrap();
    handle
        .exec(
            "bash",
            &[
                "-lc",
                "mkdir -p /workspace/report && printf ok > /workspace/report/index.html",
            ],
            Some(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    let binary = common::static_helper_binary();
    let first_pid = handle
        .start_managed(&ManagedLaunch::StaticHttp {
            host_binary: binary.clone(),
            root: "/workspace/report".into(),
            file: false,
        })
        .await
        .unwrap();

    let second_pid = handle
        .start_managed(&ManagedLaunch::StaticHttp {
            host_binary: binary.clone(),
            root: "/workspace/report".into(),
            file: false,
        })
        .await
        .unwrap();

    assert!(
        handle.managed_alive(&first_pid).await.unwrap(),
        "old executable inode remains owned after atomic replacement"
    );
    assert_ne!(first_pid.executable_inode, second_pid.executable_inode);
    let mut wrong = first_pid.clone();
    wrong.start_ticks += 1;
    assert!(
        handle.stop_managed(&wrong).await.is_err(),
        "stale process identity must not erase the real instance record"
    );
    assert!(handle.managed_alive(&first_pid).await.unwrap());
    assert_eq!(
        handle
            .list_managed(ManagedService::StaticHttp)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_ne!(
        first_pid, second_pid,
        "each publication needs its own helper identity"
    );
    handle
        .exec(
            "bash",
            &[
                "-lc",
                "cp /opt/agent-space/bin/agent-space-static-http /tmp/user-copy",
            ],
            Some(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    handle.exec("bash", &["-lc", "setsid /tmp/user-copy directory /workspace/report >/tmp/user.ready 2>/tmp/user.err < /dev/null & echo started"], Some(Duration::from_secs(10))).await.unwrap();
    microsandbox::Sandbox::get(name)
        .await
        .unwrap()
        .pause()
        .await
        .unwrap();
    drop(handle);
    let manager = agent_engine::session::SessionManager::new(engine.clone(), 2, opts);
    manager
        .adopt_running(&[name.to_string()].into_iter().collect())
        .await
        .unwrap();
    assert!(
        matches!(
            engine.get_sandbox(name).await,
            Err(agent_engine::engine::EngineError::SandboxPaused(_))
        ),
        "Host adoption must not wake a paused VM just to clean helpers"
    );
    manager.get_or_create(name).await.unwrap();
    let adopted = engine.get_sandbox(name).await.unwrap();
    let check = adopted.exec("bash", &["-lc", "for d in /proc/[0-9]*; do IFS= read -r -d '' cmd < $d/cmdline || true; case $cmd in /opt/agent-space/bin/agent-space-static-http) echo helper-alive;; /tmp/user-copy) echo user-alive;; esac; done"], Some(Duration::from_secs(5))).await.unwrap();
    assert!(!check.stdout.contains("helper-alive"), "{}", check.stdout);
    assert!(check.stdout.contains("user-alive"), "{}", check.stdout);
    assert!(adopted
        .list_managed(ManagedService::StaticHttp)
        .await
        .unwrap()
        .is_empty());
    manager.stop_all().await;
    engine.remove_sandbox(name).await.unwrap();
    engine.remove_volume(&format!("{name}-ws")).await.unwrap();
}
