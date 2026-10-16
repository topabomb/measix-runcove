mod common;

use agent_engine::config::OutboundProxyConfig as OutboundProxy;
use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, AgentHandle, NetworkAccess, VolumeSpec, WorkspaceOpts};
use microsandbox::Sandbox;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

fn opts() -> WorkspaceOpts {
    WorkspaceOpts {
        image: "nikolaik/python-nodejs:python3.12-nodejs22-slim".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![VolumeSpec::disk("/workspace", "ws", 128)],
        network_access: NetworkAccess::PublicOnly,
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}

async fn exec(handle: &Arc<dyn AgentHandle>, command: &str) -> String {
    let output = handle
        .exec("bash", &["-c", command], Some(Duration::from_secs(15)))
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    output.stdout
}

#[tokio::test]
async fn convergent_activation_and_stale_handle_cannot_stop_replacement() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = "test-upgrade-identity";
    let original = engine.create_sandbox(name, &opts()).await.unwrap();
    let stale = Sandbox::get(name).await.unwrap();
    let identity = stale.id().to_string();
    exec(
        &original,
        "echo root-marker > /root/marker; echo volume-marker > /workspace/marker",
    )
    .await;
    original.stop().await.unwrap();
    let (a, b) = tokio::join!(engine.start_sandbox(name), engine.start_sandbox(name));
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(Sandbox::get(name).await.unwrap().id().to_string(), identity);
    assert_eq!(
        exec(&a, "cat /root/marker /workspace/marker").await.trim(),
        "root-marker\nvolume-marker"
    );
    assert_eq!(exec(&b, "echo connected").await.trim(), "connected");
    for _ in 0..3 {
        // Exercise an adopted (non-owner) connection as well as an owning one.
        let attached = engine.get_sandbox(name).await.unwrap();
        attached.stop().await.unwrap();
        let (a, b) = tokio::join!(engine.start_sandbox(name), engine.start_sandbox(name));
        assert_eq!(
            exec(&a.unwrap(), "cat /workspace/marker").await.trim(),
            "volume-marker"
        );
        assert_eq!(
            exec(&b.unwrap(), "cat /root/marker").await.trim(),
            "root-marker"
        );
        assert_eq!(Sandbox::get(name).await.unwrap().id().to_string(), identity);
    }
    engine.remove_sandbox(name).await.unwrap();
    let replacement = engine.create_sandbox(name, &opts()).await.unwrap();
    assert_ne!(Sandbox::get(name).await.unwrap().id().to_string(), identity);
    assert!(original
        .stop()
        .await
        .unwrap_err()
        .to_string()
        .contains("replaced"));
    assert!(original
        .kill()
        .await
        .unwrap_err()
        .to_string()
        .contains("replaced"));
    assert!(matches!(
        stale.destroy().await,
        Err(microsandbox::MicrosandboxError::SandboxReplaced { .. })
    ));
    assert_eq!(
        exec(&replacement, "cat /workspace/marker").await.trim(),
        "volume-marker"
    );
    engine.remove_sandbox(name).await.unwrap();
    engine.remove_volume(&format!("{name}-ws")).await.unwrap();
}

#[tokio::test]
async fn single_file_mount_does_not_expose_host_siblings() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = "test-upgrade-file-mount";
    let directory = std::env::temp_dir().join(format!("msb-file-mount-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("helper"), "allowed-file").unwrap();
    std::fs::write(directory.join("host-only"), "must-stay-on-host").unwrap();
    let mut config = opts();
    config.volumes.push(VolumeSpec::bind(
        "/opt/mounted/helper",
        directory.join("helper").to_str().unwrap(),
        true,
    ));
    let handle = engine.create_sandbox(name, &config).await.unwrap();
    assert_eq!(exec(&handle, "cat /opt/mounted/helper; test ! -e /opt/mounted/host-only; ! echo overwrite > /opt/mounted/helper").await.trim(), "allowed-file");
    std::fs::write(directory.join("helper.new"), "updated-file").unwrap();
    std::fs::rename(directory.join("helper.new"), directory.join("helper")).unwrap();
    assert_eq!(
        exec(&handle, "cat /opt/mounted/helper").await,
        "updated-file"
    );
    engine.remove_sandbox(name).await.unwrap();
    engine.remove_volume(&format!("{name}-ws")).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.join("helper")).unwrap(),
        "updated-file"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

// A controlled proxy replies itself instead of opening an external connection.
// This proves traffic is routed through SOCKS, independent of Internet access.
async fn socks5_reply(listener: TcpListener) {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut header = [0; 2];
    socket.read_exact(&mut header).await.unwrap();
    assert_eq!(header[0], 5);
    let mut methods = vec![0; header[1] as usize];
    socket.read_exact(&mut methods).await.unwrap();
    assert!(methods.contains(&0));
    socket.write_all(&[5, 0]).await.unwrap();
    let mut connect = [0; 10];
    socket.read_exact(&mut connect).await.unwrap();
    assert_eq!(&connect[..8], &[5, 1, 0, 1, 1, 1, 1, 1]);
    socket
        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
        .await
        .unwrap();
    let mut request = [0; 1024];
    assert!(socket.read(&mut request).await.unwrap() > 0);
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nvia-socks5")
        .await
        .unwrap();
}

#[tokio::test]
async fn native_socks5_routes_traffic_and_policy_still_blocks() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = "test-upgrade-socks5";
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut config = opts();
    config.outbound_proxy = Some(OutboundProxy::Socks5 {
        address,
        credentials: None,
    });
    let handle = engine.create_sandbox(name, &config).await.unwrap();
    let proxy = tokio::spawn(socks5_reply(listener));
    assert_eq!(
        exec(
            &handle,
            "curl --noproxy '*' -fsS --max-time 5 http://1.1.1.1:18080/"
        )
        .await,
        "via-socks5"
    );
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .unwrap()
        .unwrap();
    // A missing proxy must fail, never silently bypass the configured route.
    exec(
        &handle,
        "! curl --noproxy '*' -fsS --max-time 3 http://1.1.1.1:18080/",
    )
    .await;
    handle.stop().await.unwrap();
    let listener = TcpListener::bind(address).await.unwrap();
    let restarted = engine.start_sandbox(name).await.unwrap();
    let proxy = tokio::spawn(socks5_reply(listener));
    assert_eq!(
        exec(
            &restarted,
            "curl --noproxy '*' -fsS --max-time 5 http://1.1.1.1:18080/"
        )
        .await,
        "via-socks5"
    );
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .unwrap()
        .unwrap();
    engine.remove_sandbox(name).await.unwrap();
    config.network_access = NetworkAccess::None;
    let listener = TcpListener::bind(address).await.unwrap();
    let blocked = engine.create_sandbox(name, &config).await.unwrap();
    exec(
        &blocked,
        "! curl --noproxy '*' -fsS --max-time 3 http://1.1.1.1:18080/",
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err()
    );
    engine.remove_sandbox(name).await.unwrap();
    engine.remove_volume(&format!("{name}-ws")).await.unwrap();
}

