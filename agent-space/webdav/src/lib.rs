#[path = "../../build-support/dav_path.rs"]
mod dav_path;
pub use dav_path::validate_path;
pub mod fs;
mod mutations;

use bytes::Bytes;
use dav_server::{
    body::Body as DavBody,
    davpath::DavPath,
    fs::{DavFileSystem, DavMetaData},
    ls::DavLockSystem,
    memls::MemLs,
    DavHandler, DavMethodSet,
};
use http::{HeaderValue, Request, Response, StatusCode};
use http_body::{Body, Frame};
use http_body_util::BodyExt;
use mutations::{Guard, Mutations};
use std::{
    future::Future,
    io,
    path::Path,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// One service instance; clones share the filesystem, DAV locks and mutation gate.
#[derive(Clone)]
pub struct Service {
    fs: fs::RootFs,
    locks: Box<MemLs>,
    prefix: String,
    mutations: Arc<Mutations>,
    idle: Duration,
}
impl Service {
    pub fn new(
        root: &Path,
        owner: &str,
        instance: &str,
        prefix: &str,
        idle: Duration,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !prefix.starts_with('/') || !prefix.ends_with('/') || idle.is_zero() {
            return Err("invalid DAV configuration".into());
        }
        validate_path(prefix, prefix).map_err(|_| "invalid DAV prefix")?;
        Ok(Self {
            fs: fs::RootFs::new(root, owner, instance)?,
            locks: MemLs::new(),
            prefix: prefix.into(),
            mutations: Arc::new(Mutations::default()),
            idle,
        })
    }
    pub async fn handle<B>(
        &self,
        mut request: Request<B>,
        cancel: CancellationToken,
    ) -> Response<ResponseBody>
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        // Normalize only the authenticated root spelling, never dot segments.
        if request.uri().path().trim_end_matches('/') == self.prefix.trim_end_matches('/') {
            if request.uri().query().is_some() {
                return response(StatusCode::BAD_REQUEST, "");
            }
            *request.uri_mut() = self.prefix.parse().unwrap();
        }
        if validate_path(request.uri().path(), &self.prefix).is_err()
            || request.uri().query().is_some()
        {
            return response(StatusCode::FORBIDDEN, "");
        }
        let method = request.method().as_str().to_owned();
        if !matches!(
            method.as_str(),
            "OPTIONS"
                | "PROPFIND"
                | "PROPPATCH"
                | "GET"
                | "HEAD"
                | "PUT"
                | "MKCOL"
                | "DELETE"
                | "COPY"
                | "MOVE"
                | "LOCK"
                | "UNLOCK"
        ) {
            return response(StatusCode::METHOD_NOT_ALLOWED, "");
        }
        let modified = if method == "PUT" {
            match request.headers().get("x-oc-mtime") {
                Some(value) => match value.to_str().ok().and_then(fs::parse_mtime) {
                    Some(time) if request.headers().get_all("x-oc-mtime").iter().count() == 1 => {
                        Some(time)
                    }
                    _ => return response(StatusCode::BAD_REQUEST, "invalid X-OC-Mtime"),
                },
                None => None,
            }
        } else {
            None
        };
        if method == "PUT" && request.headers().contains_key("content-range") {
            return response(StatusCode::BAD_REQUEST, "partial PUT is unsupported");
        }
        if matches!(method.as_str(), "DELETE" | "MOVE") && request.uri().path() == self.prefix {
            return response(StatusCode::FORBIDDEN, "");
        }
        if method == "PROPFIND"
            && !request
                .headers()
                .get("depth")
                .is_some_and(|v| v == "0" || v == "1")
        {
            let mut res = response(
                StatusCode::FORBIDDEN,
                r#"<?xml version="1.0"?><D:error xmlns:D="DAV:"><D:propfind-finite-depth/></D:error>"#,
            );
            res.headers_mut()
                .insert("content-type", HeaderValue::from_static("application/xml"));
            return res;
        }
        if let Some(destination) = request.headers().get("destination") {
            let valid = destination
                .to_str()
                .ok()
                .and_then(|v| v.parse::<http::Uri>().ok())
                .is_some_and(|uri| {
                    uri.query().is_none()
                        && validate_path(uri.path(), &self.prefix).is_ok()
                        && uri.path().trim_end_matches('/') != self.prefix.trim_end_matches('/')
                });
            if !valid {
                return response(StatusCode::FORBIDDEN, "");
            }
        }
        // Keep lock refresh available during long PUTs. New LOCK requests join
        // the mutation gate below after their bounded XML body is read.
        if method == "LOCK" {
            let seconds = request
                .headers()
                .get("timeout")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Second-"))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1800)
                .min(1800);
            request.headers_mut().insert(
                "timeout",
                HeaderValue::from_str(&format!("Second-{seconds}")).unwrap(),
            );
        }
        let eof = Arc::new(AtomicBool::new(false));
        let overflow = Arc::new(AtomicBool::new(false));
        let (parts, body) = request.into_parts();
        let tracked = TrackedBody {
            inner: Box::pin(body),
            eof: eof.clone(),
            overflow: overflow.clone(),
            remaining: if method == "PUT" {
                None
            } else {
                Some(1024 * 1024)
            },
            timer: Box::pin(tokio::time::sleep(self.idle)),
            idle: self.idle,
        };
        let mut request = Request::from_parts(parts, tracked.boxed_unsync());
        let mut new_lock = false;
        if method != "PUT" {
            let (parts, body) = request.into_parts();
            let bytes = tokio::select! {
                biased;
                _=cancel.cancelled()=>return response(StatusCode::SERVICE_UNAVAILABLE,""),
                bytes=body.collect()=>match bytes {
                    Ok(bytes)=>bytes.to_bytes(),
                    Err(_)=>return response(if overflow.load(Ordering::SeqCst) { StatusCode::PAYLOAD_TOO_LARGE } else { StatusCode::BAD_REQUEST },""),
                }
            };
            new_lock = method == "LOCK" && !bytes.is_empty();
            request = Request::from_parts(
                parts,
                http_body_util::Full::new(bytes)
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            );
        }
        let mut guard = None;
        if matches!(
            method.as_str(),
            "PUT" | "MKCOL" | "DELETE" | "COPY" | "MOVE" | "PROPPATCH"
        ) || new_lock
        {
            let mut paths = vec![mutation_path(request.uri(), &self.prefix)];
            if matches!(method.as_str(), "COPY" | "MOVE") {
                let destination = request
                    .headers()
                    .get("destination")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<http::Uri>().ok());
                let Some(destination) = destination else {
                    return response(StatusCode::BAD_REQUEST, "missing Destination");
                };
                paths.push(mutation_path(&destination, &self.prefix));
            }
            guard = Some(tokio::select! {
                biased;
                _=cancel.cancelled()=>return response(StatusCode::SERVICE_UNAVAILABLE,""),
                acquired=tokio::time::timeout(self.idle,self.mutations.clone().acquire(paths))=>match acquired {
                    Ok(guard)=>guard,
                    Err(_)=>{ let mut res=response(StatusCode::SERVICE_UNAVAILABLE,""); res.headers_mut().insert("retry-after",HeaderValue::from_static("1")); return res; }
                }
            });
        }
        // A regular file must not recursively delete a destination directory.
        if matches!(method.as_str(), "COPY" | "MOVE") {
            let source = dav_path(request.uri().path(), &self.prefix);
            let destination = request
                .headers()
                .get("destination")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| dav_path(v, &self.prefix).ok());
            if let (Ok(source), Some(destination)) = (source, destination) {
                if method == "MOVE" && fs::validate_move(&source, &destination).is_err() {
                    return response(StatusCode::FORBIDDEN, "");
                }
                if let (Ok(source), Ok(destination)) = (
                    self.fs.metadata(&source).await,
                    self.fs.metadata(&destination).await,
                ) {
                    if source.is_file() && destination.is_dir() {
                        return response(StatusCode::CONFLICT, "file cannot overwrite directory");
                    }
                }
            }
        }
        let request_fs = if method == "PUT" || new_lock {
            self.fs.for_upload().with_modified(modified)
        } else if matches!(method.as_str(), "COPY" | "DELETE") {
            self.fs.for_recursive_mutation()
        } else {
            self.fs.clone()
        };
        let handler = DavHandler::builder()
            .filesystem(Box::new(request_fs.clone()))
            .locksystem(self.locks.clone())
            .strip_prefix(self.prefix.clone())
            .autoindex(false)
            .methods(
                DavMethodSet::from_vec(vec![
                    "OPTIONS",
                    "PROPFIND",
                    "PROPPATCH",
                    "GET",
                    "HEAD",
                    "PUT",
                    "MKCOL",
                    "DELETE",
                    "COPY",
                    "MOVE",
                    "LOCK",
                    "UNLOCK",
                ])
                .unwrap(),
            )
            .read_buf_size(64 * 1024)
            .build_handler();
        let new_lock_path = if new_lock {
            let path = DavPath::from_uri(request.uri()).and_then(|mut path| {
                path.set_prefix(&self.prefix)?;
                Ok(path)
            });
            match path {
                Ok(path) => Some(path),
                Err(_) => return response(StatusCode::BAD_REQUEST, ""),
            }
        } else {
            None
        };
        let mut result = tokio::select! {
            biased;
            _=cancel.cancelled()=>return response(StatusCode::SERVICE_UNAVAILABLE,""),
            result=handler.handle(request)=>result,
        };
        if (method == "PUT" || new_lock) && result.status().is_success() {
            result = match self
                .commit_upload(
                    &request_fs,
                    result,
                    new_lock_path.as_ref(),
                    !cancel.is_cancelled() && eof.load(Ordering::SeqCst),
                )
                .await
            {
                Ok(result) => result,
                Err((status, reason)) => return response(status, reason),
            };
        }

        if method == "PUT" && modified.is_some() && result.status().is_success() {
            result
                .headers_mut()
                .insert("x-oc-mtime", HeaderValue::from_static("accepted"));
        }
        if matches!(method.as_str(), "GET" | "HEAD") {
            result.headers_mut().insert(
                "content-disposition",
                HeaderValue::from_static("attachment"),
            );
            result.headers_mut().insert(
                "x-content-type-options",
                HeaderValue::from_static("nosniff"),
            );
            result.headers_mut().insert(
                "content-security-policy",
                HeaderValue::from_static("sandbox allow-downloads"),
            );
        }
        let (parts, body) = result.into_parts();
        Response::from_parts(
            parts,
            ResponseBody {
                body,
                guard,
                cancel: Box::pin(cancel.cancelled_owned()),
            },
        )
    }
    // The protocol handler may have acquired a lock before creating its empty
    // resource. A failed final commit must not strand a lock whose token was
    // never delivered to the client.
    async fn commit_upload(
        &self,
        fs: &fs::RootFs,
        mut result: Response<DavBody>,
        new_lock_path: Option<&DavPath>,
        complete: bool,
    ) -> Result<Response<DavBody>, (StatusCode, &'static str)> {
        // flush/Drop cannot commit: EOF and protocol checks happen after flush.
        let committed = if !complete {
            Err((StatusCode::BAD_REQUEST, "upload cancelled or incomplete"))
        } else {
            fs.commit().map_err(|error| match error {
                fs::CommitError::Changed => (
                    StatusCode::PRECONDITION_FAILED,
                    "target changed during upload",
                ),
                fs::CommitError::Fs(error) => (fs_status(error), "file commit failed"),
            })
        };
        match committed {
            Ok(meta) => {
                if let Some(meta) = meta {
                    if let Some(etag) = meta.etag() {
                        result.headers_mut().insert(
                            "etag",
                            HeaderValue::from_str(&format!("\"{etag}\"")).unwrap(),
                        );
                    }
                    if let Ok(modified) = meta.modified() {
                        result.headers_mut().insert(
                            "last-modified",
                            HeaderValue::from_str(&httpdate::fmt_http_date(modified)).unwrap(),
                        );
                    }
                }
                Ok(result)
            }
            Err(error) => {
                if let Some(path) = new_lock_path {
                    if let Some(token) = result
                        .headers()
                        .get("lock-token")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix('<'))
                        .and_then(|v| v.strip_suffix('>'))
                    {
                        let _ = self.locks.unlock(path, token).await;
                    }
                }
                Err(error)
            }
        }
    }
}
fn dav_path(value: &str, prefix: &str) -> Result<DavPath, ()> {
    let uri: http::Uri = value.parse().map_err(|_| ())?;
    // from_uri preserves raw percent escapes; new follows the protocol parser's
    // decoding so aliases share the same preflight checks and mutation lock.
    let mut path = DavPath::new(uri.path()).map_err(|_| ())?;
    path.set_prefix(prefix).map_err(|_| ())?;
    Ok(path)
}
fn mutation_path(uri: &http::Uri, prefix: &str) -> Vec<u8> {
    // Inputs were validated before acquisition. Normalize decoded path components
    // so percent-encoded aliases and redundant slashes share the same reservation.
    let path = dav_path(uri.path(), prefix).expect("validated DAV path");
    path.as_bytes()
        .split(|b| *b == b'/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(&b'/')
}
fn fs_status(error: dav_server::fs::FsError) -> StatusCode {
    match error {
        dav_server::fs::FsError::InsufficientStorage => StatusCode::INSUFFICIENT_STORAGE,
        dav_server::fs::FsError::Forbidden => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
struct TrackedBody<B> {
    inner: Pin<Box<B>>,
    eof: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    remaining: Option<usize>,
    timer: Pin<Box<tokio::time::Sleep>>,
    idle: Duration,
}
impl<B> Body for TrackedBody<B>
where
    B: Body<Data = Bytes>,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.inner.as_mut().poll_frame(cx) {
            Poll::Ready(None) => {
                self.eof.store(true, Ordering::SeqCst);
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(io::Error::other(e)))),
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    if let Some(remaining) = self.remaining.as_mut() {
                        if data.len() > *remaining {
                            self.overflow.store(true, Ordering::SeqCst);
                            return Poll::Ready(Some(Err(io::Error::other(
                                "control body too large",
                            ))));
                        }
                        *remaining -= data.len();
                    }
                    if !data.is_empty() {
                        let next = tokio::time::Instant::now() + self.idle;
                        self.timer.as_mut().reset(next);
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Pending => {
                if self.timer.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Some(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "body idle timeout",
                    ))))
                } else {
                    Poll::Pending
                }
            }
        }
    }
}
/// Retains the mutation guard through lazy recursive DAV responses. Dropping a
/// disconnected response releases the guard; cancellation does not await reads.
pub struct ResponseBody {
    body: DavBody,
    guard: Option<Guard>,
    cancel: Pin<Box<dyn Future<Output = ()> + Send>>,
}
impl Body for ResponseBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if self.cancel.as_mut().poll(cx).is_ready() {
            self.guard.take();
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "request cancelled",
            ))));
        }
        let result = Pin::new(&mut self.body).poll_frame(cx);
        if let Poll::Ready(Some(Err(error))) = &result {
            // stderr is retained by the existing per-instance bounded log.
            eprintln!("DAV response body failed: {error}");
        }
        if matches!(result, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            self.guard.take();
        }
        result
    }
}
fn response(status: StatusCode, text: &str) -> Response<ResponseBody> {
    let mut response = Response::new(ResponseBody {
        body: DavBody::from(text),
        guard: None,
        cancel: Box::pin(std::future::pending()),
    });
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;

    #[tokio::test]
    async fn failed_lock_resource_commit_releases_undelivered_lock() {
        const PREFIX: &str = "/u/alice/dav/";
        const XML: &str = r#"<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockinfo>"#;
        for complete in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let service = Service::new(
                root.path(),
                "owner",
                "instance",
                PREFIX,
                Duration::from_secs(2),
            )
            .unwrap();
            let upload = service.fs.for_upload();
            let handler = DavHandler::builder()
                .filesystem(Box::new(upload.clone()))
                .locksystem(service.locks.clone())
                .strip_prefix(PREFIX)
                .build_handler();
            let request = || {
                Request::builder()
                    .method("LOCK")
                    .uri(format!("{PREFIX}file"))
                    .header("content-type", "application/xml")
                    .body(Full::new(Bytes::from_static(XML.as_bytes())))
                    .unwrap()
            };
            let result = handler.handle(request()).await;
            assert_eq!(result.status(), StatusCode::CREATED);
            let mut path = DavPath::new(&format!("{PREFIX}file")).unwrap();
            path.set_prefix(PREFIX).unwrap();
            assert_eq!(service.locks.discover(&path).await.len(), 1);
            // Real protocol layer has acquired the lock and staged its empty
            // resource. Inject cancellation or an external change at the commit
            // boundary, before any token has been returned to a client.
            if complete {
                std::fs::write(root.path().join("file"), "external").unwrap();
            }
            let result = service
                .commit_upload(&upload, result, Some(&path), complete)
                .await;
            assert_eq!(
                result.err().unwrap().0,
                if complete {
                    StatusCode::PRECONDITION_FAILED
                } else {
                    StatusCode::BAD_REQUEST
                }
            );
            drop(upload);
            assert!(service.locks.discover(&path).await.is_empty());
            if complete {
                assert_eq!(
                    std::fs::read(root.path().join("file")).unwrap(),
                    b"external"
                );
            } else {
                assert!(!root.path().join("file").exists());
            }
            let retry = service.handle(request(), CancellationToken::new()).await;
            assert!(
                retry.status().is_success(),
                "undelivered lock must not block retry"
            );
        }
    }
}
