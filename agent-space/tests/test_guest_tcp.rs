mod common;

use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, AgentHandle, VolumeSpec, WorkspaceOpts};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn opts() -> WorkspaceOpts {
    WorkspaceOpts {
        image: "nikolaik/python-nodejs:python3.12-nodejs22-slim".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![VolumeSpec::disk("/workspace", "ws", 128)],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}

async fn exec(handle: &Arc<dyn AgentHandle>, command: &str) -> String {
    let output = handle
        .exec("bash", &["-c", command], Some(Duration::from_secs(20)))
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0, "stderr={}", output.stderr);
    output.stdout
}

#[tokio::test]
async fn guest_tcp_carries_http_and_can_be_cancelled() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = "test-guest-tcp";
    let handle = engine.create_sandbox(name, &opts()).await.unwrap();
    exec(&handle, r#"python3 - << 'PY' >/tmp/guest-http.log 2>&1 &
import socket, pathlib
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 8765)); s.listen(1)
while True:
    c, _ = s.accept()
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = c.recv(1024)
        if not chunk:
            break
        data += chunk
    if not data:
        c.close(); continue
    if b"/slow" in data.split(b"\r\n", 1)[0]:
        pathlib.Path('/tmp/tcp-slow-seen').write_text('seen')
        c.settimeout(5)
        if c.recv(1) == b'':
            pathlib.Path('/tmp/tcp-cancelled').write_text('closed')
        c.close(); continue
    if b"/half-close" in data.split(b"\r\n", 1)[0]:
        while c.recv(1024):
            pass
    body = b"hello-from-guest"
    c.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\nConnection: close\r\n\r\n%s" % (len(body), body))
    c.close()
PY
for i in 1 2 3 4 5 6 7 8 9 10; do python3 -c 'import socket;s=socket.create_connection(("127.0.0.1",8765),1);s.close()' && exit 0; sleep 0.2; done; exit 1"#).await;

    let mut tcp = handle.connect_guest_tcp(8765).await.unwrap();
    tcp.writer
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(8),
        tcp.reader.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    let text = String::from_utf8_lossy(&response);
    assert!(text.contains("hello-from-guest"), "response was: {text}");
    drop(tcp);

    // A server can require write EOF before producing its response. Sending a
    // cancellation instead of H2G BulkFinish used to destroy this connection.
    let mut tcp = handle.connect_guest_tcp(8765).await.unwrap();
    tcp.writer
        .write_all(b"GET /half-close HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    tcp.writer.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(8),
        tcp.reader.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(String::from_utf8_lossy(&response).contains("hello-from-guest"));
    drop(tcp);
    for _ in 0..40 {
        let mut tcp = handle.connect_guest_tcp(8765).await.unwrap();
        tcp.writer
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(8),
            tcp.reader.read_to_end(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(String::from_utf8_lossy(&response).contains("hello-from-guest"));
    }

    let mut slow = handle.connect_guest_tcp(8765).await.unwrap();
    slow.writer
        .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    exec(
        &handle,
        "for i in $(seq 1 60); do test -f /tmp/tcp-slow-seen && exit 0; sleep .05; done; exit 1",
    )
    .await;
    drop(slow);
    let closed = exec(&handle, "for i in $(seq 1 60); do test -f /tmp/tcp-cancelled && cat /tmp/tcp-cancelled && exit 0; sleep .05; done; exit 1").await;
    assert_eq!(closed, "closed", "guest peer must observe cancellation EOF");

    engine.remove_sandbox(name).await.unwrap();
    engine.remove_volume(&format!("{name}-ws")).await.unwrap();
}
