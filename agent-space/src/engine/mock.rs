use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::sync::Mutex;

use super::{
    AgentEngine, AgentHandle, EngineError, ExecResult, SandboxMetricsSnapshot, WorkspaceOpts,
    WorkspaceStatus,
};

pub struct MockEngine {
    pub sandboxes: Mutex<Vec<String>>,
    pub managed_pids: Mutex<Vec<u32>>,
    pub stopped_pids: Mutex<Vec<u32>>,
    pub volumes: Mutex<Vec<String>>,
    pub create_count: AtomicU32,
    pub fail_start: AtomicBool,
    pub fail_volume_removal: AtomicBool,
    pub fail_stop: AtomicBool,
}

impl MockEngine {
    pub fn new() -> Self {
        Self {
            sandboxes: Mutex::new(Vec::new()),
            managed_pids: Mutex::new(Vec::new()),
            stopped_pids: Mutex::new(Vec::new()),
            volumes: Mutex::new(Vec::new()),
            create_count: AtomicU32::new(0),
            fail_start: AtomicBool::new(false),
            fail_volume_removal: AtomicBool::new(false),
            fail_stop: AtomicBool::new(false),
        }
    }

    pub fn set_fail_start(&self, fail: bool) {
        self.fail_start.store(fail, Ordering::SeqCst);
    }
}

impl Default for MockEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentEngine for MockEngine {
    async fn create_sandbox(
        &self,
        name: &str,
        _opts: &WorkspaceOpts,
    ) -> Result<Arc<dyn AgentHandle>, EngineError> {
        let mut sandboxes = self.sandboxes.lock().await;
        if sandboxes.contains(&name.to_string()) {
            return Err(EngineError::AlreadyExists(name.to_string()));
        }
        sandboxes.push(name.to_string());
        drop(sandboxes);
        self.volumes.lock().await.push(format!("{name}-ws"));
        self.create_count.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(MockHandle::new(name)))
    }

    async fn get_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
        let sandboxes = self.sandboxes.lock().await;
        if sandboxes.contains(&name.to_string()) {
            Ok(Arc::new(MockHandle::new(name)))
        } else {
            Err(EngineError::NotFound(name.to_string()))
        }
    }

    async fn start_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
        if self.fail_start.load(Ordering::SeqCst) {
            return Err(EngineError::Other("mock start failure".to_string()));
        }
        let sandboxes = self.sandboxes.lock().await;
        if sandboxes.contains(&name.to_string()) {
            Ok(Arc::new(MockHandle::new(name)))
        } else {
            Err(EngineError::NotFound(name.to_string()))
        }
    }

    async fn remove_sandbox(&self, name: &str) -> Result<(), EngineError> {
        let mut sandboxes = self.sandboxes.lock().await;
        sandboxes.retain(|n| n != name);
        Ok(())
    }

    async fn list_running(&self) -> Result<Vec<String>, EngineError> {
        let sandboxes = self.sandboxes.lock().await;
        Ok(sandboxes.clone())
    }

    async fn list_volumes(&self) -> Result<Vec<String>, EngineError> {
        Ok(self.volumes.lock().await.clone())
    }

    async fn remove_volume(&self, name: &str) -> Result<(), EngineError> {
        if self.fail_volume_removal.load(Ordering::SeqCst) {
            return Err(EngineError::Other(format!("volume removal failed: {name}")));
        }
        let mut volumes = self.volumes.lock().await;
        if !volumes.iter().any(|volume| volume == name) {
            return Err(EngineError::NotFound(name.to_string()));
        }
        volumes.retain(|volume| volume != name);
        Ok(())
    }

    async fn get_metrics(&self, name: &str) -> Result<SandboxMetricsSnapshot, EngineError> {
        let sandboxes = self.sandboxes.lock().await;
        if !sandboxes.contains(&name.to_string()) {
            return Err(EngineError::NotFound(name.to_string()));
        }
        Ok(SandboxMetricsSnapshot {
            memory_available_bytes: Some(156 * 1024 * 1024),
            observed_at_ms: Some(crate::resources::now_ms()),
            memory_limit_bytes: 256 * 1024 * 1024,
        })
    }
}

pub struct MockHandle {
    pub name: String,
    pub running: AtomicBool,
    pub exec_count: AtomicU32,
    pub tmpfs_calls: AtomicU32,
    pub fail_tmpfs: AtomicBool,
    pub tcp_ports: Mutex<Vec<u16>>,
    pub engine: Option<Arc<MockEngine>>,
}

impl MockHandle {
    pub fn new(name: &str) -> Self {
        Self::with_engine(name, None)
    }
    pub fn with_engine(name: &str, engine: Option<Arc<MockEngine>>) -> Self {
        Self {
            name: name.to_string(),
            running: AtomicBool::new(true),
            exec_count: AtomicU32::new(0),
            tmpfs_calls: AtomicU32::new(0),
            fail_tmpfs: AtomicBool::new(false),
            tcp_ports: Mutex::new(vec![3000]),
            engine,
        }
    }

