mod managed;
#[cfg(feature = "msb")]
pub mod msb;
pub mod process;

#[cfg(feature = "test-utils")]
pub mod mock;

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Shared by the request deadline and its owned startup operation.
pub struct StartupProgress {
    started: std::time::Instant,
    phase: std::sync::Mutex<(&'static str, std::time::Instant)>,
    failure_logged: std::sync::atomic::AtomicBool,
}

impl Default for StartupProgress {
    fn default() -> Self {
        Self::new("runtime_status")
    }
}

impl StartupProgress {
    pub fn new(phase: &'static str) -> Self {
        let started = std::time::Instant::now();
        Self {
            started,
            phase: std::sync::Mutex::new((phase, started)),
            failure_logged: std::sync::atomic::AtomicBool::new(false),
        }
    }
    pub fn set_phase(&self, phase: &'static str) {
        let mut current = self.phase.lock().unwrap();
        if current.0 == phase {
            return;
        }
        tracing::debug!(
            phase = current.0,
            phase_elapsed_ms = current.1.elapsed().as_millis() as u64,
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            "runtime startup phase completed"
        );
        *current = (phase, std::time::Instant::now());
    }

    pub fn timeout(&self) -> EngineError {
        let phase = self.phase.lock().unwrap();
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        let phase_elapsed_ms = phase.1.elapsed().as_millis() as u64;
        tracing::warn!(
            phase = phase.0,
            phase_elapsed_ms,
            elapsed_ms,
            "DAV startup deadline exceeded"
        );
        EngineError::Timeout(format!("DAV startup deadline exceeded: phase={}, phase_elapsed_ms={phase_elapsed_ms}, elapsed_ms={elapsed_ms}", phase.0))
    }

    pub fn failed(&self, error: &EngineError) {
        // Record the operation that failed before cleanup changes the phase;
        // the owning request must not replace it with runtime_reconcile.
        if self
            .failure_logged
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let phase = self.phase.lock().unwrap();
        tracing::warn!(phase = phase.0, phase_elapsed_ms = phase.1.elapsed().as_millis() as u64,
            elapsed_ms = self.started.elapsed().as_millis() as u64, %error, "runtime startup failed");
    }

    pub fn phase(&self) -> &'static str {
        self.phase.lock().unwrap().0
    }
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub truncated: bool,
    pub output_path: Option<String>,
}

