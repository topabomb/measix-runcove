//! Read-only admin resource observations. No activation, activity lease or polling.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use crate::engine::{AgentEngine, WorkspaceOpts, WorkspaceStatus};

pub const RESOURCE_TIMEOUT: Duration = Duration::from_secs(3);
const CACHE_TTL: Duration = Duration::from_secs(5);
const MEMORY_FRESHNESS_MS: u64 = 5000;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation<T> {
    pub status: &'static str,
    pub value: Option<T>,
    pub observed_at: Option<u64>,
    pub reason: Option<String>,
}
impl<T> Observation<T> {
    pub fn current(value: T, observed_at: u64) -> Self {
        Self {
            status: "current",
            value: Some(value),
            observed_at: Some(observed_at),
            reason: None,
        }
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            status: "unavailable",
            value: None,
            observed_at: None,
            reason: Some(reason.into()),
        }
    }
    pub fn error(reason: impl Into<String>) -> Self {
        Self {
            status: "error",
            value: None,
            observed_at: None,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Allocation<T> {
    pub value: Option<T>,
    pub source: &'static str,
    pub error: Option<String>,
}
impl<T> Allocation<T> {
    pub fn known(value: T, source: &'static str) -> Self {
        Self {
            value: Some(value),
            source,
            error: None,
        }
    }
    pub fn error(error: impl Into<String>) -> Self {
        Self {
            value: None,
            source: "unknown",
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAllocation {
    pub cpu_cores: Allocation<u32>,
    pub memory_limit_bytes: Allocation<u64>,
    pub workspace_capacity_bytes: Allocation<u64>,
}
impl ResourceAllocation {
    pub fn error(reason: &str) -> Self {
        Self {
            cpu_cores: Allocation::error(reason),
            memory_limit_bytes: Allocation::error(reason),
            workspace_capacity_bytes: Allocation::error(reason),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub used_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSummary {
    pub agent_space_id: String,
    pub runtime: Observation<String>,
    pub allocation: ResourceAllocation,
    pub memory: Observation<u64>,
    pub disk: Observation<DiskUsage>,
}

/// Read via catalog metadata, never by attaching/activating a VM.
#[derive(Debug, Clone)]
pub struct ResourceInspection {
    pub status: WorkspaceStatus,
    pub run_identity: Option<String>,
    pub allocation: ResourceAllocation,
    pub workspace_disk_verified: bool,
}

#[derive(Default)]
struct CachedResources {
    last_disk: Option<Observation<DiskUsage>>,
    live: Option<(Instant, String, Observation<u64>, Observation<DiskUsage>)>,
}

pub struct ResourceReader {
    entries: Mutex<HashMap<String, Arc<AsyncMutex<CachedResources>>>>,
    slots: Semaphore,
}
impl Default for ResourceReader {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            slots: Semaphore::new(4),
        }
    }
}

impl ResourceReader {
    pub fn remove(&self, space: &str) {
        self.entries.lock().unwrap().remove(space);
    }

    pub async fn read(
        &self,
        engine: &dyn AgentEngine,
        space: &str,
        name: &str,
        volumes: &[String],
        defaults: &WorkspaceOpts,
        allow_guest: bool,
    ) -> ResourceSummary {
        let mut summary = ResourceSummary {
            agent_space_id: space.into(),
            runtime: Observation::error("collection_busy"),
            allocation: ResourceAllocation::error("collection_busy"),
            memory: Observation::unavailable("collection_busy"),
            disk: Observation::unavailable("collection_busy"),
        };
        let Ok(_permit) = self.slots.try_acquire() else {
            if let Some(entry) = self.entries.lock().unwrap().get(space) {
                if let Ok(cache) = entry.try_lock() {
                    summary.disk = historical_disk(&cache, "collection_busy", false);
                }
            }
            return summary;
        };
        let entry = self
            .entries
            .lock()
            .unwrap()
            .entry(space.into())
            .or_default()
            .clone();
        let Ok(mut cache) = tokio::time::timeout(RESOURCE_TIMEOUT, entry.lock()).await else {
            return summary;
        };
        let inspected = tokio::time::timeout(
            RESOURCE_TIMEOUT,
            engine.inspect_resources(name, volumes, defaults),
        )
        .await;
        let inspection = match inspected {
            Ok(Ok(value)) => value,
            _ => {
                summary.runtime = Observation::error("status_unavailable");
                summary.allocation = ResourceAllocation::error("inspection_failed");
                summary.memory = Observation::error("inspection_failed");
                summary.disk = historical_disk(&cache, "inspection_failed", true);
                cache.live = None;
                return summary;
            }
        };
        let status = format!("{:?}", inspection.status).to_ascii_lowercase();
        summary.runtime = Observation::current(status.clone(), now_ms());
        summary.allocation = inspection.allocation.clone();
        if inspection.status != WorkspaceStatus::Running || !allow_guest {
            let reason = if allow_guest {
                status.as_str()
            } else {
                "account_inactive"
            };
            summary.memory = Observation::unavailable(reason);
            summary.disk = historical_disk(&cache, reason, false);
            cache.live = None;
            return summary;
        }
        let Some(run) = inspection.run_identity.as_ref() else {
            summary.memory = Observation::error("run_identity_unavailable");
            summary.disk = historical_disk(&cache, "run_identity_unavailable", true);
            return summary;
        };
        if let Some((at, identity, memory, disk)) = &cache.live {
            if at.elapsed() < CACHE_TTL && identity == run && inspection.workspace_disk_verified {
                summary.memory = memory.clone();
                refresh_memory_age(&mut summary.memory);
                summary.disk = disk.clone();
                return summary;
            }
        }
        let memory = async {
            match tokio::time::timeout(RESOURCE_TIMEOUT, engine.get_metrics(name)).await {
                Ok(Ok(metrics)) => memory_observation(&metrics),
                Ok(Err(crate::engine::EngineError::MetricsUnavailable(reason))) => {
                    Observation::unavailable(reason)
                }
                Ok(Err(_)) => Observation::error("metrics_unavailable"),
                Err(_) => Observation::error("collection_timeout"),
            }
        };
        let disk = async {
            if !inspection.workspace_disk_verified {
                return Observation::error("workspace_disk_unverified");
            }
            match tokio::time::timeout(RESOURCE_TIMEOUT, engine.workspace_disk_usage(name, run))
                .await
            {
                Ok(Ok(value)) => Observation::current(value, now_ms()),
                Ok(Err(_)) => Observation::error("workspace_stat_failed"),
                Err(_) => Observation::error("collection_timeout"),
            }
        };
        let (memory, disk) = tokio::join!(memory, disk);
        let collected_at = Instant::now();
        // A stop/restart must not promote a result from the previous runtime.
        let after = tokio::time::timeout(
            RESOURCE_TIMEOUT,
            engine.inspect_resources(name, volumes, defaults),
        )
        .await;
        match after {
            Ok(Ok(after))
                if after.status == WorkspaceStatus::Running
                    && after.run_identity.as_ref() == Some(run) => {}
            Ok(Err(_)) | Err(_) => {
                summary.runtime = Observation::error("status_unavailable");
                summary.memory = Observation::error("inspection_failed");
                summary.disk = historical_disk(&cache, "inspection_failed", true);
                cache.live = None;
                return summary;
            }
            Ok(Ok(value)) => {
                summary.runtime = Observation::current(
                    format!("{:?}", value.status).to_ascii_lowercase(),
                    now_ms(),
                );
                summary.memory = Observation::unavailable("runtime_changed");
                summary.disk = historical_disk(&cache, "runtime_changed", false);
                cache.live = None;
                return summary;
            }
        }
        summary.memory = memory;
        // Disk collection and the final catalog check may outlive a fresh memory sample.
        refresh_memory_age(&mut summary.memory);
        if disk.status == "current" {
            cache.last_disk = Some(disk.clone());
            summary.disk = disk;
        } else {
            summary.disk = historical_disk(
                &cache,
                disk.reason.as_deref().unwrap_or("workspace_stat_failed"),
                true,
            );
        }
        cache.live = Some((
            collected_at,
            run.clone(),
            summary.memory.clone(),
            summary.disk.clone(),
        ));
        summary
    }
}

fn historical_disk(cache: &CachedResources, reason: &str, failed: bool) -> Observation<DiskUsage> {
    match cache.last_disk.clone() {
        Some(mut value) => {
            value.status = "historical";
            value.reason = Some(reason.into());
            value
        }
        None if failed => Observation::error(reason),
        None => Observation::unavailable(reason),
    }
}

fn memory_observation(metrics: &crate::engine::SandboxMetricsSnapshot) -> Observation<u64> {
    let Some(available) = metrics.memory_available_bytes else {
        return Observation::unavailable("guest_memory_unavailable");
    };
    let Some(used) = metrics.memory_limit_bytes.checked_sub(available) else {
        return Observation::error("invalid_memory_sample");
    };
    let Some(at) = metrics
        .observed_at_ms
        .filter(|at| *at > 0 && *at <= now_ms())
    else {
        return Observation::unavailable("sample_time_unavailable");
    };
    let mut observation = Observation::current(used, at);
    refresh_memory_age(&mut observation);
    observation
}

fn refresh_memory_age(observation: &mut Observation<u64>) {
    if observation.status == "current"
        && observation
            .observed_at
            .is_some_and(|at| now_ms().saturating_sub(at) > MEMORY_FRESHNESS_MS)
    {
        observation.status = "historical";
        observation.reason = Some("stale_msb_snapshot".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{AgentHandle, EngineError, SandboxMetricsSnapshot};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct TestEngine {
        status: Mutex<WorkspaceStatus>,
        run: AtomicUsize,
        disk_calls: AtomicUsize,
        fail_disk: AtomicBool,
        fail_memory: AtomicBool,
        memory_age_ms: AtomicUsize,
        fail_inspection: AtomicBool,
        hold_disk: AtomicBool,
        disk_started: tokio::sync::Notify,
        disk_release: tokio::sync::Notify,
    }
    impl TestEngine {
        fn new() -> Self {
            Self {
                status: Mutex::new(WorkspaceStatus::Running),
                run: AtomicUsize::new(1),
                disk_calls: AtomicUsize::new(0),
                fail_disk: AtomicBool::new(false),
                fail_memory: AtomicBool::new(false),
                memory_age_ms: AtomicUsize::new(0),
                fail_inspection: AtomicBool::new(false),
                hold_disk: AtomicBool::new(false),
                disk_started: tokio::sync::Notify::new(),
                disk_release: tokio::sync::Notify::new(),
            }
        }
    }
    fn metrics() -> SandboxMetricsSnapshot {
        SandboxMetricsSnapshot {
            memory_available_bytes: Some(100),
            memory_limit_bytes: 256,
            observed_at_ms: Some(now_ms()),
        }
    }
    fn opts() -> WorkspaceOpts {
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
        }
    }
    #[async_trait]
    impl AgentEngine for TestEngine {
        async fn create_sandbox(
            &self,
            _: &str,
            _: &WorkspaceOpts,
        ) -> Result<Arc<dyn AgentHandle>, EngineError> {
            panic!("observation activated VM")
        }
        async fn get_sandbox(&self, _: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
            panic!("observation attached through lifecycle API")
        }
        async fn start_sandbox(&self, _: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
            panic!("observation started VM")
        }
        async fn remove_sandbox(&self, _: &str) -> Result<(), EngineError> {
            Ok(())
        }
        async fn list_running(&self) -> Result<Vec<String>, EngineError> {
            Ok(vec![])
        }
        async fn inspect_resources(
            &self,
            _: &str,
            _: &[String],
            _: &WorkspaceOpts,
        ) -> Result<ResourceInspection, EngineError> {
            if self.fail_inspection.load(Ordering::SeqCst) {
                return Err(EngineError::Other("inspection fault".into()));
            }
            Ok(ResourceInspection {
                status: self.status.lock().unwrap().clone(),
                run_identity: Some(self.run.load(Ordering::SeqCst).to_string()),
                allocation: ResourceAllocation {
                    cpu_cores: Allocation::known(2, "sandbox"),
                    memory_limit_bytes: Allocation::known(256, "sandbox"),
                    workspace_capacity_bytes: Allocation::known(1024, "volume"),
                },
                workspace_disk_verified: true,
            })
        }
        async fn get_metrics(&self, _: &str) -> Result<SandboxMetricsSnapshot, EngineError> {
            if self.fail_memory.load(Ordering::SeqCst) {
                return Err(EngineError::MetricsUnavailable("not_sampled".into()));
            }
            let mut sample = metrics();
            sample.observed_at_ms =
                Some(now_ms() - self.memory_age_ms.load(Ordering::SeqCst) as u64);
            Ok(sample)
        }
        async fn workspace_disk_usage(&self, _: &str, _: &str) -> Result<DiskUsage, EngineError> {
            self.disk_calls.fetch_add(1, Ordering::SeqCst);
            if self.hold_disk.load(Ordering::SeqCst) {
                self.disk_started.notify_one();
                self.disk_release.notified().await;
            }
            if self.fail_disk.load(Ordering::SeqCst) {
                return Err(EngineError::Other("disk fault".into()));
            }
            Ok(DiskUsage {
                used_bytes: 0,
                available_bytes: 1024,
            })
        }
    }

    async fn read(reader: &ResourceReader, engine: &TestEngine, space: &str) -> ResourceSummary {
        reader
            .read(engine, space, "test", &["test-ws".into()], &opts(), true)
            .await
    }
    async fn expire(reader: &ResourceReader, space: &str) {
        let entry = reader.entries.lock().unwrap().get(space).unwrap().clone();
        if let Some(live) = &mut entry.lock().await.live {
            live.0 -= CACHE_TTL;
        };
    }

    #[test]
    fn memory_distinguishes_missing_invalid_zero_and_stale() {
        let mut sample = metrics();
        assert_eq!(memory_observation(&sample).value, Some(156));
        sample.memory_available_bytes = None;
        assert_eq!(memory_observation(&sample).status, "unavailable");
        sample.memory_available_bytes = Some(257);
        assert_eq!(memory_observation(&sample).status, "error");
        sample.memory_available_bytes = Some(256);
        assert_eq!(memory_observation(&sample).value, Some(0));
        sample.observed_at_ms = Some(now_ms() - 10_000);
        assert_eq!(memory_observation(&sample).status, "historical");
        sample.observed_at_ms = None;
        assert_eq!(memory_observation(&sample).status, "unavailable");
    }

    #[tokio::test]
    async fn partial_failures_preserve_other_fields_and_last_disk_time() {
        let reader = ResourceReader::default();
        let engine = TestEngine::new();
        engine.fail_disk.store(true, Ordering::SeqCst);
        let first = read(&reader, &engine, "s1").await;
        assert_eq!(first.disk.status, "error");
        assert!(first.disk.value.is_none());
        assert_eq!(first.memory.value, Some(156));
        assert_eq!(first.allocation.cpu_cores.value, Some(2));
        expire(&reader, "s1").await;
        engine.fail_disk.store(false, Ordering::SeqCst);
        engine.fail_memory.store(true, Ordering::SeqCst);
        let good = read(&reader, &engine, "s1").await;
        assert_eq!(good.disk.status, "current");
        assert_eq!(good.disk.value.as_ref().unwrap().used_bytes, 0);
        assert_eq!(good.memory.status, "unavailable");
        assert!(good.memory.value.is_none());
        expire(&reader, "s1").await;
        engine.fail_disk.store(true, Ordering::SeqCst);
        let failed = read(&reader, &engine, "s1").await;
        assert_eq!(failed.disk.status, "historical");
        assert_eq!(failed.disk.observed_at, good.disk.observed_at);
        assert_eq!(failed.disk.reason.as_deref(), Some("workspace_stat_failed"));
        engine.fail_inspection.store(true, Ordering::SeqCst);
        let unknown = read(&reader, &engine, "s1").await;
        assert_eq!(unknown.runtime.status, "error");
        assert!(unknown.runtime.value.is_none());
        assert_eq!(unknown.disk.status, "historical");
    }

    #[tokio::test]
    async fn stopped_paused_and_recreated_spaces_do_not_collect_or_reuse_live_values() {
        let reader = ResourceReader::default();
        let engine = TestEngine::new();
        let original = read(&reader, &engine, "old").await;
        for status in [
            WorkspaceStatus::Stopped,
            WorkspaceStatus::Paused,
            WorkspaceStatus::Transitioning,
            WorkspaceStatus::NotFound,
            WorkspaceStatus::Unknown,
        ] {
            *engine.status.lock().unwrap() = status;
            let response = read(&reader, &engine, "old").await;
            assert_eq!(response.disk.status, "historical");
            assert_eq!(response.disk.observed_at, original.disk.observed_at);
            assert_eq!(response.memory.status, "unavailable");
        }
        assert_eq!(engine.disk_calls.load(Ordering::SeqCst), 1);
        reader.remove("old");
        assert_eq!(
            read(&reader, &engine, "new").await.disk.status,
            "unavailable"
        );
        assert!(!reader.entries.lock().unwrap().contains_key("old"));
    }

    #[tokio::test]
    async fn concurrent_queries_share_cache_and_runtime_changes_discard_late_samples() {
        let reader = Arc::new(ResourceReader::default());
        let engine = Arc::new(TestEngine::new());
        engine.hold_disk.store(true, Ordering::SeqCst);
        let r = reader.clone();
        let e = engine.clone();
        let task = tokio::spawn(async move { read(&r, &e, "space").await });
        engine.disk_started.notified().await;
        let r = reader.clone();
        let e = engine.clone();
        let duplicate = tokio::spawn(async move { read(&r, &e, "space").await });
        engine.disk_release.notify_one();
        assert_eq!(task.await.unwrap().disk.status, "current");
        assert_eq!(duplicate.await.unwrap().disk.status, "current");
        assert_eq!(engine.disk_calls.load(Ordering::SeqCst), 1);
        expire(&reader, "space").await;
        let r = reader.clone();
        let e = engine.clone();
        let task = tokio::spawn(async move { read(&r, &e, "space").await });
        engine.disk_started.notified().await;
        engine.run.fetch_add(1, Ordering::SeqCst);
        engine.disk_release.notify_one();
        let late = task.await.unwrap();
        assert_eq!(late.disk.status, "historical");
        assert_eq!(late.disk.reason.as_deref(), Some("runtime_changed"));
        assert!(late.memory.value.is_none());
    }

    #[tokio::test]
    async fn memory_ages_while_waiting_for_disk_collection() {
        let reader = Arc::new(ResourceReader::default());
        let engine = Arc::new(TestEngine::new());
        engine.memory_age_ms.store(4500, Ordering::SeqCst);
        engine.hold_disk.store(true, Ordering::SeqCst);
        let r = reader.clone();
        let e = engine.clone();
        let task = tokio::spawn(async move { read(&r, &e, "space").await });
        engine.disk_started.notified().await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        engine.disk_release.notify_one();
        let response = task.await.unwrap();
        assert_eq!(response.memory.status, "historical");
        assert_eq!(
            response.memory.reason.as_deref(),
            Some("stale_msb_snapshot")
        );
        assert_eq!(response.memory.value, Some(156));
        assert_eq!(response.disk.status, "current");
    }

    #[tokio::test]
    async fn failed_final_inspection_is_an_error_and_preserves_only_verified_history() {
        for with_history in [false, true] {
            let reader = Arc::new(ResourceReader::default());
            let engine = Arc::new(TestEngine::new());
            let old_time = if with_history {
                let original = read(&reader, &engine, "space").await;
                expire(&reader, "space").await;
                original.disk.observed_at
            } else {
                None
            };
            engine.hold_disk.store(true, Ordering::SeqCst);
            let r = reader.clone();
            let e = engine.clone();
            let task = tokio::spawn(async move { read(&r, &e, "space").await });
            engine.disk_started.notified().await;
            engine.fail_inspection.store(true, Ordering::SeqCst);
            engine.disk_release.notify_one();
            let response = task.await.unwrap();
            assert_eq!(response.runtime.status, "error");
            assert_eq!(response.memory.status, "error");
            assert!(response.memory.value.is_none());
            assert_eq!(
                response.disk.status,
                if with_history { "historical" } else { "error" }
            );
            assert_eq!(response.disk.reason.as_deref(), Some("inspection_failed"));
            assert_eq!(response.disk.observed_at, old_time);
            let entry = reader.entries.lock().unwrap().get("space").unwrap().clone();
            assert!(entry.lock().await.live.is_none());
        }
    }

    #[tokio::test]
    async fn collection_timeout_and_global_limit_are_bounded() {
        let reader = Arc::new(ResourceReader::default());
        let engine = Arc::new(TestEngine::new());
        let original = read(&reader, &engine, "saved").await;
        engine.hold_disk.store(true, Ordering::SeqCst);
        let mut tasks = Vec::new();
        for index in 0..4 {
            let r = reader.clone();
            let e = engine.clone();
            tasks.push(tokio::spawn(async move {
                read(&r, &e, &format!("space-{index}")).await
            }));
            engine.disk_started.notified().await;
        }
        let busy = read(&reader, &engine, "saved").await;
        assert_eq!(busy.disk.status, "historical");
        assert_eq!(busy.disk.observed_at, original.disk.observed_at);
        assert_eq!(busy.disk.reason.as_deref(), Some("collection_busy"));
        for task in tasks {
            let value = task.await.unwrap();
            assert_eq!(value.disk.status, "error");
            assert_eq!(value.disk.reason.as_deref(), Some("collection_timeout"));
            assert_eq!(value.memory.value, Some(156));
        }
        assert_eq!(reader.slots.available_permits(), 4);
    }

    #[tokio::test]
    async fn deleting_an_inflight_entry_cannot_reinsert_it() {
        let reader = Arc::new(ResourceReader::default());
        let engine = Arc::new(TestEngine::new());
        engine.hold_disk.store(true, Ordering::SeqCst);
        let r = reader.clone();
        let e = engine.clone();
        let task = tokio::spawn(async move { read(&r, &e, "old").await });
        engine.disk_started.notified().await;
        reader.remove("old");
        engine.disk_release.notify_one();
        task.await.unwrap();
        assert!(!reader.entries.lock().unwrap().contains_key("old"));
        *engine.status.lock().unwrap() = WorkspaceStatus::NotFound;
        assert!(read(&reader, &engine, "new").await.disk.value.is_none());
    }

    #[tokio::test]
    async fn user_detail_rechecks_identity_after_inflight_delete_and_recreation() {
        use crate::{
            session::SessionManager,
            state::{StateError, StateStore},
            workspace_service::{ServiceError, WorkspaceService},
        };
        let dir = std::env::temp_dir().join(format!("resource-race-{}", uuid::Uuid::new_v4()));
        StateStore::init(&dir).unwrap();
        let state = Arc::new(StateStore::open(&dir).unwrap());
        let engine = Arc::new(TestEngine::new());
        let sessions = Arc::new(SessionManager::new(engine.clone(), 1, opts()));
        let service = Arc::new(WorkspaceService::new(
            state.clone(),
            sessions.clone(),
            "test",
            "http://localhost",
        ));
        let old = service.create_user("alice").unwrap();
        engine.hold_disk.store(true, Ordering::SeqCst);
        let pending = service.clone();
        let task = tokio::spawn(async move { pending.get_user("alice").await });
        engine.disk_started.notified().await;
        state.begin_delete("alice", &old.agent_space_id).unwrap();
        state.finish_delete("alice", &old.agent_space_id).unwrap();
        let new = service.create_user("alice").unwrap();
        engine.disk_release.notify_one();
        assert!(matches!(
            task.await.unwrap(),
            Err(ServiceError::State(StateError::SpaceMismatch))
        ));
        *engine.status.lock().unwrap() = WorkspaceStatus::Stopped;
        let view = service.get_user("alice").await.unwrap();
        assert_eq!(view.agent_space_id, new.agent_space_id);
        assert!(view.resources.unwrap().disk.value.is_none());
        assert_eq!(sessions.session_count().await, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
