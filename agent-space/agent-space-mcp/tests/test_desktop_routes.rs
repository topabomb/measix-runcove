use agent_engine::{
    engine::{mock::MockEngine, WorkspaceOpts},
    image::SpaceImage,
    session::SessionManager,
    state::StateStore,
};
use agent_space_mcp::desktop::{self, DesktopState};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use std::{
    fs,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tower::ServiceExt;
struct Fixture {
    app: Router,
    state: Arc<StateStore>,
    engine: Arc<MockEngine>,
    sessions: Arc<SessionManager>,
    token: String,
    _dir: tempfile::TempDir,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    StateStore::init(&state_dir).unwrap();
    let state = Arc::new(StateStore::open(&state_dir).unwrap());
    let image = SpaceImage::Desktop {
        preset: "runcove-desktop".into(),
        reference: format!("repo@sha256:{}", "a".repeat(64)),
        worker_contract: 1,
    };
    let (_, token) = state
        .create_user_with_image("alice", "test", &["ws"], image.clone())
        .unwrap();
    state
        .create_user_with_image("bob", "test", &["ws"], image)
        .unwrap();
    state.create_user("custom", "test", &["ws"]).unwrap();
    let engine = Arc::new(MockEngine::new());
    let sessions = Arc::new(SessionManager::new(
        engine.clone(),
        4,
        WorkspaceOpts {
            image: "test".into(),
            cpus: 1,
            memory_mib: 256,
            shell: "/bin/bash".into(),
            volumes: vec![],
            network_access: Default::default(),
            outbound_proxy: None,
            max_tcp_connections: None,
            max_udp_connections: None,
        },
    ));
    let ui = dir.path().join("ui");
    fs::create_dir(&ui).unwrap();
    fs::write(ui.join("index.html"), "workbench").unwrap();
    let app = desktop::router(DesktopState::new(
        sessions.clone(),
        state.clone(),
        ui.to_string_lossy().into(),
    ));
    Fixture {
        app,
        state,
        engine,
        sessions,
        token: token.token,
        _dir: dir,
    }
}
async fn request(
    app: &Router,
    path: &str,
    method: &str,
    body: Value,
    cookie: Option<&str>,
    nonce: Option<&str>,
    origin: &str,
) -> axum::response::Response {
    let mut request = Request::builder()
        .uri(path)
        .method(method)
        .header("Host", "rewritten.internal:8932")
        .header("Origin", origin)
        .header("sec-fetch-site", "same-origin");
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    if let Some(nonce) = nonce {
        request = request.header("X-Desktop-Nonce", nonce);
    }
    request = request.header("Content-Type", "application/json");
    app.clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}
async fn sign_in(f: &Fixture) -> (String, String) {
    let response = request(
        &f.app,
        "/u/alice/desktop/api/login",
        "POST",
        json!({"token":f.token,"cookie_path":"/prefix/u/alice/desktop/"}),
        None,
        None,
        "https://public.example",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("Path=/prefix/u/alice/desktop/"));
    assert!(cookie.contains("HttpOnly; SameSite=Strict"));
    assert!(cookie.contains("; Secure"));
    assert!(!cookie.contains(&f.token));
    let data: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    (
        cookie.split(';').next().unwrap().to_owned(),
        data["nonce"].as_str().unwrap().to_owned(),
    )
}
#[tokio::test]
async fn routes_bind_identity_nonce_and_origin_without_waking_vm() {
    let f = fixture();
    let (cookie, nonce) = sign_in(&f).await;
    for (path, nonce, origin, status) in [
        (
            "/u/alice/desktop/api/control",
            Some(nonce.as_str()),
            "https://public.example",
            StatusCode::OK,
        ),
        (
            "/u/bob/desktop/api/control",
            Some(nonce.as_str()),
            "https://public.example",
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/u/alice/desktop/api/control",
            None,
            "https://public.example",
            StatusCode::FORBIDDEN,
        ),
        (
            "/u/alice/desktop/api/control",
            Some(nonce.as_str()),
            "https://other.example",
            StatusCode::FORBIDDEN,
        ),
    ] {
        assert_eq!(
            request(&f.app, path, "GET", json!({}), Some(&cookie), nonce, origin)
                .await
                .status(),
            status
        );
    }
    assert_eq!(f.engine.create_count.load(Ordering::SeqCst), 0);
    let response = request(
        &f.app,
        "/u/custom/desktop/",
        "GET",
        json!({}),
        None,
        None,
        "https://public.example",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let user = f.state.get("alice").unwrap();
    f.state.rotate_token("alice", &user.agent_space_id).unwrap();
    assert_eq!(
        request(
            &f.app,
            "/u/alice/desktop/api/session",
            "GET",
            json!({}),
            Some(&cookie),
            None,
            "https://public.example"
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
async fn takeover_refreshes_activity_without_polling_or_rejection_doing_so() {
    let f = fixture();
    let (cookie, nonce) = sign_in(&f).await;
    let (other_cookie, other_nonce) = sign_in(&f).await;
    let user = f.state.get("alice").unwrap();
    let hub = f.sessions.desktop(&user.sandbox_name).await.unwrap();
    let old = Instant::now() - Duration::from_secs(290);
    hub.session.set_last_work(old).await;
    hub.session.set_last_request(old).await;
    let path = "/u/alice/desktop/api/control";
    assert_eq!(
        request(
            &f.app,
            path,
            "GET",
            json!({}),
            Some(&cookie),
            Some(&nonce),
            "https://public.example"
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(hub.session.last_work().await, old);
    let accepted = Instant::now();
    let response = request(
        &f.app,
        path,
        "POST",
        json!({"mode":"HUMAN"}),
        Some(&cookie),
        Some(&nonce),
        "https://public.example",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let view: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    assert_eq!(view["control"], "HUMAN");
    assert_eq!(view["can_input"], true);
    assert_eq!(view["human_idle_secs"], 60);
    let refreshed = hub.session.last_work().await;
    assert!(refreshed >= accepted);
    assert_eq!(hub.session.last_request().await, refreshed);
    assert_eq!(
        request(
            &f.app,
            path,
            "POST",
            json!({"mode":"HUMAN"}),
            Some(&other_cookie),
            Some(&other_nonce),
            "https://public.example"
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(hub.session.last_work().await, refreshed);
    assert_eq!(
        request(
            &f.app,
            path,
            "POST",
            json!({"mode":"AI"}),
            Some(&cookie),
            Some(&nonce),
            "https://public.example"
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(hub.session.last_work().await, refreshed);
    assert_eq!(f.engine.create_count.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn cross_site_requests_and_cross_space_tokens_fail() {
    let f = fixture();
    let response = request(
        &f.app,
        "/u/bob/desktop/api/login",
        "POST",
        json!({"token":f.token}),
        None,
        None,
        "https://public.example",
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let request = Request::builder()
        .uri("/u/alice/desktop/api/login")
        .method("POST")
        .header("Content-Type", "application/json")
        .header("Origin", "https://other.example")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from(json!({"token":f.token}).to_string()))
        .unwrap();
    assert_eq!(
        f.app.oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
