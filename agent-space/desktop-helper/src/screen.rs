//! Screenshot sequences use the existing task cache and cancellation owner.
use crate::{desktop, fail, App, Job, Reply};
use axum::{extract::State, Json};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Screen {
    task_id: String,
    view: String,
    page_id: Option<String>,
    count: usize,
    interval_ms: u64,
    timeout_secs: u64,
    #[serde(default)]
    save_original: bool,
    save_dir: Option<String>,
}
pub async fn start(State(app): State<App>, Json(params): Json<Screen>) -> Reply {
    if !matches!(params.view.as_str(), "desktop" | "page")
        || !(1..=8).contains(&params.count)
        || !(1..=600).contains(&params.timeout_secs)
        || (params.count as u64 - 1).saturating_mul(params.interval_ms)
            >= params.timeout_secs * 1000
        || params.save_dir.as_ref().is_some_and(|directory| {
            !params.save_original
                || !std::path::Path::new(directory).is_absolute()
                || directory.contains('\0')
        })
    {
        return Err(fail("INVALID_ARGUMENT"));
    }
    let mut jobs = app.jobs.lock().await;
    if jobs.contains_key(&params.task_id) {
        return Err(fail("DUPLICATE_TASK"));
    }
    let job = crate::new_job(&params.task_id, params.timeout_secs, false)
        .map_err(|e| fail(&e.to_string()))?;
    jobs.insert(job.id.clone(), job.clone());
    let id = job.id.clone();
    drop(jobs);
    tokio::spawn(async move {
        execute(app, job, params).await;
    });
    Ok(Json(
        json!({"task_id":id,"state":"running","phase":"preparing"}),
    ))
}

async fn execute(app: App, job: Arc<Job>, params: Screen) {
    let accepted = job.deadline - Duration::from_secs(params.timeout_secs);
    let mut result = json!({"view":params.view,"requested_count":params.count,"captured_count":0,"frames":[],"images":[],"captures":[],"files":[]});
    let outcome = capture_sequence(&app, &job, &params, accepted, &mut result).await;
    let state = if job.cancel.is_cancelled() {
        "cancelled"
    } else if Instant::now() >= job.deadline {
        "timed_out"
    } else if outcome.is_ok() {
        "succeeded"
    } else {
        "failed"
    };
    let mut status =
        json!({"task_id":job.id,"state":state,"terminal":true,"cleanup_confirmed":true});
    if let Err(error) = outcome {
        let message = error.to_string();
        // Preserve structured errors (for example PAGE_NOT_FOUND with tabs).
        if let Ok(value) = serde_json::from_str::<Value>(&message) {
            status["error"] = value["error"].clone();
            if let Some(tabs) = value.get("tabs") {
                result["tabs"] = tabs.clone();
            }
        } else {
            status["error"] = json!(message);
        }
    }
    for key in ["images", "captures", "files"] {
        if result[key].as_array().is_some_and(|v| v.is_empty()) {
            result.as_object_mut().unwrap().remove(key);
        }
    }
    *job.result.lock().await = Some(result);
    *job.status.lock().await = status;
    *job.finished.lock().await = Some(Instant::now());
    app.desktop.lock().await.last_used = Instant::now();
    job.done.notify_waiters();
}
fn check(job: &Job) -> anyhow::Result<()> {
    anyhow::ensure!(
        !job.cancel.is_cancelled(),
        "EXECUTION_CANCELLED: remaining captures cancelled"
    );
    anyhow::ensure!(
        Instant::now() < job.deadline,
        "EXECUTION_TIMEOUT: observation deadline exceeded"
    );
    Ok(())
}
async fn capture_sequence(
    app: &App,
    job: &Job,
    params: &Screen,
    accepted: Instant,
    result: &mut Value,
) -> anyhow::Result<()> {
    {
        let mut owner = app.desktop.lock().await;
        check(job)?;
        if params.view == "desktop" {
            owner.start_display().await?;
        } else {
            owner.start().await?;
        }
        owner.last_used = Instant::now();
    }
    let mut page_id = params.page_id.clone();
    let mut next = Instant::now();
    for index in 0..params.count {
        tokio::select! {
            _ = tokio::time::sleep_until(next.into()) => {},
            _ = job.cancel.cancelled() => { check(job)?; },
            _ = tokio::time::sleep_until(job.deadline.into()) => { check(job)?; },
        }
        check(job)?;
        let permit = tokio::select! {
            permit = agent_engine::media::acquire() => permit?,
            _ = job.cancel.cancelled() => { check(job)?; unreachable!() },
            _ = tokio::time::sleep_until(job.deadline.into()) => { check(job)?; unreachable!() },
        };
        check(job)?;
        *job.status.lock().await = json!({"task_id":job.id,"state":"running","phase":"capturing","terminal":false,"requested_count":params.count,"captured_count":index});
        let started = Instant::now();
        let value = desktop::node(json!({"view":params.view,"page_id":page_id})).await?;
        if value.get("error").is_some() {
            anyhow::bail!(value.to_string());
        }
        if params.view == "page" {
            page_id = value["page_id"].as_str().map(str::to_owned);
            for key in ["page_id", "url", "title"] {
                result[key] = value[key].clone();
            }
        }
        let raw = base64::engine::general_purpose::STANDARD.decode(
            value["images"][0]["data"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("INVALID_CAPTURE: missing image"))?,
        )?;
        // Saving uses these exact capture bytes, independently of the viewing
        // codec. A storage failure still returns the current viewable frame.
        let storage_error = if params.save_original {
            save_frame(&raw, params.save_dir.as_deref(), job, index, &value, result)
                .await
                .err()
        } else {
            None
        };
        let budget = agent_engine::media::SCREEN_LIMIT / params.count;
        let processed = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            agent_engine::media::process(raw, Some(budget))
        })
        .await?;
        let picture = match processed {
            Ok(picture) => picture,
            Err(error) => {
                if let Some(storage) = storage_error {
                    result["output_warning"] = json!(storage.to_string());
                }
                return Err(error);
            }
        };
        let mut capture = value["capture"].clone();
        capture["original_width"] = json!(picture.original_width);
        capture["original_height"] = json!(picture.original_height);
        capture["width"] = json!(picture.width);
        capture["height"] = json!(picture.height);
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
        let age = now_ms.saturating_sub(capture["at"].as_u64().unwrap_or(now_ms));
        let elapsed = (accepted.elapsed().as_millis() as u64).saturating_sub(age);
        result["frames"].as_array_mut().unwrap().push(
            json!({"index":index+1,"image_id":capture["image_id"],"elapsed_ms":elapsed,
            "width":picture.width,"height":picture.height,"resized":picture.resized}),
        );
        result["captures"].as_array_mut().unwrap().push(capture);
        result["images"].as_array_mut().unwrap().push(json!({"mimeType":picture.mime_type,"data":base64::engine::general_purpose::STANDARD.encode(&picture.bytes)}));
        result["captured_count"] = json!(index + 1);
        if let Some(storage) = storage_error {
            return Err(storage);
        }
        *job.status.lock().await = json!({"task_id":job.id,"state":"running","phase":"waiting","terminal":false,"requested_count":params.count,"captured_count":index+1});
        if index + 1 < params.count {
            next = Instant::now()
                .checked_sub(Duration::from_millis(age))
                .unwrap_or(started)
                + Duration::from_millis(params.interval_ms);
        }
        check(job)?;
    }
    Ok(())
}