#[tokio::test]
async fn native_socks4_routes_tcp() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = "test-upgrade-socks4";
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = opts();
    config.outbound_proxy = Some(OutboundProxy::Socks4 {
        address: listener.local_addr().unwrap(),
        user_id: Some("workspace".into()),
    });
    let handle = engine.create_sandbox(name, &config).await.unwrap();
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 18];
        socket.read_exact(&mut request).await.unwrap();
        assert_eq!(&request[..2], &[4, 1]);
        assert_eq!(&request[4..8], &[1, 1, 1, 1]);
        assert_eq!(&request[8..], b"workspace\0");
        socket.write_all(&[0, 90, 0, 0, 0, 0, 0, 0]).await.unwrap();
        let mut http = [0; 1024];
        assert!(socket.read(&mut http).await.unwrap() > 0);
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nvia-socks4",
            )
            .await
            .unwrap();
    });
    assert_eq!(
        exec(
            &handle,
            "curl --noproxy '*' -fsS --max-time 5 http://1.1.1.1:18080/"
        )
        .await,
        "via-socks4"
    );
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .unwrap()
        .unwrap();
    engine.remove_sandbox(name).await.unwrap();
    engine.remove_volume(&format!("{name}-ws")).await.unwrap();
}

#[tokio::test]
async fn native_socks5_auth_udp_keeps_password_on_host() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = "test-upgrade-socks5-udp";
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = opts();
    // Unique test-only variable; this test binary must run serially with VM tests.
    std::env::set_var("MSB_UPGRADE_PROXY_PASSWORD", "probe-pass");
    config.outbound_proxy = Some(serde_json::from_value(serde_json::json!({
        "protocol": "socks5", "address": listener.local_addr().unwrap().to_string(),
        "credentials": {"username":"workspace", "password":{"kind":"env", "var":"MSB_UPGRADE_PROXY_PASSWORD"}}
    })).unwrap());
    let handle = engine.create_sandbox(name, &config).await.unwrap();
    std::env::remove_var("MSB_UPGRADE_PROXY_PASSWORD");
    let persisted =
        serde_json::to_string(&Sandbox::get(name).await.unwrap().config().unwrap()).unwrap();
    assert!(!persisted.contains("probe-pass"));
    exec(&handle, "test -z \"${MSB_UPGRADE_PROXY_PASSWORD+x}\"").await;
    let proxy = tokio::spawn(async move {
        let (mut control, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 2];
        control.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting[0], 5);
        let mut methods = vec![0; greeting[1] as usize];
        control.read_exact(&mut methods).await.unwrap();
        assert!(methods.contains(&2));
        control.write_all(&[5, 2]).await.unwrap();
        let mut auth = [0; 22];
        control.read_exact(&mut auth).await.unwrap();
        assert_eq!(&auth, b"\x01\x09workspace\x0aprobe-pass");
        control.write_all(&[1, 0]).await.unwrap();
        let mut associate = [0; 10];
        control.read_exact(&mut associate).await.unwrap();
        assert_eq!(&associate[..4], &[5, 3, 0, 1]);
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = udp.local_addr().unwrap().port().to_be_bytes();
        control
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, port[0], port[1]])
            .await
            .unwrap();
        let mut data = [0; 1024];
        let (len, peer) = udp.recv_from(&mut data).await.unwrap();
        assert_eq!(&data[..8], &[0, 0, 0, 1, 1, 1, 1, 1]);
        assert_eq!(&data[10..len], b"udp-through-proxy");
        udp.send_to(&data[..len], peer).await.unwrap();
        // Keep the control connection alive until the guest receives the reply.
        tokio::time::sleep(Duration::from_millis(300)).await;
    });
    assert_eq!(exec(&handle, "python3 -c 'import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(5); s.sendto(b\"udp-through-proxy\",(\"1.1.1.1\",18080)); print(s.recv(100).decode())'").await.trim(), "udp-through-proxy");
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .unwrap()
        .unwrap();
    engine.remove_sandbox(name).await.unwrap();
    engine.remove_volume(&format!("{name}-ws")).await.unwrap();
}
