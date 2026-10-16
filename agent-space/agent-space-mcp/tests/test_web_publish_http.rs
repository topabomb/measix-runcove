use agent_engine::engine::mock::{
    MockEngine, TEST_DEAD_HELPERS, TEST_HOLD_PORT, TEST_NEXT_STATUS, TEST_TCP_RELEASE,
};
use agent_engine::engine::{WorkspaceOpts, WorkspaceStatus};
use agent_engine::publication::{Publication, PublishTarget};
use agent_engine::session::SessionManager;
use agent_engine::state::StateStore;
use agent_space_mcp::web_publish::{self, PublishState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
fn opts() -> WorkspaceOpts {
    WorkspaceOpts {
        image: "mock".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}
fn publish_app(
    sessions: Arc<SessionManager>,
    state: Option<Arc<StateStore>>,
    timeout: Duration,
) -> axum::Router {
    axum::Router::new()
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(axum::middleware::from_fn_with_state(
            PublishState {
                sessions,
                state,
                origin: url::Url::parse("https://workspace.example").unwrap(),
                enabled: true,
                startup_timeout: timeout,
                io_idle_timeout: timeout,
            },
            web_publish::dispatch,
        ))
}
fn host(_item: &Publication) -> &'static str {
    "workspace.example"
}
fn uri(item: &Publication, path: &str) -> String {
    format!(
        "{}{}",
        item.path_prefix(),
        path.strip_prefix('/').unwrap_or(path)
    )
}

#[tokio::test]
async fn app_authorization_and_host_survive_without_gateway_credentials() {
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_or_create("app-auth").await.unwrap();
    let item = Publication::new(
        "api".into(),
        "alice".into(),
        "spc_a".into(),
        PublishTarget::Port(3000),
        3000,
        None,
        Duration::ZERO,
    )
    .unwrap();
    session.publish(item.clone()).unwrap();
    let app = publish_app(sessions.clone(), None, Duration::from_secs(1));
    let request = Request::get(uri(&item, "/headers"))
        .header("host", host(&item))
        .header("authorization", "Bearer app-key")
        .header("x-publish-key", &item.password)
        .header("x-forwarded-host", "evil.example")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let headers = String::from_utf8_lossy(&bytes).to_lowercase();
    assert!(headers.contains("authorization: bearer app-key"));
    assert!(headers.contains(&format!("host: {}", host(&item))));
    assert!(!headers.contains("x-publish-key"));
    assert!(!headers.contains(&item.password));
    assert!(!headers.contains("evil.example"));
    assert!(headers.contains(&format!(
        "x-forwarded-prefix: {}",
        item.path_prefix().trim_end_matches('/')
    )));
    sessions.stop_all().await;
}

#[tokio::test]
async fn unpolled_response_blocks_reclamation_until_drop_or_idle() {
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_or_create("response-lease").await.unwrap();
    let item = Publication::new(
        "api".into(),
        "alice".into(),
        "spc_a".into(),
        PublishTarget::Port(3000),
        3000,
        None,
        Duration::ZERO,
    )
    .unwrap();
    session.publish(item.clone()).unwrap();
    let config = agent_engine::config::LifecycleConfig {
        session_timeout_secs: 1,
        idle_timeout_secs: 2,
        check_interval_secs: 1,
    };
    let app = publish_app(sessions.clone(), None, Duration::from_millis(150));
    for drop_client in [true, false] {
        let response = app
            .clone()
            .oneshot(
                Request::get(uri(&item, "/held-response"))
                    .header("host", host(&item))
                    .header("x-publish-key", &item.password)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        session
            .set_last_request(std::time::Instant::now() - Duration::from_secs(10))
            .await;
        session
            .set_last_work(std::time::Instant::now() - Duration::from_secs(10))
            .await;
        assert!(sessions.collect_candidates(&config).await.is_empty());
        if drop_client {
            drop(response);
        } else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(response);
        }
        session
            .set_last_request(std::time::Instant::now() - Duration::from_secs(10))
            .await;
        session
            .set_last_work(std::time::Instant::now() - Duration::from_secs(10))
            .await;
        assert_eq!(
            sessions.collect_candidates(&config).await.len(),
            1,
            "response activity must be released"
        );
    }
    sessions.stop_all().await;
}
async fn send(
    app: &axum::Router,
    item: &Publication,
    path: &str,
    auth: Option<&str>,
) -> (StatusCode, Option<String>) {
    let mut request = Request::builder()
        .uri(uri(item, path))
        .header("host", host(item));
    if let Some(auth) = auth {
        request = request.header("authorization", auth);
    } else {
        request = request.header("x-publish-key", &item.password);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let location = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    (response.status(), location)
}

#[tokio::test]
async fn independent_path_credentials_and_revocation() {
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_or_create("independent").await.unwrap();
    let one = Publication::new(
        "one".into(),
        "alice".into(),
        "spc_a".into(),
        PublishTarget::Port(3000),
        3000,
        None,
        Duration::ZERO,
    )
    .unwrap();
    let mut two = Publication::new(
        "two".into(),
        "alice".into(),
        "spc_a".into(),
        PublishTarget::Port(3000),
        3000,
        None,
        Duration::ZERO,
    )
    .unwrap();
    two.reserve_username(std::slice::from_ref(&one)).unwrap();
    // Even equal passwords cannot make the first Basic identity authorize the second.
    two.password = one.password.clone();
    session.publish(one.clone()).unwrap();
    session.publish(two.clone()).unwrap();
    let app = publish_app(sessions.clone(), None, Duration::from_secs(2));
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", one.access_username, one.password))
    );
    assert_eq!(send(&app, &one, "/", Some(&basic)).await.0, StatusCode::OK);
    assert_eq!(
        send(&app, &two, "/", Some(&basic)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let missing = app
        .clone()
        .oneshot(
            Request::get(uri(&one, "/"))
                .header("host", host(&one))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert!(missing.headers().contains_key("www-authenticate"));
    for path in ["/", "/admin/v1/users", "/healthz", "/u/alice/mcp"] {
        assert_eq!(
            send(&app, &one, path, None).await.0,
            StatusCode::OK,
            "application root path {path}"
        );
    }
    // Authentication covers every resource type and nested route, not just the page.
    for path in [
        "/assets/app.js",
        "/assets/style.css",
        "/images/logo.svg",
        "/nested/page/",
        "/api/items?x=1",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(uri(&one, path))
                    .header("host", host(&one))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(send(&app, &one, path, Some(&basic)).await.0, StatusCode::OK);
    }
    session.unpublish("one").await;
    assert_eq!(send(&app, &one, "/", None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(send(&app, &two, "/", None).await.0, StatusCode::OK);
    let three = Publication::new(
        "one".into(),
        "alice".into(),
        "spc_a".into(),
        PublishTarget::Port(3000),
        3000,
        None,
        Duration::ZERO,
    )
    .unwrap();
    session.publish(three.clone()).unwrap();
    assert_ne!(one.id, three.id);
    assert_eq!(
        send(&app, &one, "/", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    sessions.stop_all().await;
}

#[tokio::test]
async fn disabled_account_cannot_access_a_publication() {
    let dir = tempfile::tempdir().unwrap();
    StateStore::init(dir.path()).unwrap();
    let state = Arc::new(StateStore::open(dir.path()).unwrap());
    let (user, _) = state.create_user("alice", "pub-test", &["ws"]).unwrap();
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_or_create(&user.sandbox_name).await.unwrap();
    let item = Publication::new(
        "test".into(),
        "alice".into(),
        user.agent_space_id.clone(),
        PublishTarget::Port(3000),
        3000,
        None,
        Duration::ZERO,
    )
    .unwrap();
    session.publish(item.clone()).unwrap();
    let app = publish_app(
        sessions.clone(),
        Some(state.clone()),
        Duration::from_secs(2),
    );
    assert_eq!(send(&app, &item, "/", None).await.0, StatusCode::OK);
    state
        .set_enabled("alice", &user.agent_space_id, false)
        .unwrap();
    assert_eq!(send(&app, &item, "/", None).await.0, StatusCode::NOT_FOUND);
    // A reused username must never authorize the old space's still-held item.
    state.begin_delete("alice", &user.agent_space_id).unwrap();
    state.finish_delete("alice", &user.agent_space_id).unwrap();
    let (replacement, _) = state.create_user("alice", "pub-test", &["ws"]).unwrap();
    assert_ne!(replacement.agent_space_id, user.agent_space_id);
    assert_eq!(send(&app, &item, "/", None).await.0, StatusCode::NOT_FOUND);
    sessions.stop_all().await;
}

#[tokio::test]
async fn publication_request_streams_without_buffering_the_whole_body() {
    use axum::body::to_bytes;
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_session("smcp-stream").await.unwrap();
    session
        .exec("true", std::time::Duration::from_secs(1))
        .await
        .unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_stream").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let publication = session.publication("test").unwrap();
    let app = publish_app(sessions.clone(), None, std::time::Duration::from_secs(2));
    let payload = vec![b'a'; 2 * 1024 * 1024];
    let request = Request::builder()
        .method("POST")
        .uri(uri(&publication, "/upload?x=1"))
        .header("host", host(&publication))
        .header("x-publish-key", &publication.password)
        .header("content-length", payload.len())
        .header("connection", "close, x-request-id")
        .header("x-request-id", "secret-hop")
        .header("x-app", "kept")
        .body(Body::from(payload.clone()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("connection").is_none());
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        payload
    );
}

#[tokio::test]
async fn unpublish_during_guest_connection_prevents_a_late_http_request() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_session("smcp-late-request").await.unwrap();
    session.exec("true", Duration::from_secs(1)).await.unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_late").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let publication = session.publication("test").unwrap();
    let app = publish_app(sessions.clone(), None, Duration::from_secs(2));

    TEST_HOLD_PORT.store(3000, Ordering::SeqCst);
    let request = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .uri(uri(&publication, "/"))
                .header("host", host(&publication))
                .header("x-publish-key", &publication.password)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while TEST_HOLD_PORT.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("guest connection was attempted");
    session.unpublish("test").await;
    TEST_TCP_RELEASE.notify_one();
    let response = request.await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unpublish_cancels_an_existing_streaming_response() {
    use futures::StreamExt;
    use std::time::Duration;

    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_session("smcp-held-response").await.unwrap();
    session.exec("true", Duration::from_secs(1)).await.unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_held").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let publication = session.publication("test").unwrap();
    let app = publish_app(sessions.clone(), None, Duration::from_secs(5));
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri(&publication, "/held-response"))
                .header("host", host(&publication))
                .header("x-publish-key", &publication.password)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    assert_eq!(body.next().await.unwrap().unwrap().as_ref(), b"A");
    session.unpublish("test").await;
    let end = tokio::time::timeout(Duration::from_secs(1), body.next())
        .await
        .expect("closed stream must finish promptly");
    assert!(end.is_none_or(|frame| frame.is_err()));
}

#[tokio::test]
async fn replacement_cancels_old_stream_and_preserves_basic_access() {
    use futures::StreamExt;

    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions
        .get_session("smcp-replaced-response")
        .await
        .unwrap();
    session.exec("true", Duration::from_secs(1)).await.unwrap();
    let publication = Publication::new(
        "test".into(),
        "alice".into(),
        "spc_replaced".into(),
        PublishTarget::Port(3000),
        3000,
        None,
        Duration::ZERO,
    )
    .unwrap();
    session.publish(publication.clone()).unwrap();
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            publication.access_username, publication.password
        ))
    );
    let app = publish_app(sessions.clone(), None, Duration::from_secs(5));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri(&publication, "/held-response"))
                .header("host", host(&publication))
                .header("authorization", &basic)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    assert_eq!(body.next().await.unwrap().unwrap().as_ref(), b"A");

    let mut replacement = publication.clone();
    let helper = session
        .start_managed(agent_engine::engine::ManagedLaunch::StaticHttp {
            host_binary: "mock-static-http".into(),
            root: "/workspace/site".into(),
            file: false,
        })
        .await
        .unwrap();
    replacement.target = PublishTarget::Directory("/workspace/site".into());
    replacement.port = helper.port;
    replacement.helper = Some(helper);
    replacement.epoch = Arc::new(tokio_util::sync::CancellationToken::new());
    {
        let _gate = session.publication_gate().await;
        assert!(session
            .replace_publication(replacement, &publication)
            .unwrap()
            .is_some());
    }
    let end = tokio::time::timeout(Duration::from_secs(1), body.next())
        .await
        .expect("replaced stream must finish promptly");
    assert!(end.is_none_or(|frame| frame.is_err()));
    assert!(session
        .begin_publish_request(&publication.epoch)
        .await
        .is_none());

    let response = app
        .oneshot(
            Request::builder()
                .uri(uri(&publication, "/headers"))
                .header("host", host(&publication))
                .header("authorization", basic)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let upstream = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(upstream.starts_with("GET /headers HTTP/1.1\r\n"));
    assert!(!upstream.to_ascii_lowercase().contains("authorization:"));
    assert!(!upstream.contains(&publication.password));
    sessions.stop_all().await;
}

#[tokio::test]
async fn streaming_post_without_length_is_framed_for_guest() {
    use axum::body::to_bytes;
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_session("smcp-framed").await.unwrap();
    session
        .exec("true", std::time::Duration::from_secs(1))
        .await
        .unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_framed").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let publication = session.publication("test").unwrap();
    let app = publish_app(sessions.clone(), None, std::time::Duration::from_secs(2));
    let payload = b"streamed-body-without-length".to_vec();
    let body = Body::from_stream(futures::stream::iter(vec![
        Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(&payload[..9])),
        Ok(bytes::Bytes::copy_from_slice(&payload[9..])),
    ]));
    let request = Request::builder()
        .method("POST")
        .uri(uri(&publication, "/upload"))
        .header("host", host(&publication))
        .header("x-publish-key", &publication.password)
        .body(body)
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        payload
    );
}

#[tokio::test]
async fn chunked_guest_response_is_decoded_before_relay() {
    use axum::body::to_bytes;
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_session("smcp-chunked").await.unwrap();
    session
        .exec("true", std::time::Duration::from_secs(1))
        .await
        .unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_chunked").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let publication = session.publication("test").unwrap();
    let app = publish_app(sessions.clone(), None, std::time::Duration::from_secs(2));
    let request = Request::builder()
        .uri(uri(&publication, "/chunked"))
        .header("host", host(&publication))
        .header("x-publish-key", &publication.password)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("transfer-encoding").is_none());
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        b"hello world"
    );
}

