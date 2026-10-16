#[path = "../../build-support/bounded_log.rs"]
mod bounded_log;
#[path = "../../build-support/data_process.rs"]
mod data_process;
use agent_space_webdav::Service;
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use std::{
    io::Write,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    schema_version: u32,
    root: String,
    prefix: String,
    lock_timeout_secs: u64,
    #[serde(default = "default_idle")]
    io_idle_timeout_secs: u64,
}
fn default_idle() -> u64 {
    120
}

/// Keep Hyper's real receiver alive when DAV rejects a partially read PUT.
/// Its remaining bytes are discarded by the existing connection owner.
#[derive(Clone)]
struct SharedIncoming(Arc<Mutex<Pin<Box<hyper::body::Incoming>>>>);
impl Body for SharedIncoming {
    type Data = bytes::Bytes;
    type Error = hyper::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.0.lock().unwrap().as_mut().poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.0.lock().unwrap().is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.0.lock().unwrap().size_hint()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.as_slice() == ["--build-id"] {
        println!("{}", env!("AGENT_SPACE_HELPER_BUILD_ID"));
        return Ok(());
    }
    data_process::supervise()?;
    if args.len() != 6 || args[0] != "--config" || args[2] != "--owner" || args[4] != "--instance" {
        return Err(
            "usage: agent-space-webdav --config FILE --owner SPACE_ID --instance INSTANCE_ID"
                .into(),
        );
    }
    let raw = std::fs::read(&args[1])?;
    if raw.len() > 16384 {
        return Err("configuration too large".into());
    }
    let config: Config = serde_json::from_slice(&raw)?;
    if config.schema_version != 1 || config.root != "/workspace" || config.lock_timeout_secs != 1800
    {
        return Err("unsupported guest configuration".into());
    }
    let service = Arc::new(Service::new(
        Path::new(&config.root),
        &args[3],
        &args[5],
        &config.prefix,
        Duration::from_secs(config.io_idle_timeout_secs),
    )?);
    bounded_log::install()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("READY {}", listener.local_addr()?.port());
    std::io::stdout().flush()?;
    let shutdown = CancellationToken::new();
    let signal = shutdown.clone();
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=term.recv()=>{} }
        signal.cancel();
    });
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _=shutdown.cancelled()=>break,
            Some(_)=connections.join_next()=>{},
            incoming=listener.accept()=>{
                let (stream,_)=incoming?;
                let service=service.clone();
                let cancel=shutdown.child_token();
                let drain_timeout=Duration::from_secs(config.io_idle_timeout_secs);
                connections.spawn(async move {
                    let drains=Arc::new(tokio::sync::Mutex::new(tokio::task::JoinSet::new()));
                    let request_drains=drains.clone();
                    let request_cancel=cancel.clone();
                    let connection=http1::Builder::new().serve_connection(TokioIo::new(stream),service_fn(move |request| {
                        let service=service.clone();
                        let request_cancel=request_cancel.child_token();
                        let request_drains=request_drains.clone();
                        async move {
                            let put=request.method()==http::Method::PUT;
                            let (parts,body)=request.into_parts();
                            let shared=SharedIncoming(Arc::new(Mutex::new(Box::pin(body))));
                            let response=service.handle(http::Request::from_parts(parts,shared.clone()),request_cancel.clone()).await;
                            if put && !response.status().is_success() && !shared.is_end_stream() {
                                // A fixed total bound: continued upload progress cannot
                                // keep a rejected request alive indefinitely.
                                let until=tokio::time::Instant::now()+drain_timeout;
                                let mut drains=request_drains.lock().await;
                                while drains.try_join_next().is_some() {}
                                drains.spawn(async move {
                                    let mut body=shared;
                                    loop {
                                        tokio::select! {
                                            biased;
                                            _=request_cancel.cancelled()=>break,
                                            _=tokio::time::sleep_until(until)=>break,
                                            frame=body.frame()=>if !matches!(frame,Some(Ok(_))) {break;},
                                        }
                                    }
                                });
                            }
                            Ok::<_,std::convert::Infallible>(response)
                        }
                    }));
                    tokio::select! { _=cancel.cancelled()=>{}, _=connection=>{} }
                    cancel.cancel();
                    let mut drains=drains.lock().await;
                    drains.abort_all();
                    while drains.join_next().await.is_some() {}
                });
            }
        }
    }
    while connections.join_next().await.is_some() {}
    Ok(())
}
