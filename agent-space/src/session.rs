use crate::config::LifecycleConfig;
use crate::engine::{
    AgentEngine, AgentHandle, EngineError, ExecResult, ManagedInstance, ManagedLaunch,
    ManagedService, StartupProgress, VolumeKind, WorkspaceOpts, WorkspaceStatus,
};
pub use crate::publication::{Publication, PublishTarget};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Mutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::{error, info, warn};

/// One stable runtime owner per workspace, shared by all MCP connections.
pub struct SessionManager {
    engine: Arc<dyn AgentEngine>,
    sessions: Mutex<HashMap<String, Session>>,
    slots: Arc<Semaphore>,
    max_sessions: usize,
    default_opts: WorkspaceOpts,
    images: StdMutex<HashMap<String, String>>,
    desktops: Mutex<HashMap<String, Arc<crate::desktop::DesktopHub>>>,
    max_output_lines: usize,
    max_output_bytes: usize,
    shutting_down: Arc<std::sync::atomic::AtomicBool>,
}

struct Runtime {
    handle: Option<Arc<dyn AgentHandle>>,
    slot: Option<OwnedSemaphorePermit>,
    retired: bool,
    reconnect: bool,
    reclaim_adopted_static: bool,
    prepare_tmpfs: bool,
}

struct Activity {
    last_request: Instant,
    last_work: Instant,
    pending: usize,
}

struct Workspace {
    name: String,
    publications: StdMutex<HashMap<String, Publication>>,
    publish_gate: Mutex<()>,
    helper_cleanup: StdMutex<HashSet<ManagedInstance>>,
    dav: StdMutex<Option<Arc<DavRun>>>,
    dav_gate: Mutex<()>,
    control: StdMutex<Option<Arc<crate::control::ControlGate>>>,
    desktop_port: std::sync::atomic::AtomicU16,
    runtime: Mutex<Runtime>,
    activity: StdMutex<Activity>,
    engine: Arc<dyn AgentEngine>,
    opts: WorkspaceOpts,
    slots: Arc<Semaphore>,
    shutting_down: Arc<std::sync::atomic::AtomicBool>,
    cancel: std::sync::atomic::AtomicBool,
    cancel_notify: Notify,
}

// Owned by the queued request, not the HTTP waiter. Cancellation cannot make
// an executing command appear idle. Dropping a cancelled send releases it too.
struct ActivityLease(Arc<Workspace>);
impl ActivityLease {
    fn acquire(workspace: &Arc<Workspace>) -> Self {
        let mut activity = workspace.activity.lock().unwrap();
        activity.pending += 1;
        activity.last_request = Instant::now();
        Self(workspace.clone())
    }
}
impl Drop for ActivityLease {
    fn drop(&mut self) {
        let mut activity = self.0.activity.lock().unwrap();
        activity.pending -= 1;
        activity.last_work = Instant::now();
        activity.last_request = activity.last_work;
    }
}
pub struct RequestLease {
    _lease: ActivityLease,
    dav: Option<Arc<DavRun>>,
}
impl Drop for RequestLease {
    fn drop(&mut self) {
        if let Some(dav) = &self.dav {
            dav.active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
struct DavRun {
    instance: ManagedInstance,
    launch: ManagedLaunch,
    cancel: CancellationToken,
    active: std::sync::atomic::AtomicUsize,
}
pub struct DavRequest {
    pub lease: RequestLease,
    pub port: u16,
    pub cancel: CancellationToken,
}

struct ExecRequest {
    cmd: Option<String>, // None = explicit activation, no user command
    timeout: Duration,
    file_deadline: Option<tokio::time::Instant>,
    file_phase: Option<Arc<std::sync::atomic::AtomicU8>>,
    cancel: CancellationToken,
    max_lines: usize,
    max_bytes: usize,
    admission: Option<AdmissionCheck>,
    resp: oneshot::Sender<Result<ExecResult, EngineError>>,
    _lease: ActivityLease,
}

const LIFECYCLE_IO_TIMEOUT: Duration = Duration::from_secs(10);
type AdmissionCheck = Arc<dyn Fn() -> Result<(), EngineError> + Send + Sync>;

impl Workspace {
    async fn prepare_runtime(&self, runtime: &mut Runtime) -> Result<(), EngineError> {
        if runtime.prepare_tmpfs {
            if let Some(handle) = &runtime.handle {
                for volume in &self.opts.volumes {
                    if volume.kind == VolumeKind::Tmpfs {
                        handle
                            .grow_tmpfs(&volume.guest_path, volume.size_mib)
                            .await?;
                    }
                }
                runtime.prepare_tmpfs = false;
            }
        }
        if runtime.reclaim_adopted_static {
            if let Some(handle) = &runtime.handle {
                // A previous Host's publications are gone. Complete this once,
                // before this Host can install any newly owned publication.
                handle.reclaim_managed(ManagedService::StaticHttp).await?;
                runtime.reclaim_adopted_static = false;
            }
        }
        Ok(())
    }

    fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        self.close_publications();
        self.cancel_dav();
        self.cancel_notify.notify_waiters();
    }

    fn close_publications(&self) {
        for (_, item) in self.publications.lock().unwrap().drain() {
            item.epoch.cancel();
            if let Some(helper) = item.helper {
                self.helper_cleanup.lock().unwrap().insert(helper);
            }
        }
    }

    fn publication_keeps_alive(&self) -> bool {
        self.publications
            .lock()
            .unwrap()
            .values()
            .any(|item| !item.epoch.is_cancelled() && item.keep_alive_deadline > Instant::now())
    }

    fn cancel_dav(&self) {
        if let Some(dav) = self.dav.lock().unwrap().take() {
            dav.cancel.cancel();
            self.helper_cleanup
                .lock()
                .unwrap()
                .insert(dav.instance.clone());
        }
    }

    fn clear_terminal_guest_state(&self) {
        self.close_publications();
        self.cancel_dav();
        self.helper_cleanup.lock().unwrap().clear();
        self.desktop_port
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    async fn ensure_running(&self, runtime: &mut Runtime) -> Result<(), EngineError> {
        self.ensure_running_with_progress(runtime, &StartupProgress::default())
            .await
    }

    async fn ensure_running_with_progress(
        &self,
        runtime: &mut Runtime,
        progress: &StartupProgress,
    ) -> Result<(), EngineError> {
        progress.set_phase("runtime_status");
        if self.cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(EngineError::Other("workspace is closing".into()));
        }
        if self.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(EngineError::Other("server is shutting down".into()));
        }
        if runtime.retired {
            return Err(EngineError::NotFound(format!(
                "workspace '{}' session retired",
                self.name
            )));
        }
        if runtime.handle.is_some() {
            let status = runtime.handle.as_ref().unwrap().try_status().await?;
            if status == WorkspaceStatus::Unknown {
                return Err(EngineError::Other(format!(
                    "workspace '{}' status unavailable",
                    self.name
                )));
            }
            if status == WorkspaceStatus::Running && !runtime.reconnect {
                progress.set_phase("prepare_guest");
                return self.prepare_runtime(runtime).await;
            }
            // A reconnect uses the same activation/failure reconciliation
            // below; a concurrent VM exit must release terminal capacity.
            // Paused VMs retain RAM and still consume active capacity. Only a
            // terminal VM releases its slot before activation.
            match status {
                WorkspaceStatus::Stopped => {
                    self.clear_terminal_guest_state();
                    runtime.slot.take();
                }
                WorkspaceStatus::NotFound => {
                    self.clear_terminal_guest_state();
                    runtime.slot.take();
                    runtime.handle = None;
                }
                _ => {}
            }
        }
        if runtime.slot.is_none() {
            runtime.slot = Some(
                self.slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| EngineError::WorkspaceCapacityExhausted)?,
            );
        }
        // Keep the reservation across backend I/O, including partial failures.
        // Never delete an existing root disk to recover an exec connection.
        let mut result = if runtime.handle.is_some() {
            self.engine
                .start_sandbox_with_progress(&self.name, progress)
                .await
        } else {
            progress.set_phase("lookup_vm");
            match self.engine.sandbox_status(&self.name).await {
                Ok(status) => match status {
                    WorkspaceStatus::Running
                    | WorkspaceStatus::Stopped
                    | WorkspaceStatus::Paused
                    | WorkspaceStatus::Transitioning => {
                        self.engine
                            .start_sandbox_with_progress(&self.name, progress)
                            .await
                    }
                    WorkspaceStatus::NotFound => {
                        progress.set_phase("vm_create");
                        match self.engine.create_sandbox(&self.name, &self.opts).await {
                            Err(EngineError::AlreadyExists(_)) => {
                                self.engine
                                    .start_sandbox_with_progress(&self.name, progress)
                                    .await
                            }
                            result => result,
                        }
                    }
                    WorkspaceStatus::Unknown => Err(EngineError::Other(format!(
                        "workspace '{}' status unavailable",
                        self.name
                    ))),
                },
                Err(e) => Err(e),
            }
        };
        // The sandbox may be deleted after the cached handle status check but
        // before start. Activation is still safe to retry because no user
        // command has been submitted yet.
        if matches!(result, Err(EngineError::NotFound(_))) && runtime.handle.is_some() {
            runtime.handle = None;
            result = match self.engine.create_sandbox(&self.name, &self.opts).await {
                Err(EngineError::AlreadyExists(_)) => {
                    self.engine
                        .start_sandbox_with_progress(&self.name, progress)
                        .await
                }
                result => result,
            };
        }
        let handle = match result {
            Ok(handle) => handle,
            Err(error) => {
                progress.failed(&error);
                progress.set_phase("runtime_reconcile");
                // A failed activation must not spend its cleanup budget trying
                // to connect to the very guest that failed. Metadata determines
                // whether capacity is still resident; unknown outcomes keep it.
                match tokio::time::timeout(
                    LIFECYCLE_IO_TIMEOUT,
                    self.engine.sandbox_status(&self.name),
                )
                .await
                {
                    Ok(Ok(WorkspaceStatus::Stopped | WorkspaceStatus::NotFound)) => {
                        runtime.handle = None;
                        runtime.slot.take();
                        self.clear_terminal_guest_state();
                    }
                    Ok(Ok(WorkspaceStatus::Running)) => {
                        runtime.reconnect = true;
                    }
                    _ => {} // Paused, transitioning or unknown: retain capacity.
                }
                return Err(error);
            }
        };
        // Retain ownership even if the readiness check fails, so later stop or
        // retry can safely handle resources already created by the backend.
        runtime.handle = Some(handle.clone());
        runtime.prepare_tmpfs = true;
        runtime.reconnect = false;
        match handle.try_status().await? {
            WorkspaceStatus::Running => {}
            WorkspaceStatus::Stopped | WorkspaceStatus::NotFound => {
                // The VM can exit after start returned but before readiness.
                // Confirmed terminal state must release its admission now.
                runtime.handle = None;
                runtime.slot.take();
                self.clear_terminal_guest_state();
                return Err(EngineError::SandboxNotRunning(self.name.clone()));
            }
            WorkspaceStatus::Unknown => {
                return Err(EngineError::Other(format!(
                    "workspace '{}' status unavailable",
                    self.name
                )));
            }
            _ => return Err(EngineError::SandboxNotRunning(self.name.clone())),
        }
        progress.set_phase("prepare_guest");
        self.prepare_runtime(runtime).await?;
        info!(workspace = %self.name, "workspace activated");
        Ok(())
    }

