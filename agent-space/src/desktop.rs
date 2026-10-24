//! Host-side browser jobs share the SessionManager's workspace and activity.
//! Guest supervision owns hard deadlines; disconnects never replay scripts.
#[path = "task_summary.rs"]
mod task_summary;

use crate::{
    control::ControlGate,
    engine::{EngineError, ManagedInstance, ManagedLaunch},
    session::Session,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, Notify},
};

pub struct DesktopHub {
    pub session: Session,
    pub control: Arc<ControlGate>,
    helper: Mutex<Option<ManagedInstance>>,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    captures: Mutex<Vec<Value>>,
    browser: crate::config::BrowserConfig,
}
// Server policy, not model-selectable execution parameters.
pub const ACTION_BATCH_TIMEOUT_SECS: u64 = 120;
const INITIAL_WAIT_SECS: u64 = 30;
struct Job {
    id: String,
    accepted: Instant,
    started_at: u64,
    report_started_at: bool,
    report_duration: bool,
    tool: &'static str,
    description: String,
    status: Mutex<Value>,
    done: Notify,
    cancel: tokio_util::sync::CancellationToken,
    finished: Mutex<Option<Instant>>,
}
#[derive(serde::Serialize)]
pub struct ScreenOptions {
    pub view: String,
    pub page_id: Option<String>,
    pub count: usize,
    pub interval_ms: u64,
    pub save_original: bool,
    pub save_dir: Option<String>,
}
impl DesktopHub {
    pub fn new(session: Session, browser: crate::config::BrowserConfig) -> Self {
        Self {
            control: session.enable_desktop(),
            session,
            helper: Mutex::new(None),
            jobs: Mutex::new(HashMap::new()),
            captures: Mutex::new(vec![]),
            browser,
        }
    }
    async fn ensure(&self) -> Result<u16, EngineError> {
        let mut helper = self.helper.lock().await;
        if let Some(instance) = helper.as_ref() {
            if request(
                &self.session,
                instance.port,
                "GET",
                "/health",
                None,
                Duration::from_secs(2),
            )
            .await
            .is_ok()
            {
                return Ok(instance.port);
            }
        }
        let launch = ManagedLaunch::Desktop {
            host_binary: std::env::var("AGENT_SPACE_DESKTOP")
                .unwrap_or_else(|_| "/usr/local/bin/agent-space-desktop".into()),
            max_pages: self.browser.max_pages,
        };
        let instance = match self.session.start_managed(launch.clone()).await {
            Ok(instance) => instance,
            Err(EngineError::ManagedRestartRequired(old)) => {
                request(
                    &self.session,
                    old.port,
                    "POST",
                    "/reset",
                    Some(json!({})),
                    Duration::from_secs(10),
                )
                .await?;
                self.session.note_helper_cleanup(&old);
                self.session.stop_helper(&old).await?;
                self.session.clear_helper_cleanup(&old);
                self.session.start_managed(launch).await?
            }
            Err(error) => {
                if error.to_string().contains("CLEANUP_UNCONFIRMED") {
                    self.control.block();
                }
                return Err(error);
            }
        };
        self.session.clear_helper_cleanup(&instance);
        request(
            &self.session,
            instance.port,
            "POST",
            "/reset",
            Some(json!({})),
            Duration::from_secs(10),
        )
        .await?;
        let port = instance.port;
        self.session.reserve_desktop_port(port);
        *helper = Some(instance);
        Ok(port)
    }
    pub async fn health(&self) -> Value {
        let helper = self.helper.lock().await.clone();
        if let Some(instance) = helper {
            if matches!(
                self.session.workspace_status().await,
                crate::engine::WorkspaceStatus::Running
            ) {
                if let Ok(value) = request(
                    &self.session,
                    instance.port,
                    "GET",
                    "/health",
                    None,
                    Duration::from_secs(2),
                )
                .await
                {
                    return value;
                }
            }
        }
        json!({"desktop":false})
    }
    pub async fn viewer(&self) -> Result<Value, EngineError> {
        let _lease = self.session.activity_lease()?;
        let port = self.ensure().await?;
        request(
            &self.session,
            port,
            "POST",
            "/viewer",
            Some(json!({})),
            Duration::from_secs(35),
        )
        .await
    }
    pub async fn input(&self) -> Result<(), EngineError> {
        self.session.touch_work().await;
        if let Some(helper) = self.helper.lock().await.as_ref() {
            request(
                &self.session,
                helper.port,
                "POST",
                "/input",
                Some(json!({})),
                Duration::from_secs(2),
            )
            .await?;
        }
        Ok(())
    }
    /// Structured browser work shares the existing deadline, cancellation and activity owner.
    pub async fn browser(
        self: &Arc<Self>,
        admitted: Session,
        operation: &str,
        args: Value,
        page_id: Option<String>,
        timeout: u64,
    ) -> Result<Value, EngineError> {
        if !matches!(operation, "snapshot" | "act" | "read")
            || !(1..=600).contains(&timeout)
            || args.to_string().len() > 65536
        {
            return Err(EngineError::Other(
                "INVALID_ARGUMENT: invalid browser operation, deadline or request size".into(),
            ));
        }
        self.submit(
            admitted,
            json!({"operation":operation,"args":args,"page_id":page_id}),
            timeout,
            operation == "act",
        )
        .await
    }
    pub async fn screen(
        self: &Arc<Self>,
        admitted: Session,
        options: ScreenOptions,
    ) -> Result<Value, EngineError> {
        if !["page", "desktop"].contains(&options.view.as_str())
            || !(1..=8).contains(&options.count)
            || (options.count as u64 - 1).saturating_mul(options.interval_ms) >= 600000
            || options.save_dir.as_ref().is_some_and(|directory| {
                !options.save_original
                    || !std::path::Path::new(directory).is_absolute()
                    || directory.contains('\0')
            })
        {
            return Err(EngineError::Other(
                "INVALID_ARGUMENT: invalid screenshot count, schedule, or save directory".into(),
            ));
        }
        self.submit(
            admitted,
            serde_json::to_value(options).map_err(|e| EngineError::Other(e.to_string()))?,
            600,
            false,
        )
        .await
    }
    async fn submit(
        self: &Arc<Self>,
        admitted: Session,
        mut body: Value,
        timeout: u64,
        modifies: bool,
    ) -> Result<Value, EngineError> {
        let lease = admitted.activity_lease()?;
        let operation = if modifies {
            match self.control.begin() {
                Ok(operation) => Some(operation),
                Err(error) => {
                    let mut value = json!({"error":{"code":error,"message":error}});
                    if error == "BUSY" {
                        value["active_task_id"] = json!(self.control.active_task_id());
                    }
                    return Err(EngineError::Other(value.to_string()));
                }
            }
        } else {
            None
        };
        let cancellation = operation
            .as_ref()
            .map(|op| op.cancel.clone())
            .unwrap_or_default();
        let job = Arc::new(Job {
            id: operation
                .as_ref()
                .map(|operation| operation.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            accepted: Instant::now(),
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            report_duration: modifies,
            report_started_at: modifies || body["count"].as_u64().unwrap_or(1) > 1,
            tool: task_summary::tool(&body),
            description: task_summary::describe(&body),
            status: Mutex::new(json!({"state":"running","phase":"preparing","terminal":false})),
            done: Notify::new(),
            cancel: cancellation.clone(),
            finished: Mutex::new(None),
        });
        self.prune_jobs().await;
        self.jobs.lock().await.insert(job.id.clone(), job.clone());
        let hub = self.clone();
        let running = job.clone();
        tokio::spawn(async move {
            let _lease = lease;
            let mut operation = operation;
            let deadline =
                tokio::time::Instant::from_std(running.accepted) + Duration::from_secs(timeout);
            let mut submitted_port = None;
            let prepare = async {
                let port = hub.ensure().await?;
                admitted.authorize()?;
                if let Some(operation) = &operation {
                    operation
                        .check()
                        .map_err(|e| EngineError::Other(e.into()))?;
                }
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(EngineError::Timeout(
                        "browser preparation deadline exceeded".into(),
                    ));
                }
                body["task_id"] = json!(running.id);
                body["timeout_secs"] =
                    json!(remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0));
                if modifies {
                    body["captures"] = json!(hub.captures.lock().await.clone());
                }
                submitted_port = Some(port);
                request(
                    &hub.session,
                    port,
                    "POST",
                    if body.get("operation").is_some() {
                        "/run"
                    } else {
                        "/screen"
                    },
                    Some(body),
                    Duration::from_secs(3),
                )
                .await?;
                Ok::<_, EngineError>(port)
            };
            let prepared = tokio::select! {
                result=prepare=>result,
                _=cancellation.cancelled()=>Err(EngineError::Other("EXECUTION_CANCELLED: not started".into())),
                _=tokio::time::sleep_until(deadline)=>Err(EngineError::Timeout("browser preparation deadline exceeded".into())),
            };
            let outcome = match prepared {
                Err(error) => {
                    if let Some(port) = submitted_port {
                        // An HTTP failure is not proof that the guest rejected the
                        // script. Cancel by ID and keep control locked unless the
                        // guest confirms cleanup; never replay it.
                        let _ = request(
                            &hub.session,
                            port,
                            "POST",
                            &format!("/task/{}/cancel", running.id),
                            Some(json!({})),
                            Duration::from_secs(2),
                        )
                        .await;
                        let cleaned = tokio::time::timeout(Duration::from_secs(8), async {
                            loop {
                                if request(
                                    &hub.session,
                                    port,
                                    "GET",
                                    &format!("/task/{}?summary=true", running.id),
                                    None,
                                    Duration::from_secs(2),
                                )
                                .await
                                .is_ok_and(|v| {
                                    v["cleanup_confirmed"] == true && v["terminal"] == true
                                }) {
                                    return;
                                }
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                        })
                        .await
                        .is_ok();
                        if !cleaned {
                            if let Some(operation) = &operation {
                                operation.cleanup_failed();
                            }
                        }
                    }
                    Err(error)
                }
                Ok(port) => {
                    let poll_deadline = deadline + Duration::from_secs(13);
                    let mut cancel_sent = false;
                    loop {
                        if (cancellation.is_cancelled()
                            || tokio::time::Instant::now() >= deadline
                            || admitted.authorize().is_err())
                            && !cancel_sent
                        {
                            let _ = request(
                                &hub.session,
                                port,
                                "POST",
                                &format!("/task/{}/cancel", running.id),
                                Some(json!({})),
                                Duration::from_secs(2),
                            )
                            .await;
                            cancel_sent = true;
                        }
                        match request(
                            &hub.session,
                            port,
                            "GET",
                            &format!("/task/{}?summary=true", running.id),
                            None,
                            Duration::from_secs(3),
                        )
                        .await
                        {
                            Ok(mut value) => {
                                if value["terminal"] == true
                                    && value["state"] == "cancelled"
                                    && tokio::time::Instant::now() >= deadline
                                    && !cancellation.is_cancelled()
                                    && admitted.authorize().is_ok()
                                {
                                    value["state"] = json!("timed_out");
                                    value["error"] = json!("EXECUTION_TIMEOUT");
                                }
                                if value["terminal"] == true {
                                    if let Ok(details) = request(
                                        &hub.session,
                                        port,
                                        "GET",
                                        &format!("/task/{}", running.id),
                                        None,
                                        Duration::from_secs(5),
                                    )
                                    .await
                                    {
                                        if let Some(result) = details.get("result") {
                                            value["result"] = result.clone();
                                        }
                                    } else {
                                        value["output_warning"] = json!("Task ended, but its outputs could not be retrieved. Inspect saved artifacts before repeating actions.");
                                    }
                                }
                                *running.status.lock().await = value.clone();
                                if value["terminal"] == true {
                                    if value["cleanup_confirmed"] == false {
                                        if let Some(op) = &operation {
                                            op.cleanup_failed();
                                        }
                                    }
                                    break Ok(value);
                                }
                            }
                            Err(error) => {
                                // A failed status read does not prove the task failed.
                                // Keep observing this same task; explicit cancellation,
                                // authorization and the original deadline remain above.
                                tracing::warn!(task_id = %running.id, %error,
                                    "browser task status unavailable; retaining original deadline");
                            }
                        }
                        if tokio::time::Instant::now() >= poll_deadline {
                            if let Some(op) = &operation {
                                op.cleanup_failed();
                            }
                            break Err(EngineError::Timeout(
                                "guest task supervision result unavailable".into(),
                            ));
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            };
            if let Err(error) = outcome {
                let state = if matches!(error, EngineError::Timeout(_)) {
                    "timed_out"
                } else if cancellation.is_cancelled() {
                    "cancelled"
                } else {
                    "failed"
                };
                let mut status = json!({"task_id":running.id,"state":state,"terminal":true,"error":error.to_string(),"side_effects":if submitted_port.is_some(){"may_have_occurred"}else{"not_started"}});
                if !modifies {
                    status.as_object_mut().unwrap().remove("side_effects");
                }
                if hub.control.view("").control == crate::control::Mode::Blocked {
                    status["cleanup_confirmed"] = json!(false);
                }
                *running.status.lock().await = status;
            }
            operation.take();
            running
                .finished
                .lock()
                .await
                .get_or_insert_with(Instant::now);
            running.done.notify_waiters();
        });
        self.task(&job.id, INITIAL_WAIT_SECS, false).await
    }
    pub async fn task(&self, id: &str, wait: u64, cancel: bool) -> Result<Value, EngineError> {
        if wait > 30 {
            return Err(EngineError::Other("wait_secs must be 0..30".into()));
        }
        self.prune_jobs().await;
        let job = self.jobs.lock().await.get(id).cloned().ok_or_else(|| {
            EngineError::Other(
                "TASK_NOT_FOUND: This task record is unavailable. Check the current page and existing outputs before repeating the action."
                    .into(),
            )
        })?;
        if cancel {
            job.cancel.cancel();
        }
        if wait > 0 {
            let _ = tokio::time::timeout(Duration::from_secs(wait), async {
                loop {
                    let notified = job.done.notified();
                    if job.status.lock().await["terminal"] == true {
                        break;
                    }
                    notified.await;
                }
            })
            .await;
        }
        let status = job.status.lock().await.clone();
        let mut value = status;
        value["task_id"] = json!(id);
        if value["terminal"] == true {
            if let Some(result) = value.as_object_mut().and_then(|o| o.remove("result")) {
                if let Some(result) = result.as_object() {
                    for (key, item) in result {
                        // The guest owner publishes terminal errors and effect
                        // state. A partial worker checkpoint cannot replace a
                        // later cancellation/deadline/cleanup decision.
                        if !matches!(key.as_str(), "error" | "failed_step" | "side_effects") {
                            value[key] = item.clone();
                        }
                    }
                }
            }
            if value["cleanup_confirmed"] == true {
                value.as_object_mut().unwrap().remove("cleanup_confirmed");
            }
            if let Some(captures) = value["captures"].as_array() {
                for capture in captures {
                    self.record_capture(capture).await;
                }
            }
        }
        if job.report_started_at || value["state"] != "succeeded" {
            value["started_at"] = json!(job.started_at);
        }
        let finished = {
            let mut end = job.finished.lock().await;
            if value["state"] != "running" {
                end.get_or_insert_with(Instant::now);
            }
            *end
        };
        let duration = finished
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(job.accepted);
        if job.report_duration || value["state"] != "succeeded" {
            value["duration_secs"] = json!((duration.as_secs_f64() * 10.0).round() / 10.0);
        }
        value["control"] = json!({"mode":self.control.view("").control});
        value.as_object_mut().map(|o| o.remove("terminal"));
        Ok(value)
    }
    async fn record_capture(&self, capture: &Value) {
        if capture["source"] != "desktop" && capture["source"] != "page" {
            return;
        }
        let mut captures = self.captures.lock().await;
        captures.retain(|c| c["image_id"] != capture["image_id"]);
        captures.push(capture.clone());
        captures.sort_by_key(|capture| capture["at"].as_u64().unwrap_or_default());
        if captures.len() > 32 {
            captures.remove(0);
        }
    }
    pub async fn task_list(&self) -> Value {
        self.prune_jobs().await;
        let jobs: Vec<_> = self.jobs.lock().await.values().cloned().collect();
        let mut summaries = Vec::with_capacity(jobs.len());
        for job in jobs {
            let status = job.status.lock().await;
            summaries.push((
                status["state"] != "running",
                std::cmp::Reverse(job.accepted),
                json!({"task_id":job.id,"tool":job.tool,"description":job.description,
                    "state":status["state"],"started_at":job.started_at}),
            ));
        }
        summaries.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        let truncated = summaries.len() > 10;
        let mut result = json!({"control":{"mode":self.control.view("").control},
            "tasks":summaries.into_iter().take(10).map(|(_,_,v)|v).collect::<Vec<_>>()});
        if truncated {
            result["truncated"] = json!(true);
        }
        result
    }
    async fn prune_jobs(&self) {
        let mut jobs = self.jobs.lock().await;
        let mut expired = vec![];
        for (id, job) in jobs.iter() {
            if job
                .finished
                .lock()
                .await
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(1800))
            {
                expired.push(id.clone());
            }
        }
        for id in expired {
            jobs.remove(&id);
        }
    }
    pub async fn artifacts(&self) -> Result<Value, EngineError> {
        let port = self.ensure().await?;
        request(
            &self.session,
            port,
            "GET",
            "/artifacts",
            None,
            Duration::from_secs(3),
        )
        .await
    }
}

async fn request(
    session: &Session,
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
    timeout: Duration,
) -> Result<Value, EngineError> {
    let operation = async {
        let mut tcp = session.connect_guest_tcp(port).await?;
        let body = body.map(|v| v.to_string()).unwrap_or_default();
        let header=format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",body.len());
        tcp.writer
            .write_all(header.as_bytes())
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        tcp.writer
            .write_all(body.as_bytes())
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let mut response = vec![];
        tcp.reader
            .take(24 * 1024 * 1024 + 1)
            .read_to_end(&mut response)
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        if response.len() > 24 * 1024 * 1024 {
            return Err(EngineError::Other("browser result too large".into()));
        }
        let separator = response
            .windows(4)
            .position(|s| s == b"\r\n\r\n")
            .filter(|n| *n <= 16384)
            .ok_or_else(|| EngineError::Other("invalid browser helper response".into()))?;
        let headers = String::from_utf8_lossy(&response[..separator]);
        let value: Value = serde_json::from_slice(&response[separator + 4..])
            .map_err(|e| EngineError::Other(format!("invalid browser result: {e}")))?;
        if !headers.starts_with("HTTP/1.1 200 ") {
            return Err(EngineError::Other(
                value["error"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            ));
        }
        Ok(value)
    };
    tokio::time::timeout(timeout, operation)
        .await
        .map_err(|_| {
            EngineError::Timeout(
                "browser helper communication deadline; do not replay an uncertain script".into(),
            )
        })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{mock::MockEngine, WorkspaceOpts},
        session::SessionManager,
    };

    #[tokio::test]
    async fn task_list_is_bounded_running_first_and_does_not_create_or_refresh_jobs() {
        let manager = SessionManager::new(
            Arc::new(MockEngine::new()),
            1,
            WorkspaceOpts {
                image: "mock".into(),
                cpus: 1,
                memory_mib: 256,
                shell: "/bin/bash".into(),
                volumes: vec![],
                network_access: Default::default(),
                outbound_proxy: None,
                max_tcp_connections: None,
                max_udp_connections: None,
            },
        );
        let session = manager.get_or_create("task-wait-contract").await.unwrap();
        let hub = Arc::new(DesktopHub::new(session, Default::default()));
        let make_job = |id: &str| {
            Arc::new(Job {
                id: id.into(),
                accepted: Instant::now(),
                started_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                report_started_at: id != "observation",
                report_duration: id != "observation",
                tool: "browser_act",
                description: "Wait 1s".into(),
                status: Mutex::new(json!({"state":"running","terminal":false})),
                done: Notify::new(),
                cancel: tokio_util::sync::CancellationToken::new(),
                finished: Mutex::new(None),
            })
        };
        assert_eq!(hub.task_list().await["tasks"], json!([]));
        for i in 0..12 {
            let mut job = make_job(&format!("done-{i}"));
            Arc::get_mut(&mut job).unwrap().accepted = Instant::now() - Duration::from_secs(20 - i);
            *job.status.lock().await = json!({"state":"succeeded","terminal":true});
            *job.finished.lock().await = Some(Instant::now());
            hub.jobs.lock().await.insert(job.id.clone(), job);
        }
        let mut running = make_job("old-running");
        Arc::get_mut(&mut running).unwrap().accepted = Instant::now() - Duration::from_secs(4000);
        hub.jobs.lock().await.insert(running.id.clone(), running);
        let expired = make_job("expired");
        *expired.finished.lock().await = Some(Instant::now() - Duration::from_secs(1801));
        hub.jobs.lock().await.insert(expired.id.clone(), expired);
        let listed = hub.task_list().await;
        assert_eq!(listed["tasks"].as_array().unwrap().len(), 10);
        assert_eq!(listed["tasks"][0]["task_id"], "old-running");
        assert_eq!(listed["tasks"][1]["task_id"], "done-11");
        assert_eq!(listed["truncated"], true);
        assert!(listed.get("task_id").is_none());
        assert_eq!(hub.jobs.lock().await.len(), 13);
        assert_eq!(hub.task_list().await, listed);
        assert!(hub.task("expired", 0, false).await.is_err());
        // A task outside the first ten remains directly queryable.
        assert_eq!(
            hub.task("done-0", 0, false).await.unwrap()["state"],
            "succeeded"
        );
    }

    #[tokio::test]
    async fn task_wait_accepts_thirty_seconds_and_preserves_running_tasks() {
        let manager = SessionManager::new(
            Arc::new(MockEngine::new()),
            1,
            WorkspaceOpts {
                image: "mock".into(),
                cpus: 1,
                memory_mib: 256,
                shell: "/bin/bash".into(),
                volumes: vec![],
                network_access: Default::default(),
                outbound_proxy: None,
                max_tcp_connections: None,
                max_udp_connections: None,
            },
        );
        let session = manager.get_or_create("task-wait-contract").await.unwrap();
        let hub = Arc::new(DesktopHub::new(session, Default::default()));
        let make_job = |id: &str| {
            Arc::new(Job {
                id: id.into(),
                accepted: Instant::now(),
                started_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                report_started_at: id != "observation",
                report_duration: id != "observation",
                tool: "browser_act",
                description: "Wait 1s".into(),
                status: Mutex::new(json!({"state":"running","terminal":false})),
                done: Notify::new(),
                cancel: tokio_util::sync::CancellationToken::new(),
                finished: Mutex::new(None),
            })
        };
        let observation = make_job("observation");
        *observation.status.lock().await = json!({"state":"succeeded","terminal":true});
        hub.jobs
            .lock()
            .await
            .insert(observation.id.clone(), observation.clone());
        let observed = hub.task("observation", 0, false).await.unwrap();
        assert!(observed.get("started_at").is_none());
        assert!(observed.get("duration_secs").is_none());
        *observation.status.lock().await = json!({"state":"running","terminal":false});
        assert!(hub
            .task("observation", 0, false)
            .await
            .unwrap()
            .get("duration_secs")
            .is_some());
        *observation.status.lock().await = json!({"state":"failed","terminal":true});
        assert!(hub
            .task("observation", 0, false)
            .await
            .unwrap()
            .get("started_at")
            .is_some());
        let running = make_job("running");
        let completing = make_job("completing");
        let cancelling = make_job("cancelling");
        for job in [&running, &completing, &cancelling] {
            hub.jobs.lock().await.insert(job.id.clone(), job.clone());
        }
        assert!(hub.task("running", 31, false).await.is_err());
        assert_eq!(
            hub.task("running", 0, false).await.unwrap()["state"],
            "running"
        );
        let finish = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(21)).await;
            *completing.status.lock().await = json!({"state":"succeeded","terminal":true});
            completing.done.notify_waiters();
        });
        let cancelled = cancelling.clone();
        let cancel_finish = tokio::spawn(async move {
            cancelled.cancel.cancelled().await;
            *cancelled.status.lock().await = json!({"state":"cancelled","terminal":true});
            cancelled.done.notify_waiters();
        });
        let start = Instant::now();
        let (poll, completed, cancelled) = tokio::join!(
            hub.task("running", 30, false),
            hub.task("completing", 30, false),
            hub.task("cancelling", 30, true),
        );
        assert!(start.elapsed() >= Duration::from_secs(30));
        assert_eq!(poll.unwrap()["state"], "running");
        assert_eq!(completed.unwrap()["state"], "succeeded");
        assert_eq!(cancelled.unwrap()["state"], "cancelled");
        assert!(!running.cancel.is_cancelled());
        assert!(running.finished.lock().await.is_none());
        assert!(cancelling.cancel.is_cancelled());
        let first = hub.task("completing", 0, false).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let second = hub.task("completing", 0, false).await.unwrap();
        assert_eq!(first["started_at"], second["started_at"]);
        assert_eq!(first["duration_secs"], second["duration_secs"]);
        assert!(first["started_at"].as_u64().unwrap() > 0);
        finish.await.unwrap();
        cancel_finish.await.unwrap();
    }
}
