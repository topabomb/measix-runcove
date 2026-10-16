//! Authenticated DAV ingress. Credentials stay on the Host; the guest sees
//! the external DAV path, validated condition headers and a streamed body.
#[path = "../../build-support/dav_path.rs"]
mod dav_path;

use crate::guest_http::{forwarded_headers, ConnectionGuard, GuestIo, Progress, ResponseLease};
use crate::origins::{browser_request_allowed, dav_username, OriginPolicy};
use agent_engine::{
    engine::{EngineError, ManagedLaunch},
    session::SessionManager,
    state::{StateStore, UserRecord},
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use base64::Engine;
use futures::StreamExt;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct DavState {
    pub sessions: Arc<SessionManager>,
    pub state: Arc<StateStore>,
    pub origin: url::Url,
    pub helper_binary: String,
    pub startup_timeout: Duration,
    pub io_idle_timeout: Duration,
}
pub fn router(state: DavState) -> Router {
    let prefix = format!("{}/dav", crate::space_prefix(&state.state, "{username}"));
    Router::new()
        .route(&prefix, any(handle))
        .route(&format!("{prefix}/"), any(handle))
        .route(&format!("{prefix}/{{*path}}"), any(handle))
        .with_state(state)
}

fn unauthorized() -> Response {
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Agent Space files\", charset=\"UTF-8\""),
    );
    response
}
fn unavailable() -> Response {
    let mut response = StatusCode::SERVICE_UNAVAILABLE.into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}
/// Used only before any guest HTTP request has been forwarded.
fn admission_unavailable(error: &EngineError) -> Response {
    let mut response = unavailable();
    if matches!(error, EngineError::WorkspaceCapacityExhausted) {
        response.headers_mut().insert(
            "agent-space-error-code",
            HeaderValue::from_static("workspace_capacity_exhausted"),
        );
    }
    response
}
fn authenticate(
    state: &StateStore,
    headers: &HeaderMap,
    username: &str,
) -> Result<UserRecord, StatusCode> {
    if state.ensure_admission_open().is_err() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values
        .next()
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if values.next().is_some() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (scheme, encoded) = value.split_once(' ').ok_or(StatusCode::UNAUTHORIZED)?;
    let basic;
    let (basic_name, token) = if scheme.eq_ignore_ascii_case("bearer") {
        (None, encoded)
    } else if scheme.eq_ignore_ascii_case("basic") {
        basic = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| StatusCode::UNAUTHORIZED)?,
        )
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
        let (name, secret) = basic.split_once(':').ok_or(StatusCode::UNAUTHORIZED)?;
        (Some(name), secret)
    } else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let user = state
        .authenticate_dav(token)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    if user.username != username || basic_name.is_some_and(|name| name != username) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(user)
}