    async fn stop_locked(&self, runtime: &mut Runtime) -> Result<(), EngineError> {
        self.cancel_dav();
        self.desktop_port
            .store(0, std::sync::atomic::Ordering::SeqCst);
        self.close_publications();
        if runtime.handle.is_none() && runtime.slot.is_some() {
            match self.engine.get_sandbox(&self.name).await {
                Ok(handle) => runtime.handle = Some(handle),
                Err(EngineError::SandboxPaused(_)) => {
                    runtime.handle = Some(self.engine.start_sandbox(&self.name).await?);
                }
                Err(EngineError::SandboxTransitioning(_)) => {
                    runtime.handle = Some(self.engine.start_sandbox(&self.name).await?);
                }
                Err(EngineError::NotFound(_) | EngineError::SandboxNotRunning(_)) => {}
                Err(e) => return Err(e),
            }
        }
        if let Some(handle) = &runtime.handle {
            let status = handle.try_status().await?;
            if status == WorkspaceStatus::Unknown {
                return Err(EngineError::Other(format!(
                    "workspace '{}' status unavailable",
                    self.name
                )));
            }
            if matches!(
                status,
                WorkspaceStatus::Running | WorkspaceStatus::Paused | WorkspaceStatus::Transitioning
            ) {
                handle.stop().await?;
                info!(workspace = %self.name, "workspace stopped; disks retained");
            }
        }
        runtime.slot.take();
        runtime.reclaim_adopted_static = false;
        // A stopped VM cannot retain any guest helper process. Keep pending
        // PIDs only when stop fails, so a later retry still knows about them.
        self.helper_cleanup.lock().unwrap().clear();
        Ok(())
    }

    async fn stop_for_deletion(&self, runtime: &mut Runtime) -> Result<(), EngineError> {
        let stop_error =
            match tokio::time::timeout(LIFECYCLE_IO_TIMEOUT, self.stop_locked(runtime)).await {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(error)) => error,
                Err(_) => EngineError::Timeout("workspace stop did not finish".into()),
            };
        let Some(handle) = runtime.handle.as_ref() else {
            return Err(stop_error);
        };
        match tokio::time::timeout(LIFECYCLE_IO_TIMEOUT, handle.kill())
            .await
            .map_err(|_| EngineError::Timeout("workspace kill did not finish".into()))?
        {
            Ok(()) | Err(EngineError::NotFound(_) | EngineError::SandboxNotRunning(_)) => {}
            Err(error) => return Err(error),
        }
        runtime.slot.take();
        runtime.reclaim_adopted_static = false;
        self.helper_cleanup.lock().unwrap().clear();
        Ok(())
    }

    async fn run(self: Arc<Self>, mut rx: mpsc::Receiver<ExecRequest>) {
        while let Some(req) = rx.recv().await {
            self.execute(req).await;
        }
    }

    async fn run_files(self: Arc<Self>, mut rx: mpsc::Receiver<ExecRequest>) {
        let mut running = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                Some(_) = running.join_next(), if !running.is_empty() => {},
                request = rx.recv(), if running.len() < 4 => {
                    let Some(request) = request else { break };
                    let workspace = self.clone();
                    running.spawn(async move { workspace.execute(request).await; });
                }
            }
        }
        while running.join_next().await.is_some() {}
    }

    async fn execute(self: &Arc<Self>, req: ExecRequest) {
        if req.resp.is_closed() {
            return;
        }
        if self.cancel.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = req
                .resp
                .send(Err(EngineError::Other("workspace is closing".into())));
            return;
        }
        if let Some(Err(error)) = req.admission.as_ref().map(|check| check()) {
            let _ = req.resp.send(Err(error));
            return;
        }
        if req.cancel.is_cancelled() {
            let _ = req.resp.send(Err(EngineError::Other(
                "EXECUTION_CANCELLED: command not started".into(),
            )));
            return;
        }
        let mut runtime = self.runtime.lock().await;
        let result = match self.ensure_running(&mut runtime).await {
            Ok(()) => {
                if let Some(Err(error)) = req.admission.as_ref().map(|check| check()) {
                    let _ = req.resp.send(Err(error));
                    return;
                }
                match &req.cmd {
                    Some(cmd) => {
                        if req.resp.is_closed() {
                            return;
                        }
                        if self.shutting_down.load(std::sync::atomic::Ordering::SeqCst)
                            || self.cancel.load(std::sync::atomic::Ordering::SeqCst)
                        {
                            let _ = req
                                .resp
                                .send(Err(EngineError::Other("workspace is closing".into())));
                            return;
                        }
                        if req.cancel.is_cancelled() {
                            let _ = req.resp.send(Err(EngineError::Other(
                                "EXECUTION_CANCELLED: command not started".into(),
                            )));
                            return;
                        }
                        let timeout = if let Some(deadline) = req.file_deadline {
                            let remaining =
                                deadline.saturating_duration_since(tokio::time::Instant::now());
                            if remaining.is_zero()
                                || req
                                    .file_phase
                                    .as_ref()
                                    .unwrap()
                                    .compare_exchange(
                                        0,
                                        1,
                                        std::sync::atomic::Ordering::SeqCst,
                                        std::sync::atomic::Ordering::SeqCst,
                                    )
                                    .is_err()
                            {
                                let _ = req.resp.send(Err(EngineError::Timeout(
                                    "WAIT_TIMEOUT: file operation not started".into(),
                                )));
                                return;
                            }
                            remaining
                        } else {
                            req.timeout
                        };
                        let handle = runtime.handle.clone().unwrap();
                        let command = cmd.clone();
                        drop(runtime);
                        let args = ["-lc".to_string(), command];
                        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
                        let exec = handle.exec_bounded(
                            "bash",
                            &arg_refs,
                            Some(timeout),
                            req.max_lines,
                            req.max_bytes,
                            req.cancel.clone(),
                        );
                        tokio::pin!(exec);
                        let result = tokio::select! {
                            result = &mut exec => result,
                            _ = async {
                                loop {
                                    let notified = self.cancel_notify.notified();
                                    if self.cancel.load(std::sync::atomic::Ordering::SeqCst) { break; }
                                    notified.await;
                                }
                            } => {
                                req.cancel.cancel();
                                tokio::time::timeout(Duration::from_secs(8), &mut exec).await
                                    .unwrap_or_else(|_| Err(EngineError::Timeout("execution cancellation cleanup unconfirmed".into())))
                            },
                        };
                        runtime = self.runtime.lock().await;
                        // Never replay a submitted command: a disconnected exec
                        // can already have committed side effects.
                        if let Err(ref e) = result {
                            if matches!(e, EngineError::SandboxNotRunning(_))
                                && runtime
                                    .handle
                                    .as_ref()
                                    .is_some_and(|current| Arc::ptr_eq(current, &handle))
                            {
                                runtime.reconnect = true;
                            }
                            warn!(workspace = %self.name, error = %e, "exec failed; command not replayed");
                        }
                        let mut activity = self.activity.lock().unwrap();
                        activity.last_request = Instant::now();
                        activity.last_work = activity.last_request;
                        result
                    }
                    None => Ok(ExecResult {
                        stdout: String::new(),
                        stderr: String::new(),
                        exit_code: 0,
                        truncated: false,
                        output_path: None,
                    }),
                }
            }
            Err(e) => Err(e),
        };
        let _ = req.resp.send(result);
    }
}

impl SessionManager {
    /// Admin observations bypass session creation, activation and activity accounting.
    pub(crate) fn resource_backend(&self) -> (&dyn AgentEngine, &WorkspaceOpts) {
        (self.engine.as_ref(), &self.default_opts)
    }
    pub fn new(
        engine: Arc<dyn AgentEngine>,
        max_sessions: u32,
        default_opts: WorkspaceOpts,
    ) -> Self {
        Self {
            engine,
            sessions: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(max_sessions as usize)),
            max_sessions: max_sessions as usize,
            default_opts,
            images: StdMutex::new(HashMap::new()),
            desktops: Mutex::new(HashMap::new()),
            max_output_lines: 2000,
            max_output_bytes: 51200,
            shutting_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn with_limits(mut self, max_lines: usize, max_bytes: usize) -> Self {
        self.max_output_lines = max_lines;
        self.max_output_bytes = max_bytes;
        self
    }

    /// Immutable projection of StateStore, installed before any activation.
    pub fn register_image(&self, name: &str, reference: &str) -> Result<(), EngineError> {
        let mut images = self.images.lock().unwrap();
        if images.get(name).is_some_and(|value| value != reference) {
            return Err(EngineError::Other("space image is immutable".into()));
        }
        images.insert(name.into(), reference.into());
        Ok(())
    }

    /// Capacity available for a new resident VM. Intended for startup work
    /// that must not wake or displace an adopted user workspace.
    pub fn available_capacity(&self) -> usize {
        self.slots.available_permits()
    }

    /// Probe the same in-process SDK backend used by requests and lifecycle
    /// checks, so an open TCP port cannot hide a failed metadata store.
    pub async fn check_backend(&self) -> Result<(), EngineError> {
        self.engine.list_workspaces().await.map(|_| ())
    }

    /// Bind a protocol session without starting a VM or consuming capacity.
    pub async fn find_published(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<(Session, Publication), EngineError> {
        let sessions = self.sessions.lock().await;
        for session in sessions.values() {
            if let Some(item) = session
                .publications()
                .into_iter()
                .find(|item| item.username == owner && item.name == name)
            {
                return Ok((session.clone(), item));
            }
        }
        Err(EngineError::NotFound(name.into()))
    }

    pub async fn get_session(&self, workspace_name: &str) -> Result<Session, EngineError> {
        let mut sessions = self.sessions.lock().await;
        if self.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(EngineError::Other("server is shutting down".into()));
        }
        if let Some(session) = sessions.get(workspace_name) {
            return Ok(session.clone());
        }
        let now = Instant::now();
        let workspace = Arc::new(Workspace {
            name: workspace_name.into(),
            publications: StdMutex::new(HashMap::new()),
            publish_gate: Mutex::new(()),
            helper_cleanup: StdMutex::new(HashSet::new()),
            dav: StdMutex::new(None),
            dav_gate: Mutex::new(()),
            control: StdMutex::new(None),
            desktop_port: std::sync::atomic::AtomicU16::new(0),
            runtime: Mutex::new(Runtime {
                handle: None,
                slot: None,
                retired: false,
                reconnect: false,
                reclaim_adopted_static: false,
                prepare_tmpfs: true,
            }),
            activity: StdMutex::new(Activity {
                last_request: now,
                last_work: now,
                pending: 0,
            }),
            engine: self.engine.clone(),
            opts: {
                let mut opts = self.default_opts.clone();
                if let Some(image) = self.images.lock().unwrap().get(workspace_name) {
                    opts.image = image.clone();
                }
                opts
            },
            slots: self.slots.clone(),
            shutting_down: self.shutting_down.clone(),
            cancel: std::sync::atomic::AtomicBool::new(false),
            cancel_notify: Notify::new(),
        });
        let (sender, rx) = mpsc::channel(100);
        tokio::spawn(workspace.clone().run(rx));
        let (file_sender, files) = mpsc::channel(100);
        tokio::spawn(workspace.clone().run_files(files));
        let session = Session {
            workspace_name: workspace_name.into(),
            workspace,
            sender,
            file_sender,
            max_output_lines: self.max_output_lines,
            max_output_bytes: self.max_output_bytes,
            admission: None,
        };
        sessions.insert(workspace_name.into(), session.clone());
        Ok(session)
    }

    /// Register detached VMs left by a prior host process before serving users.
    /// Sleeping workspaces remain lazy; running ones consume capacity immediately.
    pub async fn adopt_running(&self, valid_names: &HashSet<String>) -> Result<(), EngineError> {
        for name in self.engine.list_running().await? {
            if !valid_names.contains(&name) {
                continue;
            }
            let session = self.get_session(&name).await?;
            let mut runtime = session.workspace.runtime.lock().await;
            if runtime.handle.is_some() {
                continue;
            }
            let slot = self.slots.clone().try_acquire_owned().map_err(|_| {
                EngineError::ResourceExhausted(
                    "existing running workspaces exceed max_sessions".into(),
                )
            })?;
            match self.engine.get_sandbox(&name).await {
                Ok(handle) => runtime.handle = Some(handle),
                Err(EngineError::SandboxPaused(_)) => {
                    // Keep the capacity lease. The first tool call will resume
                    // and attach; startup metadata must not wake the VM.
                }
                Err(EngineError::SandboxTransitioning(_)) => {
                    // Preserve capacity without changing an in-flight
                    // transition. The first tool call will converge it.
                }
                Err(EngineError::SandboxNotRunning(_) | EngineError::NotFound(_)) => {
                    // A numeric PID may have been recycled after Host restart.
                    // Authoritative reconciliation proves this VM is terminal.
                    continue;
                }
                Err(error) => return Err(error),
            }
            runtime.slot = Some(slot);
            runtime.reclaim_adopted_static = true;
            info!(workspace = %name, "adopted running workspace");
            if let Err(error) = session.workspace.prepare_runtime(&mut runtime).await {
                warn!(workspace = %name, error = %error, "runtime preparation failed; retry before admission");
            }
        }
        Ok(())
    }

    pub async fn desktop(
        &self,
        name: &str,
    ) -> Result<Arc<crate::desktop::DesktopHub>, EngineError> {
        let session = self.get_session(name).await?;
        // Keep cache selection in the same transaction as runtime-owner removal.
        // A call that captured a revoked owner cannot replace a restored hub.
        let sessions = self.sessions.lock().await;
        if !sessions
            .get(name)
            .is_some_and(|current| Arc::ptr_eq(&current.workspace, &session.workspace))
        {
            return Err(EngineError::NotFound("workspace session changed".into()));
        }
        let mut desktops = self.desktops.lock().await;
        let hub = desktops
            .entry(name.to_owned())
            .or_insert_with(|| Arc::new(crate::desktop::DesktopHub::new(session.clone())));
        if !Arc::ptr_eq(&hub.session.workspace, &session.workspace) {
            *hub = Arc::new(crate::desktop::DesktopHub::new(session));
        }
        Ok(hub.clone())
    }
    pub async fn get_or_create(&self, workspace_name: &str) -> Result<Session, EngineError> {
        let session = self.get_session(workspace_name).await?;
        session.submit(None, Duration::ZERO).await?;
        Ok(session)
    }

    async fn find_session(&self, name: &str) -> Result<Session, EngineError> {
        self.sessions
            .lock()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| {
                EngineError::NotFound(format!("session for workspace '{}' not found", name))
            })
    }

