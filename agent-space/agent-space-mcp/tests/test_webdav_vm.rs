#[path = "../../tests/common.rs"]
mod common;
use agent_engine::{
    engine::{msb::MsbEngine, AgentEngine, ManagedService, VolumeSpec, WorkspaceOpts},
    session::SessionManager,
    state::StateStore,
};
use agent_space_mcp::webdav::{self, DavState};
use futures::FutureExt;
use reqwest::{Client, Method, StatusCode};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn real_host_dav_transfer_lock_bash_concurrency_and_restart() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("agent_engine=debug,agent_space_mcp=debug")
        .try_init();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let binary = root.join("target/debug/agent-space-webdav");
    common::verify_helper_identity(&binary, root, "webdav");
    common::cleanup_test_sandboxes().await;
    let dir = tempfile::tempdir().unwrap();
    StateStore::init(dir.path()).unwrap();
    let store = Arc::new(StateStore::open(dir.path()).unwrap());
    let (user, mcp) = store
        .create_user("alice", "test-dav-host", &["ws"])
        .unwrap();
    let key = store
        .set_dav_token(
            "alice",
            &user.agent_space_id,
            "vm-integration-0123456789abcdef0123456789abcdef",
        )
        .unwrap();
    let engine = Arc::new(MsbEngine::new());
    let opts = WorkspaceOpts {
        image: "nikolaik/python-nodejs:python3.12-nodejs22-slim".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![
            VolumeSpec::disk("/workspace", "ws", 256),
            VolumeSpec::tmpfs("/tmp", 64),
        ],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    };
    let sessions = Arc::new(SessionManager::new(engine.clone(), 1, opts.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = webdav::router(DavState {
        sessions: sessions.clone(),
        state: store.clone(),
        origin: url::Url::parse(&origin).unwrap(),
        helper_binary: binary.to_string_lossy().into_owned(),
        startup_timeout: Duration::from_secs(60),
        io_idle_timeout: Duration::from_secs(10),
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = Client::builder()
        .timeout(Duration::from_secs(70))
        .build()
        .unwrap();
    let base = format!("{origin}/u/alice/dav/");
    let request = |method: &str, path: &str| {
        client
            .request(
                Method::from_bytes(method.as_bytes()).unwrap(),
                format!("{base}{path}"),
            )
            .basic_auth("alice", Some(&key.token))
    };
    let outcome = std::panic::AssertUnwindSafe(async {
        assert_eq!(client.get(format!("{base}file")).bearer_auth(&mcp.token).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(sessions.active_space_count(), 0, "invalid credentials must not activate a VM");
        // The first authorized request creates the workspace and starts DAV.
        assert_eq!(request("PUT", "file").body("old").send().await.unwrap().status(), StatusCode::CREATED);
        let session = sessions.get_session(&user.sandbox_name).await.unwrap();
        let handle = engine.get_sandbox(&user.sandbox_name).await.unwrap();
        let initial = handle.list_managed(ManagedService::Webdav).await.unwrap();
        assert_eq!(initial.len(), 1);
        // The SDK permits 64 concurrent bulk correlations. Sequential requests
        // must release their slots rather than eventually exhausting that pool.
        for index in 0..80 {
            let response = request("HEAD", "file").send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "sequential request {index}");
            response.bytes().await.unwrap();
        }
        let mut hash = Sha256::new();
        let data: Vec<u8> = (0..64*1024*1024).map(|i| (i % 251) as u8).collect();
        hash.update(&data);
        assert_eq!(request("PUT", "large.bin").body(data).send().await.unwrap().status(), StatusCode::CREATED);
        let mut response = request("GET", "large.bin").send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-disposition"], "attachment");
        let mut received = Sha256::new();
        let mut length = 0;
        while let Some(bytes) = response.chunk().await.unwrap_or_else(|error| panic!("download failed after {length} bytes: {error:?}")) { received.update(&bytes); length += bytes.len(); }
        assert_eq!(length, 64*1024*1024);
        assert_eq!(received.finalize(), hash.finalize());
        // Real chunked transfer spans the former 30-second execution deadline.
        // Only DAV activity protects this VM while its old clocks are expired.
        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let socket = tokio::net::TcpStream::connect(origin.trim_start_matches("http://")).await.unwrap();
            let (mut reader, mut writer) = socket.into_split();
            writer.write_all(format!("PUT /u/alice/dav/slow.bin HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n", origin.trim_start_matches("http://"), key.token).as_bytes()).await.unwrap();
            let chunk = vec![b's'; 8192];
            let begun = std::time::Instant::now();
            let lifecycle = agent_engine::config::LifecycleConfig { session_timeout_secs: 1, idle_timeout_secs: 2, check_interval_secs: 1 };
            let valid = [user.sandbox_name.clone()].into_iter().collect();
            for index in 0..64 {
                writer.write_all(b"2000\r\n").await.unwrap();
                writer.write_all(&chunk).await.unwrap();
                writer.write_all(b"\r\n").await.unwrap();
                tokio::time::sleep(Duration::from_millis(500)).await;
                if index % 16 == 15 {
                    let old = std::time::Instant::now() - Duration::from_secs(5);
                    session.set_last_request(old).await;
                    session.set_last_work(old).await;
                    sessions.check_and_reclaim(&lifecycle, &valid).await.unwrap();
                    assert_eq!(session.workspace_status().await, agent_engine::engine::WorkspaceStatus::Running);
                }
            }
            writer.write_all(b"0\r\n\r\n").await.unwrap();
            let mut reply = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut reply)).await.unwrap().unwrap();
            assert!(String::from_utf8_lossy(&reply).starts_with("HTTP/1.1 201"));
            assert!(begun.elapsed() > Duration::from_secs(30));
            assert_eq!(request("GET", "slow.bin").send().await.unwrap().bytes().await.unwrap().as_ref(), vec![b's'; 64*8192]);
        }
        let response = request("GET", "large.bin").header("range", "bytes=250-253").send().await.unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.bytes().await.unwrap().as_ref(), &[250,0,1,2]);
        let response = request("PROPFIND", "").header("depth", "1").send().await.unwrap();
        assert_eq!(response.status(), StatusCode::MULTI_STATUS);
        let xml = response.text().await.unwrap();
        assert!(xml.contains("large.bin") && !xml.contains(".agent-space-upload"), "{xml}");
        let response = request("LOCK", "file").header("content-type", "application/xml")
            .body(r#"<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner>vm-test</D:owner></D:lockinfo>"#)
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let token = response.headers()["lock-token"].to_str().unwrap().to_owned();
        response.bytes().await.unwrap();
        assert_eq!(request("PUT", "file").body("forbidden").send().await.unwrap().status(), StatusCode::LOCKED);
        assert_eq!(request("PUT", "file").header("if", format!("<{base}file> ({token})")).body("locked update").send().await.unwrap().status(), StatusCode::NO_CONTENT);
        assert_eq!(request("UNLOCK", "file").header("lock-token", token).send().await.unwrap().status(), StatusCode::NO_CONTENT);
        assert_eq!(request("COPY", "file").header("destination", format!("{base}copied")).send().await.unwrap().status(), StatusCode::CREATED);
        assert_eq!(request("MOVE", "copied").header("destination", "/u/alice/dav/moved").send().await.unwrap().status(), StatusCode::CREATED);
        assert_eq!(request("GET", "moved").send().await.unwrap().text().await.unwrap(), "locked update");
        assert_eq!(request("MOVE", "moved").header("destination", format!("{origin}/u/bob/dav/stolen")).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        // The bash command only ends once an HTTP request writes its release file.
        // A DAV request queued behind bash would time out instead of passing.
        let bash_session = session.clone();
        let bash = tokio::spawn(async move { bash_session.exec("printf ready > /workspace/bash-ready; while [ ! -e /workspace/bash-release ]; do sleep .05; done", Duration::from_secs(20)).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if request("GET", "bash-ready").send().await.unwrap().status() == StatusCode::OK { break; }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }).await.expect("DAV must run concurrently with bash");
        assert!(!bash.is_finished());
        assert_eq!(request("PUT", "bash-release").body("done").send().await.unwrap().status(), StatusCode::CREATED);
        assert_eq!(bash.await.unwrap().unwrap().exit_code, 0);
        session.unpublish("test").await;
        assert_eq!(request("GET", "file").send().await.unwrap().text().await.unwrap(), "locked update");
        assert_eq!(handle.list_managed(ManagedService::Webdav).await.unwrap(), initial, "preview off must preserve the DAV process");
        // Rotate while the guest has actually staged part of an upload.
        let socket = tokio::net::TcpStream::connect(origin.trim_start_matches("http://")).await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        writer.write_all(format!("PUT /u/alice/dav/file HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n7\r\npartial\r\n", origin.trim_start_matches("http://"), key.token).as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let result = handle.exec("bash", &["-c", "find /workspace/.agent-space/uploads -mindepth 2 -type f ! -name boot-id | wc -l"], Some(Duration::from_secs(2))).await.unwrap();
                if result.stdout.trim() == "1" { break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("upload reached guest staging");
        let next = store.rotate_dav_token("alice", &user.agent_space_id).unwrap();
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), reader.read_to_end(&mut reply)).await.expect("revocation closes live HTTP upload").unwrap();
        drop(writer);
        assert!(store.authenticate(&mcp.token).is_ok());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let result = handle.exec("bash", &["-c", "test \"$(cat /workspace/file)\" = 'locked update' && test -z \"$(find /workspace/.agent-space/uploads -mindepth 2 -type f ! -name boot-id)\""], Some(Duration::from_secs(2))).await.unwrap();
                if result.exit_code == 0 { break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("cancelled upload preserves old file and removes staging");
        let abandoned = format!("/workspace/.agent-space/uploads/{}/00112233-4455-6677-8899-aabbccddeeff", initial[0].instance_id);
        let result = handle.exec("bash", &["-c", &format!("printf abandoned > {abandoned}; sync")], Some(Duration::from_secs(2))).await.unwrap();
        assert_eq!(result.exit_code, 0);
        sessions.stop_workspace(&user.sandbox_name).await.unwrap();
        let response = client.get(format!("{base}file")).bearer_auth(&next.token).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "locked update", "cold restart keeps persistent files");
        let restarted = engine.get_sandbox(&user.sandbox_name).await.unwrap();
        let result = restarted.exec("bash", &["-c", &format!("test ! -e {abandoned} && test ! -e /tmp/agent-space/{}", initial[0].instance_id)], Some(Duration::from_secs(2))).await.unwrap();
        assert_eq!(result.exit_code, 0, "reboot removes only proven old-boot staging even after /tmp records vanish");

        assert_eq!(request("GET", "file").send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    }).catch_unwind().await;
    server.abort();
    sessions.stop_all().await;
    common::cleanup_sandbox(&engine, &user.sandbox_name, &opts).await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
