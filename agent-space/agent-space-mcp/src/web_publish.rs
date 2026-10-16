//! Path-scoped runtime publication gateway. Paths are not browser origin isolation.
use crate::guest_http::{forwarded_headers, ConnectionGuard, GuestIo, Progress, ResponseLease};
use crate::origins::{browser_request_allowed, OriginPolicy};
use agent_engine::{
    publication::Publication,
    session::SessionManager,
    state::{StateStore, UserStatus},
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine;
use futures::StreamExt;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
use std::{
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct PublishState {
    pub sessions: Arc<SessionManager>,
    pub state: Option<Arc<StateStore>>,
    pub origin: url::Url,
    pub enabled: bool,
    pub startup_timeout: Duration,
    pub io_idle_timeout: Duration,
}

pub(crate) fn publication_prefix(state: Option<&StateStore>, item: &Publication) -> String {
    if state.is_some_and(StateStore::is_standalone) {
        format!("/pub/{}/", item.name)
    } else {
        item.path_prefix()
    }
}

// Recognize only the reserved publication route; other routes stay with their owner.
pub async fn dispatch(
    State(mut state): State<PublishState>,
    request: Request,
    next: Next,
) -> Response {
    let single_path;
    let path = if state
        .state
        .as_deref()
        .is_some_and(StateStore::is_standalone)
    {
        let Some(path) = request.uri().path().strip_prefix("/pub/") else {
            return next.run(request).await;
        };
        single_path = format!("workspace/pub/{path}");
        single_path.as_str()
    } else {
        let Some(path) = request.uri().path().strip_prefix("/u/") else {
            return next.run(request).await;
        };
        path
    };
    let segments: Vec<_> = path.splitn(4, '/').collect();
    if segments.get(1) != Some(&"pub") {
        return next.run(request).await;
    }
    if !state.enabled || segments.len() < 3 || segments[..3].iter().any(|part| part.is_empty()) {
        return StatusCode::NOT_FOUND.into_response();
    }
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
    let owner = segments[0].to_owned();
    let name = segments[2].to_owned();
    let remainder = segments.get(3).map(|s| s.to_string());
    forward(state, request, &owner, &name, remainder).await
}

async fn forward(
    state: PublishState,
    request: Request,
    owner: &str,
    name: &str,
    remainder: Option<String>,
) -> Response {
    let Ok((session, publication)) = state.sessions.find_published(owner, name).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if publication.username != owner {
        return StatusCode::NOT_FOUND.into_response();
    }
    let prefix = publication_prefix(state.state.as_deref(), &publication);
    let origin = &state.origin;
    if let Some(store) = &state.state {
        if store.ensure_admission_open().is_err() {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if !store
            .get_by_space(&publication.agent_space_id)
            .is_some_and(|user| {
                user.status == UserStatus::Active
                    && user.username == publication.username
                    && user.sandbox_name == session.workspace_name
            })
        {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    // Challenge only beneath the slash-terminated root: Basic clients may reuse
    // credentials for the parent of a challenged URL.
    let Some(remainder) = remainder else {
        let location = match request.uri().query() {
            Some(query) => format!("{prefix}?{query}"),
            None => prefix.clone(),
        };
        let mut response = StatusCode::PERMANENT_REDIRECT.into_response();
        response.headers_mut().insert(
            header::LOCATION,
            HeaderValue::from_str(&location).expect("request path"),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    };
    let upstream_path = match request.uri().query() {
        Some(query) => format!("/{remainder}?{query}"),
        None => format!("/{remainder}"),
    };
    let keep_authorization = match authorize(&publication, request.headers()) {
        Ok(keep) => keep,
        Err(response) => return *response,
    };
    let ready = async {
        let lease = session
            .begin_publish_request(&publication.epoch)
            .await
            .ok_or_else(|| io::Error::other("publication closed"))?;
        // Connecting resumes an existing paused VM. Check helper identity only
        // after that, but still check it when connect failed to revoke dead helpers.
        let tcp = session.connect_guest_tcp(publication.port).await;
        session.ensure_helper_alive(&publication).await?;
        let tcp = tcp?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((lease, tcp))
    };
    let (lease, tcp) = tokio::select! {
        biased;
        _ = publication.epoch.cancelled() => return StatusCode::NOT_FOUND.into_response(),
        result = tokio::time::timeout(state.startup_timeout, ready) => match result {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => return if publication.epoch.is_cancelled() { StatusCode::NOT_FOUND.into_response() } else { StatusCode::BAD_GATEWAY.into_response() },
            Err(_) => return StatusCode::GATEWAY_TIMEOUT.into_response(),
        }
    };
    let progress = Progress::new();
    let io = GuestIo::new(tcp).with_progress({
        let progress = progress.clone();
        move |bytes| progress.tick(bytes)
    });
    let (mut sender, connection) = match tokio::time::timeout(
        state.startup_timeout,
        http1::handshake::<_, Body>(TokioIo::new(io)),
    )
    .await
    {
        Ok(Ok(pair)) => pair,
        _ => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let lease = ResponseLease(Arc::new(Mutex::new(Some(lease))));
    let cancel = CancellationToken::new();
    let connection = {
        let cancel = cancel.clone();
        let epoch = publication.epoch.clone();
        let active = lease.0.clone();
        let progress = progress.clone();
        let idle = state.io_idle_timeout;
        ConnectionGuard(tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = epoch.cancelled() => {},
                _ = progress.idle(idle) => {},
                _ = connection => {
                    // Upstream EOF does not release an unconsumed client response.
                    tokio::select! { _ = epoch.cancelled() => {}, _ = progress.idle(idle) => {} }
                }
            }
            cancel.cancel();
            active.lock().unwrap().take();
        }))
    };
    let (mut parts, body) = request.into_parts();
    parts.uri = upstream_path.parse().expect("valid path and query");
    parts.headers = forwarded_headers(&parts.headers, keep_authorization);
    let private: Vec<_> = parts
        .headers
        .keys()
        .filter(|name| {
            name.as_str() == "x-publish-key"
                || name.as_str() == "forwarded"
                || name.as_str().starts_with("x-forwarded-")
        })
        .cloned()
        .collect();
    for name in private {
        parts.headers.remove(name);
    }
    let authority = &origin[url::Position::BeforeHost..url::Position::AfterPort];
    parts.headers.insert(
        header::HOST,
        HeaderValue::from_str(authority).expect("origin authority"),
    );
    parts.headers.insert(
        "x-forwarded-host",
        HeaderValue::from_str(authority).expect("origin authority"),
    );
    parts.headers.insert(
        "x-forwarded-proto",
        HeaderValue::from_str(origin.scheme()).expect("scheme"),
    );
    parts.headers.insert(
        "x-forwarded-prefix",
        HeaderValue::from_str(prefix.trim_end_matches('/')).expect("publication prefix"),
    );
    parts
        .headers
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    let stream = {
        let cancel = cancel.clone();
        let progress = progress.clone();
        async_stream::stream! {
            let mut source = body.into_data_stream();
            loop {
                tokio::select! { biased;
                    _ = cancel.cancelled() => { yield Err::<bytes::Bytes, io::Error>(io::Error::other("publication cancelled or idle")); break; },
                    frame = source.next() => match frame {
                        None => break,
                        Some(Err(error)) => { yield Err(io::Error::other(error)); break; },
                        Some(Ok(bytes)) => { progress.tick(bytes.len()); yield Ok(bytes); }
                    }
                }
            }
        }
    };
    let upstream = tokio::select! { biased;
        _ = cancel.cancelled() => return if publication.epoch.is_cancelled() { StatusCode::NOT_FOUND.into_response() } else { StatusCode::GATEWAY_TIMEOUT.into_response() },
        result = sender.send_request(http::Request::from_parts(parts, Body::from_stream(stream))) => match result { Ok(response) => response, Err(_) => return StatusCode::BAD_GATEWAY.into_response() }
    };
    drop(sender);
    progress.tick(1);
    let (mut parts, body) = upstream.into_parts();
    parts.headers = forwarded_headers(&parts.headers, true);
    // Compatibility scoping, not a same-origin script security boundary.
    let cookies: Vec<_> = parts
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|cookie| scoped_cookie(cookie, &prefix))
        .collect();
    parts.headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        parts.headers.append(header::SET_COOKIE, cookie);
    }
    parts.headers.remove("service-worker-allowed");
    parts.headers.remove("clear-site-data");
    if publication.helper.is_some() {
        if let Some(location) = parts
            .headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
        {
            if location.starts_with('/') && !location.starts_with("//") {
                if let Ok(value) = HeaderValue::from_str(&format!("{}{}", prefix, &location[1..])) {
                    parts.headers.insert(header::LOCATION, value);
                }
            }
        }
    }
    parts
        .headers
        .insert("origin-agent-cluster", HeaderValue::from_static("?1"));
    parts.headers.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    parts.headers.insert(
        "permissions-policy",
        HeaderValue::from_static("document-domain=()"),
    );
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let stream = async_stream::stream! {
        let (_lease, _connection) = (lease, connection);
        let mut source = Body::new(body).into_data_stream();
        loop {
            tokio::select! { biased;
                _ = cancel.cancelled() => { yield Err::<bytes::Bytes, io::Error>(io::Error::other("publication cancelled or idle")); break; },
                frame = source.next() => match frame {
                    None => break,
                    Some(Err(error)) => { yield Err(io::Error::other(error)); break; },
                    Some(Ok(bytes)) => { progress.tick(bytes.len()); yield Ok(bytes); }
                }
            }
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

fn scoped_cookie(cookie: &str, prefix: &str) -> Option<HeaderValue> {
    let mut parts = cookie.split(';');
    let mut result = parts.next()?.to_owned();
    let mut path = prefix.to_owned();
    for part in parts {
        if let Some((key, value)) = part.trim().split_once('=') {
            if key.trim().eq_ignore_ascii_case("domain") {
                continue;
            }
            if key.trim().eq_ignore_ascii_case("path") {
                let value = value.trim();
                path = if value.starts_with(prefix) || value == prefix.trim_end_matches('/') {
                    value.to_owned()
                } else if value.starts_with('/') {
                    format!("{prefix}{}", value.trim_start_matches('/'))
                } else {
                    prefix.to_owned()
                };
                continue;
            }
        }
        result.push(';');
        result.push_str(part);
    }
    HeaderValue::from_str(&format!("{result}; Path={path}")).ok()
}

fn credential_equal(a: &str, b: &str) -> bool {
    bool::from(
        Sha256::digest(a.as_bytes())
            .as_slice()
            .ct_eq(Sha256::digest(b.as_bytes()).as_slice()),
    )
}
fn unauthorized(publication: &Publication) -> Box<Response> {
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_str(&format!(
            "Basic realm=\"{}\", charset=\"UTF-8\"",
            publication.id
        ))
        .expect("generated realm"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Box::new(response)
}
/// Returns whether Authorization belongs to the application and may be forwarded.
fn authorize(publication: &Publication, headers: &HeaderMap) -> Result<bool, Box<Response>> {
    let key = headers.get("x-publish-key").and_then(|v| v.to_str().ok());
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if key.is_none() && authorization.is_none() {
        return Err(unauthorized(publication));
    }
    let mut attempts = publication.auth_attempts.lock().unwrap();
    let now = Instant::now();
    if now < attempts.blocked_until {
        let mut response = StatusCode::TOO_MANY_REQUESTS.into_response();
        let seconds = attempts.blocked_until.duration_since(now).as_secs() + 1;
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&seconds.to_string()).unwrap(),
        );
        return Err(Box::new(response));
    }
    if now.duration_since(attempts.window) >= Duration::from_secs(60) {
        attempts.failures = 0;
        attempts.window = now;
    }
    let basic = authorization
        .and_then(|a| a.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .and_then(|(_, encoded)| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        })
        .and_then(|bytes| String::from_utf8(bytes).ok());
    let gateway_basic = basic.as_deref().is_some_and(|value| {
        credential_equal(
            value,
            &format!("{}:{}", publication.access_username, publication.password),
        )
    });
    let duplicate = headers.get_all("x-publish-key").iter().count() > 1
        || headers.get_all(header::AUTHORIZATION).iter().count() > 1;
    let valid = !duplicate
        && match key {
            Some(key) => credential_equal(key, &publication.password),
            None => gateway_basic,
        };
    if !valid {
        attempts.failures += 1;
        if attempts.failures >= 10 {
            attempts.blocked_until = now + Duration::from_secs(30);
        }
        return Err(unauthorized(publication));
    }
    // A browser can attach cached Basic credentials from another publication
    // when a caller supplies X-Publish-Key. Never leak our generated credentials.
    let publication_basic = basic
        .as_deref()
        .and_then(|value| value.split_once(':'))
        .is_some_and(|(username, password)| {
            username.len() == 5
                && username.starts_with("web")
                && username[3..].bytes().all(|b| b.is_ascii_digit())
                && password.len() == 6
                && password.bytes().all(|b| b.is_ascii_digit())
        });
    Ok(key.is_some() && !publication_basic)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_engine::publication::PublishTarget;
    fn item() -> Publication {
        Publication::new(
            "test".into(),
            "alice".into(),
            "spc_a".into(),
            PublishTarget::Port(3000),
            3000,
            None,
            Duration::ZERO,
        )
        .unwrap()
    }
    #[test]
    fn cookie_scope_preserves_attributes_without_doubling_prefix() {
        let prefix = "/u/alice/pub/test/";
        assert_eq!(
            scoped_cookie(
                "sid=one; Domain=example; Path=/api; HttpOnly; Secure",
                prefix
            )
            .unwrap(),
            "sid=one; HttpOnly; Secure; Path=/u/alice/pub/test/api"
        );
        assert_eq!(
            scoped_cookie(&format!("sid=one; Path={prefix}api"), prefix).unwrap(),
            format!("sid=one; Path={prefix}api")
        );
        assert_eq!(
            scoped_cookie("sid=one", prefix).unwrap(),
            format!("sid=one; Path={prefix}")
        );
    }
    #[test]
    fn both_auth_methods_share_failure_budget_and_preserve_only_application_authorization() {
        let item = item();
        let mut headers = HeaderMap::new();
        let basic = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", item.access_username, item.password))
        );
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&basic).unwrap(),
        );
        assert!(!authorize(&item, &headers).unwrap());
        headers.insert(
            "x-publish-key",
            HeaderValue::from_str(&item.password).unwrap(),
        );
        assert!(
            !authorize(&item, &headers).unwrap(),
            "cached gateway Basic must not leak when header auth is also supplied"
        );
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("web12:123456")
            ))
            .unwrap(),
        );
        assert!(
            !authorize(&item, &headers).unwrap(),
            "other cached publication Basic must be stripped"
        );
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer application-token"),
        );
        assert!(authorize(&item, &headers).unwrap());
        for attempt in 0..10 {
            headers.clear();
            if attempt % 2 == 0 {
                headers.insert("x-publish-key", HeaderValue::from_static("wrong"));
            } else {
                headers.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_static("Basic d2ViOndyb25n"),
                );
            }
            assert_eq!(
                authorize(&item, &headers).unwrap_err().status(),
                StatusCode::UNAUTHORIZED
            );
        }
        headers.clear();
        headers.insert(
            "x-publish-key",
            HeaderValue::from_str(&item.password).unwrap(),
        );
        let blocked = authorize(&item, &headers).unwrap_err();
        assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(blocked.headers().contains_key(header::RETRY_AFTER));
        {
            let mut attempts = item.auth_attempts.lock().unwrap();
            attempts.window = Instant::now() - Duration::from_secs(61);
            attempts.blocked_until = Instant::now();
        }
        assert!(authorize(&item, &headers).is_ok());
        headers.append(
            "x-publish-key",
            HeaderValue::from_str(&item.password).unwrap(),
        );
        assert_eq!(
            authorize(&item, &headers).unwrap_err().status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
