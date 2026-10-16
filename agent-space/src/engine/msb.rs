use super::*;
use async_trait::async_trait;
use microsandbox::size::SizeExt;
use microsandbox::{Sandbox, Volume};
use std::sync::Arc;
use std::time::Duration;

pub struct MsbEngine;

impl MsbEngine {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MsbEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentEngine for MsbEngine {
    async fn sandbox_image(&self, name: &str) -> Result<String, EngineError> {
        let sandbox = Sandbox::get(name)
            .await
            .map_err(|error| map_lifecycle_error(name, error))?;
        let config = sandbox
            .config()
            .map_err(|error| map_lifecycle_error(name, error))?;
        match config.spec.image {
            microsandbox::sandbox::RootfsSource::Oci(image) => Ok(image.reference),
            _ => Err(EngineError::Other(
                "rootfs is not an OCI image; explicit migration mapping required".into(),
            )),
        }
    }
    async fn inspect_resources(
        &self,
        name: &str,
        volumes: &[String],
        defaults: &WorkspaceOpts,
    ) -> Result<crate::resources::ResourceInspection, EngineError> {
        use crate::resources::{Allocation, ResourceAllocation, ResourceInspection};
        let sandbox = match tokio::time::timeout(Duration::from_secs(1), Sandbox::get(name))
            .await
            .map_err(|_| EngineError::Timeout("resource catalog lookup".into()))?
        {
            Ok(value) => Some(value),
            Err(microsandbox::MicrosandboxError::SandboxNotFound(_)) => None,
            Err(error) => return Err(map_lifecycle_error(name, error)),
        };
        let mut allocation = ResourceAllocation::error("configuration_unavailable");
        let mut verified = false;
        let mut status = WorkspaceStatus::NotFound;
        let mut identity = None;
        let config = if let Some(sandbox) = &sandbox {
            status = resource_status(sandbox.status_snapshot());
            identity = resource_run_identity(sandbox);
            sandbox
                .active_config()
                .and_then(|active| match active {
                    Some(config) => Ok(config),
                    None => sandbox.config(),
                })
                .ok()
        } else {
            None
        };
        if let Some(config) = &config {
            allocation.cpu_cores =
                Allocation::known(u32::from(config.spec.resources.cpus), "sandbox");
            allocation.memory_limit_bytes = Allocation::known(
                u64::from(config.spec.resources.memory_mib) * 1024 * 1024,
                "sandbox",
            );
        } else if sandbox.is_none() {
            allocation.cpu_cores = Allocation::known(defaults.cpus, "default");
            allocation.memory_limit_bytes =
                Allocation::known(u64::from(defaults.memory_mib) * 1024 * 1024, "default");
        }
        // The current space contract has exactly one registered named workspace disk.
        // Never infer ownership from a username or sandbox-name prefix.
        if let [volume_name] = volumes {
            match tokio::time::timeout(Duration::from_secs(1), Volume::get(volume_name)).await {
                Ok(Ok(volume)) => {
                    allocation.workspace_capacity_bytes = match volume.capacity_bytes() {
                        Some(bytes) => Allocation::known(bytes, "volume"),
                        None => Allocation::error("workspace_capacity_unavailable"),
                    };
                    if let (Some(config), Some(path)) = (&config, volume.disk_path()) {
                        verified = config.spec.mounts.iter().any(|mount| {
                            matches!(mount, microsandbox::sandbox::VolumeMount::DiskImage { host, guest, .. }
                                if guest == "/workspace" && host == &path)
                        });
                        if !verified {
                            allocation.workspace_capacity_bytes =
                                Allocation::error("workspace_disk_mismatch");
                        }
                    }
                }
                Ok(Err(microsandbox::MicrosandboxError::VolumeNotFound(_)))
                    if sandbox.is_none() =>
                {
                    allocation.workspace_capacity_bytes = match defaults
                        .volumes
                        .iter()
                        .find(|v| v.guest_path == "/workspace" && v.kind == VolumeKind::Disk)
                    {
                        Some(volume) => {
                            Allocation::known(u64::from(volume.size_mib) * 1024 * 1024, "default")
                        }
                        None => Allocation::error("workspace_profile_unavailable"),
                    };
                }
                _ => {
                    allocation.workspace_capacity_bytes =
                        Allocation::error("workspace_volume_unavailable")
                }
            }
        } else {
            allocation.workspace_capacity_bytes =
                Allocation::error("workspace_volume_unregistered");
        }
        Ok(ResourceInspection {
            status,
            run_identity: identity,
            allocation,
            workspace_disk_verified: verified,
        })
    }

    async fn workspace_disk_usage(
        &self,
        name: &str,
        run_identity: &str,
    ) -> Result<crate::resources::DiskUsage, EngineError> {
        let handle = Sandbox::get(name)
            .await
            .map_err(|e| map_lifecycle_error(name, e))?;
        if resource_status(handle.status_snapshot()) != WorkspaceStatus::Running
            || resource_run_identity(&handle).as_deref() != Some(run_identity)
        {
            return Err(EngineError::SandboxNotRunning(name.into()));
        }
        // connect() only attaches; unlike connect_or_start(), it never activates.
        let sandbox = handle
            .connect()
            .await
            .map_err(|e| map_lifecycle_error(name, e))?;
        let output = sandbox
            .exec(
                "python3",
                ["-I", "-c", include_str!("../../guest/workspace_stats.py")],
            )
            .await
            .map_err(|e| classify_exec_error(name, e))?;
        if output.status().code != 0 {
            return Err(EngineError::Other(
                "workspace filesystem validation/stat failed".into(),
            ));
        }
        let stdout = output
            .stdout()
            .map_err(|_| EngineError::Other("invalid workspace statistics encoding".into()))?;
        serde_json::from_str(&stdout)
            .map_err(|_| EngineError::Other("invalid workspace statistics".into()))
    }

