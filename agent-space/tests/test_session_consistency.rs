use agent_engine::config::LifecycleConfig;
use agent_engine::engine::{
    AgentEngine, AgentHandle, EngineError, ExecResult, ManagedService, WorkspaceOpts,
    WorkspaceStatus,
};
use agent_engine::session::SessionManager;
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, Semaphore};

struct Handle {
    name: String,
    running: AtomicBool,
    paused: AtomicBool,
    transitioning: AtomicBool,
    unknown: AtomicBool,
    status_error: AtomicBool,
    calls: AtomicUsize,
    stops: AtomicUsize,
    entered: Notify,
    release: Semaphore,
    stop_entered: Notify,
    stop_release: Semaphore,
    block_stop: AtomicBool,
    fail_stop: AtomicBool,
    static_reclaims: AtomicUsize,
    fail_reclaim: AtomicBool,
    tcp_error: AtomicUsize,
    tcp_calls: AtomicUsize,
    managed_starts: AtomicUsize,
    fail_managed_start: AtomicBool,
}
impl Handle {
    fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            running: AtomicBool::new(true),
            paused: AtomicBool::new(false),
            transitioning: AtomicBool::new(false),
            unknown: AtomicBool::new(false),
            status_error: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Semaphore::new(0),
            stop_entered: Notify::new(),
            stop_release: Semaphore::new(0),
            block_stop: AtomicBool::new(false),
            fail_stop: AtomicBool::new(false),
            static_reclaims: AtomicUsize::new(0),
            fail_reclaim: AtomicBool::new(false),
            tcp_error: AtomicUsize::new(0),
            tcp_calls: AtomicUsize::new(0),
            managed_starts: AtomicUsize::new(0),
            fail_managed_start: AtomicBool::new(false),
        }
    }
}
#[async_trait]
impl AgentHandle for Handle {
    async fn start_managed(
        &self,
        launch: &agent_engine::engine::ManagedLaunch,
    ) -> Result<agent_engine::engine::ManagedInstance, EngineError> {
        self.managed_starts.fetch_add(1, Ordering::SeqCst);
        if self.fail_managed_start.swap(false, Ordering::SeqCst) {
            return Err(EngineError::SandboxNotRunning(
                "helper copy client closed".into(),
            ));
        }
        Ok(agent_engine::engine::ManagedInstance {
            service: launch.service(),
            instance_id: "dav-owned-instance".into(),
            pid: 42,
            start_ticks: 17,
            boot_id: "same-boot".into(),
            executable_device: 1,
            executable_inode: 2,
            binary_digest: "helper".into(),
            launch_digest: "config".into(),
            port: 1234,
        })
    }
    async fn managed_alive(
        &self,
        _: &agent_engine::engine::ManagedInstance,
    ) -> Result<bool, EngineError> {
        Ok(true)
    }
    async fn connect_guest_tcp(
        &self,
        _: u16,
    ) -> Result<agent_engine::engine::GuestTcp, EngineError> {
        self.tcp_calls.fetch_add(1, Ordering::SeqCst);
        match self.tcp_error.swap(0, Ordering::SeqCst) {
            1 => {
                return Err(EngineError::SandboxNotRunning(
                    "cached agent client closed".into(),
                ))
            }
            2 => return Err(EngineError::Other("guest peer refused connection".into())),
            _ => {}
        }
        Ok(agent_engine::engine::GuestTcp {
            reader: Box::new(tokio::io::empty()),
            writer: Box::new(tokio::io::sink()),
        })
    }
    async fn exec(
        &self,
        _: &str,
        args: &[&str],
        _: Option<Duration>,
    ) -> Result<ExecResult, EngineError> {
        if !self.running.load(Ordering::SeqCst) {
            return Err(EngineError::SandboxNotRunning(self.name.clone()));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        if args.contains(&"blocked") {
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
        }
        if args.contains(&"uncertain") {
            return Err(EngineError::SandboxNotRunning(
                "side effect committed, exit event lost".into(),
            ));
        }
        Ok(ExecResult {
            stdout: "done".into(),
            stderr: String::new(),
            exit_code: 0,
            truncated: false,
            output_path: None,
        })
    }
    async fn stop(&self) -> Result<(), EngineError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        if self.fail_stop.load(Ordering::SeqCst) {
            return Err(EngineError::Other("stop failed".into()));
        }
        if self.block_stop.load(Ordering::SeqCst) {
            self.stop_entered.notify_one();
            self.stop_release.acquire().await.unwrap().forget();
        }
        self.running.store(false, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        self.transitioning.store(false, Ordering::SeqCst);
        Ok(())
    }
    async fn kill(&self) -> Result<(), EngineError> {
        self.stop().await
    }
    async fn status(&self) -> WorkspaceStatus {
        if self.unknown.load(Ordering::SeqCst) {
            return WorkspaceStatus::Unknown;
        }
        if self.paused.load(Ordering::SeqCst) {
            return WorkspaceStatus::Paused;
        }
        if self.transitioning.load(Ordering::SeqCst) {
            return WorkspaceStatus::Transitioning;
        }
        if self.running.load(Ordering::SeqCst) {
            WorkspaceStatus::Running
        } else {
            WorkspaceStatus::Stopped
        }
    }
    async fn try_status(&self) -> Result<WorkspaceStatus, EngineError> {
        if self.status_error.load(Ordering::SeqCst) {
            Err(EngineError::Other(
                "database disk image is malformed (code: 11)".into(),
            ))
        } else {
            Ok(self.status().await)
        }
    }
    async fn reclaim_managed(&self, service: ManagedService) -> Result<(), EngineError> {
        assert_eq!(service, ManagedService::StaticHttp);
        self.static_reclaims.fetch_add(1, Ordering::SeqCst);
        if self.fail_reclaim.load(Ordering::SeqCst) {
            return Err(EngineError::Other("helper cleanup failed".into()));
        }
        Ok(())
    }
    fn name(&self) -> &str {
        &self.name
    }
}
#[derive(Default)]
struct Engine {
    handles: Mutex<HashMap<String, Arc<Handle>>>,
    creates: AtomicUsize,
    starts: AtomicUsize,
    removes: AtomicUsize,
    fail_start: AtomicBool,
    partial_create_error: AtomicBool,
    create_pause: Mutex<Option<Arc<Semaphore>>>,
    create_entered: Notify,
    volumes: Mutex<HashSet<String>>,
}
#[async_trait]
impl AgentEngine for Engine {
    async fn create_sandbox(
        &self,
        name: &str,
        _: &WorkspaceOpts,
    ) -> Result<Arc<dyn AgentHandle>, EngineError> {
        // Force callers to overlap inside the backend; immediate mocks hid the
        // old check/create/insert race.
        tokio::task::yield_now().await;
        if let Some(gate) = self.create_pause.lock().await.clone() {
            self.create_entered.notify_one();
            gate.acquire().await.unwrap().forget();
        }
        let mut handles = self.handles.lock().await;
        if handles.contains_key(name) {
            return Err(EngineError::AlreadyExists(name.into()));
        }
        self.creates.fetch_add(1, Ordering::SeqCst);
        let h = Arc::new(Handle::new(name));
        handles.insert(name.into(), h.clone());
        if self.partial_create_error.load(Ordering::SeqCst) {
            return Err(EngineError::Other(
                "VM started but acknowledgement lost".into(),
            ));
        }
        Ok(h)
    }
    async fn get_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
        let handle = self
            .handles
            .lock()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| EngineError::NotFound(name.into()))?;
        if handle.paused.load(Ordering::SeqCst) {
            return Err(EngineError::SandboxPaused(name.into()));
        }
        if handle.transitioning.load(Ordering::SeqCst) {
            return Err(EngineError::SandboxTransitioning(name.into()));
        }
        Ok(handle)
    }
    async fn start_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        if self.fail_start.load(Ordering::SeqCst) {
            return Err(EngineError::Other("transient start error".into()));
        }
        tokio::task::yield_now().await;
        let h = self
            .handles
            .lock()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| EngineError::NotFound(name.into()))?;
        h.running.store(true, Ordering::SeqCst);
        h.paused.store(false, Ordering::SeqCst);
        h.transitioning.store(false, Ordering::SeqCst);
        Ok(h)
    }
    async fn remove_sandbox(&self, name: &str) -> Result<(), EngineError> {
        self.removes.fetch_add(1, Ordering::SeqCst);
        self.handles.lock().await.remove(name);
        Ok(())
    }
    async fn list_volumes(&self) -> Result<Vec<String>, EngineError> {
        Ok(self.volumes.lock().await.iter().cloned().collect())
    }
    async fn remove_volume(&self, name: &str) -> Result<(), EngineError> {
        self.volumes.lock().await.remove(name);
        Ok(())
    }
    async fn list_running(&self) -> Result<Vec<String>, EngineError> {
        Ok(self.handles.lock().await.keys().cloned().collect())
    }
}
fn setup(limit: u32) -> (Arc<Engine>, Arc<SessionManager>) {
    let engine = Arc::new(Engine::default());
    let mgr = Arc::new(SessionManager::new(
        engine.clone(),
        limit,
        WorkspaceOpts {
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
    ));
    (engine, mgr)
}
fn config() -> LifecycleConfig {
    LifecycleConfig {
        session_timeout_secs: 1,
        idle_timeout_secs: 2,
        check_interval_secs: 1,
    }
}

#[tokio::test]
async fn restored_desktop_binds_a_fresh_runtime_and_control_owner() {
    let (_, mgr) = setup(2);
    let old = mgr.desktop("a").await.unwrap();
    mgr.get_or_create("a").await.unwrap();
    old.control.take("viewer").await.unwrap();
    mgr.revoke_workspace("a").await.unwrap();
    let fresh = mgr.get_or_create("a").await.unwrap();
    fresh
        .exec("new-owner", Duration::from_secs(1))
        .await
        .unwrap();
    let restored = mgr.desktop("a").await.unwrap();
    assert!(!Arc::ptr_eq(&old, &restored));
    assert!(old
        .session
        .exec("retired", Duration::from_secs(1))
        .await
        .is_err());
    restored
        .session
        .exec("desktop-path", Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(
        restored.control.view("viewer").control,
        agent_engine::control::Mode::Ai
    );
    assert!(restored.control.begin().is_ok());
}

#[tokio::test]
async fn deleting_recorded_resources_releases_desktop_cache_and_image_projection() {
    let (_, mgr) = setup(2);
    mgr.register_image("a", "old-image").unwrap();
    let hub = mgr.desktop("a").await.unwrap();
    let weak = Arc::downgrade(&hub);
    drop(hub);
    mgr.destroy_recorded_resources("a", &[]).await.unwrap();
    assert!(weak.upgrade().is_none());
    mgr.register_image("a", "new-image").unwrap();
    assert!(mgr.desktop("a").await.is_ok());
}
fn valid() -> HashSet<String> {
    ["a".into(), "b".into()].into_iter().collect()
}
async fn expire(mgr: &SessionManager, name: &str) {
    let old = Instant::now() - Duration::from_secs(10);
    mgr.set_session_last_request(name, old).await;
    mgr.set_session_last_work(name, old).await;
}

#[tokio::test]
async fn concurrent_connections_share_one_queue_and_one_activation() {
    let (engine, mgr) = setup(2);
    let (a, b) = tokio::join!(mgr.get_or_create("a"), mgr.get_or_create("a"));
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
    assert_eq!(engine.starts.load(Ordering::SeqCst), 0);
    let h = engine.handles.lock().await["a"].clone();
    let task = tokio::spawn(async move { a.exec("blocked", Duration::from_secs(10)).await });
    h.entered.notified().await;
    assert!(tokio::time::timeout(
        Duration::from_millis(30),
        b.exec("second", Duration::from_secs(1))
    )
    .await
    .is_err());
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    h.release.add_permits(1);
    task.await.unwrap().unwrap();
    b.exec("third", Duration::from_secs(1)).await.unwrap();
    assert_eq!(
        h.calls.load(Ordering::SeqCst),
        2,
        "cancelled queued request must not execute"
    );
}

#[tokio::test]
async fn active_capacity_is_atomic_and_sleep_releases_it() {
    let (engine, mgr) = setup(1);
    let (a, b) = tokio::join!(mgr.get_or_create("a"), mgr.get_or_create("b"));
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
    let (winner, loser) = if a.is_ok() { ("a", "b") } else { ("b", "a") };
    mgr.stop_workspace(winner).await.unwrap();
    mgr.get_or_create(loser).await.unwrap();
    assert!(matches!(
        mgr.get_or_create(winner).await,
        Err(EngineError::WorkspaceCapacityExhausted)
    ));
    mgr.stop_workspace(loser).await.unwrap();
    mgr.get_or_create(winner).await.unwrap();
    assert_eq!(mgr.session_count().await, 2);
}

#[tokio::test]
async fn paused_workspace_keeps_capacity_and_next_tool_resumes_it() {
    let (engine, mgr) = setup(1);
    let a = mgr.get_or_create("a").await.unwrap();
    let handle = engine.handles.lock().await["a"].clone();
    handle.paused.store(true, Ordering::SeqCst);

    assert!(matches!(
        mgr.get_or_create("b").await,
        Err(EngineError::WorkspaceCapacityExhausted)
    ));
    a.exec("resume", Duration::from_secs(1)).await.unwrap();
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
    assert_eq!(a.workspace_status().await, WorkspaceStatus::Running);
}

#[tokio::test]
async fn failed_adoption_cleanup_is_retried_before_new_work() {
    let (engine, mgr) = setup(1);
    let handle = Arc::new(Handle::new("a"));
    handle.fail_reclaim.store(true, Ordering::SeqCst);
    engine
        .handles
        .lock()
        .await
        .insert("a".into(), handle.clone());
    mgr.adopt_running(&valid()).await.unwrap();
    assert!(mgr.get_or_create("a").await.is_err());
    assert_eq!(handle.static_reclaims.load(Ordering::SeqCst), 2);
    handle.fail_reclaim.store(false, Ordering::SeqCst);
    mgr.get_or_create("a").await.unwrap();
    assert_eq!(handle.static_reclaims.load(Ordering::SeqCst), 3);
    mgr.get_or_create("a").await.unwrap();
    assert_eq!(handle.static_reclaims.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn startup_adopts_paused_workspace_without_waking_or_losing_capacity() {
    let (engine, mgr) = setup(1);
    let handle = Arc::new(Handle::new("a"));
    handle.paused.store(true, Ordering::SeqCst);
    engine
        .handles
        .lock()
        .await
        .insert("a".into(), handle.clone());

    mgr.adopt_running(&valid()).await.unwrap();
    let adopted = mgr.get_session("a").await.unwrap();
    assert_eq!(adopted.workspace_status().await, WorkspaceStatus::Paused);
    assert!(matches!(
        mgr.get_or_create("b").await,
        Err(EngineError::WorkspaceCapacityExhausted)
    ));
    assert!(handle.paused.load(Ordering::SeqCst));
    assert_eq!(handle.static_reclaims.load(Ordering::SeqCst), 0);

    mgr.get_or_create("a").await.unwrap();
    assert!(!handle.paused.load(Ordering::SeqCst));
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
    assert_eq!(handle.static_reclaims.load(Ordering::SeqCst), 1);
    mgr.get_or_create("a").await.unwrap();
    assert_eq!(handle.static_reclaims.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn adopted_workspace_that_resumes_externally_attaches_for_guest_tcp() {
    let (engine, mgr) = setup(1);
    let handle = Arc::new(Handle::new("a"));
    handle.paused.store(true, Ordering::SeqCst);
    engine
        .handles
        .lock()
        .await
        .insert("a".into(), handle.clone());

    mgr.adopt_running(&valid()).await.unwrap();
    let adopted = mgr.get_session("a").await.unwrap();
    assert_eq!(adopted.workspace_status().await, WorkspaceStatus::Paused);
    handle.paused.store(false, Ordering::SeqCst);

    adopted.probe_guest_tcp(3000).await.unwrap();
    assert_eq!(engine.starts.load(Ordering::SeqCst), 0);
    assert_eq!(engine.creates.load(Ordering::SeqCst), 0);
    assert_eq!(adopted.workspace_status().await, WorkspaceStatus::Running);
    assert_eq!(handle.static_reclaims.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn startup_adopts_transitioning_workspace_and_tool_converges_it() {
    let (engine, mgr) = setup(1);
    let handle = Arc::new(Handle::new("a"));
    handle.transitioning.store(true, Ordering::SeqCst);
    engine
        .handles
        .lock()
        .await
        .insert("a".into(), handle.clone());

    mgr.adopt_running(&valid()).await.unwrap();
    let adopted = mgr.get_session("a").await.unwrap();
    assert_eq!(
        adopted.workspace_status().await,
        WorkspaceStatus::Transitioning
    );
    assert!(matches!(
        mgr.get_or_create("b").await,
        Err(EngineError::WorkspaceCapacityExhausted)
    ));
    mgr.get_or_create("a").await.unwrap();
    assert!(!handle.transitioning.load(Ordering::SeqCst));
    assert_eq!(handle.static_reclaims.load(Ordering::SeqCst), 1);
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn busy_command_survives_reclaim_and_cancelled_http_waiter() {
    let (engine, mgr) = setup(2);
    let a = mgr.get_or_create("a").await.unwrap();
    mgr.get_or_create("b").await.unwrap();
    let h = engine.handles.lock().await["a"].clone();
    let task = tokio::spawn(async move { a.exec("blocked", Duration::from_secs(20)).await });
    h.entered.notified().await;
    task.abort();
    let _ = task.await;
    expire(&mgr, "a").await;
    expire(&mgr, "b").await;
    mgr.check_and_reclaim(&config(), &valid()).await.unwrap();
    assert_eq!(h.stops.load(Ordering::SeqCst), 0);
    assert_eq!(
        engine.handles.lock().await["b"]
            .stops
            .load(Ordering::SeqCst),
        1
    );
    h.release.add_permits(1);
    let a = mgr.get_session("a").await.unwrap();
    a.exec("next", Duration::from_secs(1)).await.unwrap();
    assert!(a.last_request().await.elapsed() < Duration::from_secs(1));
    mgr.check_and_reclaim(&config(), &valid()).await.unwrap();
    assert_eq!(h.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn request_arriving_during_stop_waits_then_wakes_same_session() {
    let (engine, mgr) = setup(1);
    let a = mgr.get_or_create("a").await.unwrap();
    let h = engine.handles.lock().await["a"].clone();
    expire(&mgr, "a").await;
    h.block_stop.store(true, Ordering::SeqCst);
    let m = mgr.clone();
    let stop = tokio::spawn(async move { m.check_and_reclaim(&config(), &valid()).await });
    h.stop_entered.notified().await;
    let call = tokio::spawn(async move { a.exec("after-stop", Duration::from_secs(1)).await });
    tokio::task::yield_now().await;
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    h.stop_release.add_permits(1);
    stop.await.unwrap().unwrap();
    call.await.unwrap().unwrap();
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_wake_preserves_old_session_and_disks_without_recreation() {
    let (engine, mgr) = setup(1);
    let a = mgr.get_or_create("a").await.unwrap();
    mgr.stop_workspace("a").await.unwrap();
    engine.fail_start.store(true, Ordering::SeqCst);
    assert!(a.exec("first", Duration::from_secs(1)).await.is_err());
    assert!(mgr.get_or_create("a").await.is_err());
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
    assert_eq!(engine.removes.load(Ordering::SeqCst), 0);
    engine.fail_start.store(false, Ordering::SeqCst);
    a.exec("second", Duration::from_secs(1)).await.unwrap();
    assert_eq!(mgr.session_count().await, 1);
}

#[tokio::test]
async fn backend_deleted_between_status_and_start_is_recreated_before_exec() {
    let (engine, mgr) = setup(1);
    let session = mgr.get_or_create("a").await.unwrap();
    mgr.stop_workspace("a").await.unwrap();
    engine.handles.lock().await.remove("a");

    session
        .exec("after-external-delete", Duration::from_secs(1))
        .await
        .unwrap();

    assert_eq!(engine.creates.load(Ordering::SeqCst), 2);
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        engine.handles.lock().await["a"]
            .calls
            .load(Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn uncertain_exec_is_not_replayed_and_next_request_reconnects() {
    let (engine, mgr) = setup(1);
    let a = mgr.get_or_create("a").await.unwrap();
    assert!(a.exec("uncertain", Duration::from_secs(1)).await.is_err());
    assert_eq!(
        engine.handles.lock().await["a"]
            .calls
            .load(Ordering::SeqCst),
        1
    );
    assert_eq!(engine.starts.load(Ordering::SeqCst), 0);
    a.exec("next", Duration::from_secs(1)).await.unwrap();
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dav_closed_client_recovers_while_vm_and_exec_remain_healthy() {
    let (engine, mgr) = setup(1);
    let session = mgr.get_or_create("a").await.unwrap();
    session
        .exec("prepare", Duration::from_secs(1))
        .await
        .unwrap();
    let handle = engine.handles.lock().await["a"].clone();
    let launch = agent_engine::engine::ManagedLaunch::Webdav {
        host_binary: "fixture-helper".into(),
        owner: "a".into(),
        config: serde_json::json!({}),
    };
    let first = session
        .begin_dav_request(launch.clone(), Duration::from_secs(1))
        .await
        .unwrap();
    handle.tcp_error.store(1, Ordering::SeqCst);
    assert!(matches!(
        session.connect_dav_guest_tcp(1234).await,
        Err(EngineError::SandboxNotRunning(_))
    ));
    // The failed request is returned once; nothing is sent/replayed internally.
    assert_eq!(handle.tcp_calls.load(Ordering::SeqCst), 1);
    assert_eq!(engine.starts.load(Ordering::SeqCst), 0);
    assert_eq!(handle.status().await, WorkspaceStatus::Running);
    let next = session
        .begin_dav_request(launch, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(next.port, first.port);
    assert!(
        !first.cancel.is_cancelled(),
        "reattaching must preserve the helper epoch"
    );
    assert_eq!(handle.managed_starts.load(Ordering::SeqCst), 1);
    let _tcp = session.connect_dav_guest_tcp(1234).await.unwrap();
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
    assert_eq!(handle.stops.load(Ordering::SeqCst), 0);
    session.connect_dav_guest_tcp(1234).await.unwrap();
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
    handle.tcp_error.store(2, Ordering::SeqCst);
    assert!(session.connect_dav_guest_tcp(1234).await.is_err());
    session.connect_dav_guest_tcp(1234).await.unwrap();
    assert_eq!(
        engine.starts.load(Ordering::SeqCst),
        1,
        "peer refusal must not reattach VM"
    );
}

#[tokio::test]
async fn publication_closed_client_uses_the_same_recovery_owner() {
    let (engine, mgr) = setup(1);
    let session = mgr.get_or_create("a").await.unwrap();
    session
        .exec("prepare", Duration::from_secs(1))
        .await
        .unwrap();
    engine.handles.lock().await["a"]
        .tcp_error
        .store(1, Ordering::SeqCst);
    assert!(session.connect_guest_tcp(1234).await.is_err());
    session.connect_guest_tcp(1234).await.unwrap();
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn closed_client_during_helper_install_recovers_on_next_admission() {
    let (engine, mgr) = setup(1);
    let session = mgr.get_or_create("a").await.unwrap();
    session
        .exec("prepare", Duration::from_secs(1))
        .await
        .unwrap();
    let handle = engine.handles.lock().await["a"].clone();
    handle.fail_managed_start.store(true, Ordering::SeqCst);
    let launch = agent_engine::engine::ManagedLaunch::Webdav {
        host_binary: "fixture-helper".into(),
        owner: "a".into(),
        config: serde_json::json!({}),
    };
    assert!(matches!(
        session
            .begin_dav_request(launch.clone(), Duration::from_secs(1))
            .await,
        Err(EngineError::SandboxNotRunning(_))
    ));
    assert_eq!(engine.starts.load(Ordering::SeqCst), 0);
    session
        .begin_dav_request(launch, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(engine.starts.load(Ordering::SeqCst), 1);
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn removed_sessions_cannot_resurrect_resources() {
    let (engine, mgr) = setup(1);
    let a = mgr.get_or_create("a").await.unwrap();
    mgr.remove("a").await.unwrap();
    assert!(a.exec("forbidden", Duration::from_secs(1)).await.is_err());
    assert_eq!(engine.starts.load(Ordering::SeqCst), 0);
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn revoking_a_workspace_cancels_execution_and_retires_old_sessions() {
    let (engine, mgr) = setup(1);
    let old = mgr.get_or_create("a").await.unwrap();
    let handle = engine.handles.lock().await["a"].clone();
    let running_session = old.clone();
    let running = tokio::spawn(async move {
        running_session
            .exec("blocked", Duration::from_secs(30))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), handle.entered.notified())
        .await
        .unwrap();

    let queued_session = old.clone();
    let queued =
        tokio::spawn(async move { queued_session.exec("queued", Duration::from_secs(30)).await });
    tokio::task::yield_now().await;

    tokio::time::timeout(Duration::from_secs(2), mgr.revoke_workspace("a"))
        .await
        .unwrap()
        .unwrap();
    assert!(running.await.unwrap().is_err());
    assert!(queued.await.unwrap().is_err());
    assert_eq!(handle.calls.load(Ordering::SeqCst), 1);
    assert!(old.exec("forbidden", Duration::from_secs(1)).await.is_err());
    assert_eq!(mgr.session_count().await, 0);

    let fresh = mgr.get_or_create("a").await.unwrap();
    fresh.exec("new", Duration::from_secs(1)).await.unwrap();
    assert_eq!(handle.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_queued_request_rechecks_its_bound_credential_before_execution() {
    let (engine, mgr) = setup(1);
    let base = mgr.get_or_create("a").await.unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let current = allowed.clone();
    let checked = base.with_admission_check(move || {
        if current.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(EngineError::Other("credential revoked".into()))
        }
    });
    let handle = engine.handles.lock().await["a"].clone();
    let first_session = checked.clone();
    let first =
        tokio::spawn(async move { first_session.exec("blocked", Duration::from_secs(30)).await });
    tokio::time::timeout(Duration::from_secs(2), handle.entered.notified())
        .await
        .unwrap();
    let queued = tokio::spawn(async move { checked.exec("old-key", Duration::from_secs(1)).await });
    tokio::task::yield_now().await;
    allowed.store(false, Ordering::SeqCst);
    handle.release.add_permits(1);

    first.await.unwrap().unwrap();
    assert!(queued.await.unwrap().is_err());
    assert_eq!(handle.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn partial_activation_failure_retains_the_running_resource_slot() {
    let (engine, mgr) = setup(1);
    engine.partial_create_error.store(true, Ordering::SeqCst);
    assert!(mgr.get_or_create("a").await.is_err());
    assert!(matches!(
        mgr.get_or_create("b").await,
        Err(EngineError::WorkspaceCapacityExhausted)
    ));
    let a = mgr.get_session("a").await.unwrap();
    a.exec("next", Duration::from_secs(1)).await.unwrap();
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unknown_status_does_not_release_capacity_or_restart_the_vm() {
    let (engine, mgr) = setup(1);
    let a = mgr.get_or_create("a").await.unwrap();
    engine.handles.lock().await["a"]
        .unknown
        .store(true, Ordering::SeqCst);
    assert!(a.exec("next", Duration::from_secs(1)).await.is_err());
    assert!(matches!(
        mgr.get_or_create("b").await,
        Err(EngineError::WorkspaceCapacityExhausted)
    ));
    assert_eq!(engine.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn metadata_failure_preserves_workspace_and_recovers_without_replaying_commands() {
    let (engine, mgr) = setup(1);
    let a = mgr.get_or_create("a").await.unwrap();
    let handle = engine.handles.lock().await["a"].clone();
    handle.status_error.store(true, Ordering::SeqCst);

    let error = a
        .exec("write-once", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("database disk image is malformed"));
    assert_eq!(handle.calls.load(Ordering::SeqCst), 0);
    assert!(mgr.stop_workspace("a").await.is_err());
    assert_eq!(handle.stops.load(Ordering::SeqCst), 0);
    assert!(matches!(
        mgr.get_or_create("b").await,
        Err(EngineError::WorkspaceCapacityExhausted)
    ));

    handle.status_error.store(false, Ordering::SeqCst);
    a.exec("write-once", Duration::from_secs(1)).await.unwrap();
    assert_eq!(handle.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn startup_adoption_counts_existing_running_workspaces() {
    let (engine, mgr) = setup(1);
    engine
        .handles
        .lock()
        .await
        .insert("a".into(), Arc::new(Handle::new("a")));
    mgr.adopt_running(&valid()).await.unwrap();
    assert_eq!(engine.creates.load(Ordering::SeqCst), 0);
    assert_eq!(engine.starts.load(Ordering::SeqCst), 0);
    assert!(matches!(
        mgr.get_or_create("b").await,
        Err(EngineError::WorkspaceCapacityExhausted)
    ));
    mgr.stop_workspace("a").await.unwrap();
    mgr.get_or_create("b").await.unwrap();
}

#[tokio::test]
async fn cancellation_during_activation_does_not_submit_the_user_command() {
    let (engine, mgr) = setup(1);
    let gate = Arc::new(Semaphore::new(0));
    *engine.create_pause.lock().await = Some(gate.clone());
    let session = mgr.get_session("a").await.unwrap();
    let caller = session.clone();
    let task =
        tokio::spawn(async move { caller.exec("must-not-run", Duration::from_secs(1)).await });
    engine.create_entered.notified().await;
    task.abort();
    let _ = task.await;
    gate.add_permits(1);
    session.exec("next", Duration::from_secs(1)).await.unwrap();
    assert_eq!(
        engine.handles.lock().await["a"]
            .calls
            .load(Ordering::SeqCst),
        1
    );
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn removing_a_lazy_session_also_removes_persisted_backend_resources() {
    let (engine, mgr) = setup(1);
    let h = Arc::new(Handle::new("a"));
    h.running.store(false, Ordering::SeqCst);
    engine.handles.lock().await.insert("a".into(), h);
    let old = mgr.get_session("a").await.unwrap();
    mgr.remove("a").await.unwrap();
    assert!(!engine.handles.lock().await.contains_key("a"));
    assert!(old.exec("forbidden", Duration::from_secs(1)).await.is_err());
}

#[tokio::test]
async fn orphan_cleanup_uses_only_the_selected_engines_volume_store() {
    let (engine, mgr) = setup(1);
    *engine.volumes.lock().await = [
        "smcp-keep-ws".into(),
        "smcp-delete-ws".into(),
        "outside-ws".into(),
    ]
    .into_iter()
    .collect();
    let reported = mgr
        .report_unregistered_resources(
            &["smcp-keep".into()].into_iter().collect(),
            &["smcp-keep-ws".into()].into_iter().collect(),
        )
        .await
        .unwrap();
    assert!(reported.contains(&"smcp-delete-ws".to_string()));
    assert!(reported.contains(&"outside-ws".to_string()));
    let volumes = engine.volumes.lock().await;
    assert!(volumes.contains("smcp-keep-ws"));
    assert!(volumes.contains("outside-ws"));
    assert!(volumes.contains("smcp-delete-ws"));
}

#[tokio::test]
async fn failed_orphan_stop_protects_its_volume() {
    let (engine, mgr) = setup(1);
    mgr.get_or_create("smcp-deleted").await.unwrap();
    engine.handles.lock().await["smcp-deleted"]
        .fail_stop
        .store(true, Ordering::SeqCst);
    engine.volumes.lock().await.insert("smcp-deleted-ws".into());
    let reported = mgr
        .report_unregistered_resources(&HashSet::new(), &HashSet::new())
        .await
        .unwrap();
    assert!(reported.iter().any(|name| name.contains("smcp-deleted")));
    assert!(engine.volumes.lock().await.contains("smcp-deleted-ws"));
    assert_eq!(engine.removes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn blocked_command_in_one_space_does_not_block_another() {
    let (engine, manager) = setup(2);
    let a = manager.get_or_create("a").await.unwrap();
    let b = manager.get_or_create("b").await.unwrap();
    let handle = engine.handles.lock().await["a"].clone();
    let blocked = tokio::spawn(async move { a.exec("blocked", Duration::from_secs(5)).await });
    handle.entered.notified().await;
    let other = tokio::time::timeout(
        Duration::from_secs(1),
        b.exec("fast", Duration::from_secs(1)),
    )
    .await
    .expect("separate spaces must not share the execution queue")
    .unwrap();
    assert_eq!(other.exit_code, 0);
    assert!(!blocked.is_finished());
    handle.release.add_permits(1);
    assert_eq!(blocked.await.unwrap().unwrap().exit_code, 0);
    manager.stop_all().await;
}

// File requests use the same VM owner but a bounded concurrent lane.
async fn wait_calls(h: &Handle, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while h.calls.load(Ordering::SeqCst) < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn files_overlap_at_most_four_and_queued_timeout_never_executes() {
    let (engine, mgr) = setup(1);
    let s = mgr.get_or_create("a").await.unwrap();
    let h = engine.handles.lock().await["a"].clone();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move {
            s.exec_file("blocked", Duration::from_secs(10)).await
        }));
    }
    wait_calls(&h, 4).await;
    let error = s
        .exec_file("must-not-run", Duration::from_millis(40))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not started"));
    assert_eq!(h.calls.load(Ordering::SeqCst), 4);
    expire(&mgr, "a").await;
    mgr.check_and_reclaim(&config(), &valid()).await.unwrap();
    assert_eq!(
        h.stops.load(Ordering::SeqCst),
        0,
        "in-flight files retain activity"
    );
    h.release.add_permits(4);
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    s.exec_file("after", Duration::from_secs(1)).await.unwrap();
    assert_eq!(h.calls.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn desktop_control_never_blocks_basic_tools_and_bash_does_not_block_files() {
    let (engine, mgr) = setup(1);
    let s = mgr.get_or_create("a").await.unwrap();
    let h = engine.handles.lock().await["a"].clone();
    let gate = s.enable_desktop();
    let browser = gate.begin().unwrap();
    s.exec_file("file-during-browser", Duration::from_secs(1))
        .await
        .unwrap();
    s.exec("bash-during-browser", Duration::from_secs(1))
        .await
        .unwrap();
    drop(browser);
    gate.take("human").await.unwrap();
    let bash = {
        let s = s.clone();
        tokio::spawn(async move { s.exec("blocked", Duration::from_secs(10)).await })
    };
    h.entered.notified().await;
    s.exec_file("file-during-human-and-bash", Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(h.calls.load(Ordering::SeqCst), 4);
    h.release.add_permits(1);
    bash.await.unwrap().unwrap();
    assert_eq!(
        gate.view("human").control,
        agent_engine::control::Mode::Human
    );
}

#[tokio::test]
async fn abandoned_file_waiter_retains_ownership_and_revocation_cancels_all_files() {
    let (engine, mgr) = setup(1);
    let s = mgr.get_or_create("a").await.unwrap();
    let h = engine.handles.lock().await["a"].clone();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move {
            s.exec_file("blocked", Duration::from_secs(10)).await
        }));
    }
    wait_calls(&h, 4).await;
    tasks.remove(0).abort();
    expire(&mgr, "a").await;
    mgr.check_and_reclaim(&config(), &valid()).await.unwrap();
    assert_eq!(h.stops.load(Ordering::SeqCst), 0);
    let queued = {
        let s = s.clone();
        tokio::spawn(async move { s.exec_file("never", Duration::from_secs(10)).await })
    };
    tokio::task::yield_now().await;
    mgr.revoke_workspace("a").await.unwrap();
    for task in tasks {
        assert!(task.await.unwrap().is_err());
    }
    assert!(queued.await.unwrap().is_err());
    assert_eq!(h.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn file_deadline_during_activation_never_runs_late_and_owner_is_retained() {
    let (engine, mgr) = setup(1);
    let s = mgr.get_session("a").await.unwrap();
    let release = Arc::new(Semaphore::new(0));
    *engine.create_pause.lock().await = Some(release.clone());
    let call = {
        let s = s.clone();
        tokio::spawn(async move { s.exec_file("never", Duration::from_millis(50)).await })
    };
    engine.create_entered.notified().await;
    assert!(call
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("not started"));
    release.add_permits(1);
    s.exec_file("after", Duration::from_secs(2)).await.unwrap();
    let h = engine.handles.lock().await["a"].clone();
    assert_eq!(engine.creates.load(Ordering::SeqCst), 1);
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn queued_files_recheck_credentials_without_replaying_active_files() {
    let (engine, mgr) = setup(1);
    let s = mgr.get_or_create("a").await.unwrap();
    let h = engine.handles.lock().await["a"].clone();
    let mut running = Vec::new();
    for _ in 0..4 {
        let s = s.clone();
        running.push(tokio::spawn(async move {
            s.exec_file("blocked", Duration::from_secs(5)).await
        }));
    }
    wait_calls(&h, 4).await;
    let enabled = Arc::new(AtomicBool::new(true));
    let check = enabled.clone();
    let bound = s.with_admission_check(move || {
        if check.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(EngineError::Other("credential revoked".into()))
        }
    });
    let queued =
        tokio::spawn(async move { bound.exec_file("never", Duration::from_secs(5)).await });
    tokio::task::yield_now().await;
    enabled.store(false, Ordering::SeqCst);
    h.release.add_permits(4);
    for job in running {
        job.await.unwrap().unwrap();
    }
    assert!(queued
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("credential revoked"));
    assert_eq!(h.calls.load(Ordering::SeqCst), 4);
}