impl ExecResult {
    pub(super) fn from_owned_output(received: &[u8]) -> Result<Self, EngineError> {
        let data: serde_json::Value = serde_json::from_slice(received)
            .map_err(|e| EngineError::ExecError(format!("invalid execution result: {e}")))?;
        let result = Self {
            stdout: data["stdout"].as_str().unwrap_or_default().into(),
            stderr: data["stderr"].as_str().unwrap_or_default().into(),
            exit_code: data["exit_code"].as_i64().unwrap_or(-1) as i32,
            truncated: data["truncated"] == true,
            output_path: None,
        };
        let timed_out = data["timed_out"] == true;
        let cleanup_confirmed = data["cleanup_confirmed"] == true;
        if timed_out || data["cancelled"] == true || !cleanup_confirmed {
            return Err(EngineError::ExecutionEnded {
                output: Box::new(result),
                timed_out,
                cleanup_confirmed,
            });
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceStatus {
    /// Backend status could not be read; do not assume resources are released.
    Unknown,
    Running,
    /// The VM is resident and still consumes its active-workspace capacity.
    Paused,
    /// A start or graceful drain is in progress and still owns capacity.
    Transitioning,
    Stopped,
    NotFound,
}

/// Network egress policy applied to a sandbox.
///
/// Maps to microsandbox's `NetworkPolicy::from_profiles`. The `Unrestricted`
/// variant is the default and applies no policy at all (fully backward
/// compatible — current behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkAccess {
    /// No network policy — unrestricted (default).
    #[default]
    Unrestricted,
    /// Public internet only (denies private LAN and host/gateway).
    PublicOnly,
    /// Public internet + private LAN (denies host/gateway).
    PrivateAndPublic,
    /// No egress network access at all.
    None,
}

impl NetworkAccess {
    /// Map to microsandbox profiles, if any policy should be applied.
    #[cfg(feature = "msb")]
    pub fn to_profiles(&self) -> Option<Vec<microsandbox::NetworkProfile>> {
        match self {
            NetworkAccess::Unrestricted => None,
            NetworkAccess::PublicOnly => Some(vec![microsandbox::NetworkProfile::Public]),
            NetworkAccess::PrivateAndPublic => Some(vec![
                microsandbox::NetworkProfile::Public,
                microsandbox::NetworkProfile::Private,
            ]),
            NetworkAccess::None => Some(vec![]),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceOpts {
    pub image: String,
    pub cpus: u32,
    pub memory_mib: u32,
    pub shell: String,
    pub volumes: Vec<VolumeSpec>,
    pub network_access: NetworkAccess,
    /// Host-side SOCKS proxy, applied after the destination access policy.
    pub outbound_proxy: Option<crate::config::OutboundProxyConfig>,
    /// Per-sandbox concurrent connection caps. Zero means unlimited.
    pub max_tcp_connections: Option<usize>,
    pub max_udp_connections: Option<usize>,
}

/// Point-in-time guest memory sample for a running sandbox.
#[derive(Debug, Clone)]
pub struct SandboxMetricsSnapshot {
    /// Guest-available memory; absence means the sample cannot report usage.
    pub memory_available_bytes: Option<u64>,
    /// MSB snapshot time, not the guest's memory-report update time.
    pub observed_at_ms: Option<u64>,
    /// Configured guest memory limit in bytes.
    pub memory_limit_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VolumeKind {
    Disk,
    Tmpfs,
    Bind,
}

#[derive(Debug, Clone)]
pub struct VolumeSpec {
    pub guest_path: String,
    pub name: Option<String>,
    pub kind: VolumeKind,
    pub size_mib: u32,
    pub host_path: Option<String>,
    pub readonly: bool,
}

impl VolumeSpec {
    pub fn disk(guest_path: &str, name: &str, size_mib: u32) -> Self {
        Self {
            guest_path: guest_path.to_string(),
            name: Some(name.to_string()),
            kind: VolumeKind::Disk,
            size_mib,
            host_path: None,
            readonly: false,
        }
    }

    pub fn tmpfs(guest_path: &str, size_mib: u32) -> Self {
        Self {
            guest_path: guest_path.to_string(),
            name: None,
            kind: VolumeKind::Tmpfs,
            size_mib,
            host_path: None,
            readonly: false,
        }
    }

    pub fn bind(guest_path: &str, host_path: &str, readonly: bool) -> Self {
        Self {
            guest_path: guest_path.to_string(),
            name: None,
            kind: VolumeKind::Bind,
            size_mib: 0,
            host_path: Some(host_path.to_string()),
            readonly,
        }
    }
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("metrics unavailable: {0}")]
    MetricsUnavailable(String),
    #[error("sandbox not found: {0}")]
    NotFound(String),
    #[error("sandbox already exists: {0}")]
    AlreadyExists(String),
    #[error("execution error: {0}")]
    ExecError(String),
    #[error("execution deadline/cancellation; cleanup confirmed={cleanup_confirmed}")]
    ExecutionEnded {
        output: Box<ExecResult>,
        timed_out: bool,
        cleanup_confirmed: bool,
    },
    #[error("sandbox not running: {0}")]
    SandboxNotRunning(String),
    #[error("sandbox paused: {0}")]
    SandboxPaused(String),
    #[error("sandbox lifecycle transition in progress: {0}")]
    SandboxTransitioning(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("resource exhausted: {0}")]
    ResourceExhausted(String),
    #[error("resource exhausted: maximum number of active workspaces reached")]
    WorkspaceCapacityExhausted,
    #[error("managed service requires owner drain before replacement: {0:?}")]
    ManagedRestartRequired(Box<ManagedInstance>),
    #[error("engine error: {0}")]
    Other(String),
}

#[async_trait]
pub trait AgentEngine: Send + Sync {
    /// Immutable persisted rootfs input, without waking the sandbox.
    async fn sandbox_image(&self, _name: &str) -> Result<String, EngineError> {
        Err(EngineError::Other("image inspection unsupported".into()))
    }
    /// Metadata only. Implementations must never activate or resume a VM.
    async fn inspect_resources(
        &self,
        _name: &str,
        _volumes: &[String],
        _defaults: &WorkspaceOpts,
    ) -> Result<crate::resources::ResourceInspection, EngineError> {
        Err(EngineError::Other("resource inspection unsupported".into()))
    }

    /// Read the already-running guest's workspace filesystem, without activity leases.
    async fn workspace_disk_usage(
        &self,
        _name: &str,
        _run_identity: &str,
    ) -> Result<crate::resources::DiskUsage, EngineError> {
        Err(EngineError::Other(
            "workspace statistics unsupported".into(),
        ))
    }
    async fn create_sandbox(
        &self,
        name: &str,
        opts: &WorkspaceOpts,
    ) -> Result<Arc<dyn AgentHandle>, EngineError>;
    async fn get_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError>;
    async fn start_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError>;
    /// Inspect authoritative residency. Production engines use metadata only;
    /// the compatibility default obtains the existing handle without starting.
    async fn sandbox_status(&self, name: &str) -> Result<WorkspaceStatus, EngineError> {
        match self.get_sandbox(name).await {
            Ok(handle) => handle.try_status().await,
            Err(EngineError::NotFound(_)) => Ok(WorkspaceStatus::NotFound),
            Err(EngineError::SandboxNotRunning(_)) => Ok(WorkspaceStatus::Stopped),
            Err(EngineError::SandboxPaused(_)) => Ok(WorkspaceStatus::Paused),
            Err(EngineError::SandboxTransitioning(_)) => Ok(WorkspaceStatus::Transitioning),
            Err(error) => Err(error),
        }
    }
    async fn start_sandbox_with_progress(
        &self,
        name: &str,
        progress: &StartupProgress,
    ) -> Result<Arc<dyn AgentHandle>, EngineError> {
        progress.set_phase("vm_start");
        self.start_sandbox(name).await
    }
    async fn remove_sandbox(&self, name: &str) -> Result<(), EngineError>;
    async fn list_running(&self) -> Result<Vec<String>, EngineError>;
    /// Include stopped workspaces for orphan cleanup.
    async fn list_workspaces(&self) -> Result<Vec<String>, EngineError> {
        self.list_running().await
    }

    /// Engine-owned managed volumes. Engines without named volumes return none.
    async fn list_volumes(&self) -> Result<Vec<String>, EngineError> {
        Ok(Vec::new())
    }
    async fn remove_volume(&self, name: &str) -> Result<(), EngineError> {
        Err(EngineError::Other(format!(
            "volume removal is not supported by this engine: {}",
            name
        )))
    }

    /// Get a live guest memory sample for a running sandbox.
    async fn get_metrics(&self, name: &str) -> Result<SandboxMetricsSnapshot, EngineError> {
        Err(EngineError::Other(format!(
            "get_metrics is not supported by this engine (sandbox '{}')",
            name
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedService {
    StaticHttp,
    Webdav,
    Desktop,
}
impl ManagedService {
    pub fn executable(self) -> &'static str {
        match self {
            Self::StaticHttp => "/opt/agent-space/bin/agent-space-static-http",
            Self::Webdav => "/opt/agent-space/bin/agent-space-webdav",
            Self::Desktop => "/opt/agent-space/bin/agent-space-desktop",
        }
    }
}

/// Startup requests select fixed project-owned services only.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagedLaunch {
    Desktop {
        host_binary: String,
    },
    StaticHttp {
        host_binary: String,
        root: String,
        file: bool,
    },
    Webdav {
        host_binary: String,
        owner: String,
        config: serde_json::Value,
    },
}
impl ManagedLaunch {
    pub fn service(&self) -> ManagedService {
        match self {
            Self::StaticHttp { .. } => ManagedService::StaticHttp,
            Self::Webdav { .. } => ManagedService::Webdav,
            Self::Desktop { .. } => ManagedService::Desktop,
        }
    }
    pub fn host_binary(&self) -> &str {
        match self {
            Self::StaticHttp { host_binary, .. }
            | Self::Webdav { host_binary, .. }
            | Self::Desktop { host_binary } => host_binary,
        }
    }
    pub fn settings(&self) -> serde_json::Value {
        match self {
            Self::Desktop { .. } => serde_json::json!({"worker_contract":1}),
            Self::StaticHttp { root, file, .. } => serde_json::json!({"root":root,"file":file}),
            Self::Webdav { owner, config, .. } => {
                serde_json::json!({"owner":owner,"config":config})
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedInstance {
    pub service: ManagedService,
    pub instance_id: String,
    pub pid: u32,
    pub start_ticks: u64,
    pub boot_id: String,
    pub executable_device: u64,
    pub executable_inode: u64,
    pub binary_digest: String,
    pub launch_digest: String,
    pub port: u16,
}

#[async_trait]
pub trait AgentHandle: Send + Sync {
    /// Grow an existing tmpfs mount without clearing files or shrinking it.
    async fn grow_tmpfs(&self, _guest_path: &str, _size_mib: u32) -> Result<(), EngineError> {
        Err(EngineError::Other("tmpfs growth is unsupported".into()))
    }
    async fn exec(
        &self,
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
    ) -> Result<ExecResult, EngineError>;
    async fn exec_bounded(
        &self,
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
        _max_lines: usize,
        _max_bytes: usize,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExecResult, EngineError> {
        tokio::select! {
            result = self.exec(program, args, timeout) => result,
            _ = cancel.cancelled() => Err(EngineError::Other("execution cancelled".into())),
        }
    }
    async fn stop(&self) -> Result<(), EngineError>;
    async fn kill(&self) -> Result<(), EngineError>;
    async fn status(&self) -> WorkspaceStatus;
    /// Preserve backend failures for lifecycle decisions. Engines without a
    /// fallible status API can use the existing status implementation.
    async fn try_status(&self) -> Result<WorkspaceStatus, EngineError> {
        match self.status().await {
            WorkspaceStatus::Unknown => Err(EngineError::Other(format!(
                "workspace '{}' status unavailable",
                self.name()
            ))),
            status => Ok(status),
        }
    }
    async fn start_managed(&self, _launch: &ManagedLaunch) -> Result<ManagedInstance, EngineError> {
        Err(EngineError::Other(format!(
            "managed services are not supported by {}",
            self.name()
        )))
    }
    async fn managed_alive(&self, _instance: &ManagedInstance) -> Result<bool, EngineError> {
        Err(EngineError::Other(
            "managed service inspection is unsupported".into(),
        ))
    }
    async fn stop_managed(&self, _instance: &ManagedInstance) -> Result<(), EngineError> {
        Err(EngineError::Other(
            "managed service stop is unsupported".into(),
        ))
    }
    async fn list_managed(
        &self,
        _service: ManagedService,
    ) -> Result<Vec<ManagedInstance>, EngineError> {
        Ok(Vec::new())
    }
    async fn reclaim_managed(&self, service: ManagedService) -> Result<(), EngineError> {
        for instance in self.list_managed(service).await? {
            self.stop_managed(&instance).await?;
        }
        Ok(())
    }
    /// TCP is independent of the bash execution queue.
    async fn connect_guest_tcp(&self, _port: u16) -> Result<GuestTcp, EngineError> {
        Err(EngineError::Other(format!(
            "guest tcp is not supported by {}",
            self.name()
        )))
    }
    fn name(&self) -> &str;
}

pub struct GuestTcp {
    pub reader: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    pub writer: Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
}
