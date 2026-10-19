//! Loopback-only guest browser owner. No MCP or user credentials live here.
mod desktop;
mod pages;
mod process;
mod screen;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{Mutex, Notify},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct App {
    port: u16,
    desktop: Arc<Mutex<desktop::Desktop>>,
    jobs: Arc<Mutex<HashMap<String, Arc<Job>>>>,
}
struct Job {
    id: String,
    finished: Mutex<Option<Instant>>,
    deadline: Instant,
    status: Mutex<Value>,
    cancel: CancellationToken,
    done: Notify,
    directory: String,
    result: Mutex<Option<Value>>,
    modifies: bool,
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn fail(code: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::CONFLICT, Json(json!({"error":code})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    task_id: String,
    code: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default)]
    page_id: Option<String>,
    #[serde(default)]
    captures: Vec<Value>,
}
fn default_timeout() -> u64 {
    60
}
#[derive(Deserialize)]
struct Poll {
    #[serde(default)]
    summary: bool,
}
async fn health(State(app): State<App>) -> Reply {
    // Helper liveness must remain observable while its desktop is starting.
    // Waiting behind startup made the host mistake a busy helper for a dead one.
    let value = match app.desktop.try_lock() {
        Ok(mut desktop) => json!({"desktop":desktop.alive(),"worker_contract":1}),
        Err(_) => json!({"desktop":null,"preparing":true,"worker_contract":1}),
    };
    Ok(Json(value))
}
async fn viewer(State(app): State<App>) -> Reply {
    let mut desktop = app.desktop.lock().await;
    desktop.start().await.map_err(|e| fail(&e.to_string()))?;
    desktop.last_used = Instant::now();
    let pages = desktop
        .pages
        .clone()
        .ok_or_else(|| fail("BROWSER_UNAVAILABLE"))?;
    drop(desktop);
    pages
        .ensure_page()
        .await
        .map_err(|e| fail(&e.to_string()))?;
    Ok(Json(json!({"desktop":true})))
}
async fn input(State(app): State<App>) -> Reply {
    app.desktop.lock().await.last_used = Instant::now();
    Ok(Json(json!({"ok":true})))
}
async fn artifacts() -> Reply {
    // The user owns every workspace ancestor and may replace it with a link.
    // Use the workspace file identity for the complete walk, including ancestors.
    let mut command = Command::new(std::env::current_exe().map_err(|e| fail(&e.to_string()))?);
    command
        .arg("--list-artifacts")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    process::isolate(&mut command, Some(process::workspace_uid()));
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|_| fail("ARTIFACTS_TIMEOUT"))?
        .map_err(|e| fail(&e.to_string()))?;
    if !output.status.success() {
        return Err(fail("ARTIFACTS_UNAVAILABLE"));
    }
    Ok(Json(
        serde_json::from_slice(&output.stdout).map_err(|_| fail("INVALID_ARTIFACTS"))?,
    ))
}
fn list_artifacts() -> anyhow::Result<Value> {
    anyhow::ensure!(
        unsafe { libc::geteuid() } == process::workspace_uid(),
        "artifact listing requires workspace ownership"
    );
    let mut files = vec![];
    match std::fs::read_dir(desktop::ARTIFACTS) {
        Ok(entries) => {
            for entry in entries.flatten().take(1000) {
                if let Ok(meta) = entry.path().symlink_metadata() {
                    if meta.is_file() {
                        files.push(json!({"name":entry.file_name().to_string_lossy(),"bytes":meta.len(),"path":entry.path().to_string_lossy()}));
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(json!({"files":files}))
}
async fn observe(State(app): State<App>, Json(params): Json<Value>) -> Reply {
    if !matches!(params["view"].as_str(), None | Some("dom" | "ax")) {
        return Err(fail("INVALID_VIEW"));
    }
    let mut desktop = app.desktop.lock().await;
    desktop.start().await.map_err(|e| fail(&e.to_string()))?;
    desktop.last_used = Instant::now();
    drop(desktop);
    let mut value = desktop::node(params)
        .await
        .map_err(|e| fail(&e.to_string()))?;
    if let Some(fields) = value.as_object_mut() {
        fields.remove("profilePicker");
    }
    Ok(Json(value))
}
async fn run(State(app): State<App>, Json(params): Json<Run>) -> Reply {
    if params.code.is_empty()
        || params.code.len() > 65536
        || !(1..=600).contains(&params.timeout_secs)
        || params.captures.len() > 32
        || uuid::Uuid::parse_str(&params.task_id).is_err()
    {
        return Err(fail("INVALID_ARGUMENT"));
    }
    let mut jobs = app.jobs.lock().await;
    if jobs.contains_key(&params.task_id) {
        return Err(fail("DUPLICATE_TASK"));
    }
    for job in jobs.values() {
        let status = job.status.lock().await;
        if job.modifies && status["terminal"] != true {
            return Err(fail("BUSY"));
        }
    }
    let id = params.task_id.clone();
    let job = new_job(&id, params.timeout_secs, true).map_err(|e| fail(&e.to_string()))?;
    jobs.insert(id.clone(), job.clone());
    drop(jobs);
    tokio::spawn(async move {
        execute(app, job, params).await;
    });
    Ok(Json(
        json!({"task_id":id,"state":"running","phase":"preparing"}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewPage {
    task_id: String,
}
async fn new_page(State(app): State<App>, Json(params): Json<NewPage>) -> Reply {
    let job = app
        .jobs
        .lock()
        .await
        .get(&params.task_id)
        .cloned()
        .ok_or_else(|| fail("TASK_NOT_FOUND"))?;
    if !job.modifies
        || job.cancel.is_cancelled()
        || Instant::now() >= job.deadline
        || job.status.lock().await["terminal"] == true
    {
        return Err(fail("EXECUTION_CANCELLED"));
    }
    let pages = app
        .desktop
        .lock()
        .await
        .pages
        .clone()
        .ok_or_else(|| fail("BROWSER_UNAVAILABLE"))?;
    let id = tokio::select! {
        result = pages.create() => result.map_err(|e| fail(&e.to_string()))?,
        _ = job.cancel.cancelled() => return Err(fail("EXECUTION_CANCELLED")),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(job.deadline)) => return Err(fail("EXECUTION_TIMEOUT")),
    };
    Ok(Json(json!({"page_id":id})))
}
fn new_job(id: &str, timeout: u64, modifies: bool) -> anyhow::Result<Arc<Job>> {
    anyhow::ensure!(uuid::Uuid::parse_str(id).is_ok(), "INVALID_TASK_ID");
    let accepted = Instant::now();
    let directory = format!(
        "/tmp/agent-space/{}/job-{id}",
        std::env::var("AGENT_SPACE_HELPER_INSTANCE")?
    );
    std::fs::create_dir(&directory)?;
    let c = std::ffi::CString::new(directory.clone())?;
    unsafe {
        let uid = process::workspace_uid();
        if libc::chown(c.as_ptr(), uid, uid) != 0 {
            anyhow::bail!("JOB_OWNERSHIP");
        }
    }
    let job = Arc::new(Job {
        id: id.to_owned(),
        finished: Mutex::new(None),
        deadline: accepted + Duration::from_secs(timeout),
        status: Mutex::new(
            json!({"task_id":id,"state":"running","phase":"preparing","terminal":false}),
        ),
        cancel: CancellationToken::new(),
        done: Notify::new(),
        directory,
        result: Mutex::new(None),
        modifies,
    });
    Ok(job)
}
async fn execute(app: App, job: Arc<Job>, params: Run) {
    let until = tokio::time::Instant::from_std(job.deadline);
    let prepare = async {
        let mut desktop = app.desktop.lock().await;
        desktop.start().await?;
        desktop.last_used = Instant::now();
        let pages = desktop
            .pages
            .clone()
            .ok_or_else(|| anyhow::anyhow!("BROWSER_UNAVAILABLE: page admission unavailable"))?;
        drop(desktop);
        let mut value = desktop::node(json!({"page_id":params.page_id})).await?;
        if params.page_id.is_none()
            && value["page_id"].is_null()
            && value["nativeWindow"] != true
            && value["profilePicker"] != true
        {
            let id = pages.ensure_page().await?;
            value = desktop::node(json!({"page_id":id})).await?;
        }
        if value.get("error").is_some() {
            *job.result.lock().await = Some(json!({"tabs":value["tabs"]}));
            anyhow::bail!(value.to_string());
        }
        Ok((value, pages))
    };
    let prepared = tokio::select! {
     result=prepare=>result,
     _=job.cancel.cancelled()=>Err(anyhow::anyhow!("EXECUTION_CANCELLED")),
     _=tokio::time::sleep_until(until)=>Err(anyhow::anyhow!("EXECUTION_TIMEOUT")),
    };
    let started = prepared.is_ok();
    let outcome = match prepared {
        Ok((observation, pages)) => worker(&job, &params, observation, app.port, pages).await,
        Err(error) => {
            let mut desktop = app.desktop.lock().await;
            if !desktop.display_alive() {
                if let Err(cleanup) = desktop.stop().await {
                    drop(desktop);
                    return finish_unconfirmed(
                        &app,
                        &job,
                        format!("CLEANUP_UNCONFIRMED: preparation cleanup: {cleanup}"),
                    )
                    .await;
                }
            }
            Err(error)
        }
    };
    let state = if outcome.is_ok() {
        "succeeded"
    } else if job.cancel.is_cancelled() {
        "cancelled"
    } else if Instant::now() >= job.deadline {
        "timed_out"
    } else {
        "failed"
    };
    *job.status.lock().await =
        json!({"task_id":job.id,"state":"running","phase":"cleaning","terminal":false});
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        desktop::node(json!({"action":"restore"})),
    )
    .await;
    let error = match outcome {
        Ok(value) => {
            *job.result.lock().await = Some(value);
            None
        }
        Err(error) => Some(error.to_string()),
    };
    let cleanup_confirmed = !error
        .as_ref()
        .is_some_and(|e| e.contains("CLEANUP_UNCONFIRMED"));
    let mut final_status = json!({"task_id":job.id,"state":state,"terminal":true,"cleanup_confirmed":cleanup_confirmed});
    if let Some(error) = error {
        final_status["error"] = json!(error);
        final_status["side_effects"] = json!(if started {
            "may_have_occurred"
        } else {
            "not_started"
        });
    }
    *job.status.lock().await = final_status;
    *job.finished.lock().await = Some(Instant::now());
    app.desktop.lock().await.last_used = Instant::now();
    job.done.notify_waiters();
}
async fn finish_unconfirmed(app: &App, job: &Job, error: String) {
    *job.status.lock().await = json!({"task_id":job.id,"state":"failed","terminal":true,"cleanup_confirmed":false,"error":error,"side_effects":"may_have_occurred"});
    *job.finished.lock().await = Some(Instant::now());
    app.desktop.lock().await.last_used = Instant::now();
    job.done.notify_waiters();
}
async fn worker(
    job: &Job,
    params: &Run,
    observation: Value,
    port: u16,
    pages: pages::Pages,
) -> anyhow::Result<Value> {
    let mut page_events = pages.violations();
    page_events.borrow_and_update();
    let remaining = job.deadline.saturating_duration_since(Instant::now());
    anyhow::ensure!(
        !remaining.is_zero() && !job.cancel.is_cancelled(),
        "EXECUTION_TIMEOUT_OR_CANCELLED"
    );
    let worker_spec = json!({"code":params.code,"page_id":observation["page_id"],"window":observation["window"],"nativeWindow":observation["nativeWindow"],"profilePicker":observation["profilePicker"],"pageResponsive":observation["health"]["page"],"captures":params.captures,"timeoutMs":remaining.as_millis(),"guard_port":port,"task_id":job.id});
    let spec = json!({"program":"node","args":[format!("{}/browser-worker.cjs",desktop::NODE)],"uid":process::workspace_uid(),
    "timeout_secs":remaining.as_secs_f64(),"max_bytes":32768,"max_lines":500,"cleanup_on_exit":true});
    let mut command = Command::new("python3");
    process::workspace_env(&mut command);
    command
        .args([
            "-I",
            "-c",
            include_str!("../../guest/exec.py"),
            &spec.to_string(),
        ])
        .env("DISPLAY", ":1")
        .env("AGENT_TASK", worker_spec.to_string())
        .env("AGENT_RESULT", format!("{}/result.json", job.directory))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process::isolate(&mut command, None);
    let mut child = command.spawn()?;
    let identity = process::identity(
        child
            .id()
            .ok_or_else(|| anyhow::anyhow!("missing worker PID"))?,
    )
    .ok_or_else(|| anyhow::anyhow!("worker owner exited"))?;
    *job.status.lock().await =
        json!({"task_id":job.id,"state":"running","phase":"executing","terminal":false});
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let reader = tokio::spawn(async move {
        let mut bytes = vec![];
        stdout.take(512 * 1024).read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let errors = tokio::spawn(async move {
        let mut bytes = vec![];
        stderr.take(8192).read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let mut page_limit = false;
    let ending = tokio::select! {
      exit=child.wait()=>{exit?;false},
      _=job.cancel.cancelled()=>true,
      changed=page_events.changed()=>{page_limit=changed.is_ok();true},
      _=tokio::time::sleep_until(tokio::time::Instant::from_std(job.deadline))=>true,
    };
    if ending {
        *job.status.lock().await =
            json!({"task_id":job.id,"state":"running","phase":"cleaning","terminal":false});
        if process::identity(identity.pid) == Some(identity) {
            unsafe {
                libc::kill(identity.pid as i32, libc::SIGTERM);
            }
        }
        if tokio::time::timeout(Duration::from_secs(4), child.wait())
            .await
            .is_err()
        {
            process::stop_tree(identity).await?;
            let _ = child.wait().await;
        }
    }
    let bytes = reader.await??;
    let diagnostic = errors.await??;
    anyhow::ensure!(
        !bytes.is_empty(),
        "CLEANUP_UNCONFIRMED: worker result lost: {}",
        String::from_utf8_lossy(&diagnostic)
    );
    let output: Value = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("CLEANUP_UNCONFIRMED: invalid worker owner result: {e}"))?;
    let stdout = output["stdout"].as_str().unwrap_or_default();
    let stderr = output["stderr"].as_str().unwrap_or_default();
    let saved = read_worker_result(
        std::path::Path::new(&job.directory)
            .join("result.json")
            .as_path(),
    );
    let mut result = saved.as_ref().cloned().unwrap_or_else(|_| json!({}));
    if !stdout.is_empty() || !stderr.is_empty() {
        let mut logs = serde_json::Map::new();
        if !stdout.is_empty() {
            logs.insert("stdout".into(), json!(stdout));
        }
        if !stderr.is_empty() {
            logs.insert("stderr".into(), json!(stderr));
        }
        if output["truncated"] == true {
            logs.insert("truncated".into(), json!(true));
        }
        result["logs"] = Value::Object(logs);
    }
    *job.result.lock().await = Some(result.clone());
    anyhow::ensure!(output["cleanup_confirmed"] == true, "CLEANUP_UNCONFIRMED");
    // The worker can exit before this connection receives its popup event.
    // Drain a real target snapshot before publishing success, retaining outputs.
    // Forced endings already stopped the worker. Do not extend cancellation's
    // existing cleanup budget with an additional browser round trip.
    let synchronized = if ending { Ok(()) } else { pages.sync().await };
    page_limit |= page_events.has_changed().unwrap_or(false);
    anyhow::ensure!(!page_limit, "{}", pages.limit_error());
    anyhow::ensure!(output["cancelled"] != true, "EXECUTION_CANCELLED");
    anyhow::ensure!(output["timed_out"] != true, "EXECUTION_TIMEOUT");
    if let Err(error) = synchronized {
        // Closing the last Page can shut Chromium down normally. Preserve that
        // completed action; the existing lifecycle owner handles the next open.
        if tokio::net::TcpStream::connect(("127.0.0.1", 9222))
            .await
            .is_ok()
        {
            return Err(error);
        }
    }
    if output["exit_code"] != 0 {
        let cause = stderr
            .trim()
            .strip_prefix("Error: ")
            .unwrap_or(stderr.trim());
        if cause.split_once(": ").is_some_and(|(code, _)| {
            !code.is_empty() && code.bytes().all(|c| c.is_ascii_uppercase() || c == b'_')
        }) {
            anyhow::bail!(cause.to_owned());
        }
        anyhow::bail!("SCRIPT_ERROR: {cause}");
    }
    saved?;
    Ok(result)
}
fn read_worker_result(path: &std::path::Path) -> anyhow::Result<Value> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    let limit = 12 * 1024 * 1024;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.uid() == process::workspace_uid()
            && metadata.nlink() == 1
            && metadata.len() <= limit,
        "invalid worker result ownership/type/size"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= limit as usize, "worker result too large");
    decode_worker_result(&bytes)
}
fn decode_worker_result(bytes: &[u8]) -> anyhow::Result<Value> {
    let result: Value = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        result.is_object(),
        "SCRIPT_ERROR: worker result must be an object"
    );
    anyhow::ensure!(
        result.as_object().unwrap().keys().all(|key| matches!(
            key.as_str(),
            "value" | "images" | "files" | "captures" | "output_warning"
        )),
        "SCRIPT_ERROR: worker result contains unsupported fields"
    );
    Ok(result)
}
async fn task(State(app): State<App>, Path(id): Path<String>, Query(poll): Query<Poll>) -> Reply {
    let job = app
        .jobs
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| fail("TASK_NOT_FOUND"))?;
    let mut status = job.status.lock().await.clone();
    if status["terminal"] == true && !poll.summary {
        if let Some(value) = job.result.lock().await.as_ref() {
            status["result"] = value.clone();
        }
    } else if status["terminal"] != true {
        status["remaining_secs"] = json!(job
            .deadline
            .saturating_duration_since(Instant::now())
            .as_secs());
    }
    Ok(Json(status))
}
async fn cancel(State(app): State<App>, Path(id): Path<String>) -> Reply {
    let job = app
        .jobs
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| fail("TASK_NOT_FOUND"))?;
    job.cancel.cancel();
    Ok(Json(json!({"task_id":id,"state":"cancelling"})))
}
async fn reset(State(app): State<App>) -> Reply {
    let jobs = app.jobs.lock().await.values().cloned().collect::<Vec<_>>();
    for job in &jobs {
        job.cancel.cancel();
    }
    for job in &jobs {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let notified = job.done.notified();
                if job.status.lock().await["terminal"] == true {
                    break;
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| fail("CLEANUP_UNCONFIRMED"))?;
        if job.status.lock().await["cleanup_confirmed"] == false {
            return Err(fail("CLEANUP_UNCONFIRMED"));
        }
    }
    Ok(Json(json!({"ok":true})))
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--list-artifacts") {
        println!("{}", list_artifacts()?);
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("--prepare-workspace") {
        anyhow::ensure!(
            unsafe { libc::geteuid() } == process::workspace_uid(),
            "workspace preparation requires workspace ownership"
        );
        return desktop::prepare_workspace();
    }
    if std::env::args().nth(1).as_deref() == Some("--save-capture") {
        use std::io::Read;
        anyhow::ensure!(
            unsafe { libc::geteuid() } == process::workspace_uid(),
            "capture storage requires workspace ownership"
        );
        let mut bytes = vec![];
        std::io::stdin()
            .take(agent_engine::media::SOURCE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= agent_engine::media::SOURCE_LIMIT,
            "capture too large"
        );
        let directory = std::env::args()
            .nth(2)
            .unwrap_or_else(|| desktop::ARTIFACTS.into());
        println!(
            "{}",
            screen::store_capture(std::path::Path::new(&directory), &bytes)?
        );
        return Ok(());
    }
    unsafe {
        anyhow::ensure!(
            libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) == 0,
            "subreaper failed"
        );
    }
    // Standalone observers and scripts use UID1001. Permit traversal
    // while keeping the controller's identity and logs private in Standalone.
    if let Ok(instance) = std::env::var("AGENT_SPACE_HELPER_INSTANCE") {
        use std::os::unix::fs::PermissionsExt;
        let directory = format!("/tmp/agent-space/{instance}");
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o711))?;
    }
    desktop::install_sources()?;
    let max_pages = match std::env::args().nth(1).as_deref() {
        None => 4,
        Some("--max-pages") => std::env::args()
            .nth(2)
            .ok_or_else(|| anyhow::anyhow!("missing --max-pages"))?
            .parse::<usize>()?,
        Some(_) => anyhow::bail!("unknown desktop helper argument"),
    };
    anyhow::ensure!(max_pages > 0, "browser.max_pages must be greater than zero");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let mut desktop = desktop::Desktop::default();
    desktop.max_pages = max_pages;
    let app = App {
        port,
        desktop: Arc::new(Mutex::new(desktop)),
        jobs: Arc::new(Mutex::new(HashMap::new())),
    };
    let upkeep = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let mut jobs = upkeep.jobs.lock().await;
            let mut expired = vec![];
            let mut active = false;
            for (id, job) in jobs.iter() {
                if job.status.lock().await["terminal"] != true {
                    active = true;
                } else if job
                    .finished
                    .lock()
                    .await
                    .is_some_and(|t| t.elapsed() > Duration::from_secs(1800))
                {
                    expired.push(id.clone());
                }
            }
            for id in expired {
                if let Some(job) = jobs.remove(&id) {
                    let directory = std::path::Path::new(&job.directory);
                    if directory.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                        if let Ok(entries) = std::fs::read_dir(directory) {
                            let mut clean = true;
                            for file in entries.flatten() {
                                if matches!(
                                    file.file_name().to_str(),
                                    Some("result.json" | "result.json.tmp")
                                ) && file.path().symlink_metadata().is_ok_and(|m| m.is_file())
                                {
                                    let _ = std::fs::remove_file(file.path());
                                } else {
                                    clean = false;
                                }
                            }
                            if clean {
                                let _ = std::fs::remove_dir(directory);
                            }
                        }
                    }
                }
            }
            drop(jobs);
            if !active {
                let mut desktop = upkeep.desktop.lock().await;
                if !desktop.alive() {
                    desktop.pages = None;
                }
                if desktop.last_used.elapsed() > Duration::from_secs(300) {
                    if let Err(error) = desktop.sleep().await {
                        eprintln!("desktop cleanup: {error}");
                    }
                }
            }
        }
    });
    println!("READY {port}");
    let router = Router::new()
        .route("/health", get(health))
        .route("/viewer", post(viewer))
        .route("/observe", post(observe))
        .route("/input", post(input))
        .route("/artifacts", get(artifacts))
        .route("/run", post(run))
        .route("/page", post(new_page))
        .route("/screen", post(screen::start))
        .route("/task/{id}", get(task))
        .route("/task/{id}/cancel", post(cancel))
        .route("/reset", post(reset))
        .with_state(app.clone());
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    axum::serve(listener, router)
        .with_graceful_shutdown({
            let shutdown_app = app.clone();
            async move {
                terminate.recv().await;
                for job in shutdown_app.jobs.lock().await.values() {
                    job.cancel.cancel();
                }
            }
        })
        .await?;
    for job in app.jobs.lock().await.values() {
        job.cancel.cancel();
    }
    app.desktop.lock().await.stop().await?;
    Ok(())
}

#[cfg(test)]
mod result_tests {
    use super::*;
    #[test]
    fn worker_result_rejects_non_object_json_before_attaching_logs() {
        for bytes in [b"[]".as_slice(), b"null", b"7", b"true", b"\"text\""] {
            assert!(decode_worker_result(bytes).is_err());
        }
        assert!(decode_worker_result(b"{\"value\":7}").unwrap().is_object());
        assert!(
            decode_worker_result(b"{\"state\":\"succeeded\",\"cleanup_confirmed\":true}").is_err()
        );
    }
    #[test]
    fn worker_result_never_follows_user_links_or_accepts_non_user_owned_files() {
        let dir = std::env::temp_dir().join(format!("runcove-result-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let private = dir.join("private.json");
        std::fs::write(&private, b"{\"secret\":true}").unwrap();
        let result = dir.join("result.json");
        std::os::unix::fs::symlink(&private, &result).unwrap();
        assert!(read_worker_result(&result).is_err());
        std::fs::hard_link(&private, dir.join("linked.json")).unwrap();
        assert!(read_worker_result(&private).is_err());
        assert_eq!(std::fs::read(&private).unwrap(), b"{\"secret\":true}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