    pub async fn find_existing_session(&self, name: &str) -> Option<Session> {
        self.sessions.lock().await.get(name).cloned()
    }

    /// Fence queued execution and close all publications immediately after the
    /// account state has been durably disabled or marked deleting.
    pub async fn cancel_workspace(&self, workspace_name: &str) {
        self.cancel_workspace_if(workspace_name, || true).await;
    }

    /// Capture a particular owner before rechecking a reversible account state.
    /// No await may separate the check from cancelling this captured owner.
    pub(crate) async fn cancel_workspace_if(
        &self,
        workspace_name: &str,
        should_cancel: impl FnOnce() -> bool,
    ) {
        if let Some(session) = self.find_existing_session(workspace_name).await {
            if should_cancel() {
                session.workspace.cancel();
            }
        }
    }

    pub async fn remove(&self, workspace_name: &str) -> Result<(), EngineError> {
        let session = self.find_session(workspace_name).await?;
        session.workspace.cancel();
        let _publication_gate = tokio::time::timeout(
            Duration::from_secs(5),
            session.workspace.publish_gate.lock(),
        )
        .await
        .map_err(|_| EngineError::Timeout("publication change did not finish".into()))?;
        let mut runtime =
            tokio::time::timeout(Duration::from_secs(5), session.workspace.runtime.lock())
                .await
                .map_err(|_| EngineError::Timeout("workspace activation did not finish".into()))?;
        // Retire before awaiting I/O. Old Session clones can never resurrect a
        // removed user, including when removal fails and has to be retried.
        runtime.retired = true;
        session.workspace.stop_locked(&mut runtime).await?;
        // A lazily registered session or a failed activation can still have
        // persisted backend resources without an attached handle.
        match self.engine.remove_sandbox(workspace_name).await {
            Ok(()) | Err(EngineError::NotFound(_)) => {
                runtime.handle = None;
            }
            Err(e) => return Err(e),
        }
        for vol in &self.default_opts.volumes {
            if vol.kind == VolumeKind::Disk {
                let scoped_name = format!(
                    "{}-{}",
                    workspace_name,
                    vol.name.as_deref().unwrap_or("vol")
                );
                self.engine.remove_volume(&scoped_name).await?;
            }
        }
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(workspace_name)
            .is_some_and(|s| Arc::ptr_eq(&s.workspace, &session.workspace))
        {
            sessions.remove(workspace_name);
        }
        drop(sessions);
        self.desktops.lock().await.remove(workspace_name);
        self.images.lock().unwrap().remove(workspace_name);
        info!(workspace = %workspace_name, "removed session");
        Ok(())
    }

    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .lock()
            .await
            .values()
            .map(|s| {
                let a = s.workspace.activity.lock().unwrap();
                SessionInfo {
                    workspace_name: s.workspace_name.clone(),
                    last_request: a.last_request,
                    last_work: a.last_work,
                }
            })
            .collect()
    }

    pub async fn stop_all(&self) {
        self.stop_all_with_timeout(Duration::from_secs(30)).await;
    }

    async fn stop_all_with_timeout(&self, budget: Duration) {
        use futures::{stream::FuturesUnordered, StreamExt};
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + budget;
        let sessions: Vec<_> = self.sessions.lock().await.values().cloned().collect();
        // Revoke every entrance first. One stuck runtime must not postpone
        // cancellation or consume all other workspaces' shutdown opportunity.
        for session in &sessions {
            session.workspace.cancel();
        }
        let mut stopping = sessions
            .into_iter()
            .map(|session| async move {
                let outcome = tokio::time::timeout_at(deadline, async {
                    let _publication_gate = session.workspace.publish_gate.lock().await;
                    let mut runtime = session.workspace.runtime.lock().await;
                    runtime.retired = true;
                    session.workspace.stop_locked(&mut runtime).await
                })
                .await;
                (session, outcome)
            })
            .collect::<FuturesUnordered<_>>();
        while let Some((session, outcome)) = stopping.next().await {
            match outcome {
                Ok(Ok(())) => {
                    self.sessions.lock().await.remove(&session.workspace_name);
                }
                Ok(Err(error)) => {
                    error!(workspace = %session.workspace_name, %error, "shutdown cleanup incomplete; ownership retained")
                }
                Err(_) => {
                    error!(workspace = %session.workspace_name, "shutdown deadline reached; ownership retained")
                }
            }
        }
    }

    pub async fn sessions_status(
        &self,
        workspace_name: &str,
    ) -> Result<WorkspaceStatus, EngineError> {
        if let Some(session) = self.find_existing_session(workspace_name).await {
            return Ok(session.workspace_status().await);
        }
        match self.engine.get_sandbox(workspace_name).await {
            Ok(handle) => Ok(handle.status().await),
            Err(EngineError::NotFound(_)) => Ok(WorkspaceStatus::NotFound),
            Err(EngineError::SandboxNotRunning(_)) => Ok(WorkspaceStatus::Stopped),
            Err(EngineError::SandboxPaused(_)) => Ok(WorkspaceStatus::Paused),
            Err(EngineError::SandboxTransitioning(_)) => Ok(WorkspaceStatus::Transitioning),
            Err(error) => Err(error),
        }
    }

    pub async fn stop_workspace(&self, workspace_name: &str) -> Result<(), EngineError> {
        if let Ok(session) = self.find_session(workspace_name).await {
            let _publication_gate = session.workspace.publish_gate.lock().await;
            let mut runtime = session.workspace.runtime.lock().await;
            return session.workspace.stop_locked(&mut runtime).await;
        }
        // A process restart can leave a registered VM without an in-memory
        // Session. Disabled users must still be stopped before their pending
        // intent is cleared.
        let handle = match self.engine.get_sandbox(workspace_name).await {
            Ok(handle) => handle,
            Err(EngineError::NotFound(_) | EngineError::SandboxNotRunning(_)) => return Ok(()),
            Err(EngineError::SandboxPaused(_) | EngineError::SandboxTransitioning(_)) => {
                self.engine.start_sandbox(workspace_name).await?
            }
            Err(error) => return Err(error),
        };
        tokio::time::timeout(LIFECYCLE_IO_TIMEOUT, handle.stop())
            .await
            .map_err(|_| EngineError::Timeout("workspace stop did not finish".into()))?
    }

    /// Revoke queued and executing work before disabling an account. The old
    /// Session is retired so restoring the account can bind a fresh one.
    pub async fn revoke_workspace(&self, workspace_name: &str) -> Result<(), EngineError> {
        let Ok(session) = self.find_session(workspace_name).await else {
            return tokio::time::timeout(LIFECYCLE_IO_TIMEOUT, self.stop_workspace(workspace_name))
                .await
                .map_err(|_| EngineError::Timeout("workspace stop did not finish".into()))?;
        };
        session.workspace.cancel();
        let _publication_gate = tokio::time::timeout(
            Duration::from_secs(5),
            session.workspace.publish_gate.lock(),
        )
        .await
        .map_err(|_| EngineError::Timeout("publication change did not finish".into()))?;
        let mut runtime =
            tokio::time::timeout(Duration::from_secs(5), session.workspace.runtime.lock())
                .await
                .map_err(|_| EngineError::Timeout("workspace activation did not finish".into()))?;
        runtime.retired = true;
        tokio::time::timeout(
            LIFECYCLE_IO_TIMEOUT,
            session.workspace.stop_locked(&mut runtime),
        )
        .await
        .map_err(|_| EngineError::Timeout("workspace stop did not finish".into()))??;
        drop(runtime);
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(workspace_name)
            .is_some_and(|current| Arc::ptr_eq(&current.workspace, &session.workspace))
        {
            sessions.remove(workspace_name);
            self.desktops.lock().await.remove(workspace_name);
        }
        Ok(())
    }

    pub fn start_periodic_check(
        self: &Arc<Self>,
        config: LifecycleConfig,
        workspace_names: HashSet<String>,
    ) {
        if config.check_interval_secs == 0 {
            return;
        }
        let mgr = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(config.check_interval_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if mgr.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                if let Err(e) = mgr.check_and_reclaim(&config, &workspace_names).await {
                    error!(error = %e, "lifecycle check failed");
                }
            }
        });
    }

    pub async fn check_and_reclaim(
        &self,
        config: &LifecycleConfig,
        valid_workspace_names: &HashSet<String>,
    ) -> Result<(), EngineError> {
        let _ = valid_workspace_names;
        for (name, reason) in self.collect_candidates_impl(config).await {
            // Idle and session expiry only stop the VM and keep its disks.
            // Destruction is driven exclusively by a persisted deleting record.
            if let Err(e) = self.verify_and_stop_impl(&name, config, reason).await {
                warn!(workspace = %name, error = %e, "lifecycle reclaim failed");
            }
        }
        Ok(())
    }

    async fn collect_candidates_impl(
        &self,
        config: &LifecycleConfig,
    ) -> Vec<(String, &'static str)> {
        let sessions = self.sessions.lock().await;
        sessions
            .iter()
            .filter_map(|(name, s)| {
                let a = s.workspace.activity.lock().unwrap();
                if a.pending != 0 || s.workspace.publication_keeps_alive() {
                    return None;
                }
                if a.last_request.elapsed() >= Duration::from_secs(config.session_timeout_secs) {
                    Some((name.clone(), "session"))
                } else if a.last_work.elapsed() >= Duration::from_secs(config.idle_timeout_secs) {
                    Some((name.clone(), "idle"))
                } else {
                    None
                }
            })
            .collect()
    }

    async fn verify_and_stop_impl(
        &self,
        name: &str,
        config: &LifecycleConfig,
        reason: &str,
    ) -> Result<(), EngineError> {
        let Ok(session) = self.find_session(name).await else {
            return Ok(());
        };
        let Ok(_publication_gate) = session.workspace.publish_gate.try_lock() else {
            return Ok(());
        };
        // Never wait behind a busy workspace; one long command must not delay
        // reclaiming other users. The same gate covers start, exec and stop.
        let Ok(mut runtime) = session.workspace.runtime.try_lock() else {
            return Ok(());
        };
        {
            let a = session.workspace.activity.lock().unwrap();
            let expired = match reason {
                "session" => {
                    a.last_request.elapsed() >= Duration::from_secs(config.session_timeout_secs)
                }
                "idle" => a.last_work.elapsed() >= Duration::from_secs(config.idle_timeout_secs),
                _ => false,
            };
            if !expired
                || a.pending != 0
                || runtime.retired
                || session.workspace.publication_keeps_alive()
            {
                return Ok(());
            }
        }
        // Requests accepted after this check wait for stop to finish, then
        // activate under the same gate before executing. No stale handle race.
        session.workspace.stop_locked(&mut runtime).await
    }

    #[cfg(feature = "test-utils")]
    pub async fn collect_candidates(
        &self,
        config: &LifecycleConfig,
    ) -> Vec<(String, &'static str)> {
        self.collect_candidates_impl(config).await
    }
    #[cfg(feature = "test-utils")]
    pub async fn verify_and_stop(
        &self,
        name: &str,
        config: &LifecycleConfig,
        reason: &str,
    ) -> Result<(), EngineError> {
        self.verify_and_stop_impl(name, config, reason).await
    }
    #[cfg(feature = "test-utils")]
    pub async fn set_session_last_request(&self, name: &str, t: Instant) -> bool {
        if let Ok(s) = self.find_session(name).await {
            s.set_last_request(t).await;
            true
        } else {
            false
        }
    }
    #[cfg(feature = "test-utils")]
    pub async fn set_session_last_work(&self, name: &str, t: Instant) -> bool {
        if let Ok(s) = self.find_session(name).await {
            s.set_last_work(t).await;
            true
        } else {
            false
        }
    }
    pub async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    pub fn active_space_count(&self) -> usize {
        self.max_sessions
            .saturating_sub(self.slots.available_permits())
    }
}