    async fn create_sandbox(
        &self,
        name: &str,
        opts: &WorkspaceOpts,
    ) -> Result<Arc<dyn AgentHandle>, EngineError> {
        // Reject before creating any disk. A narrowing cast can turn 257 CPUs
        // into one and silently contradict the configured allocation.
        let cpus = u8::try_from(opts.cpus)
            .ok()
            .filter(|cpus| *cpus > 0)
            .ok_or_else(|| EngineError::Other("workspace.cpus must be between 1 and 255".into()))?;
        // Pre-create disk volumes and collect their host paths
        let mut disk_mounts: Vec<(String, String, bool)> = Vec::new();
        for vol in &opts.volumes {
            if vol.kind == VolumeKind::Disk {
                let vol_name = vol.name.as_deref().unwrap_or("vol");
                let scoped_name = format!("{}-{}", name, vol_name);
                let size = vol.size_mib.mib();

                // Reuse persisted disks; activation must never replace user data.
                match Volume::builder(&scoped_name)
                    .disk()
                    .size(size)
                    .create()
                    .await
                {
                    Ok(volume) => {
                        let disk_path = volume.disk_path().ok_or_else(|| {
                            EngineError::Other(format!("volume '{}' has no disk_path", scoped_name))
                        })?;
                        tracing::info!(volume = %scoped_name, disk_path = %disk_path.display(), "created disk volume, mounting via virtio-blk");
                        disk_mounts.push((
                            vol.guest_path.clone(),
                            disk_path.to_string_lossy().to_string(),
                            vol.readonly,
                        ));
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if matches!(e, microsandbox::MicrosandboxError::VolumeAlreadyExists(_)) {
                            tracing::info!(volume = %scoped_name, "volume already exists, reusing");
                            let volume = Volume::get(&scoped_name).await.map_err(|e| {
                                EngineError::Other(format!(
                                    "failed to get existing volume '{}': {}",
                                    scoped_name, e
                                ))
                            })?;
                            let disk_path = volume.disk_path().ok_or_else(|| {
                                EngineError::Other(format!(
                                    "existing volume '{}' has no disk_path",
                                    scoped_name
                                ))
                            })?;
                            disk_mounts.push((
                                vol.guest_path.clone(),
                                disk_path.to_string_lossy().to_string(),
                                vol.readonly,
                            ));
                        } else {
                            return Err(EngineError::Other(format!(
                                "failed to create volume '{}': {}",
                                scoped_name, msg
                            )));
                        }
                    }
                }
            }
        }

        let mut builder = Sandbox::builder(name)
            .image(opts.image.as_str())
            .shell(&opts.shell)
            .cpus(cpus)
            .memory(opts.memory_mib)
            .detached(true);

        for (guest_path, host_disk_path, readonly) in &disk_mounts {
            builder = builder.volume(guest_path, |v| {
                let b = v.disk(host_disk_path);
                if *readonly {
                    b.readonly()
                } else {
                    b
                }
            });
        }

        for vol in &opts.volumes {
            match vol.kind {
                VolumeKind::Disk => {}
                VolumeKind::Tmpfs => {
                    builder = builder.volume(&vol.guest_path, |v| v.tmpfs().size(vol.size_mib));
                }
                VolumeKind::Bind => {
                    let host = vol.host_path.as_deref().unwrap_or("");
                    builder = builder.volume(&vol.guest_path, |v| {
                        let b = v.bind(host);
                        if vol.readonly {
                            b.readonly()
                        } else {
                            b
                        }
                    });
                }
            }
        }

        let profiles = opts.network_access.to_profiles();
        if profiles.is_some()
            || opts.max_tcp_connections.is_some()
            || opts.max_udp_connections.is_some()
        {
            builder = builder.network(|mut network| {
                if let Some(profiles) = profiles {
                    network = network.policy(microsandbox::NetworkPolicy::from_profiles(profiles));
                }
                if let Some(limit) = opts.max_tcp_connections {
                    network = network.max_tcp_connections(limit);
                }
                if let Some(limit) = opts.max_udp_connections {
                    network = network.max_udp_connections(limit);
                }
                network
            });
        }

        if let Some(proxy) = &opts.outbound_proxy {
            let proxy = proxy
                .to_msb()
                .map_err(|error| EngineError::Other(format!("invalid outbound proxy: {error}")))?;
            builder = builder.proxy(|_| proxy);
        }

        let sb = builder
            .create()
            .await
            .map_err(|e| map_lifecycle_error(name, e))?;

        Ok(Arc::new(MsbHandle {
            name: name.to_string(),
            inner: Some(sb),
        }))
    }

    async fn get_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
        let handle = inspect_sandbox(name).await?;
        // Starting/Draining still hold resources. Do not misreport a failed
        // attach as a stopped VM and release the manager's capacity lease.
        use microsandbox::sandbox::SandboxStatus;
        match handle.status_snapshot() {
            SandboxStatus::Running => {}
            SandboxStatus::Paused => {
                return Err(EngineError::SandboxPaused(name.into()));
            }
            SandboxStatus::Starting | SandboxStatus::Draining => {
                return Err(EngineError::SandboxTransitioning(name.into()));
            }
            SandboxStatus::Created | SandboxStatus::Stopped | SandboxStatus::Crashed => {
                return Err(EngineError::SandboxNotRunning(name.into()));
            }
        }
        let sb = handle
            .connect_with_timeout(Duration::from_secs(5))
            .await
            .map_err(|e| map_lifecycle_error(name, e))?;
        Ok(Arc::new(MsbHandle {
            name: name.into(),
            inner: Some(sb),
        }))
    }

