//! Space-scoped workbench. Authentication uses the existing MCP identity;
//! browser page sessions contain no plaintext MCP/DAV token.
use agent_engine::{
    desktop::DesktopHub,
    session::SessionManager,
    state::{StateStore, UserRecord},
};
use axum::{
    extract::FromRequestParts,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Request, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use futures::SinkExt;
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
#[derive(Clone)]
pub struct DesktopState {
    pub sessions: Arc<SessionManager>,
    pub state: Arc<StateStore>,
    pub ui_dir: String,
    cookies: Arc<Mutex<HashMap<String, Arc<PageSession>>>>,
}
struct PageSession {
    id: String,
    username: String,
    space: String,
    key: String,
    nonce: String,
    origin: Option<String>,
    path: String,
    expires: Instant,
    cancel: CancellationToken,
}
impl DesktopState {
    pub fn new(sessions: Arc<SessionManager>, state: Arc<StateStore>, ui_dir: String) -> Self {
        Self {
            sessions,
            state,
            ui_dir,
            cookies: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
pub fn router(state: DesktopState) -> Router {
    let upkeep = state.clone();
    tokio::spawn(async move {
        let mut changes = upkeep.state.subscribe_changes();
        loop {
            tokio::select! {_ = tokio::time::sleep(Duration::from_secs(30))=>{},r=changes.changed()=>if r.is_err(){break;}}
            let mut cookies = upkeep.cookies.lock().await;
            cookies.retain(|_, page| {
                let keep = page.expires > Instant::now()
                    && upkeep
                        .state
                        .authorize_bound_current(&page.username, &page.space, &page.key)
                        .is_ok();
                if !keep {
                    page.cancel.cancel();
                }
                keep
            });
        }
    });
    let prefix = format!(
        "{}/desktop",
        crate::space_prefix(&state.state, "{username}")
    );
    let route = |tail: &str| format!("{prefix}{tail}");
    Router::new()
        .route(&route(""), get(redirect))
        .route(&route("/"), get(index))
        .route(&route("/assets/{*path}"), get(asset))
        .route(&route("/api/login"), post(login))
        .route(&route("/api/session"), get(session))
        .route(&route("/api/logout"), post(logout))
        .route(&route("/api/control"), get(control).post(change_control))
        .route(&route("/api/health"), get(health))
        .route(&route("/api/viewer"), post(viewer))
        .route(&route("/api/artifacts"), get(artifacts))
        .route(&route("/api/files/{*path}"), any(files))
        .route(&route("/ws"), get(websocket))
        .with_state(state)
}
/// Resolve the sole local identity or the managed route parameter once.
struct WorkspacePath {
    name: String,
    path: String,
}
impl FromRequestParts<DesktopState> for WorkspacePath {
    type Rejection = StatusCode;
    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &DesktopState,
    ) -> Result<Self, Self::Rejection> {
        let mut params =
            match Path::<HashMap<String, String>>::from_request_parts(parts, state).await {
                Ok(Path(values)) => values,
                Err(_) if state.state.is_standalone() => HashMap::new(),
                Err(_) => return Err(StatusCode::BAD_REQUEST),
            };
        let name = params
            .remove("username")
            .or_else(|| state.state.is_standalone().then(|| "workspace".into()))
            .ok_or(StatusCode::NOT_FOUND)?;
        Ok(Self {
            name,
            path: params.remove("path").unwrap_or_default(),
        })
    }
}
/// The workbench deliberately does not use a Host/IP allowlist. Its cookie
/// origin and nonce protect browser requests even when a proxy rewrites Host.
pub(crate) fn username(path: &str) -> Option<&str> {
    let (name, rest) = path.strip_prefix("/u/")?.split_once('/')?;
    if agent_engine::state::validate_username(name).is_ok()
        && (rest == "desktop" || rest.starts_with("desktop/"))
    {
        Some(name)
    } else {
        None
    }
}
fn error(code: StatusCode, name: &str) -> Response {
    (code, Json(json!({"error":name}))).into_response()
}
fn cookie_name(name: &str) -> String {
    format!("runcove_desktop_{name}")
}
fn origin(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}
fn browser_allowed(headers: &HeaderMap, page: Option<&PageSession>) -> bool {
    if headers.get_all(header::ORIGIN).iter().count() > 1 {
        return false;
    }
    if let Some(origin) = origin(headers) {
        if url::Url::parse(&origin).is_err()
            || !origin.starts_with("http://") && !origin.starts_with("https://")
        {
            return false;
        }
        if page.is_some_and(|page| page.origin.as_ref().is_some_and(|value| value != &origin)) {
            return false;
        }
    }
    headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| matches!(v, "same-origin" | "none"))
}
impl DesktopState {
    fn user(&self, name: &str) -> Result<UserRecord, (StatusCode, &'static str)> {
        let user = self
            .state
            .list()
            .into_iter()
            .find(|u| u.username == name)
            .ok_or((StatusCode::NOT_FOUND, "SPACE_NOT_FOUND"))?;
        if !user.image.as_ref().is_some_and(|i| i.browser()) {
            return Err((StatusCode::NOT_FOUND, "DESKTOP_UNAVAILABLE"));
        }
        Ok(user)
    }
    async fn page(
        &self,
        name: &str,
        headers: &HeaderMap,
        nonce: bool,
    ) -> Result<Arc<PageSession>, Response> {
        let key = cookie_name(name) + "=";
        let id = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .find_map(|v| v.trim().strip_prefix(&key))
            .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "SESSION_EXPIRED"))?;
        let page = self
            .cookies
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "SESSION_EXPIRED"))?;
        if page.username != name
            || page.cancel.is_cancelled()
            || page.expires <= Instant::now()
            || self
                .state
                .authorize_bound_current(name, &page.space, &page.key)
                .is_err()
        {
            page.cancel.cancel();
            return Err(error(StatusCode::UNAUTHORIZED, "SESSION_EXPIRED"));
        }
        if !browser_allowed(headers, Some(&page)) {
            return Err(error(StatusCode::FORBIDDEN, "CROSS_SITE_REQUEST"));
        }
        if nonce
            && headers.get("x-desktop-nonce").and_then(|v| v.to_str().ok()) != Some(&page.nonce)
        {
            return Err(error(StatusCode::FORBIDDEN, "INVALID_NONCE"));
        }
        Ok(page)
    }
    async fn hub(&self, name: &str) -> Result<Arc<DesktopHub>, Response> {
        let user = self.user(name).map_err(|(code, name)| error(code, name))?;
        self.sessions
            .register_image(&user.sandbox_name, user.image.as_ref().unwrap().reference())
            .map_err(|_| error(StatusCode::CONFLICT, "IMAGE_MISMATCH"))?;
        self.sessions
            .desktop(&user.sandbox_name)
            .await
            .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "DESKTOP_UNAVAILABLE"))
    }
}
async fn redirect() -> Response {
    (
        StatusCode::TEMPORARY_REDIRECT,
        [(header::LOCATION, "desktop/")],
    )
        .into_response()
}
async fn index(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
) -> Response {
    if let Err((code, name)) = state.user(&name) {
        return error(code, name);
    }
    serve(&state, "index.html").await
}
async fn asset(
    State(state): State<DesktopState>,
    WorkspacePath { name, path }: WorkspacePath,
) -> Response {
    if let Err((code, name)) = state.user(&name) {
        return error(code, name);
    }
    serve(&state, &format!("assets/{path}")).await
}
async fn serve(state: &DesktopState, path: &str) -> Response {
    if !std::path::Path::new(path)
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mime = match std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    match tokio::fs::read(std::path::Path::new(&state.ui_dir).join(path)).await {
        Ok(data) => {
            let mut response = (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CACHE_CONTROL, "no-store"),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                ],
                data,
            )
                .into_response();
            response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'self'"));
            response
        }
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "UI_NOT_BUILT"),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    token: String,
    #[serde(default)]
    cookie_path: Option<String>,
}
async fn login(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Response {
    if !browser_allowed(&headers, None) {
        return error(StatusCode::FORBIDDEN, "CROSS_SITE_REQUEST");
    }
    let user = match state.state.authenticate(&body.token) {
        Ok(user) if user.username == name && user.image.as_ref().is_some_and(|i| i.browser()) => {
            user
        }
        _ => return error(StatusCode::UNAUTHORIZED, "INVALID_TOKEN"),
    };
    let path = body
        .cookie_path
        .unwrap_or_else(|| format!("{}/desktop/", crate::space_prefix(&state.state, &name)));
    if path.len() > 2048 || !path.starts_with('/') || path.contains(['\r', '\n', ';']) {
        return error(StatusCode::BAD_REQUEST, "INVALID_COOKIE_PATH");
    }
    let id = format!("{}{}", uuid_string(), uuid_string());
    let nonce = uuid_string();
    let page = Arc::new(PageSession {
        id: id.clone(),
        username: name.clone(),
        space: user.agent_space_id,
        key: user.credential.unwrap().key_id,
        nonce: nonce.clone(),
        origin: origin(&headers),
        path: path.clone(),
        expires: Instant::now() + Duration::from_secs(12 * 3600),
        cancel: CancellationToken::new(),
    });
    let secure = page
        .origin
        .as_ref()
        .is_some_and(|o| o.starts_with("https://"));
    state.cookies.lock().await.insert(id.clone(), page);
    let cookie = format!(
        "{}={id}; Path={path}; HttpOnly; SameSite=Strict; Max-Age=43200{}",
        cookie_name(&name),
        if secure { "; Secure" } else { "" }
    );
    (
        [(header::SET_COOKIE, cookie)],
        Json(json!({"ok":true,"nonce":nonce})),
    )
        .into_response()
}
fn uuid_string() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
async fn session(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
    headers: HeaderMap,
) -> Response {
    match state.page(&name, &headers, false).await {
        Ok(page) => Json(json!({"nonce":page.nonce})).into_response(),
        Err(e) => e,
    }
}
async fn logout(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
    headers: HeaderMap,
) -> Response {
    let page = match state.page(&name, &headers, true).await {
        Ok(p) => p,
        Err(e) => return e,
    };
    page.cancel.cancel();
    state.cookies.lock().await.remove(&page.id);
    if let Ok(hub) = state.hub(&name).await {
        hub.control.disconnect(&page.id);
    }
    (
        [(
            header::SET_COOKIE,
            format!(
                "{}=; Path={}; HttpOnly; SameSite=Strict; Max-Age=0",
                cookie_name(&name),
                page.path
            ),
        )],
        Json(json!({"ok":true})),
    )
        .into_response()
}
async fn control(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
    headers: HeaderMap,
) -> Response {
    let page = match state.page(&name, &headers, true).await {
        Ok(p) => p,
        Err(e) => return e,
    };
    let hub = match state.hub(&name).await {
        Ok(h) => h,
        Err(e) => return e,
    };
    Json(hub.control.view(&page.id)).into_response()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    mode: String,
}
async fn change_control(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
    headers: HeaderMap,
    Json(body): Json<Control>,
) -> Response {
    let page = match state.page(&name, &headers, true).await {
        Ok(p) => p,
        Err(e) => return e,
    };
    let hub = match state.hub(&name).await {
        Ok(h) => h,
        Err(e) => return e,
    };
    let result = match body.mode.as_str() {
        "HUMAN" => hub.control.take(&page.id).await,
        "AI" => hub.control.give(&page.id).map_err(str::to_owned),
        _ => Err("INVALID_CONTROL".into()),
    };
    match result {
        Ok(()) => Json(hub.control.view(&page.id)).into_response(),
        Err(code) => error(StatusCode::CONFLICT, &code),
    }
}
async fn health(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = state.page(&name, &headers, true).await {
        return e;
    }
    let hub = match state.hub(&name).await {
        Ok(h) => h,
        Err(e) => return e,
    };
    Json(hub.health().await).into_response()
}
async fn viewer(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = state.page(&name, &headers, true).await {
        return e;
    }
    let hub = match state.hub(&name).await {
        Ok(h) => h,
        Err(e) => return e,
    };
    match hub.viewer().await {
        Ok(v) => Json(v).into_response(),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "DESKTOP_START_FAILED"),
    }
}
async fn artifacts(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = state.page(&name, &headers, true).await {
        return e;
    }
    let hub = match state.hub(&name).await {
        Ok(h) => h,
        Err(e) => return e,
    };
    match hub.artifacts().await {
        Ok(v) => Json(v).into_response(),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "ARTIFACTS_UNAVAILABLE"),
    }
}
async fn files(
    State(state): State<DesktopState>,
    WorkspacePath { name, path }: WorkspacePath,
    mut request: Request,
) -> Response {
    let write = request.method() == http::Method::PUT;
    if !matches!(request.method().as_str(), "GET" | "HEAD" | "PUT") {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let page = match state.page(&name, request.headers(), write).await {
        Ok(p) => p,
        Err(e) => return e,
    };
    let user = match state
        .state
        .authorize_bound_current(&name, &page.space, &page.key)
    {
        Ok(u) => u,
        Err(_) => return error(StatusCode::UNAUTHORIZED, "SESSION_EXPIRED"),
    };
    let encoded = path
        .split('/')
        .map(|s| {
            percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
        })
        .collect::<Vec<_>>()
        .join("/");
    *request.uri_mut() = format!("{}/dav/{encoded}", crate::space_prefix(&state.state, &name))
        .parse()
        .unwrap();
    if write {
        request
            .headers_mut()
            .insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
    }
    let dav = crate::webdav::DavState {
        sessions: state.sessions.clone(),
        state: state.state.clone(),
        origin: url::Url::parse("http://127.0.0.1").unwrap(),
        helper_binary: std::env::var("AGENT_SPACE_WEBDAV")
            .unwrap_or_else(|_| "/usr/local/bin/agent-space-webdav".into()),
        startup_timeout: Duration::from_secs(30),
        io_idle_timeout: Duration::from_secs(60),
    };
    crate::webdav::forward_authenticated(dav, user, request, true, state.state.subscribe_changes())
        .await
}
async fn websocket(
    State(state): State<DesktopState>,
    WorkspacePath { name, .. }: WorkspacePath,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let page = match state.page(&name, &headers, false).await {
        Ok(p) => p,
        Err(e) => return e,
    };
    let nonce = request
        .uri()
        .query()
        .unwrap_or_default()
        .strip_prefix("nonce=");
    if nonce != Some(&page.nonce) {
        return error(StatusCode::FORBIDDEN, "INVALID_NONCE");
    }
    let hub = match state.hub(&name).await {
        Ok(h) => h,
        Err(e) => return e,
    };
    if hub.health().await["desktop"] != true {
        return error(StatusCode::CONFLICT, "DESKTOP_ASLEEP");
    }
    let upgrade =
        match WebSocketUpgrade::from_request_parts(&mut request.into_parts().0, &state).await {
            Ok(upgrade) => upgrade,
            Err(_) => return error(StatusCode::BAD_REQUEST, "INVALID_WEBSOCKET"),
        };
    upgrade
        .protocols(["binary"])
        .max_message_size(1024 * 1024)
        .on_upgrade(move |ws| bridge(ws, hub, page))
}
async fn bridge(mut ws: WebSocket, hub: Arc<DesktopHub>, page: Arc<PageSession>) {
    let mut tcp = match hub.session.connect_guest_tcp(5901).await {
        Ok(tcp) => tcp,
        Err(_) => return,
    };
    let mut input = crate::rfb::Input::default();
    let mut data = [0; 65536];
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut input_at = Instant::now() - Duration::from_secs(2);
    loop {
        tokio::select! {
            _=page.cancel.cancelled()=>break,
            _=tick.tick()=>{if Instant::now()>=page.expires{break;}
                    if !hub.control.view(&page.id).can_input{for packet in input.release(){if tcp.writer.write_all(&packet).await.is_err(){break;}}}},
            result=tcp.reader.read(&mut data)=>match result{Ok(n) if n>0=>if ws.send(Message::Binary(data[..n].to_vec().into())).await.is_err(){break;},_=>break},
            message=ws.recv()=>match message {
                Some(Ok(Message::Binary(data)))=>{
                    let packets=match input.feed(&data){Ok(p)=>p,Err(_)=>break};
                    for packet in packets {
                        if !packet.input||hub.control.input(&page.id) {
                            if packet.input {input.accepted(&packet.bytes);hub.session.touch_work().await;if input_at.elapsed()>Duration::from_secs(1){let _=hub.input().await;input_at=Instant::now();}}
                            if tcp.writer.write_all(&packet.bytes).await.is_err(){break;}
                        }
                    }
                },
                Some(Ok(Message::Ping(data)))=>if ws.send(Message::Pong(data)).await.is_err(){break;},
                Some(Ok(Message::Pong(_)))=>{},
                _=>break,
            }
        }
    }
    for packet in input.release() {
        let _ = tcp.writer.write_all(&packet).await;
    }
    hub.control.disconnect(&page.id);
    let _ = ws.close().await;
}