pub struct SessionInfo {
    pub workspace_name: String,
    pub last_request: Instant,
    pub last_work: Instant,
}

#[derive(Clone)]
pub struct Session {
    pub workspace_name: String,
    workspace: Arc<Workspace>,
    sender: mpsc::Sender<ExecRequest>,
    file_sender: mpsc::Sender<ExecRequest>,
    max_output_lines: usize,
    max_output_bytes: usize,
    admission: Option<AdmissionCheck>,
}

impl Session {
    pub fn authorize(&self) -> Result<(), EngineError> {
        self.check_admission()
    }
    pub async fn touch_work(&self) {
        let mut a = self.workspace.activity.lock().unwrap();
        a.last_work = Instant::now();
        a.last_request = a.last_work;
    }
    pub fn with_admission_check(
        mut self,
        check: impl Fn() -> Result<(), EngineError> + Send + Sync + 'static,
    ) -> Self {
        self.admission = Some(Arc::new(check));
        self
    }

    fn check_admission(&self) -> Result<(), EngineError> {
        if let Some(check) = &self.admission {
            check()?;
        }
        Ok(())
    }

    /// Observe terminal runtime state without creating or resuming a VM.
    pub async fn refresh_publications(&self) {
        // Keep the observed VM state and invalidation in the same publication
        // transaction: a concurrent publish may already belong to a new run.
        let _gate = self.workspace.publish_gate.lock().await;
        if self.publications().is_empty() {
            return;
        }
        if matches!(
            self.workspace_status().await,
            WorkspaceStatus::Stopped | WorkspaceStatus::NotFound
        ) {
            self.workspace.close_publications();
        }
    }
    pub fn publications(&self) -> Vec<Publication> {
        let mut items: Vec<_> = self
            .workspace
            .publications
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        items
    }
    pub fn publication(&self, name: &str) -> Option<Publication> {
        self.workspace
            .publications
            .lock()
            .unwrap()
            .get(name)
            .cloned()
    }
    /// Caller holds publication_gate through target preparation and commit.
    pub fn publish(&self, item: Publication) -> Result<(), EngineError> {
        self.commit_publication(item, None).map(|_| ())
    }
    /// Caller holds publication_gate; a failed preparation never removes the old item.
    pub fn replace_publication(
        &self,
        item: Publication,
        previous: &Publication,
    ) -> Result<Option<Publication>, EngineError> {
        self.commit_publication(item, Some(previous))
    }
    fn commit_publication(
        &self,
        item: Publication,
        expected: Option<&Publication>,
    ) -> Result<Option<Publication>, EngineError> {
        let mut publications = self.workspace.publications.lock().unwrap();
        self.check_admission()?;
        if self
            .workspace
            .cancel
            .load(std::sync::atomic::Ordering::SeqCst)
            || self
                .workspace
                .shutting_down
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(EngineError::Other("workspace is closing".into()));
        }
        let current = publications.get(&item.name);
        match (current, expected) {
            (None, None) => {}
            (Some(current), Some(expected))
                if Arc::ptr_eq(&current.epoch, &expected.epoch)
                    && !current.epoch.is_cancelled() => {}
            _ => {
                return Err(EngineError::Other(
                    "publication changed before commit".into(),
                ))
            }
        }
        if let Some(helper) = &item.helper {
            self.clear_helper_cleanup(helper);
        }
        let previous = publications.insert(item.name.clone(), item);
        if let Some(previous) = &previous {
            previous.epoch.cancel();
            if let Some(helper) = &previous.helper {
                self.note_helper_cleanup(helper);
            }
        }
        Ok(previous)
    }
    pub fn extend_publication(&self, name: &str, duration: Duration) -> Option<Publication> {
        let mut items = self.workspace.publications.lock().unwrap();
        let item = items.get_mut(name)?;
        item.extend(duration);
        Some(item.clone())
    }
    pub async fn unpublish(&self, name: &str) {
        self.unpublish_matching(name, None).await;
    }
    async fn unpublish_matching(&self, name: &str, expected: Option<&Arc<CancellationToken>>) {
        let _gate = self.workspace.publish_gate.lock().await;
        let previous = {
            let mut items = self.workspace.publications.lock().unwrap();
            if expected.is_some_and(|epoch| {
                items
                    .get(name)
                    .is_none_or(|item| !Arc::ptr_eq(&item.epoch, epoch))
            }) {
                return;
            }
            items.remove(name)
        };
        if let Some(item) = previous {
            item.epoch.cancel();
            if let Some(helper) = item.helper {
                self.note_helper_cleanup(&helper);
                if self.stop_helper(&helper).await.is_ok() {
                    self.clear_helper_cleanup(&helper);
                }
            }
        }
    }
    pub async fn publication_gate(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.workspace.publish_gate.lock().await
    }
    pub fn reserve_desktop_port(&self, port: u16) {
        self.workspace
            .desktop_port
            .store(port, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn managed_port(&self, port: u16) -> bool {
        (self.workspace.control.lock().unwrap().is_some()
            && (matches!(port, 5901 | 9222)
                || port
                    == self
                        .workspace
                        .desktop_port
                        .load(std::sync::atomic::Ordering::SeqCst)))
            || self
                .workspace
                .dav
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|run| run.instance.port == port)
            || self
                .workspace
                .helper_cleanup
                .lock()
                .unwrap()
                .iter()
                .any(|instance| instance.port == port)
    }
    pub fn helper_cleanup_pending(&self) -> bool {
        !self.workspace.helper_cleanup.lock().unwrap().is_empty()
    }
    pub fn note_helper_cleanup(&self, pid: &ManagedInstance) {
        self.workspace
            .helper_cleanup
            .lock()
            .unwrap()
            .insert(pid.clone());
    }
    pub fn clear_helper_cleanup(&self, pid: &ManagedInstance) {
        self.workspace.helper_cleanup.lock().unwrap().remove(pid);
    }
    pub async fn retry_helper_cleanup(&self) -> Result<(), EngineError> {
        let pending = self
            .workspace
            .helper_cleanup
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for instance in pending {
            self.stop_helper(&instance).await?;
            self.clear_helper_cleanup(&instance);
        }
        Ok(())
    }
    pub async fn stop_helper(&self, instance: &ManagedInstance) -> Result<(), EngineError> {
        // Cleanup must neither queue behind user bash nor reactivate a stopped VM.
        // Revocation of the caller does not revoke ownership of its candidate.
        let runtime = self.workspace.runtime.lock().await;
        if let Some(handle) = runtime.handle.as_ref() {
            handle.stop_managed(instance).await?;
        } else if runtime.slot.is_some() || runtime.reconnect {
            return Err(EngineError::Other(
                "workspace runtime needs reconciliation before helper cleanup".into(),
            ));
        }
        Ok(())
    }
}