    pub fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::SeqCst);
    }
}

#[async_trait]
impl AgentHandle for MockHandle {
    async fn grow_tmpfs(&self, _guest_path: &str, _size_mib: u32) -> Result<(), EngineError> {
        self.tmpfs_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_tmpfs.load(Ordering::SeqCst) {
            return Err(EngineError::Other("mock tmpfs preparation failure".into()));
        }
        Ok(())
    }
    async fn exec(
        &self,
        program: &str,
        args: &[&str],
        _timeout: Option<Duration>,
    ) -> Result<ExecResult, EngineError> {
        self.exec_count.fetch_add(1, Ordering::SeqCst);
        if TEST_EXEC_HOLD.swap(false, Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        if !self.running.load(Ordering::SeqCst) {
            return Err(EngineError::SandboxNotRunning(self.name.clone()));
        }
        Ok(ExecResult {
            stdout: format!("mock:{} {}", program, args.join(" ")),
            stderr: String::new(),
            exit_code: 0,
            truncated: false,
            output_path: None,
        })
    }

    async fn stop(&self) -> Result<(), EngineError> {
        let hold = TEST_HOLD_STOP_WORKSPACE.lock().unwrap().as_deref() == Some(&self.name);
        if hold {
            futures::future::pending::<()>().await;
        }
        if TEST_FAIL_WORKSPACE_STOP.load(Ordering::SeqCst) {
            return Err(EngineError::Other(format!("stop failed for {}", self.name)));
        }
        self.running.store(false, Ordering::SeqCst);
        TEST_STOPPED_WORKSPACES
            .lock()
            .unwrap()
            .push(self.name.clone());
        Ok(())
    }

    async fn kill(&self) -> Result<(), EngineError> {
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn status(&self) -> WorkspaceStatus {
        if let Some(status) = TEST_NEXT_STATUS.lock().unwrap().take() {
            return status;
        }
        if self.running.load(Ordering::SeqCst) {
            WorkspaceStatus::Running
        } else {
            WorkspaceStatus::Stopped
        }
    }

    async fn connect_guest_tcp(&self, port: u16) -> Result<super::GuestTcp, EngineError> {
        if TEST_HOLD_PORT.swap(0, Ordering::SeqCst) == port {
            TEST_TCP_RELEASE.notified().await;
        }
        if !self.tcp_ports.lock().await.contains(&port) {
            return Err(EngineError::Other(format!("port {port} is not listening")));
        }
        let (read_client, mut write_server) = tokio::io::duplex(4096);
        let (mut read_server, write_client) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let n = match tokio::io::AsyncReadExt::read(&mut read_server, &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if head.contains("transfer-encoding: chunked") {
                        if request[end + 4..].windows(5).any(|w| w == b"0\r\n\r\n") {
                            break;
                        }
                    } else if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let end = request
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .unwrap_or(request.len());
            if request.starts_with(b"GET /chunked ") {
                let _ = tokio::io::AsyncWriteExt::write_all(
                    &mut write_server,
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
                ).await;
                return;
            }
            #[cfg(feature = "test-utils")]
            if request.starts_with(b"GET /u/alice/dav/truncated-response ") {
                let _ = tokio::io::AsyncWriteExt::write_all(&mut write_server,
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 8\r\nContent-Range: bytes 0-7/4294967296\r\nETag: \"large\"\r\nConnection: close\r\n\r\nABC").await;
                TEST_DAV_TRUNCATE.notified().await;
                return;
            }
            #[cfg(feature = "test-utils")]
            if request.starts_with(b"GET /u/alice/dav/progress-response ") {
                let _ = tokio::io::AsyncWriteExt::write_all(
                    &mut write_server,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n",
                )
                .await;
                for _ in 0..12 {
                    if tokio::io::AsyncWriteExt::write_all(&mut write_server, b"A")
                        .await
                        .is_err()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                return;
            }
            #[cfg(feature = "test-utils")]
            if request.starts_with(b"GET /held-headers ")
                || request.starts_with(b"GET /u/alice/dav/held-headers ")
            {
                TEST_HEADERS_SEEN.notify_one();
                let mut byte = [0];
                let _ = tokio::io::AsyncReadExt::read(&mut read_server, &mut byte).await;
                TEST_HTTP_DISCONNECTED.notify_one();
                return;
            }
            #[cfg(feature = "test-utils")]
            if request.starts_with(b"GET /held-response ")
                || request.starts_with(b"GET /u/alice/dav/held-response ")
            {
                let _ = tokio::io::AsyncWriteExt::write_all(
                    &mut write_server,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nA",
                )
                .await;
                tokio::time::sleep(Duration::from_secs(30)).await;
                let _ = tokio::io::AsyncWriteExt::write_all(&mut write_server, b"B").await;
                return;
            }
            let body = if request.starts_with(b"GET /u/alice/dav/headers ")
                || request.starts_with(b"GET /headers ")
            {
                request[..end].to_vec()
            } else if request.starts_with(b"POST /upload ") {
                let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                if head.contains("transfer-encoding: chunked") {
                    let mut chunks = request.get(end + 4..).unwrap_or_default();
                    let mut decoded = Vec::new();
                    while let Some(line) = chunks.windows(2).position(|w| w == b"\r\n") {
                        let size = usize::from_str_radix(
                            std::str::from_utf8(&chunks[..line]).unwrap_or(""),
                            16,
                        )
                        .unwrap_or(0);
                        if size == 0 || chunks.len() < line + 2 + size + 2 {
                            break;
                        }
                        decoded.extend_from_slice(&chunks[line + 2..line + 2 + size]);
                        chunks = &chunks[line + 2 + size + 2..];
                    }
                    decoded
                } else {
                    request.get(end + 4..).unwrap_or_default().to_vec()
                }
            } else {
                request.get(end + 4..).unwrap_or(b"mock-http").to_vec()
            };
            let body = if body.is_empty() {
                b"mock-http".to_vec()
            } else {
                body
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ =
                tokio::io::AsyncWriteExt::write_all(&mut write_server, response.as_bytes()).await;
            let _ = tokio::io::AsyncWriteExt::write_all(&mut write_server, &body).await;
        });
        Ok(super::GuestTcp {
            reader: Box::new(read_client),
            writer: Box::new(write_client),
        })
    }

    async fn start_managed(
        &self,
        launch: &super::ManagedLaunch,
    ) -> Result<super::ManagedInstance, EngineError> {
        let pid = 44000 + self.exec_count.fetch_add(1, Ordering::SeqCst);
        TEST_STARTED_PIDS.lock().unwrap().push(pid);
        self.tcp_ports.lock().await.push(pid as u16);
        let mut instance = managed_instance(pid);
        instance.service = launch.service();
        Ok(instance)
    }
    async fn managed_alive(&self, instance: &super::ManagedInstance) -> Result<bool, EngineError> {
        Ok(!TEST_DEAD_HELPERS.lock().unwrap().contains(&instance.pid))
    }
    async fn stop_managed(&self, instance: &super::ManagedInstance) -> Result<(), EngineError> {
        let pid = instance.pid;
        if TEST_FAIL_STOP_ONCE.swap(false, Ordering::SeqCst) {
            return Err(EngineError::Other(format!("stop failed for {pid}")));
        }
        TEST_STOPPED_PIDS.lock().unwrap().push(pid);
        self.tcp_ports
            .lock()
            .await
            .retain(|port| *port != pid as u16);
        Ok(())
    }
    fn name(&self) -> &str {
        &self.name
    }
}

pub static TEST_STARTED_PIDS: StdMutex<Vec<u32>> = StdMutex::new(Vec::new());
pub static TEST_STOPPED_WORKSPACES: StdMutex<Vec<String>> = StdMutex::new(Vec::new());
pub static TEST_STOPPED_PIDS: StdMutex<Vec<u32>> = StdMutex::new(Vec::new());
pub static TEST_DEAD_HELPERS: StdMutex<Vec<u32>> = StdMutex::new(Vec::new());
pub static TEST_FAIL_WORKSPACE_STOP: AtomicBool = AtomicBool::new(false);
pub static TEST_EXEC_HOLD: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub static TEST_HOLD_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
pub static TEST_FAIL_STOP_ONCE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub static TEST_NEXT_STATUS: StdMutex<Option<WorkspaceStatus>> = StdMutex::new(None);
pub static TEST_TCP_RELEASE: tokio::sync::Notify = tokio::sync::Notify::const_new();

#[cfg(feature = "test-utils")]
pub static TEST_HEADERS_SEEN: tokio::sync::Notify = tokio::sync::Notify::const_new();

#[cfg(feature = "test-utils")]
pub static TEST_DAV_TRUNCATE: tokio::sync::Notify = tokio::sync::Notify::const_new();
#[cfg(feature = "test-utils")]
pub static TEST_HTTP_DISCONNECTED: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub static TEST_HOLD_STOP_WORKSPACE: StdMutex<Option<String>> = StdMutex::new(None);

/// Stable process identity for session/proxy tests that do not run a guest.
pub fn managed_instance(pid: u32) -> super::ManagedInstance {
    super::ManagedInstance {
        service: super::ManagedService::StaticHttp,
        instance_id: uuid::Uuid::from_u128(pid as u128).simple().to_string(),
        pid,
        start_ticks: 1,
        boot_id: "mock-boot".into(),
        executable_device: 1,
        executable_inode: pid as u64,
        binary_digest: "mock-binary".into(),
        launch_digest: "mock-settings".into(),
        port: pid as u16,
    }
}