async fn save_frame(
    bytes: &[u8],
    directory: Option<&str>,
    job: &Job,
    index: usize,
    capture: &Value,
    result: &mut Value,
) -> anyhow::Result<()> {
    let directory = directory.unwrap_or(desktop::ARTIFACTS);
    let mut file = save_capture(bytes, directory, job.deadline)
        .await
        .map_err(|e| {
            anyhow::anyhow!("SCREEN_SAVE_FAILED: Cannot save original to {directory}: {e}")
        })?;
    file["frame_index"] = json!(index + 1);
    file["image_id"] = capture["capture"]["image_id"].clone();
    file["width"] = capture["capture"]["width"].clone();
    file["height"] = capture["capture"]["height"].clone();
    result["files"].as_array_mut().unwrap().push(file);
    Ok(())
}

async fn save_capture(bytes: &[u8], directory: &str, deadline: Instant) -> anyhow::Result<Value> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .arg("--save-capture")
        .arg(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::process::isolate(&mut command, Some(1001));
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(5).min(deadline.saturating_duration_since(Instant::now())),
        async {
            stdin.write_all(bytes).await?;
            drop(stdin);
            child.wait_with_output().await
        },
    )
    .await??;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Called only in the UID1001 storage child. Publish a complete file atomically;
/// a collision can never overwrite an existing screenshot or follow its link.
pub fn store_capture(directory: &std::path::Path, bytes: &[u8]) -> anyhow::Result<Value> {
    use std::{fs, io::Write, os::unix::fs::OpenOptionsExt};
    anyhow::ensure!(directory.is_absolute(), "save_dir must be absolute");
    anyhow::ensure!(
        bytes.len() <= agent_engine::media::SOURCE_LIMIT,
        "capture too large"
    );
    fs::create_dir_all(directory)?;
    let directory = fs::canonicalize(directory)?;
    let pending = directory.join(format!(".screen-{}.part", uuid::Uuid::new_v4()));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&pending)?;
    let saved = (|| {
        output.write_all(bytes)?;
        drop(output);
        const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        for _ in 0..3 {
            let mut random = uuid::Uuid::new_v4().as_u128();
            let suffix: String = (0..10)
                .map(|_| {
                    let character = BASE62[(random % 62) as usize] as char;
                    random /= 62;
                    character
                })
                .collect();
            let path = directory.join(format!("screen-{suffix}.png"));
            match fs::hard_link(&pending, &path) {
                Ok(()) => {
                    return Ok(json!({"path":path,"bytes":bytes.len(),"mime_type":"image/png"}))
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("could not allocate a unique screenshot filename")
    })();
    let _ = fs::remove_file(pending);
    saved
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn originals_keep_exact_bytes_and_unique_compact_names() {
        let root = std::env::temp_dir().join(format!("screen-save-{}", uuid::Uuid::new_v4()));
        let directory = root.join("nested");
        let bytes = b"exact original PNG bytes";
        let first = store_capture(&directory, bytes).unwrap();
        let second = store_capture(&directory, bytes).unwrap();
        assert_ne!(first["path"], second["path"]);
        for saved in [first, second] {
            let path = std::path::Path::new(saved["path"].as_str().unwrap());
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            let name = path.file_stem().unwrap().to_str().unwrap();
            assert_eq!(name.len(), 17);
            assert!(name.starts_with("screen-"));
            assert!(name[7..].bytes().all(|c| c.is_ascii_alphanumeric()));
        }
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
        assert!(store_capture(std::path::Path::new("relative"), bytes).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