impl Session {
    async fn submit(
        &self,
        cmd: Option<String>,
        timeout: Duration,
    ) -> Result<ExecResult, EngineError> {
        self.check_admission()?;
        if cmd.is_some() {
            self.workspace.activity.lock().unwrap().last_work = Instant::now();
        }
        let (resp, rx) = oneshot::channel();
        let cancel = CancellationToken::new();
        let req = ExecRequest {
            cmd,
            timeout,
            file_deadline: None,
            file_phase: None,
            cancel,
            max_lines: self.max_output_lines,
            max_bytes: self.max_output_bytes,
            admission: self.admission.clone(),
            resp,
            _lease: ActivityLease::acquire(&self.workspace),
        };
        self.sender
            .send(req)
            .await
            .map_err(|_| EngineError::Other("command queue closed".into()))?;
        rx.await
            .map_err(|_| EngineError::Other("command response channel closed".into()))?
    }
    pub fn enable_desktop(&self) -> Arc<crate::control::ControlGate> {
        self.workspace
            .control
            .lock()
            .unwrap()
            .get_or_insert_with(|| Arc::new(Default::default()))
            .clone()
    }
    pub fn activity_lease(&self) -> Result<RequestLease, EngineError> {
        self.check_admission()?;
        Ok(RequestLease {
            _lease: ActivityLease::acquire(&self.workspace),
            dav: None,
        })
    }
    pub async fn exec(&self, cmd: &str, timeout: Duration) -> Result<ExecResult, EngineError> {
        self.submit(Some(cmd.into()), timeout)
            .await
            .map(|r| self.apply_truncation(r))
    }
    /// Only the fixed read/write/edit implementations use this bounded concurrent lane.
    /// The owned request retains workspace activity until actual guest completion.
    pub async fn exec_file(&self, cmd: &str, timeout: Duration) -> Result<ExecResult, EngineError> {
        self.exec_file_report(cmd, timeout, None, CancellationToken::new())
            .await
            .map(|r| self.apply_truncation(r))
    }
    /// Internal fixed file script reports can carry bounded binary data. Their
    /// caller validates the complete report before constructing MCP content.
    pub async fn exec_file_report(
        &self,
        cmd: &str,
        timeout: Duration,
        report_bytes: Option<usize>,
        cancel: CancellationToken,
    ) -> Result<ExecResult, EngineError> {
        self.check_admission()?;
        let deadline = tokio::time::Instant::now() + timeout;
        // 0 queued/preparing, 1 submitted, 2 abandoned before submission.
        let phase = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let (resp, mut rx) = oneshot::channel();
        let req = ExecRequest {
            cmd: Some(cmd.into()),
            timeout,
            file_deadline: Some(deadline),
            file_phase: Some(phase.clone()),
            cancel: cancel.clone(),
            max_lines: self.max_output_lines,
            max_bytes: report_bytes.unwrap_or(self.max_output_bytes),
            admission: self.admission.clone(),
            resp,
            _lease: ActivityLease::acquire(&self.workspace),
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(EngineError::Other("EXECUTION_CANCELLED: file operation not started".into())),
            _ = tokio::time::sleep_until(deadline) => return Err(EngineError::Timeout("WAIT_TIMEOUT: file operation not started".into())),
            result = self.file_sender.send(req) => result.map_err(|_| EngineError::Other("file queue closed: operation not started".into()))?,
        }
        let result = tokio::select! {
            result = &mut rx => result,
            _ = tokio::time::sleep_until(deadline) => {
                if phase.compare_exchange(0, 2, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst).is_ok() {
                    return Err(EngineError::Timeout("WAIT_TIMEOUT: file operation not started".into()));
                }
                // Once submitted, await the existing execution owner's bounded cleanup.
                cancel.cancel();
                rx.await
            },
            _ = cancel.cancelled() => {
                if phase.compare_exchange(0, 2, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst).is_ok() {
                    return Err(EngineError::Other("EXECUTION_CANCELLED: file operation not started".into()));
                }
                rx.await
            },
        };
        result.map_err(|_| {
            EngineError::Other("file response lost: inspect the file before retrying".into())
        })?
    }
    pub fn truncate_text(&self, result: ExecResult) -> ExecResult {
        let mut result = result;
        // Reproduce the execution owner's byte tail and keep-ended line tail
        // before the existing host guard. A larger media-report lane must not
        // change text output (including its final newline).
        if result.stdout.len() > self.max_output_bytes {
            let mut start = result.stdout.len() - self.max_output_bytes;
            while !result.stdout.is_char_boundary(start) {
                start += 1;
            }
            result.stdout = result.stdout[start..].to_owned();
            result.truncated = true;
        }
        let mut ends = Vec::new();
        let mut chars = result.stdout.char_indices().peekable();
        while let Some((index, c)) = chars.next() {
            if matches!(
                c,
                '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                    ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
            ) {
                let mut end = index + c.len_utf8();
                if c == '\r' && chars.peek().is_some_and(|(_, c)| *c == '\n') {
                    end = chars.next().unwrap().0 + 1;
                }
                ends.push(end);
            }
        }
        if !result.stdout.is_empty() && ends.last() != Some(&result.stdout.len()) {
            ends.push(result.stdout.len());
        }
        if ends.len() > self.max_output_lines {
            result.stdout =
                result.stdout[ends[ends.len() - self.max_output_lines - 1]..].to_owned();
            result.truncated = true;
        }
        self.apply_truncation(result)
    }
    pub async fn exec_read(&self, cmd: &str, timeout: Duration) -> Result<ExecResult, EngineError> {
        self.submit(Some(cmd.into()), timeout)
            .await
            .map(|r| self.apply_truncation(r))
    }

    /// Acquire activity before waiting for startup. An owned startup task keeps
    /// guest ownership even if the HTTP caller disappears while activating.
    pub async fn begin_dav_request(
        &self,
        launch: ManagedLaunch,
        timeout: Duration,
    ) -> Result<DavRequest, EngineError> {
        self.check_admission()?;
        if launch.service() != ManagedService::Webdav {
            return Err(EngineError::Other("expected DAV launch".into()));
        }
        let lease = ActivityLease::acquire(&self.workspace);
        let owner_lease = ActivityLease::acquire(&self.workspace);
        let owner = self.clone();
        let (tx, rx) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + timeout;
        let progress = Arc::new(StartupProgress::new("wait_dav_lock"));
        let owner_progress = progress.clone();
        tokio::spawn(
            async move {
                let _lease = owner_lease;
                // Deadlines may abandon lock waits, but never interrupt a backend
                // startup after it has acquired runtime ownership. Finish the
                // bounded operation, retain its handle and reconcile failures.
                let result = owner.ensure_dav(launch, deadline, &owner_progress).await;
                if let Err(error) = &result {
                    owner_progress.failed(error);
                }
                let _ = tx.send(result);
            }
            .in_current_span(),
        );
        let dav = tokio::time::timeout_at(deadline, rx)
            .await
            .map_err(|_| progress.timeout())?
            .map_err(|_| EngineError::Other("DAV startup owner ended".into()))??;
        self.check_admission()?;
        // A close/replace can precede this lease transfer. A stale run carries
        // a cancelled token and cannot be used by the proxy.
        dav.active.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(DavRequest {
            port: dav.instance.port,
            cancel: dav.cancel.clone(),
            lease: RequestLease {
                _lease: lease,
                dav: Some(dav),
            },
        })
    }

    async fn ensure_dav(
        &self,
        launch: ManagedLaunch,
        deadline: tokio::time::Instant,
        progress: &StartupProgress,
    ) -> Result<Arc<DavRun>, EngineError> {
        let _gate = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => return Err(progress.timeout()),
            gate = self.workspace.dav_gate.lock() => gate,
        };
        progress.set_phase("wait_runtime_lock");
        let mut runtime = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => return Err(progress.timeout()),
            runtime = self.workspace.runtime.lock() => runtime,
        };
        self.check_admission()?;
        self.workspace
            .ensure_running_with_progress(&mut runtime, progress)
            .await?;
        self.check_admission()?;
        if tokio::time::Instant::now() >= deadline {
            return Err(progress.timeout());
        }
        let handle = runtime
            .handle
            .as_ref()
            .ok_or_else(|| EngineError::SandboxNotRunning(self.workspace_name.clone()))?
            .clone();
        let result = async {
            let previous = self.workspace.dav.lock().unwrap().clone();
            if let Some(previous) = previous {
                progress.set_phase("dav_helper_check");
                let alive = handle.managed_alive(&previous.instance).await?;
                if alive && previous.launch == launch && !previous.cancel.is_cancelled() {
                    return Ok(previous);
                }
                if alive && previous.active.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                    return Err(EngineError::ResourceExhausted(
                        "DAV instance is draining; retry after active transfers finish".into(),
                    ));
                }
                previous.cancel.cancel();
                progress.set_phase("dav_helper_stop");
                self.note_helper_cleanup(&previous.instance);
                handle.stop_managed(&previous.instance).await?;
                self.clear_helper_cleanup(&previous.instance);
                self.workspace.dav.lock().unwrap().take();
            }
            progress.set_phase("dav_helper_start");
            let instance = match handle.start_managed(&launch).await {
                Ok(instance) => instance,
                Err(EngineError::ManagedRestartRequired(old)) => {
                    progress.set_phase("dav_helper_stop");
                    // A newly attached Host has no active request using this guest
                    // instance. The state lock excludes another live Host owner.
                    self.note_helper_cleanup(&old);
                    handle.stop_managed(&old).await?;
                    self.clear_helper_cleanup(&old);
                    progress.set_phase("dav_helper_start");
                    handle.start_managed(&launch).await?
                }
                Err(error) => return Err(error),
            };
            let dav = Arc::new(DavRun {
                instance,
                launch,
                cancel: CancellationToken::new(),
                active: std::sync::atomic::AtomicUsize::new(0),
            });
            // close may race startup without waiting for runtime; do not publish
            // ownership after its cancellation pass has already finished.
            let mut slot = self.workspace.dav.lock().unwrap();
            if self
                .workspace
                .cancel
                .load(std::sync::atomic::Ordering::SeqCst)
                || self
                    .workspace
                    .shutting_down
                    .load(std::sync::atomic::Ordering::SeqCst)
            {
                dav.cancel.cancel();
                self.note_helper_cleanup(&dav.instance);
                return Err(EngineError::Other("workspace is closing".into()));
            }
            *slot = Some(dav.clone());
            Ok(dav)
        }
        .await;
        // Health checks, helper copies and cleanup all share the same transport
        // recovery rule. Preserve ownership records when any step disconnects.
        if matches!(result, Err(EngineError::SandboxNotRunning(_))) {
            runtime.reconnect = true;
        }
        result
    }

    pub async fn start_managed(
        &self,
        launch: ManagedLaunch,
    ) -> Result<ManagedInstance, EngineError> {
        self.check_admission()?;
        let lease = ActivityLease::acquire(&self.workspace);
        let owner = self.clone();
        let (tx, rx) = oneshot::channel();
        // The owner finishes startup even if the requesting HTTP future disappears.
        // Guest records survive a Host disconnect; local candidates remain tracked
        // until publication commits or cleanup succeeds.
        tokio::spawn(async move {
            let _lease = lease;
            let result = async {
                let mut runtime = owner.workspace.runtime.lock().await;
                owner.check_admission()?;
                owner.workspace.ensure_running(&mut runtime).await?;
                owner.check_admission()?;
                let handle = runtime
                    .handle
                    .as_ref()
                    .ok_or_else(|| EngineError::SandboxNotRunning(owner.workspace_name.clone()))?;
                let instance = handle.start_managed(&launch).await?;
                owner.note_helper_cleanup(&instance);
                Ok(instance)
            }
            .await;
            if let Err(Ok(instance)) = tx.send(result) {
                if owner.stop_helper(&instance).await.is_ok() {
                    owner.clear_helper_cleanup(&instance);
                }
            }
        });
        rx.await
            .map_err(|_| EngineError::Other("managed startup owner ended".into()))?
    }
    pub async fn connect_dav_guest_tcp(
        &self,
        port: u16,
    ) -> Result<crate::engine::GuestTcp, EngineError> {
        let mut runtime = self.workspace.runtime.lock().await;
        self.check_admission()?;
        let handle = runtime
            .handle
            .as_ref()
            .ok_or_else(|| EngineError::SandboxNotRunning(self.workspace_name.clone()))?;
        match handle.try_status().await? {
            WorkspaceStatus::Running => self.workspace.ensure_running(&mut runtime).await?,
            WorkspaceStatus::Paused | WorkspaceStatus::Transitioning => {
                self.workspace.ensure_running(&mut runtime).await?
            }
            _ => return Err(EngineError::SandboxNotRunning(self.workspace_name.clone())),
        }
        self.check_admission()?;
        let result = runtime
            .handle
            .as_ref()
            .unwrap()
            .connect_guest_tcp(port)
            .await;
        // Only prepare a fresh agent attachment for the next request. No HTTP
        // method has been sent yet here; submitted writes are never replayed.
        if matches!(result, Err(EngineError::SandboxNotRunning(_))) {
            runtime.reconnect = true;
        }
        result
    }

    pub async fn probe_guest_tcp(&self, port: u16) -> Result<crate::engine::GuestTcp, EngineError> {
        self.connect_guest_tcp(port).await
    }
    pub async fn ensure_helper_alive(&self, publication: &Publication) -> Result<(), EngineError> {
        let Some(pid) = publication.helper.as_ref() else {
            return Ok(());
        };
        let runtime = self.workspace.runtime.lock().await;
        let Some(handle) = runtime.handle.as_ref() else {
            return Ok(());
        };
        if !handle.managed_alive(pid).await? {
            drop(runtime);
            self.unpublish_matching(&publication.name, Some(&publication.epoch))
                .await;
            return Err(EngineError::NotFound(format!(
                "helper {} exited",
                pid.instance_id
            )));
        }
        Ok(())
    }
    pub async fn connect_guest_tcp(
        &self,
        port: u16,
    ) -> Result<crate::engine::GuestTcp, EngineError> {
        self.check_admission()?;
        if matches!(
            self.workspace_status().await,
            WorkspaceStatus::Paused | WorkspaceStatus::Transitioning
        ) {
            self.submit(None, Duration::ZERO).await?;
        }
        let mut runtime = self.workspace.runtime.lock().await;
        self.check_admission()?;
        if runtime.handle.is_none() {
            let handle = match self
                .workspace
                .engine
                .get_sandbox(&self.workspace_name)
                .await
            {
                Ok(handle) => handle,
                Err(EngineError::NotFound(_) | EngineError::SandboxNotRunning(_)) => {
                    return Err(EngineError::SandboxNotRunning(self.workspace_name.clone()));
                }
                Err(error) => return Err(error),
            };
            if handle.try_status().await? != WorkspaceStatus::Running {
                return Err(EngineError::SandboxNotRunning(self.workspace_name.clone()));
            }
            if runtime.slot.is_none() {
                runtime.slot = Some(
                    self.workspace
                        .slots
                        .clone()
                        .try_acquire_owned()
                        .map_err(|_| EngineError::WorkspaceCapacityExhausted)?,
                );
            }
            runtime.handle = Some(handle);
            runtime.prepare_tmpfs = true;
        }
        let Some(handle) = runtime.handle.clone() else {
            return Err(EngineError::SandboxNotRunning(self.workspace_name.clone()));
        };
        match handle.try_status().await? {
            WorkspaceStatus::Running => {
                self.workspace.ensure_running(&mut runtime).await?;
                let result = runtime
                    .handle
                    .as_ref()
                    .unwrap()
                    .connect_guest_tcp(port)
                    .await;
                if matches!(result, Err(EngineError::SandboxNotRunning(_))) {
                    runtime.reconnect = true;
                }
                result
            }
            WorkspaceStatus::Unknown => Err(EngineError::Other(format!(
                "workspace '{}' status unavailable",
                self.workspace_name
            ))),
            WorkspaceStatus::Stopped | WorkspaceStatus::NotFound => {
                self.workspace.close_publications();
                Err(EngineError::SandboxNotRunning(self.workspace_name.clone()))
            }
            _ => Err(EngineError::SandboxNotRunning(self.workspace_name.clone())),
        }
    }

    pub async fn touch(&self) {
        self.workspace.activity.lock().unwrap().last_request = Instant::now();
    }
    pub async fn begin_publish_request(
        &self,
        epoch: &Arc<CancellationToken>,
    ) -> Option<RequestLease> {
        let runtime = self.workspace.runtime.lock().await;
        if runtime.retired
            || self
                .workspace
                .cancel
                .load(std::sync::atomic::Ordering::SeqCst)
            || self
                .workspace
                .shutting_down
                .load(std::sync::atomic::Ordering::SeqCst)
            || epoch.is_cancelled()
            || self
                .workspace
                .publications
                .lock()
                .unwrap()
                .values()
                .all(|current| !Arc::ptr_eq(&current.epoch, epoch))
        {
            return None;
        }
        let lease = ActivityLease::acquire(&self.workspace);
        drop(runtime);
        Some(RequestLease {
            _lease: lease,
            dav: None,
        })
    }
    pub async fn workspace_status(&self) -> WorkspaceStatus {
        let runtime = self.workspace.runtime.lock().await;
        match &runtime.handle {
            Some(h) => h.status().await,
            None if runtime.slot.is_some() => {
                match self
                    .workspace
                    .engine
                    .get_sandbox(&self.workspace_name)
                    .await
                {
                    Ok(handle) => handle.status().await,
                    Err(EngineError::SandboxPaused(_)) => WorkspaceStatus::Paused,
                    Err(EngineError::SandboxTransitioning(_)) => WorkspaceStatus::Transitioning,
                    Err(EngineError::SandboxNotRunning(_)) => WorkspaceStatus::Stopped,
                    Err(EngineError::NotFound(_)) => WorkspaceStatus::NotFound,
                    Err(_) => WorkspaceStatus::Unknown,
                }
            }
            None => WorkspaceStatus::Stopped,
        }
    }
    pub async fn last_request(&self) -> Instant {
        self.workspace.activity.lock().unwrap().last_request
    }
    pub async fn last_work(&self) -> Instant {
        self.workspace.activity.lock().unwrap().last_work
    }
    #[cfg(feature = "test-utils")]
    pub async fn set_last_request(&self, t: Instant) {
        self.workspace.activity.lock().unwrap().last_request = t;
    }
    #[cfg(feature = "test-utils")]
    pub async fn set_last_work(&self, t: Instant) {
        self.workspace.activity.lock().unwrap().last_work = t;
    }
    fn apply_truncation(&self, mut result: ExecResult) -> ExecResult {
        let line_count = result.stdout.lines().count();
        let byte_count = result.stdout.len();

        if line_count <= self.max_output_lines && byte_count <= self.max_output_bytes {
            return result;
        }

        // First pass: truncate by line count (keep tail)
        let mut tail: String = result
            .stdout
            .lines()
            .skip(line_count.saturating_sub(self.max_output_lines))
            .collect::<Vec<_>>()
            .join("\n");

        // Second pass: truncate by byte size (keep tail)
        if tail.len() > self.max_output_bytes {
            let mut start = tail.len() - self.max_output_bytes;
            while !tail.is_char_boundary(start) {
                start += 1;
            }
            // Align to next newline to avoid cutting mid-line
            let start = tail[start..]
                .find('\n')
                .map(|i| start + i + 1)
                .unwrap_or(start);
            tail = tail[start..].to_string();
        }

        result.stdout = tail;
        result.truncated = true;
        result.output_path = None;
        result
    }
}

