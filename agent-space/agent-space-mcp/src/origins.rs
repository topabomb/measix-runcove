//! Entry validation is separate from canonical link generation and authentication.
use agent_engine::{config::Config, state::validate_username};
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

#[derive(Clone, Debug)]
pub struct RequestOrigin(pub url::Url);

#[derive(Clone)]
pub struct OriginPolicy {
    canonical: Option<url::Url>,
}
impl OriginPolicy {
    pub fn from_config(config: &Config) -> Result<Self, url::ParseError> {
        Self::new(config.server.public_base_url.as_deref())
    }
    pub fn new(canonical: Option<&str>) -> Result<Self, url::ParseError> {
        Ok(Self {
            canonical: canonical
                .filter(|v| !v.trim().is_empty())
                .map(url::Url::parse)
                .transpose()?,
        })
    }
    pub fn resolve(&self, request: &Request) -> Result<url::Url, StatusCode> {
        let headers = request
            .headers()
            .get_all(header::HOST)
            .iter()
            .collect::<Vec<_>>();
        if headers.len() > 1 {
            return Err(StatusCode::BAD_REQUEST);
        }
        let host = headers
            .first()
            .map(|v| v.to_str().map_err(|_| StatusCode::BAD_REQUEST))
            .transpose()?;
        let uri_host = request.uri().authority().map(|a| a.as_str());
        let authority = host.or(uri_host).ok_or(StatusCode::BAD_REQUEST)?;
        let parsed = authority
            .parse::<http::uri::Authority>()
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        if authority.contains('@') || parsed.port().is_some() && parsed.port_u16().is_none() {
            return Err(StatusCode::BAD_REQUEST);
        }
        // Forwarded headers are not trusted. A configured canonical authority
        // provides its external scheme only when the request actually uses it.
        let canonical = self
            .canonical
            .as_ref()
            .filter(|url| matches_origin(request, url));
        let scheme = canonical.map(|u| u.scheme()).unwrap_or("http");
        if request.uri().scheme_str().is_some_and(|s| s != scheme) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let origin = url::Url::parse(&format!("{scheme}://{authority}"))
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        if !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        if let Some(uri_host) = uri_host {
            let other = url::Url::parse(&format!("{scheme}://{uri_host}"))
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            if other.origin() != origin.origin() {
                return Err(StatusCode::BAD_REQUEST);
            }
        }
        let local = match origin.host() {
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
            Some(url::Host::Ipv6(ip)) => {
                ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
            }
            None => false,
        };
        if !local && canonical.is_none() {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(origin)
    }
}

pub async fn origin_boundary(
    State(policy): State<OriginPolicy>,
    mut request: Request,
    next: Next,
) -> Response {
    // The workbench owns login Origin and every Cookie/nonce check. A reverse
    // proxy may rewrite Host, so it must not pass through the API host policy.
    let path = request.uri().path();
    if crate::desktop::username(path).is_some()
        || path == "/desktop"
        || path.starts_with("/desktop/")
    {
        return next.run(request).await;
    }
    let origin = match policy.resolve(&request) {
        Ok(origin) => origin,
        Err(status) => return status.into_response(),
    };
    if !browser_request_allowed(&request, &origin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    request.extensions_mut().insert(RequestOrigin(origin));
    next.run(request).await
}

// Reject ambient cross-origin browser requests, including no-CORS
// requests which do not carry Origin. Permit an explicit top-level user visit.
pub(crate) fn browser_request_allowed(request: &Request, expected: &url::Url) -> bool {
    let origins = request
        .headers()
        .get_all(header::ORIGIN)
        .iter()
        .collect::<Vec<_>>();
    if origins.len() > 1
        || origins.first().is_some_and(|value| {
            value.to_str().ok() != Some(expected.origin().ascii_serialization().as_str())
        })
    {
        return false;
    }
    match request
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
    {
        None | Some("none" | "same-origin") => true,
        Some(_) => {
            origins.is_empty()
                && matches!(request.method().as_str(), "GET" | "HEAD")
                && request
                    .headers()
                    .get("sec-fetch-mode")
                    .is_some_and(|v| v == "navigate")
                && request
                    .headers()
                    .get("sec-fetch-dest")
                    .is_some_and(|v| v == "document")
                && request
                    .headers()
                    .get("sec-fetch-user")
                    .is_some_and(|v| v == "?1")
        }
    }
}
pub(crate) fn matches_origin(request: &Request, expected: &url::Url) -> bool {
    let headers = request
        .headers()
        .get_all(header::HOST)
        .iter()
        .collect::<Vec<_>>();
    if headers.len() > 1 {
        return false;
    }
    let host = headers.first().and_then(|v| v.to_str().ok());
    let uri_host = request.uri().authority().map(|v| v.as_str());
    if host.is_none() && uri_host.is_none() {
        return false;
    }
    if request
        .uri()
        .scheme_str()
        .is_some_and(|s| s != expected.scheme())
    {
        return false;
    }
    [host, uri_host].into_iter().flatten().all(|value| {
        value.parse::<http::uri::Authority>().is_ok()
            && !value.contains('@')
            && url::Url::parse(&format!("{}://{value}", expected.scheme()))
                .is_ok_and(|u| u.origin() == expected.origin())
    })
}
pub(crate) fn dav_username(path: &str) -> Option<&str> {
    let (username, rest) = path.strip_prefix("/u/")?.split_once('/')?;
    if validate_username(username).is_err() || !(rest == "dav" || rest.starts_with("dav/")) {
        return None;
    }
    Some(username)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, Router};
    use tower::ServiceExt;
    #[tokio::test]
    async fn network_changes_do_not_change_authentication_or_browser_boundary() {
        let policy = OriginPolicy::new(Some("https://old.example")).unwrap();
        let app = Router::new()
            .fallback(|| async { StatusCode::UNAUTHORIZED })
            .layer(axum::middleware::from_fn_with_state(
                policy,
                origin_boundary,
            ));
        for host in [
            "127.0.0.1:18932",
            "localhost:18932",
            "192.168.1.249:18932",
            "192.168.31.236:18932",
            "[::1]:18932",
            "old.example",
        ] {
            for path in [
                "/healthz",
                "/admin/v1/users",
                "/u/alice/mcp",
                "/u/alice/dav/a",
            ] {
                let request = Request::builder()
                    .uri(path)
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap();
                assert_eq!(
                    app.clone().oneshot(request).await.unwrap().status(),
                    StatusCode::UNAUTHORIZED,
                    "{host} {path}"
                );
            }
        }
        for (host, origin, expected) in [
            (
                "192.168.31.236:18932",
                "http://192.168.31.236:18932",
                StatusCode::UNAUTHORIZED,
            ),
            (
                "old.example",
                "https://old.example",
                StatusCode::UNAUTHORIZED,
            ),
            (
                "127.0.0.1:18932",
                "https://evil.example",
                StatusCode::FORBIDDEN,
            ),
            ("127.0.0.1:18932", "null", StatusCode::FORBIDDEN),
            (
                "rebind.example",
                "http://rebind.example",
                StatusCode::FORBIDDEN,
            ),
        ] {
            let request = Request::builder()
                .uri("/u/alice/mcp")
                .header("host", host)
                .header("origin", origin)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                expected
            );
        }
        let request = Request::builder()
            .uri("/admin/v1/users")
            .header("host", "127.0.0.1:18932")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    #[test]
    fn reject_ambiguous_authorities_and_ignore_forwarded_headers() {
        let policy = OriginPolicy::new(None).unwrap();
        let request = Request::builder()
            .uri("/")
            .header("host", "127.0.0.1:18932")
            .header("x-forwarded-host", "evil.example")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            policy.resolve(&request).unwrap().as_str(),
            "http://127.0.0.1:18932/"
        );
        let duplicate = Request::builder()
            .uri("/")
            .header("host", "localhost")
            .header("host", "evil.example")
            .body(Body::empty())
            .unwrap();
        assert!(policy.resolve(&duplicate).is_err());
        let mismatch = Request::builder()
            .uri("http://evil.example/")
            .header("host", "localhost")
            .body(Body::empty())
            .unwrap();
        assert!(policy.resolve(&mismatch).is_err());
        for host in [
            "localhost:99999",
            "user@localhost",
            "evil.example",
            "0.0.0.0:8932",
        ] {
            let r = Request::builder()
                .uri("/")
                .header("host", host)
                .body(Body::empty())
                .unwrap();
            assert!(policy.resolve(&r).is_err(), "{host}");
        }
    }
}