#[tokio::test]
async fn paused_publication_resumes_but_stopped_publication_does_not_start() {
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_session("smcp-paused").await.unwrap();
    session
        .exec("true", std::time::Duration::from_secs(1))
        .await
        .unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_paused").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let publication = session.publication("test").unwrap();
    *TEST_NEXT_STATUS.lock().unwrap() = Some(WorkspaceStatus::Paused);
    let app = publish_app(sessions.clone(), None, std::time::Duration::from_secs(2));
    assert_eq!(send(&app, &publication, "/", None).await.0, StatusCode::OK);
    sessions.stop_workspace("smcp-paused").await.unwrap();
    assert_eq!(
        send(&app, &publication, "/", None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn exited_directory_helper_closes_publication() {
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_session("smcp-helper").await.unwrap();
    session
        .exec("true", std::time::Duration::from_secs(1))
        .await
        .unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_helper").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                Some(agent_engine::engine::mock::managed_instance(47000)),
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let publication = session.publication("test").unwrap();
    let app = publish_app(sessions.clone(), None, std::time::Duration::from_secs(2));
    assert_eq!(send(&app, &publication, "/", None).await.0, StatusCode::OK);
    TEST_DEAD_HELPERS.lock().unwrap().push(47000);
    assert_eq!(
        send(&app, &publication, "/", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(session.publication("test").is_none());
}

#[tokio::test]
async fn unpublish_cancels_upload_before_the_request_body_finishes() {
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_session("publish-upload-cancel").await.unwrap();
    session
        .exec("true", std::time::Duration::from_secs(1))
        .await
        .unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_cancel").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let publication = session.publication("test").unwrap();
    let app = publish_app(sessions.clone(), None, std::time::Duration::from_secs(10));
    let (signal, started) = tokio::sync::oneshot::channel();
    let body = Body::from_stream(futures::stream::once(async move {
        signal.send(()).unwrap();
        futures::future::pending::<Result<bytes::Bytes, std::io::Error>>().await
    }));
    let request = app.oneshot(
        Request::post(uri(&publication, "/upload"))
            .header("host", host(&publication))
            .header("x-publish-key", &publication.password)
            .body(body)
            .unwrap(),
    );
    let (response, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(request, async {
            started.await.unwrap();
            session.unpublish("test").await;
        })
    })
    .await
    .expect("revocation must interrupt the pending body");
    assert_eq!(response.unwrap().status(), StatusCode::NOT_FOUND);
    sessions.stop_all().await;
}

#[tokio::test]
#[serial_test::serial]
async fn unpublish_interrupts_wait_for_upstream_headers_and_closes_guest_connection() {
    use agent_engine::engine::mock::{TEST_HEADERS_SEEN, TEST_HTTP_DISCONNECTED};
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_session("publish-header-cancel").await.unwrap();
    session
        .exec("true", std::time::Duration::from_secs(1))
        .await
        .unwrap();
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_headers").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                None,
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    let publication = session.publication("test").unwrap();
    let app = publish_app(sessions.clone(), None, std::time::Duration::from_secs(10));
    let request = app.oneshot(
        Request::get(uri(&publication, "/held-headers"))
            .header("host", host(&publication))
            .header("x-publish-key", &publication.password)
            .body(Body::empty())
            .unwrap(),
    );
    let (response, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(request, async {
            TEST_HEADERS_SEEN.notified().await;
            session.unpublish("test").await;
            TEST_HTTP_DISCONNECTED.notified().await;
        })
    })
    .await
    .expect("off must close the upstream while waiting for response headers");
    assert_eq!(response.unwrap().status(), StatusCode::NOT_FOUND);
    sessions.stop_all().await;
}

#[tokio::test]
async fn canonical_root_and_identity_auth_scope() {
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts()));
    let session = sessions.get_or_create("path-scope").await.unwrap();
    let item = Publication::new(
        "api".into(),
        "alice".into(),
        "spc_a".into(),
        PublishTarget::Port(3000),
        3000,
        None,
        Duration::ZERO,
    )
    .unwrap();
    session.publish(item.clone()).unwrap();
    let app = publish_app(sessions.clone(), None, Duration::from_secs(1));
    let root = item.path_prefix();
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("{}?x=%2F", root.trim_end_matches('/')))
                .header("host", host(&item))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(response.headers()["location"], format!("{root}?x=%2F"));
    assert!(!response.headers().contains_key("www-authenticate"));
    let response = app
        .clone()
        .oneshot(
            Request::get(&root)
                .header("host", host(&item))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()["www-authenticate"],
        format!("Basic realm=\"{}\", charset=\"UTF-8\"", item.id)
    );
    for path in [
        root.replace("/alice/", "/bob/"),
        root.replace("/api/", "/missing/"),
        "/u/alice/pub/".into(),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(path)
                    .header("host", host(&item))
                    .header("x-publish-key", &item.password)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response.headers().contains_key("www-authenticate"));
    }
    for (origin, expected) in [
        ("https://workspace.example", StatusCode::OK),
        ("https://external.example", StatusCode::FORBIDDEN),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(&root)
                    .header("host", host(&item))
                    .header("origin", origin)
                    .header("x-publish-key", &item.password)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    sessions.stop_all().await;
}