fn resource_uri(value: &str, origin: &url::Url, prefix: &str) -> Result<String, StatusCode> {
    if value.contains('#') || value.starts_with("//") {
        return Err(StatusCode::FORBIDDEN);
    }
    let uri = value
        .parse::<http::Uri>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if uri.query().is_some() {
        return Err(StatusCode::FORBIDDEN);
    }
    let path = if uri.path() == prefix.trim_end_matches('/') {
        prefix
    } else {
        uri.path()
    };
    dav_path::validate_path(path, prefix).map_err(|_| StatusCode::FORBIDDEN)?;
    if uri.scheme().is_some() || uri.authority().is_some() {
        let url = url::Url::parse(value).map_err(|_| StatusCode::BAD_REQUEST)?;
        if url.origin() != origin.origin() || !url.username().is_empty() || url.password().is_some()
        {
            return Err(StatusCode::FORBIDDEN);
        }
    } else if !value.starts_with('/') {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(path.to_string())
}

/// RFC 4918 10.4 grammar: only Resource-Tag outside lists denotes a path.
/// State tokens inside lists are opaque and are never treated as resources.
fn normalize_if(value: &str, origin: &url::Url, prefix: &str) -> Result<String, StatusCode> {
    let mut input = value.trim();
    let tagged = input.starts_with('<');
    let mut output = String::new();
    let mut lists = 0;
    while !input.is_empty() {
        if input.starts_with('<') {
            if !tagged {
                return Err(StatusCode::BAD_REQUEST);
            }
            let end = input.find('>').ok_or(StatusCode::BAD_REQUEST)?;
            let path = resource_uri(&input[1..end], origin, prefix)?;
            output.push_str(&format!(
                "<{}{}> ",
                origin.origin().ascii_serialization(),
                path
            ));
            input = input[end + 1..].trim_start();
        }
        if !input.starts_with('(') {
            return Err(StatusCode::BAD_REQUEST);
        }
        output.push('(');
        input = input[1..].trim_start();
        let mut count = 0;
        while !input.starts_with(')') {
            if let Some(rest) = input.strip_prefix("Not") {
                output.push_str("Not ");
                input = rest.trim_start();
            }
            if input.starts_with('<') {
                let end = input.find('>').ok_or(StatusCode::BAD_REQUEST)?;
                let token = &input[1..end];
                if token.is_empty()
                    || token.bytes().any(|b| b.is_ascii_whitespace() || b == b'<')
                    || !token.contains(':')
                {
                    return Err(StatusCode::BAD_REQUEST);
                }
                output.push_str(&input[..=end]);
                input = input[end + 1..].trim_start();
            } else if input.starts_with('[') {
                let start = if input.starts_with("[W/\"") {
                    4
                } else if input.starts_with("[\"") {
                    2
                } else {
                    return Err(StatusCode::BAD_REQUEST);
                };
                let end = start + input[start..].find('"').ok_or(StatusCode::BAD_REQUEST)? + 1;
                if input.as_bytes().get(end) != Some(&b']') {
                    return Err(StatusCode::BAD_REQUEST);
                }
                let etag = input[1..end].strip_prefix("W/").unwrap_or(&input[1..end]);
                if etag.len() < 2
                    || !etag.starts_with('"')
                    || !etag.ends_with('"')
                    || etag[1..etag.len() - 1].contains('"')
                {
                    return Err(StatusCode::BAD_REQUEST);
                }
                output.push_str(&input[..=end]);
                input = input[end + 1..].trim_start();
            } else {
                return Err(StatusCode::BAD_REQUEST);
            }
            output.push(' ');
            count += 1;
        }
        if count == 0 {
            return Err(StatusCode::BAD_REQUEST);
        }
        output.push_str(") ");
        input = input[1..].trim_start();
        lists += 1;
    }
    if lists == 0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(output.trim_end().to_string())
}

pub async fn handle(State(mut state): State<DavState>, request: Request) -> Response {
    state.origin = match OriginPolicy::new(Some(state.origin.as_str()))
        .unwrap()
        .resolve(&request)
    {
        Ok(origin) => origin,
        Err(status) => return status.into_response(),
    };
    if !browser_request_allowed(&request, &state.origin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let username = if state.state.is_standalone() {
        Some("workspace".to_owned())
    } else {
        dav_username(request.uri().path()).map(str::to_owned)
    };
    let Some(username) = username else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Subscribe before authentication so a rotation during startup is not lost.
    let changes = state.state.subscribe_changes();
    let user = match authenticate(&state.state, request.headers(), &username) {
        Ok(user) => user,
        Err(StatusCode::UNAUTHORIZED) => return unauthorized(),
        Err(code) => return code.into_response(),
    };
    forward_authenticated(state, user, request, false, changes).await
}

/// Desktop cookies use the space's MCP identity; file bytes still use the
/// existing DAV helper, filesystem validation, Range and streaming lifecycle.
#[tracing::instrument(skip_all, fields(username = %user.username, agentSpaceId = %user.agent_space_id, sandboxName = %user.sandbox_name))]
pub(crate) async fn forward_authenticated(
    state: DavState,
    user: UserRecord,
    mut request: Request,
    mcp: bool,
    mut changes: tokio::sync::watch::Receiver<()>,
) -> Response {
    let startup_started = std::time::Instant::now();
    let username = user.username.clone();
    let prefix = format!("{}/dav/", crate::space_prefix(&state.state, &username));
    let path = match resource_uri(&request.uri().to_string(), &state.origin, &prefix) {
        Ok(path) => path,
        Err(code) => return code.into_response(),
    };
    *request.uri_mut() = path.parse().unwrap();
    for name in ["destination", "if"] {
        if request.headers().get_all(name).iter().count() > 1 {
            return StatusCode::BAD_REQUEST.into_response();
        }
        if let Some(value) = request.headers().get(name) {
            let normalized = value
                .to_str()
                .map_err(|_| StatusCode::BAD_REQUEST)
                .and_then(|value| {
                    if name == "destination" {
                        resource_uri(value, &state.origin, &prefix)
                    } else {
                        normalize_if(value, &state.origin, &prefix)
                    }
                });
            match normalized.and_then(|value| {
                HeaderValue::from_str(&value).map_err(|_| StatusCode::BAD_REQUEST)
            }) {
                Ok(value) => {
                    request.headers_mut().insert(name, value);
                }
                Err(code) => return code.into_response(),
            }
        }
    }
    let key = if mcp {
        user.credential.as_ref()
    } else {
        user.dav_credential.as_ref()
    }
    .unwrap()
    .key_id
    .clone();
    let external_cancel = if mcp {
        request
            .extensions_mut()
            .remove::<CancellationToken>()
            .unwrap_or_default()
    } else {
        CancellationToken::new()
    };
    let cancel = CancellationToken::new();
    let lifetime = cancel.clone().drop_guard();
    let authority = {
        let store = state.state.clone();
        let name = user.username.clone();
        let space = user.agent_space_id.clone();
        let key = key.clone();
        move || {
            if mcp {
                store
                    .authorize_bound_current(&name, &space, &key)
                    .map(|_| ())
            } else {
                store.authorize_dav_current(&name, &space, &key).map(|_| ())
            }
        }
    };
    let monitor = {
        let cancel = cancel.clone();
        let authority = authority.clone();
        ConnectionGuard(tokio::spawn(async move {
            loop {
                if authority().is_err() {
                    cancel.cancel();
                    return;
                }
                tokio::select! { _ = cancel.cancelled() => return, _ = external_cancel.cancelled() => {cancel.cancel();return;}, result = changes.changed() => if result.is_err() { cancel.cancel(); return; } }
            }
        }))
    };
    let session = match state.sessions.get_session(&user.sandbox_name).await {
        Ok(session) => session.with_admission_check({
            let authority = authority.clone();
            move || authority().map_err(|_| EngineError::Other("DAV authority was revoked".into()))
        }),
        Err(error) => {
            tracing::warn!(%error, phase = "session_owner", elapsed_ms = startup_started.elapsed().as_millis() as u64, "DAV runtime admission or connection failed");
            return unavailable();
        }
    };
    let launch = ManagedLaunch::Webdav {
        host_binary: state.helper_binary.clone(),
        owner: user.agent_space_id,
        config: serde_json::json!({"schemaVersion":1,"root":"/workspace","prefix":prefix,"lockTimeoutSecs":1800,"ioIdleTimeoutSecs":state.io_idle_timeout.as_secs()}),
    };
    let ready = tokio::select! { biased; _ = cancel.cancelled() => return unauthorized(), result = session.begin_dav_request(launch, state.startup_timeout) => match result { Ok(ready) => ready, Err(error) => { tracing::warn!(%error, elapsed_ms = startup_started.elapsed().as_millis() as u64, "DAV runtime admission or connection failed"); return admission_unavailable(&error); } } };
    let lease = ResponseLease(Arc::new(Mutex::new(Some(ready.lease))));
    let progress = Progress::new();
    let tcp_started = std::time::Instant::now();
    let tcp = tokio::select! { biased; _ = cancel.cancelled() => return unauthorized(), _ = ready.cancel.cancelled() => return unavailable(), _ = progress.idle(state.io_idle_timeout) => { tracing::warn!(phase = "dav_guest_tcp", elapsed_ms = startup_started.elapsed().as_millis() as u64, phase_elapsed_ms = tcp_started.elapsed().as_millis() as u64, "DAV guest connection deadline exceeded"); return unavailable(); }, result = session.connect_dav_guest_tcp(ready.port) => match result { Ok(tcp) => tcp, Err(error) => { tracing::warn!(%error, phase = "dav_guest_tcp", elapsed_ms = startup_started.elapsed().as_millis() as u64, phase_elapsed_ms = tcp_started.elapsed().as_millis() as u64, "DAV runtime admission or connection failed"); return admission_unavailable(&error); } } };
    if authority().is_err() {
        return unauthorized();
    }
    let handshake_started = std::time::Instant::now();
    let (mut sender, connection) = tokio::select! { biased; _ = cancel.cancelled() => return unauthorized(), _ = ready.cancel.cancelled() => return unavailable(), _ = progress.idle(state.io_idle_timeout) => { tracing::warn!(phase = "dav_guest_http_handshake", elapsed_ms = startup_started.elapsed().as_millis() as u64, phase_elapsed_ms = handshake_started.elapsed().as_millis() as u64, "DAV guest connection deadline exceeded"); return unavailable(); }, result = http1::handshake::<_, Body>(TokioIo::new(GuestIo::new(tcp).with_progress({ let progress = progress.clone(); move |bytes| progress.tick(bytes) }))) => match result { Ok(pair) => pair, Err(error) => { tracing::warn!(%error, phase = "dav_guest_http_handshake", elapsed_ms = startup_started.elapsed().as_millis() as u64, phase_elapsed_ms = handshake_started.elapsed().as_millis() as u64, "DAV guest HTTP handshake failed"); return StatusCode::BAD_GATEWAY.into_response(); } } };
    let connection = {
        let active = lease.0.clone();
        let cancel = cancel.clone();
        let progress = progress.clone();
        let epoch = ready.cancel;
        let idle = state.io_idle_timeout;
        ConnectionGuard(tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {},
                _ = epoch.cancelled() => {},
                _ = progress.idle(idle) => { tracing::debug!("DAV guest connection idle"); },
                result = connection => { if let Err(error) = result { tracing::debug!(%error, "DAV guest HTTP connection ended"); } tokio::select! { _ = cancel.cancelled() => {}, _ = epoch.cancelled() => {}, _ = progress.idle(idle) => {} } },
            }
            cancel.cancel();
            active.lock().unwrap().take();
        }))
    };
    let (mut parts, body) = request.into_parts();
    let unlimited = parts.method == http::Method::PUT;
    parts.headers = forwarded_headers(&parts.headers, false);
    let private_headers = parts
        .headers
        .keys()
        .filter(|name| {
            matches!(name.as_str(), "cookie" | "forwarded" | "expect")
                || name.as_str().starts_with("x-forwarded-")
                || name.as_str().starts_with("x-agent-space-")
                || name.as_str().starts_with("x-msb-")
        })
        .cloned()
        .collect::<Vec<_>>();
    for name in private_headers {
        parts.headers.remove(name);
    }
    parts
        .headers
        .insert(header::HOST, HeaderValue::from_static("127.0.0.1"));
    parts
        .headers
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    if !unlimited
        && parts
            .headers
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|len| len > 1024 * 1024)
    {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let overflow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stream = {
        let cancel = cancel.clone();
        let progress = progress.clone();
        let overflow = overflow.clone();
        async_stream::stream! {
            let mut incoming = body.into_data_stream(); let mut size = 0usize;
            loop {
                let frame = tokio::select! { biased; _ = cancel.cancelled() => { yield Err::<bytes::Bytes, io::Error>(io::Error::other("DAV cancelled")); break; }, frame = incoming.next() => frame };
                match frame {
                    None => break,
                    Some(Err(error)) => { yield Err(io::Error::other(error)); break; },
                    Some(Ok(bytes)) => {
                        size = size.saturating_add(bytes.len());
                        if !unlimited && size > 1024*1024 { overflow.store(true, std::sync::atomic::Ordering::SeqCst); yield Err(io::Error::other("DAV control body too large")); break; }
                        progress.tick(bytes.len()); yield Ok(bytes);
                    }
                }
            }
        }
    };
    let upstream = tokio::select! { biased; _ = cancel.cancelled() => return unavailable(), result = sender.send_request(http::Request::from_parts(parts, Body::from_stream(stream))) => match result { Ok(response) => response, Err(_) => return if overflow.load(std::sync::atomic::Ordering::SeqCst) { StatusCode::PAYLOAD_TOO_LARGE.into_response() } else { StatusCode::BAD_GATEWAY.into_response() } } };
    drop(sender);
    progress.tick(1); // Arrival of response headers is actual upstream progress.
    let (mut parts, body) = upstream.into_parts();
    parts.headers = forwarded_headers(&parts.headers, false);
    // This header proves Host admission failed before forwarding; a guest
    // response cannot supply that guarantee.
    parts.headers.remove("agent-space-error-code");
    parts.headers.remove(header::SET_COOKIE);
    parts.headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment"),
    );
    parts.headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    parts.headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox allow-downloads"),
    );
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let stream = async_stream::stream! {
        let (_lease, _connection, _monitor, _lifetime) = (lease, connection, monitor, lifetime);
        let mut incoming = Body::new(body).into_data_stream();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => { yield Err::<bytes::Bytes, io::Error>(io::Error::other("DAV cancelled or idle")); break; },
                frame = incoming.next() => match frame {
                    None => break,
                    Some(Err(error)) => { yield Err(io::Error::other(error)); break; },
                    Some(Ok(bytes)) => { progress.tick(bytes.len()); yield Ok(bytes); },
                }
            }
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_typed_workspace_capacity_rejection_has_an_error_code() {
        let capacity = admission_unavailable(&EngineError::WorkspaceCapacityExhausted);
        assert_eq!(capacity.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(capacity.headers()["retry-after"], "1");
        assert_eq!(
            capacity.headers()["agent-space-error-code"],
            "workspace_capacity_exhausted"
        );
        for error in [
            EngineError::ResourceExhausted("DAV instance is draining".into()),
            EngineError::ResourceExhausted("maximum number of active workspaces reached".into()),
            EngineError::Timeout("startup deadline exceeded".into()),
            EngineError::SandboxNotRunning("cached client closed".into()),
            EngineError::Other("guest peer refused connection".into()),
        ] {
            let response = admission_unavailable(&error);
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()["retry-after"], "1");
            assert!(!response.headers().contains_key("agent-space-error-code"));
        }
    }

    #[test]
    fn destinations_and_if_tags_keep_conditions_inside_one_dav_root() {
        let origin = url::Url::parse("https://files.example").unwrap();
        let prefix = "/u/alice/dav/";
        assert_eq!(
            resource_uri("https://files.example/u/alice/dav/a%20b", &origin, prefix).unwrap(),
            "/u/alice/dav/a%20b"
        );
        for value in [
            "//evil/u/alice/dav/a",
            "https://evil/u/alice/dav/a",
            "https://name@files.example/u/alice/dav/a",
            "/u/bob/dav/a",
            "/u/alice/dav/%2e%2e/a",
            "/u/alice/dav/a%2fb",
            "/u/alice/dav/a?x",
            "/u/alice/dav/a#x",
        ] {
            assert!(resource_uri(value, &origin, prefix).is_err(), "{value}");
        }
        for path in [
            ".agent-space-upload/a",
            ".agent-space/notes",
            "nested/.agent-space/uploads/a",
        ] {
            let path = format!("{prefix}{path}");
            assert_eq!(resource_uri(&path, &origin, prefix).unwrap(), path);
        }
        let valid = r#"</u/alice/dav/a> (<opaquelocktoken:abc> [W/"x]y"]) (Not <DAV:no-lock>)"#;
        let normalized = normalize_if(valid, &origin, prefix).unwrap();
        assert!(normalized.contains("<https://files.example/u/alice/dav/a>"));
        assert!(
            normalized.contains("<opaquelocktoken:abc>") && normalized.contains(r#"[W/"x]y"]"#)
        );
        assert!(
            normalize_if(
                "(Not <https://opaque-lock-provider/token>)",
                &origin,
                prefix
            )
            .is_ok(),
            "state tokens are not resource URLs"
        );
        for invalid in [
            "",
            "()",
            "</u/alice/dav/a>",
            "(<urn:x>) </u/alice/dav/a> (<urn:x>)",
            "<https://evil/u/alice/dav/a> (<urn:x>)",
            "</u/bob/dav/a> (<urn:x>)",
            "(Not)",
            "((<urn:x>))",
        ] {
            assert!(normalize_if(invalid, &origin, prefix).is_err(), "{invalid}");
        }
    }
    #[tokio::test(start_paused = true)]
    async fn idle_deadline_extends_only_on_nonempty_progress() {
        let progress = Progress::new();
        let mut timer = Box::pin(progress.idle(Duration::from_secs(10)));
        tokio::time::advance(Duration::from_secs(9)).await;
        progress.tick(4);
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(futures::poll!(&mut timer).is_pending());
        progress.tick(0);
        tokio::time::advance(Duration::from_secs(1)).await;
        timer.await;
    }
}
