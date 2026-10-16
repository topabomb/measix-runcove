use agent_engine::{
    engine::{
        mock::{MockEngine, TEST_DAV_TRUNCATE, TEST_HEADERS_SEEN, TEST_HTTP_DISCONNECTED},
        WorkspaceOpts,
    },
    session::SessionManager,
    state::{IssuedToken, StateStore, UserRecord},
};
use agent_space_mcp::webdav::{self, DavState};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use base64::Engine;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<StateStore>,
    sessions: Arc<SessionManager>,
    engine: Arc<MockEngine>,
    user: UserRecord,
    dav: IssuedToken,
    mcp: IssuedToken,
    app: Router,
}

#[tokio::test]
async fn real_http_reports_upstream_truncation_and_allows_download_progress() {
    let f = fixture_with_idle(Duration::from_millis(250));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = f.app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let begun = std::time::Instant::now();
    let response = client
        .get(format!("http://{address}/u/alice/dav/progress-response"))
        .header("host", "files.example")
        .bearer_auth(&f.dav.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"AAAAAAAAAAAA");
    assert!(begun.elapsed() > Duration::from_millis(500));
    for _ in 0..80 {
        let response = client
            .get(format!("http://{address}/u/alice/dav/truncated-response"))
            .header("host", "files.example")
            .header("range", "bytes=0-7")
            .header("if-match", "\"large\"")
            .bearer_auth(&f.dav.token)
            .send()
            .await
            .unwrap();
        // End upstream only after the client has received the declared length.
        TEST_DAV_TRUNCATE.notify_one();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()["content-length"], "8");
        assert_eq!(response.headers()["content-range"], "bytes 0-7/4294967296");
        assert_eq!(response.headers()["etag"], "\"large\"");
        assert!(
            response.bytes().await.is_err(),
            "proxy concealed truncated body"
        );
    }
    let session = f.sessions.get_session(&f.user.sandbox_name).await.unwrap();
    let old = std::time::Instant::now() - Duration::from_secs(5);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            session.set_last_request(old).await;
            session.set_last_work(old).await;
            let config = agent_engine::config::LifecycleConfig {
                session_timeout_secs: 1,
                idle_timeout_secs: 1,
                check_interval_secs: 1,
            };
            if f.sessions.collect_candidates(&config).await.len() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed response retained activity");
    server.abort();
    f.sessions.stop_all().await;
}
fn fixture() -> Fixture {
    fixture_with_idle(Duration::from_secs(5))
}
fn fixture_with_idle(idle: Duration) -> Fixture {
    fixture_with_capacity(idle, 1)
}
fn fixture_with_capacity(idle: Duration, capacity: u32) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    StateStore::init(dir.path()).unwrap();
    let store = Arc::new(StateStore::open(dir.path()).unwrap());
    let (user, mcp) = store.create_user("alice", "smcp", &["ws"]).unwrap();
    let dav = store
        .rotate_dav_token("alice", &user.agent_space_id)
        .unwrap();
    let engine = Arc::new(MockEngine::new());
    let sessions = Arc::new(SessionManager::new(
        engine.clone(),
        capacity,
        WorkspaceOpts {
            image: "mock".into(),
            cpus: 1,
            memory_mib: 128,
            shell: "/bin/bash".into(),
            volumes: vec![],
            network_access: Default::default(),
            outbound_proxy: None,
            max_tcp_connections: None,
            max_udp_connections: None,
        },
    ));
    let app = webdav::router(DavState {
        sessions: sessions.clone(),
        state: store.clone(),
        origin: url::Url::parse("https://files.example").unwrap(),
        helper_binary: "mock".into(),
        startup_timeout: Duration::from_secs(2),
        io_idle_timeout: idle,
    });
    Fixture {
        _dir: dir,
        store,
        sessions,
        engine,
        user,
        dav,
        mcp,
        app,
    }
}
fn request(method: &str, path: &str, auth: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "files.example")
        .header("authorization", auth)
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn full_capacity_rejects_only_new_workspaces_before_forwarding_and_recovers() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = fixture_with_capacity(Duration::from_secs(5), 2);
    let (second, _) = f.store.create_user("bob", "smcp", &["ws"]).unwrap();
    let (waiting, _) = f.store.create_user("charlie", "smcp", &["ws"]).unwrap();
    let waiting_dav = f
        .store
        .rotate_dav_token("charlie", &waiting.agent_space_id)
        .unwrap();
    let active_auth = format!("Bearer {}", f.dav.token);
    let waiting_auth = format!("Bearer {}", waiting_dav.token);
    let response = f
        .app
        .clone()
        .oneshot(request(
            "PROPFIND",
            "/u/alice/dav/",
            &active_auth,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    f.sessions
        .get_or_create(&second.sandbox_name)
        .await
        .unwrap();
    assert_eq!(f.sessions.active_space_count(), 2);
    for method in ["PROPFIND", "PUT"] {
        let polled = Arc::new(AtomicBool::new(false));
        let body = Body::from_stream(futures::stream::once({
            let polled = polled.clone();
            async move {
                polled.store(true, Ordering::SeqCst);
                Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"not forwarded"))
            }
        }));
        let response = f
            .app
            .clone()
            .oneshot(request(method, "/u/charlie/dav/", &waiting_auth, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["retry-after"], "1");
        assert_eq!(
            response.headers()["agent-space-error-code"],
            "workspace_capacity_exhausted"
        );
        assert!(axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap()
            .is_empty());
        assert!(
            !polled.load(Ordering::SeqCst),
            "capacity rejection must not forward request bytes"
        );
    }
    assert_eq!(f.engine.create_count.load(Ordering::SeqCst), 2);
    let response = f
        .app
        .clone()
        .oneshot(request(
            "PROPFIND",
            "/u/alice/dav/",
            &active_auth,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("agent-space-error-code"));
    axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    f.sessions
        .stop_workspace(&second.sandbox_name)
        .await
        .unwrap();
    let response = f
        .app
        .clone()
        .oneshot(request(
            "PROPFIND",
            "/u/charlie/dav/",
            &waiting_auth,
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("agent-space-error-code"));
    axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    assert_eq!(f.engine.create_count.load(Ordering::SeqCst), 3);
    f.sessions.stop_all().await;
}

#[tokio::test]
async fn failed_activation_and_draining_helpers_are_not_reported_as_full_capacity() {
    let f = fixture();
    let session = f
        .sessions
        .get_or_create(&f.user.sandbox_name)
        .await
        .unwrap();
    f.sessions
        .stop_workspace(&f.user.sandbox_name)
        .await
        .unwrap();
    f.engine.set_fail_start(true);
    let auth = format!("Bearer {}", f.dav.token);
    let response = f
        .app
        .clone()
        .oneshot(request("PROPFIND", "/u/alice/dav/", &auth, Body::empty()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    assert!(!response.headers().contains_key("agent-space-error-code"));
    f.engine.set_fail_start(false);
    let transfer = session
        .begin_dav_request(
            agent_engine::engine::ManagedLaunch::Webdav {
                host_binary: "previous-helper".into(),
                owner: f.user.agent_space_id.clone(),
                config: serde_json::json!({}),
            },
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    let response = f
        .app
        .clone()
        .oneshot(request("PROPFIND", "/u/alice/dav/", &auth, Body::empty()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!response.headers().contains_key("agent-space-error-code"));
    drop(transfer);
    f.sessions.stop_all().await;
}

#[tokio::test]
#[serial_test::serial]
async fn connection_timeout_is_not_reported_as_full_capacity() {
    use agent_engine::engine::mock::{TEST_HOLD_PORT, TEST_TCP_RELEASE};
    use std::sync::atomic::Ordering;
    let f = fixture_with_idle(Duration::from_millis(100));
    TEST_HOLD_PORT.store(44000, Ordering::SeqCst);
    let response = f
        .app
        .clone()
        .oneshot(request(
            "PROPFIND",
            "/u/alice/dav/",
            &format!("Bearer {}", f.dav.token),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(
        TEST_HOLD_PORT.load(Ordering::SeqCst),
        0,
        "test must reach guest TCP connection"
    );
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    assert!(!response.headers().contains_key("agent-space-error-code"));
    TEST_TCP_RELEASE.notify_waiters();
    f.sessions.stop_all().await;
}
#[tokio::test]
async fn auth_is_purpose_and_user_scoped_before_activation_and_secrets_stay_on_host() {
    let f = fixture();
    for (credential, path, code) in [
        (
            format!("Bearer {}", f.mcp.token),
            "/u/alice/dav/a",
            StatusCode::UNAUTHORIZED,
        ),
        (
            "Bearer invalid".into(),
            "/u/alice/dav/a",
            StatusCode::UNAUTHORIZED,
        ),
        (
            format!("Bearer {}", f.dav.token),
            "/u/bob/dav/a",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let response = f
            .app
            .clone()
            .oneshot(request("GET", path, &credential, Body::empty()))
            .await
            .unwrap();
        assert_eq!(response.status(), code);
        if code == StatusCode::UNAUTHORIZED {
            assert!(response.headers().contains_key("www-authenticate"));
        }
    }
    assert_eq!(f.sessions.active_space_count(), 0);
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("alice:{}", f.dav.token))
    );
    let mut req = request("GET", "/u/alice/dav/headers", &basic, Body::empty());
    req.headers_mut()
        .insert("cookie", "private=secret".parse().unwrap());
    req.headers_mut()
        .insert("x-forwarded-host", "untrusted.example".parse().unwrap());
    req.headers_mut()
        .insert("x-agent-space-user", "bob".parse().unwrap());
    let response = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-security-policy"],
        "sandbox allow-downloads"
    );
    let body = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    let received = String::from_utf8(body.to_vec())
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        !received.contains("authorization:")
            && !received.contains("cookie:")
            && !received.contains("x-forwarded-")
            && !received.contains("x-agent-space-")
    );
    assert!(received.contains("get /u/alice/dav/headers http/1.1"));
    f.sessions.stop_all().await;
}
#[tokio::test]
async fn rotating_dav_key_interrupts_upload_without_waiting_for_body_poll() {
    let f = fixture();
    let (signal, started) = tokio::sync::oneshot::channel();
    let body = Body::from_stream(futures::stream::once(async move {
        signal.send(()).unwrap();
        futures::future::pending::<Result<bytes::Bytes, std::io::Error>>().await
    }));
    let req = f.app.clone().oneshot(request(
        "PUT",
        "/u/alice/dav/file",
        &format!("Bearer {}", f.dav.token),
        body,
    ));
    let (response, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(req, async {
            started.await.unwrap();
            f.store
                .rotate_dav_token("alice", &f.user.agent_space_id)
                .unwrap();
        })
    })
    .await
    .expect("credential revocation must cancel pending uploads");
    assert!(!response.unwrap().status().is_success());
    assert!(f.store.authenticate(&f.mcp.token).is_ok());
    f.sessions.stop_all().await;
}
#[tokio::test]
#[serial_test::serial]
async fn revocation_closes_waiting_headers_and_unpolled_downloads() {
    let f = fixture();
    let request = f.app.clone().oneshot(request(
        "GET",
        "/u/alice/dav/held-headers",
        &format!("Bearer {}", f.dav.token),
        Body::empty(),
    ));
    let (response, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(request, async {
            TEST_HEADERS_SEEN.notified().await;
            f.store
                .revoke_dav_token("alice", &f.user.agent_space_id)
                .unwrap();
            TEST_HTTP_DISCONNECTED.notified().await;
        })
    })
    .await
    .unwrap();
    assert!(!response.unwrap().status().is_success());
    let next = f
        .store
        .rotate_dav_token("alice", &f.user.agent_space_id)
        .unwrap();
    let response = f
        .app
        .clone()
        .oneshot(crate::request(
            "GET",
            "/u/alice/dav/held-response",
            &format!("Bearer {}", next.token),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    f.store
        .revoke_dav_token("alice", &f.user.agent_space_id)
        .unwrap();
    tokio::task::yield_now().await;
    assert!(tokio::time::timeout(
        Duration::from_secs(1),
        axum::body::to_bytes(response.into_body(), 1024)
    )
    .await
    .unwrap()
    .is_err());
    f.sessions.stop_all().await;
}

// Exercise the real proxy and response owner, not just the Progress timer.
#[tokio::test]
#[serial_test::serial]
async fn idle_headers_upload_and_unpolled_response_release_activity() {
    use agent_engine::config::LifecycleConfig;
    let config = LifecycleConfig {
        session_timeout_secs: 1,
        idle_timeout_secs: 1,
        check_interval_secs: 1,
    };
    for phase in ["headers", "upload", "response"] {
        let f = fixture_with_idle(Duration::from_millis(100));
        let auth = format!("Bearer {}", f.dav.token);
        let response = if phase == "upload" {
            let body = Body::from_stream(futures::stream::pending::<
                Result<bytes::Bytes, std::io::Error>,
            >());
            tokio::time::timeout(
                Duration::from_secs(2),
                f.app
                    .clone()
                    .oneshot(request("PUT", "/u/alice/dav/file", &auth, body)),
            )
            .await
            .unwrap()
            .unwrap()
        } else {
            let path = if phase == "headers" {
                "/u/alice/dav/held-headers"
            } else {
                "/u/alice/dav/held-response"
            };
            tokio::time::timeout(
                Duration::from_secs(2),
                f.app
                    .clone()
                    .oneshot(request("GET", path, &auth, Body::empty())),
            )
            .await
            .unwrap()
            .unwrap()
        };
        if phase == "response" {
            assert_eq!(response.status(), StatusCode::OK);
            // Keep the body alive without polling it. The connection owner must
            // release its activity on idle anyway, so a slow client cannot pin VM.
            tokio::time::sleep(Duration::from_millis(250)).await;
        } else {
            assert!(!response.status().is_success(), "{phase}");
        }
        let session = f.sessions.get_session(&f.user.sandbox_name).await.unwrap();
        let old = std::time::Instant::now() - Duration::from_secs(5);
        session.set_last_request(old).await;
        session.set_last_work(old).await;
        assert_eq!(
            f.sessions.collect_candidates(&config).await.len(),
            1,
            "{phase} must release its lease"
        );
        f.sessions
            .verify_and_stop(&f.user.sandbox_name, &config, "idle")
            .await
            .unwrap();
        assert_eq!(f.sessions.active_space_count(), 0);
        if phase == "response" {
            assert!(axum::body::to_bytes(response.into_body(), 1024)
                .await
                .is_err());
        }
        f.sessions.stop_all().await;
    }
}

#[tokio::test]
#[serial_test::serial]
async fn active_download_blocks_reclaim_and_client_drop_releases_it() {
    use agent_engine::config::LifecycleConfig;
    let f = fixture();
    let response = f
        .app
        .clone()
        .oneshot(request(
            "GET",
            "/u/alice/dav/held-response",
            &format!("Bearer {}", f.dav.token),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session = f.sessions.get_session(&f.user.sandbox_name).await.unwrap();
    let config = LifecycleConfig {
        session_timeout_secs: 1,
        idle_timeout_secs: 1,
        check_interval_secs: 1,
    };
    let old = std::time::Instant::now() - Duration::from_secs(5);
    session.set_last_request(old).await;
    session.set_last_work(old).await;
    assert!(f.sessions.collect_candidates(&config).await.is_empty());
    f.sessions
        .verify_and_stop(&f.user.sandbox_name, &config, "idle")
        .await
        .unwrap();
    assert_eq!(f.sessions.active_space_count(), 1);
    let finished = std::time::Instant::now();
    drop(response);
    assert!(session.last_work().await >= finished);
    assert!(session.last_request().await >= finished);
    session.set_last_request(old).await;
    session.set_last_work(old).await;
    assert_eq!(f.sessions.collect_candidates(&config).await.len(), 1);
    f.sessions.stop_all().await;
}

#[tokio::test]
#[serial_test::serial]
async fn account_invalidation_and_shutdown_cancel_unpolled_downloads() {
    for action in ["disable", "delete", "shutdown"] {
        let f = fixture();
        let response = f
            .app
            .clone()
            .oneshot(request(
                "GET",
                "/u/alice/dav/held-response",
                &format!("Bearer {}", f.dav.token),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        match action {
            "disable" => {
                f.store
                    .set_enabled("alice", &f.user.agent_space_id, false)
                    .unwrap();
            }
            "delete" => {
                f.store
                    .begin_delete("alice", &f.user.agent_space_id)
                    .unwrap();
            }
            _ => f.sessions.stop_all().await,
        }
        assert!(
            tokio::time::timeout(
                Duration::from_secs(1),
                axum::body::to_bytes(response.into_body(), 1024)
            )
            .await
            .unwrap()
            .is_err(),
            "{action}"
        );
        if action != "shutdown" {
            let denied = f
                .app
                .clone()
                .oneshot(request(
                    "GET",
                    "/u/alice/dav/file",
                    &format!("Bearer {}", f.dav.token),
                    Body::empty(),
                ))
                .await
                .unwrap();
            assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        }
        f.sessions.stop_all().await;
    }
}

#[tokio::test]
#[serial_test::serial]
async fn ongoing_upload_progress_extends_idle_deadline() {
    let f = fixture_with_idle(Duration::from_millis(250));
    let stream = async_stream::stream! {
        for _ in 0..8 {
            tokio::time::sleep(Duration::from_millis(75)).await;
            yield Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"progress"));
        }
    };
    let start = std::time::Instant::now();
    let response = f
        .app
        .clone()
        .oneshot(request(
            "PUT",
            "/u/alice/dav/file",
            &format!("Bearer {}", f.dav.token),
            Body::from_stream(stream),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        start.elapsed() > Duration::from_millis(500),
        "transfer spans multiple idle intervals"
    );
    axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    f.sessions.stop_all().await;
}

#[tokio::test]
#[serial_test::serial]
async fn invalidation_interrupts_activation_and_connect_without_waiting_for_gate() {
    use agent_engine::engine::mock::{TEST_HOLD_PORT, TEST_TCP_RELEASE};
    use std::sync::atomic::Ordering;
    for phase in ["activation", "connect"] {
        let f = fixture();
        let blocked_engine = if phase == "activation" {
            Some(f.engine.sandboxes.lock().await)
        } else {
            None
        };
        if phase == "connect" {
            TEST_HOLD_PORT.store(44000, Ordering::SeqCst);
        }
        let mut call = Box::pin(f.app.clone().oneshot(request(
            "GET",
            "/u/alice/dav/file",
            &format!("Bearer {}", f.dav.token),
            Body::empty(),
        )));
        assert!(futures::poll!(&mut call).is_pending());
        tokio::task::yield_now().await;
        if phase == "connect" {
            tokio::time::timeout(Duration::from_secs(1), async {
                while TEST_HOLD_PORT.load(Ordering::SeqCst) != 0 {
                    assert!(futures::poll!(&mut call).is_pending());
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        f.store
            .rotate_dav_token("alice", &f.user.agent_space_id)
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(1), call)
            .await
            .expect("revocation cannot wait for startup/TCP")
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{phase}");
        drop(blocked_engine);
        if phase == "connect" {
            TEST_TCP_RELEASE.notify_waiters();
        }
        tokio::time::timeout(Duration::from_secs(2), f.sessions.stop_all())
            .await
            .unwrap();
        assert_eq!(f.sessions.active_space_count(), 0);
    }
}

#[tokio::test]
async fn custom_token_auth_and_retry_preserve_transfer_until_replacement() {
    let f = fixture();
    let token = "integration-0123456789abcdef0123456789abcdef"; // gitleaks:allow -- synthetic test credential
    let first = f
        .store
        .set_dav_token("alice", &f.user.agent_space_id, token)
        .unwrap();
    for auth in [
        format!("Bearer {token}"),
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("alice:{token}"))
        ),
    ] {
        let response = f
            .app
            .clone()
            .oneshot(request("GET", "/u/alice/dav/file", &auth, Body::empty()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
    }
    let (signal, started) = tokio::sync::oneshot::channel();
    let body = Body::from_stream(futures::stream::once(async move {
        signal.send(()).unwrap();
        futures::future::pending::<Result<bytes::Bytes, std::io::Error>>().await
    }));
    let pending = tokio::spawn(f.app.clone().oneshot(request(
        "PUT",
        "/u/alice/dav/file",
        &format!("Bearer {token}"),
        body,
    )));
    started.await.unwrap();
    assert_eq!(
        f.store
            .set_dav_token("alice", &f.user.agent_space_id, token)
            .unwrap(),
        first
    );
    // An unrelated committed state notification must not revoke the stable key.
    f.store.create_user("bob", "smcp", &["ws"]).unwrap();
    let mut pending = pending;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut pending)
            .await
            .is_err()
    );
    f.store
        .set_dav_token(
            "alice",
            &f.user.agent_space_id,
            "replacement-0123456789abcdef0123456789abcdef",
        )
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!response.status().is_success());
    assert!(f.store.authenticate(&f.mcp.token).is_ok());
    f.sessions.stop_all().await;
}

#[tokio::test]
async fn lan_entry_preserves_dav_auth_browser_and_destination_boundaries() {
    let f = fixture();
    let auth = format!("Bearer {}", f.dav.token);
    for host in [
        "127.0.0.1:18933",
        "localhost:18933",
        "192.168.1.249:18933",
        "192.168.31.236:18933",
    ] {
        let mut req = request("GET", "/u/alice/dav/headers", &auth, Body::empty());
        req.headers_mut().insert("host", host.parse().unwrap());
        let response = f.app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{host}");
        let _ = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
    }
    for (name, value) in [
        ("origin", "http://evil.example"),
        ("destination", "http://192.168.1.249:18933/u/alice/dav/b"),
        ("destination", "http://192.168.31.236:18933/u/bob/dav/b"),
        (
            "if",
            "<http://192.168.1.249:18933/u/alice/dav/a> ([\"old\"])",
        ),
    ] {
        let mut req = request("MOVE", "/u/alice/dav/a", &auth, Body::empty());
        req.headers_mut()
            .insert("host", "192.168.31.236:18933".parse().unwrap());
        req.headers_mut().insert(
            http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        assert_eq!(
            f.app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN,
            "{name}: {value}"
        );
    }
    let mut req = request("MOVE", "/u/alice/dav/a", &auth, Body::empty());
    req.headers_mut()
        .insert("host", "192.168.31.236:18933".parse().unwrap());
    req.headers_mut().insert(
        "destination",
        "http://192.168.31.236:18933/u/alice/dav/b".parse().unwrap(),
    );
    let response = f.app.clone().oneshot(req).await.unwrap();
    assert_ne!(response.status(), StatusCode::FORBIDDEN);
    let _ = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    f.sessions.stop_all().await;
}

#[tokio::test]
async fn two_http_listeners_keep_admin_and_dav_routes_separate_even_with_forged_host() {
    use agent_engine::workspace_service::WorkspaceService;
    use agent_space_mcp::{
        admin_api::{self, AdminState},
        origins::{origin_boundary, OriginPolicy},
    };
    let f = fixture();
    let admin = admin_api::router(AdminState {
        service: Arc::new(
            WorkspaceService::new(f.store.clone(), f.sessions.clone(), "smcp", "")
                .with_dav_base_url(Some(""))
                .with_dav_public_port(18933),
        ),
        admin_token: "test-admin".into(),
    })
    .layer(axum::middleware::from_fn_with_state(
        OriginPolicy::new(None).unwrap(),
        origin_boundary,
    ));
    let main_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dav_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let main_addr = main_listener.local_addr().unwrap();
    let dav_addr = dav_listener.local_addr().unwrap();
    let dav = f.app.clone();
    let main_task = tokio::spawn(async move { axum::serve(main_listener, admin).await.unwrap() });
    let dav_task = tokio::spawn(async move { axum::serve(dav_listener, dav).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for host in [
        "localhost:18932",
        "192.168.1.249:18933",
        "192.168.31.236:18932",
    ] {
        let r = client
            .get(format!("http://{main_addr}/admin/v1/users"))
            .header("host", host)
            .bearer_auth("test-admin")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let r = client
            .get(format!("http://{dav_addr}/admin/v1/users"))
            .header("host", host)
            .bearer_auth("test-admin")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = client
            .get(format!("http://{main_addr}/u/alice/dav/headers"))
            .header("host", host)
            .bearer_auth(&f.dav.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = client
            .get(format!("http://{dav_addr}/u/alice/dav/headers"))
            .header("host", host)
            .bearer_auth(&f.dav.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        r.bytes().await.unwrap();
    }
    main_task.abort();
    dav_task.abort();
    f.sessions.stop_all().await;
}