    async fn sandbox_status(&self, name: &str) -> Result<WorkspaceStatus, EngineError> {
        match inspect_sandbox(name).await {
            Ok(handle) => Ok(resource_status(handle.status_snapshot())),
            Err(EngineError::NotFound(_)) => Ok(WorkspaceStatus::NotFound),
            Err(error) => Err(error),
        }
    }

    async fn start_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
        self.start_sandbox_with_progress(name, &StartupProgress::default())
            .await
    }

    async fn start_sandbox_with_progress(
        &self,
        name: &str,
        progress: &StartupProgress,
    ) -> Result<Arc<dyn AgentHandle>, EngineError> {
        progress.set_phase("lookup_vm");
        let handle = inspect_sandbox(name).await?;
        // The SDK serializes named transitions and retains the captured ID.
        // 0.7.x deliberately requires an explicit resume for resident paused
        // VMs. Draining is observed until the same VM reaches Stopped, then the
        // SDK starts it again. One budget covers every activation transition.
        let sb = tokio::time::timeout(Duration::from_secs(60), async {
            let mut disk_release_deadline = None;
            loop {
                let current = handle.refresh().await?;
                match current.status_snapshot() {
                    microsandbox::sandbox::SandboxStatus::Paused => {
                        progress.set_phase("vm_resume");
                        current.resume().await?;
                        continue;
                    }
                    microsandbox::sandbox::SandboxStatus::Draining
                    | microsandbox::sandbox::SandboxStatus::Starting => {
                        progress.set_phase("wait_vm_transition");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    _ => {}
                }

                let result =
                    if current.status_snapshot() == microsandbox::sandbox::SandboxStatus::Running {
                        progress.set_phase("guest_connect");
                        current.connect_with_timeout(Duration::from_secs(5)).await
                    } else {
                        progress.set_phase("vm_start");
                        current.start_detached().await
                    };
                match result {
                    Ok(sandbox) => return Ok(sandbox),
                    Err(error @ microsandbox::MicrosandboxError::SandboxNotRunning(_))
                    | Err(error @ microsandbox::MicrosandboxError::SandboxStillRunning(_)) => {
                        // Pause or drain may race the observation above. Retry
                        // only when the authoritative state confirms a known
                        // transition; otherwise preserve the SDK error.
                        let latest = handle.refresh().await?;
                        let transitioning = matches!(
                            latest.status_snapshot(),
                            microsandbox::sandbox::SandboxStatus::Paused
                                | microsandbox::sandbox::SandboxStatus::Starting
                                | microsandbox::sandbox::SandboxStatus::Draining
                        );
                        let concurrent_start = matches!(
                            error,
                            microsandbox::MicrosandboxError::SandboxStillRunning(_)
                        ) && latest.status_snapshot()
                            == microsandbox::sandbox::SandboxStatus::Running;
                        if transitioning || concurrent_start {
                            continue;
                        }
                        return Err(error);
                    }
                    Err(microsandbox::MicrosandboxError::InvalidConfig(message))
                        if message.contains("already attached with an incompatible disk mode") =>
                    {
                        // Explicit /workspace disks are not SDK owned-mount
                        // markers. SIGKILL can release the runtime lock before
                        // deferred kernel disk teardown; observe the real lock
                        // by retrying attachment, never unlinking or overriding it.
                        let deadline = *disk_release_deadline.get_or_insert_with(|| {
                            tokio::time::Instant::now() + Duration::from_secs(5)
                        });
                        let latest = handle.refresh().await?;
                        if tokio::time::Instant::now() >= deadline
                            || !matches!(
                                latest.status_snapshot(),
                                microsandbox::sandbox::SandboxStatus::Created
                                    | microsandbox::sandbox::SandboxStatus::Stopped
                                    | microsandbox::sandbox::SandboxStatus::Crashed
                            )
                        {
                            return Err(microsandbox::MicrosandboxError::InvalidConfig(message));
                        }
                        progress.set_phase("wait_disk_release");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .map_err(|_| {
            EngineError::Timeout(format!(
                "sandbox '{name}' activation exceeded 60s: phase={}",
                progress.phase()
            ))
        })?
        .map_err(|e| map_lifecycle_error(name, e))?;
        Ok(Arc::new(MsbHandle {
            name: name.into(),
            inner: Some(sb),
        }))
    }

    async fn remove_sandbox(&self, name: &str) -> Result<(), EngineError> {
        let handle = Sandbox::get(name)
            .await
            .map_err(|e| map_lifecycle_error(name, e))?;
        handle
            .destroy()
            .await
            .map_err(|e| map_lifecycle_error(name, e))
    }

    async fn list_running(&self) -> Result<Vec<String>, EngineError> {
        list_workspaces(true).await
    }

    async fn list_workspaces(&self) -> Result<Vec<String>, EngineError> {
        list_workspaces(false).await
    }

    async fn list_volumes(&self) -> Result<Vec<String>, EngineError> {
        microsandbox::Volume::list()
            .await
            .map(|volumes| volumes.into_iter().map(|v| v.name().to_string()).collect())
            .map_err(|e| EngineError::Other(format!("list volumes: {}", e)))
    }

    async fn remove_volume(&self, name: &str) -> Result<(), EngineError> {
        remove_managed_volume(name).await
    }

    async fn get_metrics(&self, name: &str) -> Result<SandboxMetricsSnapshot, EngineError> {
        let sb = Sandbox::get(name)
            .await
            .map_err(|e| map_lifecycle_error(name, e))?;
        if sb
            .config()
            .map_err(|e| EngineError::Other(e.to_string()))?
            .effective_metrics_interval()
            .is_none()
        {
            return Err(EngineError::MetricsUnavailable("sampling_disabled".into()));
        }
        if sb.status_snapshot() != microsandbox::sandbox::SandboxStatus::Running {
            return Err(EngineError::SandboxNotRunning(name.into()));
        }
        let backend = microsandbox::backend::default_backend();
        let local = backend
            .as_local()
            .ok_or_else(|| EngineError::Other("local metrics required".into()))?;
        let report = microsandbox::sandbox::sandbox_metrics_report_local(local, name)
            .await
            .map_err(|e| EngineError::Other(format!("failed to get metrics for '{name}': {e}")))?
            .ok_or_else(|| EngineError::MetricsUnavailable("not_sampled".into()))?;
        if report.state == microsandbox::sandbox::SandboxMetricsState::Exited {
            return Err(EngineError::MetricsUnavailable("runtime_exited".into()));
        }
        let m = report.metrics;
        Ok(SandboxMetricsSnapshot {
            memory_available_bytes: m.memory_available_bytes,
            observed_at_ms: u64::try_from(m.timestamp.timestamp_millis()).ok(),
            memory_limit_bytes: m.memory_limit_bytes,
        })
    }
}

async fn inspect_sandbox(name: &str) -> Result<microsandbox::sandbox::SandboxHandle, EngineError> {
    tokio::time::timeout(Duration::from_secs(10), async {
        Sandbox::get(name)
            .await
            .map_err(|error| map_lifecycle_error(name, error))
    })
    .await
    .map_err(|_| EngineError::Timeout(format!("sandbox '{name}' metadata lookup exceeded 10s")))?
}

fn resource_status(status: microsandbox::sandbox::SandboxStatus) -> WorkspaceStatus {
    use microsandbox::sandbox::SandboxStatus;
    match status {
        SandboxStatus::Running => WorkspaceStatus::Running,
        SandboxStatus::Paused => WorkspaceStatus::Paused,
        SandboxStatus::Starting | SandboxStatus::Draining => WorkspaceStatus::Transitioning,
        SandboxStatus::Created | SandboxStatus::Stopped | SandboxStatus::Crashed => {
            WorkspaceStatus::Stopped
        }
    }
}

fn resource_run_identity(handle: &microsandbox::sandbox::SandboxHandle) -> Option<String> {
    let local = handle.local()?;
    let pid = local.pid?;
    // PID plus kernel start ticks prevents reuse of a cached sample after a restart.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(") ")?;
    let start = fields.split_whitespace().nth(19)?;
    Some(format!("{}:{pid}:{start}", local.db_id))
}

async fn list_workspaces(running_only: bool) -> Result<Vec<String>, EngineError> {
    let mut cursor = None;
    let mut names = Vec::new();
    loop {
        let page = Sandbox::list_with(|list| match cursor.take() {
            Some(cursor) => list.cursor(cursor),
            None => list,
        })
        .await
        .map_err(|e| EngineError::Other(format!("list workspaces: {}", e)))?;
        for h in page.sandboxes {
            if !running_only
                || matches!(
                    h.status_snapshot(),
                    microsandbox::sandbox::SandboxStatus::Running
                        | microsandbox::sandbox::SandboxStatus::Paused
                        | microsandbox::sandbox::SandboxStatus::Starting
                        | microsandbox::sandbox::SandboxStatus::Draining
                )
            {
                names.push(h.name().to_string());
            }
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Ok(names);
        }
    }
}

pub struct MsbHandle {
    name: String,
    inner: Option<Sandbox>,
}

#[async_trait]
impl AgentHandle for MsbHandle {
    async fn grow_tmpfs(&self, guest_path: &str, size_mib: u32) -> Result<(), EngineError> {
        let size = size_mib.to_string();
        let result = self
            .exec(
                "python3",
                &[
                    "-I",
                    "-c",
                    include_str!("../../guest/tmpfs.py"),
                    guest_path,
                    &size,
                ],
                Some(Duration::from_secs(10)),
            )
            .await?;
        if result.exit_code != 0 {
            return Err(EngineError::Other(format!(
                "tmpfs preparation failed for {guest_path}: {}",
                result.stderr.trim()
            )));
        }
        Ok(())
    }
    async fn exec(
        &self,
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
    ) -> Result<ExecResult, EngineError> {
        self.exec_bounded(
            program,
            args,
            timeout,
            20000,
            1024 * 1024,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
    }
    async fn exec_bounded(
        &self,
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
        max_lines: usize,
        max_bytes: usize,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExecResult, EngineError> {
        use microsandbox::ExecEvent;
        let sb = self
            .inner
            .as_ref()
            .ok_or_else(|| EngineError::Other("sandbox handle not connected".into()))?;
        if program.is_empty() {
            return Ok(ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                truncated: false,
                output_path: None,
            });
        }
        let spec = serde_json::json!({"program": program, "args": args,
            "timeout_secs": timeout.map(|t|t.as_secs_f64()),
            "max_lines": max_lines.max(1), "max_bytes": max_bytes.max(1)})
        .to_string();
        let mut stream = sb
            .exec_stream(
                "python3",
                ["-I", "-c", include_str!("../../guest/exec.py"), &spec],
            )
            .await
            .map_err(|e| classify_exec_error(&self.name, e))?;
        let control = stream.control();
        let mut received = Vec::new();
        let mut errors = Vec::new();
        let mut cancelling = false;
        // The guest enforces the execution deadline. This additional transport
        // budget is only for detecting lost supervision/results; never replay.
        let budget = timeout.unwrap_or(Duration::from_secs(86400)) + Duration::from_secs(8);
        let mut stop_at = tokio::time::Instant::now() + budget;
        loop {
            let event = tokio::select! {
                event = stream.recv() => event,
                _ = cancel.cancelled(), if !cancelling => {
                    cancelling = true;
                    stop_at = stop_at.min(tokio::time::Instant::now() + Duration::from_secs(8));
                    control.signal(libc::SIGTERM).await.map_err(|e|classify_exec_error(&self.name,e))?;
                    continue;
                },
                _ = tokio::time::sleep_until(stop_at) => {
                    let _ = control.signal(libc::SIGTERM).await;
                    return Err(EngineError::Timeout("execution result/cleanup unavailable; side effects may have occurred".into()));
                }
            };
            match event {
                Some(ExecEvent::Stdout(data)) => {
                    if received.len() + data.len()
                        > max_bytes.saturating_mul(12).saturating_add(4096)
                    {
                        let _ = control.signal(libc::SIGTERM).await;
                        return Err(EngineError::ExecError(
                            "bounded execution result overflow".into(),
                        ));
                    }
                    received.extend_from_slice(&data);
                }
                Some(ExecEvent::Stderr(data)) => {
                    errors.extend_from_slice(&data);
                    if errors.len() > 4096 {
                        errors.drain(..errors.len() - 4096);
                    }
                }
                Some(ExecEvent::Exited { code }) => {
                    if code != 0 {
                        return Err(EngineError::ExecError(
                            String::from_utf8_lossy(&errors).into(),
                        ));
                    }
                    break;
                }
                Some(ExecEvent::Failed(e)) => {
                    return Err(EngineError::ExecError(format!(
                        "execution owner failed: {e:?}"
                    )))
                }
                None => {
                    return Err(EngineError::ExecError(
                        "execution result lost; side effects may have occurred".into(),
                    ))
                }
                _ => {}
            }
        }
        ExecResult::from_owned_output(&received)
    }

    async fn stop(&self) -> Result<(), EngineError> {
        let sb = self
            .inner
            .as_ref()
            .ok_or_else(|| EngineError::NotFound(self.name.clone()))?;
        // A paused guest cannot receive the graceful shutdown request. Resume
        // this exact VM before stopping it so capacity is actually released.
        if sb
            .status()
            .await
            .map_err(|e| map_lifecycle_error(&self.name, e))?
            == microsandbox::sandbox::SandboxStatus::Paused
        {
            sb.resume()
                .await
                .map_err(|e| map_lifecycle_error(&self.name, e))?;
        }
        // Let the desktop owner close Chromium and flush its profile before
        // the VM shuts down. A bounded cleanup failure still falls through to
        // authoritative VM shutdown; it is never mistaken for process cleanup.
        match tokio::time::timeout(
            Duration::from_secs(8),
            self.reclaim_managed(ManagedService::Desktop),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(workspace=%self.name,error=%error,"desktop graceful shutdown failed; stopping VM")
            }
            Err(error) => {
                tracing::warn!(workspace=%self.name,error=%error,"desktop graceful shutdown deadline; stopping VM")
            }
        }
        // Receiver calls preserve the SDK identity. Resolving by name here
        // would let an old session stop a replacement workspace.
        sb.stop()
            .await
            .map_err(|e| map_lifecycle_error(&self.name, e))?;
        Ok(())
    }

    async fn kill(&self) -> Result<(), EngineError> {
        let sb = self
            .inner
            .as_ref()
            .ok_or_else(|| EngineError::NotFound(self.name.clone()))?;
        sb.kill()
            .await
            .map_err(|e| map_lifecycle_error(&self.name, e))?;
        Ok(())
    }

    async fn status(&self) -> WorkspaceStatus {
        self.try_status().await.unwrap_or(WorkspaceStatus::Unknown)
    }

    async fn try_status(&self) -> Result<WorkspaceStatus, EngineError> {
        if let Some(sb) = &self.inner {
            // Cached handles need the same generation check as cold lookup:
            // a stale active PID must never authorize later stop/kill calls.
            match inspect_sandbox(&self.name).await {
                Ok(_) => {}
                Err(EngineError::NotFound(_)) => return Ok(WorkspaceStatus::NotFound),
                Err(error) => return Err(error),
            }
            match sb.status().await {
                Ok(microsandbox::sandbox::SandboxStatus::Running) => Ok(WorkspaceStatus::Running),
                Ok(microsandbox::sandbox::SandboxStatus::Paused) => Ok(WorkspaceStatus::Paused),
                Ok(
                    microsandbox::sandbox::SandboxStatus::Starting
                    | microsandbox::sandbox::SandboxStatus::Draining,
                ) => Ok(WorkspaceStatus::Transitioning),
                Ok(
                    microsandbox::sandbox::SandboxStatus::Created
                    | microsandbox::sandbox::SandboxStatus::Stopped
                    | microsandbox::sandbox::SandboxStatus::Crashed,
                ) => Ok(WorkspaceStatus::Stopped),
                Err(microsandbox::MicrosandboxError::SandboxNotFound(_)) => {
                    Ok(WorkspaceStatus::NotFound)
                }
                Err(e) => {
                    tracing::warn!(workspace = %self.name, error = %e, "unable to read workspace status");
                    Err(map_lifecycle_error(&self.name, e))
                }
            }
        } else {
            Ok(WorkspaceStatus::NotFound)
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    async fn start_managed(&self, launch: &ManagedLaunch) -> Result<ManagedInstance, EngineError> {
        super::managed::start(self, launch).await
    }
    async fn managed_alive(&self, instance: &ManagedInstance) -> Result<bool, EngineError> {
        let Some(sandbox) = self.inner.as_ref() else {
            return Ok(false);
        };
        managed_action(sandbox, "alive", &serde_json::to_value(instance).unwrap()).await
    }
    async fn stop_managed(&self, instance: &ManagedInstance) -> Result<(), EngineError> {
        let Some(sandbox) = self.inner.as_ref() else {
            return Ok(());
        };
        let result = managed_action::<serde_json::Value>(
            sandbox,
            "stop",
            &serde_json::to_value(instance).unwrap(),
        )
        .await;
        match &result {
            Ok(_) => {
                tracing::info!(workspace = %self.name, service = ?instance.service, instance = %instance.instance_id, "stopped managed service and cleaned its records")
            }
            Err(error) => {
                tracing::warn!(workspace = %self.name, service = ?instance.service, instance = %instance.instance_id, %error, "managed service cleanup failed; ownership retained")
            }
        }
        result?;
        Ok(())
    }
    async fn list_managed(
        &self,
        service: ManagedService,
    ) -> Result<Vec<ManagedInstance>, EngineError> {
        let Some(sandbox) = self.inner.as_ref() else {
            return Ok(Vec::new());
        };
        managed_action(sandbox, "list", &serde_json::json!({"service":service})).await
    }
    async fn connect_guest_tcp(&self, port: u16) -> Result<super::GuestTcp, EngineError> {
        let sandbox = self
            .inner
            .as_ref()
            .ok_or_else(|| EngineError::SandboxNotRunning(self.name.clone()))?;
        open_guest_tcp(sandbox.client_arc(), port).await
    }
}

#[async_trait]
impl super::managed::ManagedRuntime for MsbHandle {
    async fn action(
        &self,
        action: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, EngineError> {
        let sandbox = self
            .inner
            .as_ref()
            .ok_or_else(|| EngineError::SandboxNotRunning(self.name.clone()))?;
        managed_action(sandbox, action, payload).await
    }
    async fn copy_binary(&self, source: &str, destination: &str) -> Result<(), EngineError> {
        let sandbox = self
            .inner
            .as_ref()
            .ok_or_else(|| EngineError::SandboxNotRunning(self.name.clone()))?;
        sandbox
            .fs()
            .copy_from_host(source, destination)
            .await
            .map_err(|e| classify_exec_error("copy helper", e))
    }
}

async fn open_guest_tcp(
    client: std::sync::Arc<microsandbox::AgentClient>,
    port: u16,
) -> Result<super::GuestTcp, EngineError> {
    use bytes::Bytes;
    use microsandbox::protocol::bulk::{
        BulkAccepted, BulkCancel, BulkCancelReason, BulkCredit, BulkFinish, BulkFlow, BulkKind,
        BulkOffer, BulkReceiveState, BulkRecord, BulkSendState,
    };
    use microsandbox::protocol::message::MessageType;
    use microsandbox::protocol::tcp::{TcpConnect, TcpFailed};
    use microsandbox_agent_client::AgentFrame;
    use tokio::io::AsyncReadExt;
    use tokio_util::{io::StreamReader, sync::CancellationToken};

    // The guard outlives both pumps. An ordinary terminal disarms cancellation;
    // only abandonment or a local protocol/transport failure sends BulkCancel.
    struct CloseTcp {
        client: std::sync::Arc<microsandbox::AgentClient>,
        id: u32,
        terminal: std::sync::atomic::AtomicBool,
    }
    impl Drop for CloseTcp {
        fn drop(&mut self) {
            if self.terminal.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let (client, id) = (self.client.clone(), self.id);
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        client.cancel_bulk(
                            id,
                            &BulkCancel {
                                kind: BulkKind::Tcp,
                                reason: BulkCancelReason::CallerCancelled,
                                message: "Host guest TCP connection ended".into(),
                            },
                        ),
                    )
                    .await;
                });
            }
        }
    }
    let offer = BulkOffer::tcp();
    let (id, mut frames) = client
        .stream_frames(
            MessageType::TcpConnect,
            &TcpConnect {
                host: "127.0.0.1".into(),
                port,
                bulk: Some(offer),
            },
        )
        .await
        .map_err(|error| classify_exec_error("guest tcp connect", error.into()))?;
    let close = std::sync::Arc::new(CloseTcp {
        client: client.clone(),
        id,
        terminal: std::sync::atomic::AtomicBool::new(false),
    });
    let accepted = tokio::time::timeout(Duration::from_secs(5), async {
        match frames.recv().await {
            Some(AgentFrame::Control(message)) if message.t == MessageType::TcpConnected => {}
            Some(AgentFrame::Control(message)) if message.t == MessageType::TcpFailed => {
                close
                    .terminal
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let failed: TcpFailed = message
                    .payload()
                    .map_err(|error| EngineError::Other(format!("guest TCP failure: {error}")))?;
                return Err(EngineError::Other(format!(
                    "guest TCP did not connect: {}",
                    failed.error
                )));
            }
            None => {
                return Err(EngineError::SandboxNotRunning(
                    "guest TCP transport ended during connect".into(),
                ))
            }
            _ => return Err(EngineError::Other("guest TCP did not connect".into())),
        }
        match frames.recv().await {
            Some(AgentFrame::Control(message)) if message.t == MessageType::BulkAccepted => {
                let accepted: BulkAccepted = message
                    .payload()
                    .map_err(|e| EngineError::Other(format!("guest TCP acceptance: {e}")))?;
                accepted
                    .validate_against(
                        offer,
                        BulkKind::Tcp,
                        BulkFlow::HostToGuest.mask() | BulkFlow::GuestToHost.mask(),
                    )
                    .map_err(|e| EngineError::Other(format!("guest TCP negotiation: {e}")))
            }
            None => Err(EngineError::SandboxNotRunning(
                "guest TCP transport ended during negotiation".into(),
            )),
            Some(AgentFrame::Control(message))
                if matches!(message.t, MessageType::TcpClosed | MessageType::TcpFailed) =>
            {
                close
                    .terminal
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Err(EngineError::Other(
                    "guest TCP closed during negotiation".into(),
                ))
            }
            _ => Err(EngineError::Other(
                "guest TCP missing bulk acceptance".into(),
            )),
        }
    })
    .await
    .map_err(|_| EngineError::Timeout("guest tcp connect timed out".into()))??;
    let sender = std::sync::Arc::new(std::sync::Mutex::new(
        BulkSendState::new(
            BulkKind::Tcp,
            BulkFlow::HostToGuest,
            accepted.max_record_payload,
            accepted.host_to_guest_credit_limit,
        )
        .map_err(|e| EngineError::Other(e.to_string()))?,
    ));
    let receiver = std::sync::Arc::new(std::sync::Mutex::new(
        BulkReceiveState::new(
            BulkKind::Tcp,
            BulkFlow::GuestToHost,
            accepted.max_record_payload,
            accepted.guest_to_host_credit_limit,
            offer.guest_to_host_credit_limit,
        )
        .map_err(|e| EngineError::Other(e.to_string()))?,
    ));
    let credit_ready = std::sync::Arc::new(tokio::sync::Notify::new());
    let cancel = CancellationToken::new();
    let read_finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (output, mut incoming) = tokio::sync::mpsc::channel::<std::io::Result<(Bytes, u64)>>(32);
    let (host_to_guest, mut input) = tokio::io::duplex(64 * 1024);
    let (unused, writer) = tokio::io::split(host_to_guest);
    drop(unused);
    {
        let client = client.clone();
        let sender = sender.clone();
        let ready = credit_ready.clone();
        let cancel = cancel.clone();
        let close = close.clone();
        tokio::spawn(async move {
            let _close = close;
            let transfer = async {
                let mut buffer = vec![0u8; 16 * 1024];
                loop {
                    let n = input.read(&mut buffer).await?;
                    if cancel.is_cancelled()
                        || _close.terminal.load(std::sync::atomic::Ordering::SeqCst)
                    {
                        return Ok(());
                    }
                    if n == 0 {
                        let finish = sender
                            .lock()
                            .unwrap()
                            .finish()
                            .map_err(std::io::Error::other)?;
                        client
                            .send(id, MessageType::BulkFinish, &finish)
                            .await
                            .map_err(std::io::Error::other)?;
                        return Ok::<_, std::io::Error>(());
                    }
                    let mut sent = 0;
                    while sent < n {
                        if cancel.is_cancelled()
                            || _close.terminal.load(std::sync::atomic::Ordering::SeqCst)
                        {
                            return Ok(());
                        }
                        let allowed = {
                            let mut sender = sender.lock().unwrap();
                            let count = (n - sent)
                                .min(accepted.max_record_payload as usize)
                                .min(sender.available_credit() as usize);
                            if count == 0 {
                                None
                            } else {
                                let offset = sender.admit(count).map_err(std::io::Error::other)?;
                                Some((count, offset))
                            }
                        };
                        let Some((count, offset)) = allowed else {
                            ready.notified().await;
                            continue;
                        };
                        client
                            .send_bulk(BulkRecord {
                                id,
                                kind: BulkKind::Tcp,
                                flow: BulkFlow::HostToGuest,
                                offset,
                                payload: Bytes::copy_from_slice(&buffer[sent..sent + count]),
                            })
                            .await
                            .map_err(std::io::Error::other)?;
                        sent += count;
                    }
                }
            };
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {},
                result = transfer => if let Err(error) = result {
                    tracing::debug!(%error, "guest TCP write ended");
                    cancel.cancel();
                },
            }
            // Write EOF is a TCP half-close: the peer can still send its response.
        });
    }
    {
        let sender = sender.clone();
        let receiver = receiver.clone();
        let read_finished = read_finished.clone();
        let cancel = cancel.clone();
        let close = close.clone();
        tokio::spawn(async move {
            let mut output = Some(output);
            let transfer = async {
                while let Some(frame) = frames.recv().await {
                    match frame {
                        AgentFrame::Bulk(record) => {
                            let end = receiver
                                .lock()
                                .unwrap()
                                .accept_record(&record)
                                .map_err(std::io::Error::other)?;
                            let channel = output
                                .as_ref()
                                .ok_or_else(|| std::io::Error::other("guest TCP data after EOF"))?;
                            channel
                                .send(Ok((record.payload, end)))
                                .await
                                .map_err(std::io::Error::other)?;
                        }
                        AgentFrame::Control(message) => match message.t {
                            MessageType::BulkCredit => {
                                let credit: BulkCredit =
                                    message.payload().map_err(std::io::Error::other)?;
                                sender
                                    .lock()
                                    .unwrap()
                                    .apply_credit(credit)
                                    .map_err(std::io::Error::other)?;
                                credit_ready.notify_one();
                            }
                            MessageType::BulkFinish => {
                                let finish: BulkFinish =
                                    message.payload().map_err(std::io::Error::other)?;
                                receiver
                                    .lock()
                                    .unwrap()
                                    .accept_finish(finish)
                                    .map_err(std::io::Error::other)?;
                                read_finished.store(true, std::sync::atomic::Ordering::SeqCst);
                                output.take(); // Deliver queued bytes, then the verified read EOF.
                            }
                            MessageType::TcpClosed if output.is_none() => {
                                close
                                    .terminal
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                                return Ok(());
                            }
                            MessageType::TcpClosed
                            | MessageType::TcpFailed
                            | MessageType::BulkCancel => {
                                if matches!(
                                    message.t,
                                    MessageType::TcpClosed | MessageType::TcpFailed
                                ) {
                                    close
                                        .terminal
                                        .store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                                return Err(std::io::Error::other(
                                    "guest TCP closed before verified EOF",
                                ));
                            }
                            _ => return Err(std::io::Error::other("unexpected guest TCP frame")),
                        },
                    }
                }
                Err::<(), _>(std::io::Error::other("guest TCP transport ended"))
            };
            tokio::select! {
                _ = cancel.cancelled() => {},
                result = transfer => if let Err(error) = result {
                    tracing::debug!(%error, "guest TCP read ended");
                    if let Some(output) = output {
                        tokio::select! { _ = cancel.cancelled() => {}, _ = output.send(Err(error)) => {} }
                    }
                },
            }
            cancel.cancel();
        });
    }
    // Credit follows consumption by the application, not arrival in the VM
    // transport. Both queues and the negotiated window remain bounded.
    let lifetime = cancel.drop_guard();
    let reader_stream = async_stream::try_stream! {
        let _lifetime = lifetime;
        while let Some(chunk) = incoming.recv().await {
            let (bytes, end) = chunk?;
            let credit_result = { let mut state = receiver.lock().unwrap(); state.consume(end) };
            let credit = credit_result.map_err(std::io::Error::other)?;
            if let Some(credit) = credit.filter(|_| !read_finished.load(std::sync::atomic::Ordering::SeqCst)
                && !close.terminal.load(std::sync::atomic::Ordering::SeqCst)) {
                client.send(id, MessageType::BulkCredit, &credit).await.map_err(std::io::Error::other)?;
            }
            yield bytes;
        }
        if !read_finished.load(std::sync::atomic::Ordering::SeqCst) {
            Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "guest TCP ended without a verified bulk finish"))?;
        }
    };
    Ok(super::GuestTcp {
        reader: Box::new(StreamReader::new(Box::pin(reader_stream)
            as std::pin::Pin<
                Box<dyn futures::Stream<Item = std::io::Result<Bytes>> + Send>,
            >)),
        writer: Box::new(writer),
    })
}

fn map_lifecycle_error(name: &str, error: microsandbox::MicrosandboxError) -> EngineError {
    match error {
        microsandbox::MicrosandboxError::SandboxNotFound(_) => EngineError::NotFound(name.into()),
        microsandbox::MicrosandboxError::SandboxAlreadyExists(_) => {
            EngineError::AlreadyExists(name.into())
        }
        microsandbox::MicrosandboxError::SandboxNotRunning(_) => {
            EngineError::SandboxNotRunning(name.into())
        }
        e => EngineError::Other(format!("sandbox '{}': {}", name, e)),
    }
}

fn classify_exec_error(sandbox_name: &str, error: microsandbox::MicrosandboxError) -> EngineError {
    use microsandbox::{AgentClientError as A, MicrosandboxError as M};
    if let M::ExecTimeout(duration) = error {
        return EngineError::Timeout(format!("exec timed out after {:?}", duration));
    }
    let unavailable = match &error {
        M::SandboxNotRunning(_) | M::SandboxNotFound(_) => true,
        M::AgentClient(e) => matches!(
            e,
            A::Connect { .. }
                | A::Handshake(_)
                | A::Io(_)
                | A::ReaderClosed(_)
                | A::Closed
                | A::SandboxNotFound(_)
        ),
        M::Io(e) => matches!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::UnexpectedEof
        ),
        M::Runtime(msg) => {
            msg == "exec session ended without exit event"
                || msg.contains("no agent socket")
                || msg.contains("has no agent endpoint")
        }
        _ => false,
    };
    if unavailable {
        tracing::warn!(workspace = %sandbox_name, error = %error, "workspace connection unavailable");
        EngineError::SandboxNotRunning(format!("{}: {}", sandbox_name, error))
    } else {
        EngineError::ExecError(error.to_string())
    }
}

// The SDK's process shutdown and the asynchronous dropping of its disk guards
// can be separated by a scheduler turn. Retry only the explicit busy-lock error;
// never unlink a locked volume or suppress an unrelated deletion failure.
async fn remove_managed_volume(name: &str) -> Result<(), EngineError> {
    for attempt in 0..40 {
        match microsandbox::Volume::remove(name).await {
            Ok(()) | Err(microsandbox::MicrosandboxError::VolumeNotFound(_)) => return Ok(()),
            Err(microsandbox::MicrosandboxError::InvalidConfig(msg))
                if msg.contains("currently attached by a running sandbox") && attempt < 39 =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => {
                return Err(EngineError::Other(format!(
                    "remove volume '{}': {}",
                    name, e
                )))
            }
        }
    }
    unreachable!()
}

async fn managed_action<T: serde::de::DeserializeOwned>(
    sandbox: &Sandbox,
    action: &str,
    payload: &serde_json::Value,
) -> Result<T, EngineError> {
    let payload = serde_json::to_string(payload).unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        sandbox.exec(
            "python3",
            [
                "-c",
                include_str!("../../guest/managed.py"),
                action,
                &payload,
            ],
        ),
    )
    .await
    .map_err(|_| {
        EngineError::Timeout(format!(
            "managed {action} timed out; guest ownership retained"
        ))
    })?
    .map_err(|e| classify_exec_error(&format!("managed {action}"), e))?;
    if output.status().code != 0 {
        return Err(EngineError::Other(format!(
            "managed {action}: {}",
            output.stderr().unwrap_or_default()
        )));
    }
    serde_json::from_str(&output.stdout().unwrap_or_default())
        .map_err(|e| EngineError::Other(format!("managed {action} returned invalid identity: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_agent_connection_requests_reconnect() {
        for error in [
            microsandbox::AgentClientError::Closed,
            microsandbox::AgentClientError::ReaderClosed(1),
        ] {
            assert!(matches!(
                classify_exec_error("test", error.into()),
                EngineError::SandboxNotRunning(_)
            ));
        }
    }

    #[test]
    fn missing_user_file_does_not_classify_the_vm_as_dead() {
        let error = microsandbox::MicrosandboxError::Runtime("no such file or directory".into());
        assert!(matches!(
            classify_exec_error("test", error),
            EngineError::ExecError(_)
        ));
    }

    #[test]
    fn lost_exit_event_requests_reconnect_without_replay() {
        let error = microsandbox::MicrosandboxError::Runtime(
            "exec session ended without exit event".into(),
        );
        assert!(matches!(
            classify_exec_error("test", error),
            EngineError::SandboxNotRunning(_)
        ));
    }
}