impl SessionManager {
    /// Report engine resources that are not in the persisted ownership set.
    ///
    /// Unknown resources are never deleted. Ownership guesses from a username
    /// or a static config roster can destroy a disk that belongs to a rebuilt
    /// or imported account.
    pub async fn report_unregistered_resources(
        &self,
        registered_sandboxes: &HashSet<String>,
        registered_volumes: &HashSet<String>,
    ) -> Result<Vec<String>, EngineError> {
        let mut unknown = Vec::new();
        for name in self.engine.list_workspaces().await? {
            if !registered_sandboxes.contains(&name) {
                warn!(workspace = %name, "unregistered sandbox reported; not deleted");
                unknown.push(name);
            }
        }
        for name in self.engine.list_volumes().await? {
            if !registered_volumes.contains(&name) {
                warn!(volume = %name, "unregistered volume reported; not deleted");
                unknown.push(name);
            }
        }
        unknown.sort();
        Ok(unknown)
    }

    /// Destroy one persisted deleting record. NotFound for an exact resource is
    /// success; every other engine error keeps the record for retry.
    pub async fn destroy_recorded_resources(
        &self,
        sandbox_name: &str,
        volume_names: &[String],
    ) -> Result<(), EngineError> {
        if let Ok(session) = self.find_session(sandbox_name).await {
            session.workspace.cancel();
            let _publication_gate = tokio::time::timeout(
                Duration::from_secs(5),
                session.workspace.publish_gate.lock(),
            )
            .await
            .map_err(|_| EngineError::Timeout("publication change did not finish".into()))?;
            let mut runtime =
                tokio::time::timeout(Duration::from_secs(5), session.workspace.runtime.lock())
                    .await
                    .map_err(|_| {
                        EngineError::Timeout("workspace execution did not finish".into())
                    })?;
            runtime.retired = true;
            session.workspace.stop_for_deletion(&mut runtime).await?;
        }
        match tokio::time::timeout(
            LIFECYCLE_IO_TIMEOUT,
            self.engine.remove_sandbox(sandbox_name),
        )
        .await
        .map_err(|_| EngineError::Timeout("sandbox deletion did not finish".into()))?
        {
            Ok(()) | Err(EngineError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
        for volume in volume_names {
            match tokio::time::timeout(LIFECYCLE_IO_TIMEOUT, self.engine.remove_volume(volume))
                .await
                .map_err(|_| EngineError::Timeout("volume deletion did not finish".into()))?
            {
                Ok(()) | Err(EngineError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(sandbox_name)
            .is_some_and(|session| session.workspace_name == sandbox_name)
        {
            sessions.remove(sandbox_name);
        }
        self.desktops.lock().await.remove(sandbox_name);
        self.images.lock().unwrap().remove(sandbox_name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ExecResult;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    fn publication(name: &str, duration: Duration) -> Publication {
        Publication::new(
            name.into(),
            "alice".into(),
            "spc_test".into(),
            PublishTarget::Port(3000),
            3000,
            None,
            duration,
        )
        .unwrap()
    }
    fn age_activity(session: &Session) {
        let mut activity = session.workspace.activity.lock().unwrap();
        activity.last_request = Instant::now() - Duration::from_secs(100);
        activity.last_work = activity.last_request;
    }

    #[tokio::test]
    async fn terminal_refresh_waits_for_publication_transaction() {
        let session = make_session(100, 1000);
        let item = publication("old", Duration::from_secs(60));
        session.publish(item.clone()).unwrap();
        let gate = session.publication_gate().await;
        let refresh = session.refresh_publications();
        tokio::pin!(refresh);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut refresh)
                .await
                .is_err()
        );
        assert!(!item.epoch.is_cancelled());
        drop(gate);
        refresh.await;
        assert!(item.epoch.is_cancelled());
        assert!(session.publications().is_empty());
    }

    #[tokio::test]
    async fn finite_keepalive_protects_both_reapers_and_expires_without_touching_activity() {
        let manager = SessionManager::new(
            Arc::new(crate::engine::mock::MockEngine::new()),
            1,
            make_session(100, 1000).workspace.opts.clone(),
        );
        let session = manager.get_or_create("keepalive").await.unwrap();
        for (request, work, expected_reason) in [(1, 1000, "session"), (1000, 1, "idle")] {
            let config = LifecycleConfig {
                session_timeout_secs: request,
                idle_timeout_secs: work,
                check_interval_secs: 1,
            };
            age_activity(&session);
            session
                .publish(publication("short", Duration::from_secs(60)))
                .unwrap();
            session
                .publish(publication("long", Duration::from_secs(120)))
                .unwrap();
            assert!(manager.collect_candidates_impl(&config).await.is_empty());
            session
                .workspace
                .publications
                .lock()
                .unwrap()
                .get_mut("short")
                .unwrap()
                .keep_alive_deadline = Instant::now() - Duration::from_secs(1);
            assert!(manager.collect_candidates_impl(&config).await.is_empty());
            let before = session.last_work().await;
            session.unpublish("long").await;
            assert_eq!(
                session.last_work().await,
                before,
                "unpublish does not grant extra idle time"
            );
            assert_eq!(
                manager.collect_candidates_impl(&config).await[0].1,
                expected_reason
            );
            assert!(
                session.publication("short").is_some(),
                "keepalive expiry is not URL expiry"
            );
            session.unpublish("short").await;
        }
        manager.stop_all().await;
    }

    #[tokio::test]
    async fn renewal_after_candidate_collection_prevents_stop_and_vm_stop_revokes_all() {
        let manager = SessionManager::new(
            Arc::new(crate::engine::mock::MockEngine::new()),
            1,
            make_session(100, 1000).workspace.opts.clone(),
        );
        let session = manager.get_or_create("renew-race").await.unwrap();
        let config = LifecycleConfig {
            session_timeout_secs: 1,
            idle_timeout_secs: 1,
            check_interval_secs: 1,
        };
        let first = publication("first", Duration::ZERO);
        let second = publication("second", Duration::ZERO);
        session.publish(first.clone()).unwrap();
        session.publish(second.clone()).unwrap();
        age_activity(&session);
        let candidates = manager.collect_candidates_impl(&config).await;
        assert_eq!(candidates.len(), 1);
        {
            let _gate = session.publication_gate().await;
            session
                .extend_publication("second", Duration::from_secs(60))
                .unwrap();
        }
        manager
            .verify_and_stop_impl(&candidates[0].0, &config, candidates[0].1)
            .await
            .unwrap();
        assert_eq!(session.workspace_status().await, WorkspaceStatus::Running);
        manager.stop_workspace("renew-race").await.unwrap();
        assert!(session.publications().is_empty());
        assert!(first.epoch.is_cancelled());
        assert!(second.epoch.is_cancelled());
        session.exec("true", Duration::from_secs(1)).await.unwrap();
        assert!(
            session.publications().is_empty(),
            "restarting VM does not restore URLs"
        );
        manager.stop_all().await;
    }

    #[test]
    fn desktop_management_ports_are_never_publishable() {
        let session = make_session(100, 1000);
        assert!(!session.managed_port(9222));
        session.enable_desktop();
        assert!(session.managed_port(9222));
        assert!(session.managed_port(5901));
        session.reserve_desktop_port(30123);
        assert!(session.managed_port(30123));
        assert!(!session.managed_port(8124));
    }

    #[tokio::test]
    async fn stale_helper_health_cannot_close_a_recreated_name() {
        let session = make_session(100, 1000);
        let old = publication("same", Duration::ZERO);
        session.publish(old.clone()).unwrap();
        session.unpublish("same").await;
        let new = publication("same", Duration::ZERO);
        session.publish(new.clone()).unwrap();
        session.unpublish_matching("same", Some(&old.epoch)).await;
        assert_eq!(session.publication("same").unwrap().id, new.id);
        assert!(!new.epoch.is_cancelled());
    }

    #[tokio::test]
    async fn tmpfs_preparation_retries_failure_and_runs_once_per_connection() {
        use std::sync::atomic::Ordering;
        let mut session = make_session(100, 1000);
        Arc::get_mut(&mut session.workspace).unwrap().opts.volumes =
            vec![crate::engine::VolumeSpec::tmpfs("/tmp", 64)];
        let handle = Arc::new(crate::engine::mock::MockHandle::new("test"));
        let mut runtime = session.workspace.runtime.lock().await;
        runtime.handle = Some(handle.clone());
        handle.fail_tmpfs.store(true, Ordering::SeqCst);
        assert!(session
            .workspace
            .prepare_runtime(&mut runtime)
            .await
            .is_err());
        assert!(runtime.prepare_tmpfs);
        assert_eq!(handle.exec_count.load(Ordering::SeqCst), 0);
        handle.fail_tmpfs.store(false, Ordering::SeqCst);
        session
            .workspace
            .prepare_runtime(&mut runtime)
            .await
            .unwrap();
        session
            .workspace
            .prepare_runtime(&mut runtime)
            .await
            .unwrap();
        assert!(!runtime.prepare_tmpfs);
        assert_eq!(handle.tmpfs_calls.load(Ordering::SeqCst), 2);
        runtime.prepare_tmpfs = true;
        session
            .workspace
            .prepare_runtime(&mut runtime)
            .await
            .unwrap();
        assert_eq!(handle.tmpfs_calls.load(Ordering::SeqCst), 3);
    }

    fn make_session(max_lines: usize, max_bytes: usize) -> Session {
        let engine = Arc::new(crate::engine::mock::MockEngine::new());
        let now = Instant::now();
        let (sender, _rx) = mpsc::channel(10);
        Session {
            workspace_name: "test".into(),
            file_sender: sender.clone(),
            sender,
            max_output_lines: max_lines,
            max_output_bytes: max_bytes,
            admission: None,
            workspace: Arc::new(Workspace {
                name: "test".into(),
                publications: StdMutex::new(HashMap::new()),
                publish_gate: Mutex::new(()),
                helper_cleanup: StdMutex::new(HashSet::new()),
                dav: StdMutex::new(None),
                dav_gate: Mutex::new(()),
                control: StdMutex::new(None),
                desktop_port: std::sync::atomic::AtomicU16::new(0),
                engine,
                slots: Arc::new(Semaphore::new(1)),
                shutting_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                cancel: std::sync::atomic::AtomicBool::new(false),
                cancel_notify: Notify::new(),
                opts: WorkspaceOpts {
                    image: "test".into(),
                    cpus: 1,
                    memory_mib: 256,
                    shell: "/bin/bash".into(),
                    volumes: vec![],
                    network_access: Default::default(),
                    outbound_proxy: None,
                    max_tcp_connections: None,
                    max_udp_connections: None,
                },
                runtime: Mutex::new(Runtime {
                    handle: None,
                    slot: None,
                    retired: false,
                    reconnect: false,
                    reclaim_adopted_static: false,
                    prepare_tmpfs: true,
                }),
                activity: StdMutex::new(Activity {
                    last_request: now,
                    last_work: now,
                    pending: 0,
                }),
            }),
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn cancelled_managed_start_finishes_with_owned_cleanup_outside_bash_queue() {
        use crate::engine::mock::{TEST_STARTED_PIDS, TEST_STOPPED_PIDS};
        TEST_STARTED_PIDS.lock().unwrap().clear();
        TEST_STOPPED_PIDS.lock().unwrap().clear();
        // make_session has a closed bash queue. Managed work must not use it.
        let session = make_session(2000, 51200);
        let runtime = session.workspace.runtime.lock().await;
        let mut launch = Box::pin(session.start_managed(ManagedLaunch::StaticHttp {
            host_binary: "mock".into(),
            root: "/workspace".into(),
            file: false,
        }));
        assert!(futures::poll!(&mut launch).is_pending());
        drop(launch);
        drop(runtime);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !TEST_STOPPED_PIDS.lock().unwrap().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled caller must not orphan startup");
        assert_eq!(
            *TEST_STARTED_PIDS.lock().unwrap(),
            *TEST_STOPPED_PIDS.lock().unwrap()
        );
        assert!(!session.helper_cleanup_pending());
        assert_eq!(session.workspace.activity.lock().unwrap().pending, 0);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn dav_slot_survives_unpublish_and_drains_before_replacement() {
        let session = make_session(2000, 51200); // closed bash queue
        let launch = ManagedLaunch::Webdav {
            host_binary: "mock".into(),
            owner: "space".into(),
            config: serde_json::json!({"prefix":"/u/alice/dav/"}),
        };
        let first = session
            .begin_dav_request(launch.clone(), Duration::from_secs(2))
            .await
            .unwrap();
        let second = session
            .begin_dav_request(launch.clone(), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(first.port, second.port);
        session.unpublish("test").await;
        assert!(!first.cancel.is_cancelled());
        let replacement = ManagedLaunch::Webdav {
            host_binary: "mock-new".into(),
            owner: "space".into(),
            config: serde_json::json!({"prefix":"/u/alice/dav/"}),
        };
        assert!(matches!(
            session
                .begin_dav_request(replacement.clone(), Duration::from_secs(2))
                .await,
            Err(EngineError::ResourceExhausted(_))
        ));
        assert!(
            !first.cancel.is_cancelled(),
            "live upload may not be killed by upgrade"
        );
        let old = Instant::now() - Duration::from_secs(3600);
        session.set_last_request(old).await;
        session.set_last_work(old).await;
        let finished = Instant::now();
        let cancellation = first.cancel.clone();
        drop(first);
        drop(second);
        assert!(session.last_work().await >= finished);
        assert!(session.last_request().await >= finished);
        let next = session
            .begin_dav_request(replacement, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(cancellation.is_cancelled());
        assert!(!next.cancel.is_cancelled());
        session.workspace.cancel();
        assert!(next.cancel.is_cancelled());
        drop(next);
        assert_eq!(session.workspace.activity.lock().unwrap().pending, 0);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn dav_start_deadline_and_cancelled_waiter_keep_owned_lifecycle() {
        let session = make_session(2000, 51200);
        let launch = ManagedLaunch::Webdav {
            host_binary: "mock".into(),
            owner: "space".into(),
            config: serde_json::json!({}),
        };
        let gate = session.workspace.dav_gate.lock().await;
        assert!(matches!(
            session
                .begin_dav_request(launch.clone(), Duration::from_millis(20))
                .await,
            Err(EngineError::Timeout(_))
        ));
        drop(gate);
        tokio::task::yield_now().await;
        assert!(session.workspace.dav.lock().unwrap().is_none());
        assert_eq!(session.workspace.activity.lock().unwrap().pending, 0);

        let runtime = session.workspace.runtime.lock().await;
        assert!(session
            .begin_dav_request(launch.clone(), Duration::from_millis(20))
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("phase=wait_runtime_lock"));
        tokio::time::timeout(Duration::from_secs(1), async {
            while session.workspace.activity.lock().unwrap().pending != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut waiting = Box::pin(session.begin_dav_request(launch, Duration::from_secs(2)));
        assert!(futures::poll!(&mut waiting).is_pending());
        drop(waiting);
        assert_eq!(
            session.workspace.activity.lock().unwrap().pending,
            1,
            "startup owner survives caller drop"
        );
        drop(runtime);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if session.workspace.activity.lock().unwrap().pending == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            session.workspace.dav.lock().unwrap().is_some(),
            "completed startup remains owned by the slot"
        );
        session.workspace.cancel();
        assert!(
            session.helper_cleanup_pending(),
            "close retains cleanup identity"
        );
        session.retry_helper_cleanup().await.unwrap();
        assert!(!session.helper_cleanup_pending());
    }

    struct RecoveryEngine {
        inner: crate::engine::mock::MockEngine,
        entered: Notify,
        release: Semaphore,
        fail: std::sync::atomic::AtomicBool,
        state: std::sync::atomic::AtomicU8,
        starts: std::sync::atomic::AtomicUsize,
        exit_before_ready: std::sync::atomic::AtomicBool,
    }

    impl RecoveryEngine {
        fn new() -> Self {
            Self {
                inner: crate::engine::mock::MockEngine::new(),
                entered: Notify::new(),
                release: Semaphore::new(0),
                fail: std::sync::atomic::AtomicBool::new(false),
                state: std::sync::atomic::AtomicU8::new(0),
                starts: std::sync::atomic::AtomicUsize::new(0),
                exit_before_ready: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    #[async_trait::async_trait]
    impl AgentEngine for RecoveryEngine {
        async fn create_sandbox(
            &self,
            name: &str,
            opts: &WorkspaceOpts,
        ) -> Result<Arc<dyn AgentHandle>, EngineError> {
            self.inner.create_sandbox(name, opts).await
        }
        async fn get_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
            self.inner.get_sandbox(name).await
        }
        async fn sandbox_status(&self, _name: &str) -> Result<WorkspaceStatus, EngineError> {
            Ok(match self.state.load(std::sync::atomic::Ordering::SeqCst) {
                0 => WorkspaceStatus::Stopped,
                1 => WorkspaceStatus::Running,
                _ => WorkspaceStatus::Unknown,
            })
        }
        async fn start_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(EngineError::Other("injected VM startup failure".into()));
            }
            if self
                .exit_before_ready
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                self.state.store(0, std::sync::atomic::Ordering::SeqCst);
                let handle = crate::engine::mock::MockHandle::new(name);
                handle
                    .running
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                return Ok(Arc::new(handle));
            }
            self.state.store(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.start_sandbox(name).await
        }
        async fn remove_sandbox(&self, name: &str) -> Result<(), EngineError> {
            self.inner.remove_sandbox(name).await
        }
        async fn list_running(&self) -> Result<Vec<String>, EngineError> {
            self.inner.list_running().await
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn dav_vm_start_survives_deadline_and_failure_releases_known_terminal_capacity() {
        use std::sync::atomic::Ordering;
        let engine = Arc::new(RecoveryEngine::new());
        engine.inner.sandboxes.lock().await.push("existing".into());
        let manager = SessionManager::new(
            engine.clone(),
            1,
            make_session(100, 1000).workspace.opts.clone(),
        );
        let session = manager.get_session("existing").await.unwrap();
        let launch = ManagedLaunch::Webdav {
            host_binary: "mock".into(),
            owner: "space".into(),
            config: serde_json::json!({}),
        };
        let error = session
            .begin_dav_request(launch.clone(), Duration::from_millis(30))
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("phase=vm_start"));
        assert_eq!(manager.active_space_count(), 1);
        assert_eq!(session.workspace.activity.lock().unwrap().pending, 1);
        engine.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(1), async {
            while session.workspace.activity.lock().unwrap().pending != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            session.workspace.runtime.lock().await.handle.is_some(),
            "late startup keeps its runtime handle"
        );
        assert!(
            session.workspace.dav.lock().unwrap().is_none(),
            "expired request must not launch a helper after VM activation"
        );
        let ready = session
            .begin_dav_request(launch.clone(), Duration::from_secs(1))
            .await
            .unwrap();
        drop(ready);
        assert_eq!(
            engine.starts.load(Ordering::SeqCst),
            1,
            "retry uses the owned VM"
        );

        let mut runtime = session.workspace.runtime.lock().await;
        runtime.handle = None;
        runtime.slot.take();
        drop(runtime);
        engine.state.store(0, Ordering::SeqCst);
        engine.fail.store(true, Ordering::SeqCst);
        engine.release.add_permits(1);
        assert!(session
            .begin_dav_request(launch.clone(), Duration::from_secs(1))
            .await
            .is_err());
        assert_eq!(
            manager.active_space_count(),
            0,
            "a confirmed terminal VM holds no capacity"
        );
        assert_eq!(session.workspace.activity.lock().unwrap().pending, 0);
        engine.fail.store(false, Ordering::SeqCst);
        engine.release.add_permits(1);
        drop(
            session
                .begin_dav_request(launch, Duration::from_secs(1))
                .await
                .unwrap(),
        );
        assert_eq!(manager.active_space_count(), 1);
        assert_eq!(
            engine.inner.create_count.load(Ordering::SeqCst),
            0,
            "original space is never recreated"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn dav_vm_exit_before_readiness_releases_capacity_and_guest_state() {
        use std::sync::atomic::Ordering;
        let engine = Arc::new(RecoveryEngine::new());
        engine.inner.sandboxes.lock().await.push("existing".into());
        let manager = SessionManager::new(
            engine.clone(),
            1,
            make_session(100, 1000).workspace.opts.clone(),
        );
        let session = manager.get_session("existing").await.unwrap();
        session.workspace.desktop_port.store(5900, Ordering::SeqCst);
        engine.exit_before_ready.store(true, Ordering::SeqCst);
        engine.release.add_permits(1);
        let launch = ManagedLaunch::Webdav {
            host_binary: "mock".into(),
            owner: "space".into(),
            config: serde_json::json!({}),
        };
        assert!(matches!(
            session
                .begin_dav_request(launch.clone(), Duration::from_secs(1))
                .await,
            Err(EngineError::SandboxNotRunning(_))
        ));
        assert_eq!(manager.active_space_count(), 0);
        assert!(session.workspace.runtime.lock().await.handle.is_none());
        assert_eq!(session.workspace.desktop_port.load(Ordering::SeqCst), 0);
        assert_eq!(session.workspace.activity.lock().unwrap().pending, 0);
        engine.exit_before_ready.store(false, Ordering::SeqCst);
        engine.release.add_permits(1);
        drop(
            session
                .begin_dav_request(launch, Duration::from_secs(1))
                .await
                .unwrap(),
        );
        assert_eq!(manager.active_space_count(), 1);
        assert_eq!(engine.inner.create_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn dav_cancelled_vm_start_finishes_and_unknown_failure_retains_capacity() {
        use std::sync::atomic::Ordering;
        let engine = Arc::new(RecoveryEngine::new());
        engine.inner.sandboxes.lock().await.push("existing".into());
        let manager = SessionManager::new(
            engine.clone(),
            1,
            make_session(100, 1000).workspace.opts.clone(),
        );
        let session = manager.get_session("existing").await.unwrap();
        let launch = ManagedLaunch::Webdav {
            host_binary: "mock".into(),
            owner: "space".into(),
            config: serde_json::json!({}),
        };
        let caller = {
            let session = session.clone();
            let launch = launch.clone();
            tokio::spawn(async move {
                session
                    .begin_dav_request(launch, Duration::from_secs(1))
                    .await
            })
        };
        engine.entered.notified().await;
        caller.abort();
        let _ = caller.await;
        engine.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(1), async {
            while session.workspace.activity.lock().unwrap().pending != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(
            session
                .begin_dav_request(launch.clone(), Duration::from_secs(1))
                .await
                .unwrap(),
        );
        assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
        assert_eq!(manager.active_space_count(), 1);

        let mut runtime = session.workspace.runtime.lock().await;
        runtime.handle = None;
        drop(runtime);
        engine.state.store(0, Ordering::SeqCst);
        engine.fail.store(true, Ordering::SeqCst);
        // Change the authoritative outcome while startup is in progress.
        let caller = {
            let session = session.clone();
            tokio::spawn(async move {
                session
                    .begin_dav_request(launch, Duration::from_secs(1))
                    .await
            })
        };
        engine.entered.notified().await;
        engine.state.store(2, Ordering::SeqCst);
        engine.release.add_permits(1);
        assert!(caller.await.unwrap().is_err());
        assert_eq!(
            manager.active_space_count(),
            1,
            "unknown residency must not free a possibly running VM's slot"
        );
        assert_eq!(session.workspace.activity.lock().unwrap().pending, 0);
    }

    fn make_result(stdout: &str) -> ExecResult {
        ExecResult {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: 0,
            truncated: false,
            output_path: None,
        }
    }

    #[tokio::test]
    async fn status_query_does_not_hold_the_session_map_while_waiting_for_runtime() {
        let session = make_session(2000, 51200);
        let manager = Arc::new(SessionManager::new(
            session.workspace.engine.clone(),
            1,
            session.workspace.opts.clone(),
        ));
        manager
            .sessions
            .lock()
            .await
            .insert("test".into(), session.clone());

        let runtime = session.workspace.runtime.lock().await;
        let querying = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.sessions_status("test").await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        tokio::time::timeout(Duration::from_secs(1), manager.cancel_workspace("test"))
            .await
            .expect("status query must not delay workspace cancellation");
        drop(runtime);
        assert_eq!(querying.await.unwrap().unwrap(), WorkspaceStatus::Stopped);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn shutdown_has_one_deadline_and_retains_unfinished_owners() {
        let engine = Arc::new(crate::engine::mock::MockEngine::new());
        let opts = make_session(2000, 51200).workspace.opts.clone();
        let manager = SessionManager::new(engine, 4, opts);
        let publishing = manager.get_session("shutdown-publish").await.unwrap();
        let runtime_busy = manager.get_session("shutdown-runtime").await.unwrap();
        let backend_busy = manager.get_or_create("shutdown-backend").await.unwrap();
        let healthy = manager.get_or_create("shutdown-healthy").await.unwrap();
        let publish_gate = publishing.workspace.publish_gate.lock().await;
        let runtime_gate = runtime_busy.workspace.runtime.lock().await;
        *crate::engine::mock::TEST_HOLD_STOP_WORKSPACE
            .lock()
            .unwrap() = Some("shutdown-backend".into());
        tokio::time::timeout(
            Duration::from_secs(1),
            manager.stop_all_with_timeout(Duration::from_millis(30)),
        )
        .await
        .expect("one global deadline");
        assert_eq!(manager.session_count().await, 3);
        assert!(manager
            .find_existing_session(&healthy.workspace_name)
            .await
            .is_none());
        assert!(publishing
            .workspace
            .cancel
            .load(std::sync::atomic::Ordering::SeqCst));
        assert!(runtime_busy
            .workspace
            .cancel
            .load(std::sync::atomic::Ordering::SeqCst));
        assert!(backend_busy.workspace.runtime.lock().await.handle.is_some());
        drop(publish_gate);
        drop(runtime_gate);
        *crate::engine::mock::TEST_HOLD_STOP_WORKSPACE
            .lock()
            .unwrap() = None;
        manager.stop_all().await;
        assert_eq!(manager.session_count().await, 0);
    }

    #[tokio::test]
    async fn probing_a_stopped_port_does_not_create_a_workspace() {
        let engine = Arc::new(crate::engine::mock::MockEngine::new());
        let opts = make_session(2000, 51200).workspace.opts.clone();
        let manager = SessionManager::new(engine.clone(), 1, opts);
        let session = manager.get_session("test-probe").await.unwrap();

        assert!(matches!(
            session.probe_guest_tcp(3000).await,
            Err(EngineError::SandboxNotRunning(_))
        ));
        assert_eq!(
            engine
                .create_count
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let session = make_session(2000, 5);
        let result = session.apply_truncation(make_result("中文测试"));
        assert!(result.truncated);
        assert!(result.stdout.len() <= 5);
        assert!(result.stdout.ends_with('试'));
    }

    #[test]
    fn truncation_no_truncation_needed() {
        let session = make_session(2000, 51200);
        let result = session.apply_truncation(make_result("hello\nworld"));
        assert!(!result.truncated);
        assert_eq!(result.stdout, "hello\nworld");
    }

    #[test]
    fn truncation_by_line_count() {
        let session = make_session(3, 51200);
        let input: String = (1..=10)
            .map(|i| format!("line-{:02}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let result = session.apply_truncation(make_result(&input));
        assert!(result.truncated);
        // Should keep last 3 lines
        assert!(
            result.stdout.contains("line-08"),
            "should contain line-08: {}",
            result.stdout
        );
        assert!(
            result.stdout.contains("line-09"),
            "should contain line-09: {}",
            result.stdout
        );
        assert!(
            result.stdout.contains("line-10"),
            "should contain line-10: {}",
            result.stdout
        );
        assert!(
            !result.stdout.contains("line-01"),
            "should NOT contain line-01: {}",
            result.stdout
        );
        assert!(
            !result.stdout.contains("line-07"),
            "should NOT contain line-07: {}",
            result.stdout
        );
    }

    #[test]
    fn truncation_by_byte_size() {
        let session = make_session(2000, 50);
        // Create lines that exceed 50 bytes after line truncation
        let input: String = (1..=5)
            .map(|i| format!("line-{:010}-end", i))
            .collect::<Vec<_>>()
            .join("\n");
        let result = session.apply_truncation(make_result(&input));
        assert!(result.truncated);
        assert!(
            result.stdout.len() <= 50 + 20,
            "byte count {} should be near limit",
            result.stdout.len()
        );
    }

    #[test]
    fn truncation_byte_second_pass_preserves_tail() {
        let session = make_session(2000, 30);
        // 3 long lines, each ~20 bytes
        let input = "aaaaaaaaaaaaaaaaaaaa\nbbbbbbbbbbbbbbbbbbbb\ncccccccccccccccccccc";
        let result = session.apply_truncation(make_result(input));
        assert!(result.truncated);
        // Should keep tail content
        assert!(
            result.stdout.contains("cccccccccccccccccccc"),
            "should contain last line: {}",
            result.stdout
        );
    }
    #[test]
    fn media_lane_text_tail_matches_owner_newlines_and_unicode_boundaries() {
        assert_eq!(
            make_session(2000, 5)
                .truncate_text(make_result("abcdef\n"))
                .stdout,
            "cdef\n"
        );
        assert_eq!(
            make_session(2000, 7)
                .truncate_text(make_result("中hello\n"))
                .stdout,
            "hello\n"
        );
        let result = make_session(2, 100).truncate_text(make_result("one\r\ntwo\u{2028}three\n"));
        assert_eq!(result.stdout, "two\u{2028}three\n");
        assert!(result.truncated);
    }
}
