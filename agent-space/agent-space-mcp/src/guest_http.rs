use agent_engine::{engine::GuestTcp, session::RequestLease};
use axum::http::{header, HeaderMap};
use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// Shared only so cancellation can release activity even before a caller polls
// the response body. Ordinary connection EOF does not end a slow download lease.
pub(crate) struct ResponseLease(pub(crate) Arc<Mutex<Option<RequestLease>>>);
impl Drop for ResponseLease {
    fn drop(&mut self) {
        self.0.lock().unwrap().take();
    }
}

pub(crate) struct GuestIo {
    tcp: GuestTcp,
    progress: Option<Box<dyn Fn(usize) + Send + Sync>>,
}
impl GuestIo {
    pub(crate) fn new(tcp: GuestTcp) -> Self {
        Self {
            tcp,
            progress: None,
        }
    }
    pub(crate) fn with_progress(
        mut self,
        callback: impl Fn(usize) + Send + Sync + 'static,
    ) -> Self {
        self.progress = Some(Box::new(callback));
        self
    }
}

impl AsyncRead for GuestIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut *self.tcp.reader).poll_read(cx, buf);
        if let Some(progress) = &self.progress {
            progress(buf.filled().len() - before);
        }
        result
    }
}

impl AsyncWrite for GuestIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut *self.tcp.writer).poll_write(cx, buf);
        if let (Poll::Ready(Ok(bytes)), Some(progress)) = (&result, &self.progress) {
            progress(*bytes);
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.tcp.writer).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.tcp.writer).poll_shutdown(cx)
    }
}

pub(crate) struct ConnectionGuard(pub(crate) tokio::task::JoinHandle<()>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn forwarded_headers(headers: &HeaderMap, keep_authorization: bool) -> HeaderMap {
    let listed = connection_headers(headers);
    let mut forwarded = HeaderMap::new();
    for (name, value) in headers {
        if hop_by_hop(name)
            || listed.iter().any(|item| item == name)
            || (!keep_authorization && name == header::AUTHORIZATION)
        {
            continue;
        }
        forwarded.append(name.clone(), value.clone());
    }
    forwarded
}

fn connection_headers(headers: &HeaderMap) -> Vec<header::HeaderName> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| header::HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect()
}

fn hop_by_hop(name: &header::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "host"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

#[derive(Clone)]
pub(crate) struct Progress(Arc<Mutex<tokio::time::Instant>>);
impl Progress {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(tokio::time::Instant::now())))
    }
    pub(crate) fn tick(&self, bytes: usize) {
        if bytes > 0 {
            *self.0.lock().unwrap() = tokio::time::Instant::now();
        }
    }
    pub(crate) async fn idle(&self, duration: std::time::Duration) {
        loop {
            let until = *self.0.lock().unwrap() + duration;
            tokio::time::sleep_until(until).await;
            if tokio::time::Instant::now() >= *self.0.lock().unwrap() + duration {
                return;
            }
        }
    }
}
