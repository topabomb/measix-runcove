//! One existing container workspace. Docker owns its lifetime and resources.
//! This adapter never creates/deletes VMs, disks, users or other workspaces.
use super::{
    managed::{self, ManagedRuntime},
    AgentEngine, AgentHandle, EngineError, ExecResult, GuestTcp, ManagedInstance, ManagedLaunch,
    ManagedService, WorkspaceOpts, WorkspaceStatus,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

pub const WORKSPACE: &str = "standalone";
const PAYLOAD_UID: u32 = 1001;

pub struct ProcessEngine {
    handle: Arc<ProcessHandle>,
}

impl Default for ProcessEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessEngine {
    pub fn new() -> Self {
        Self {
            handle: Arc::new(ProcessHandle),
        }
    }

    fn current(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
        if name != WORKSPACE {
            return Err(EngineError::NotFound(name.into()));
        }
        Ok(self.handle.clone())
    }
}

#[async_trait]
impl AgentEngine for ProcessEngine {
    async fn create_sandbox(
        &self,
        _name: &str,
        _opts: &WorkspaceOpts,
    ) -> Result<Arc<dyn AgentHandle>, EngineError> {
        Err(EngineError::Other(
            "Standalone cannot create workspaces".into(),
        ))
    }

    async fn get_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
        self.current(name)
    }

    async fn start_sandbox(&self, name: &str) -> Result<Arc<dyn AgentHandle>, EngineError> {
        self.current(name)
    }

    async fn remove_sandbox(&self, _name: &str) -> Result<(), EngineError> {
        Err(EngineError::Other(
            "Docker owns the Standalone workspace".into(),
        ))
    }

    async fn list_running(&self) -> Result<Vec<String>, EngineError> {
        Ok(vec![WORKSPACE.into()])
    }
}

struct ProcessHandle;

/// Only non-secret runtime paths reach execution owners and their children.
fn python() -> Command {
    let mut command = Command::new("python3");
    command
        .env_clear()
        .env("PATH", "/opt/agent-tools/python/bin:/opt/agent-tools/node_modules/.bin:/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/home/desktop")
        .env("USER", "desktop")
        .env("LANG", "C.UTF-8")
        .env("NODE_PATH", "/opt/agent-tools/node_modules")
        .env("PLAYWRIGHT_BROWSERS_PATH", "/opt/agent-tools/browsers")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

async fn read_limited(
    input: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, EngineError> {
    let mut bytes = Vec::new();
    input
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| EngineError::ExecError(e.to_string()))?;
    if bytes.len() > limit {
        return Err(EngineError::ExecError(
            "execution owner result overflow".into(),
        ));
    }
    Ok(bytes)
}

async fn run_owner(
    command: Command,
    budget: Duration,
    max_bytes: usize,
    cancel: CancellationToken,
) -> Result<Vec<u8>, EngineError> {
    // The monitor keeps ownership if an HTTP waiter drops its future. Dropping
    // the waiter cancels it; the monitor still signals and reaps its direct child.
    let cancel = cancel.child_token();
    let guard = cancel.clone().drop_guard();
    let outcome =
        tokio::spawn(async move { monitor_owner(command, budget, max_bytes, cancel).await })
            .await
            .map_err(|e| EngineError::ExecError(format!("execution monitor: {e}")))?;
    guard.disarm();
    outcome
}

async fn monitor_owner(
    mut command: Command,
    budget: Duration,
    max_bytes: usize,
    cancel: CancellationToken,
) -> Result<Vec<u8>, EngineError> {
    let mut child = command
        .spawn()
        .map_err(|e| EngineError::ExecError(format!("execution owner: {e}")))?;
    let stdout = read_limited(child.stdout.take().unwrap(), max_bytes);
    let stderr = read_limited(child.stderr.take().unwrap(), 8192);
    let output = async { tokio::try_join!(stdout, stderr) };
    tokio::pin!(output);
    let mut deadline = tokio::time::Instant::now() + budget;
    let mut cancelling = false;
    let mut captured = None;
    let completed = loop {
        tokio::select! {
            result = &mut output, if captured.is_none() => match result {
                Ok(bytes) => captured = Some(bytes),
                Err(error) => break Err(error),
            },
            status = child.wait(), if captured.is_some() => {
                break status
                    .map(|status| (captured.take().unwrap(), status))
                    .map_err(|error| EngineError::ExecError(error.to_string()));
            },
            _ = cancel.cancelled(), if !cancelling => {
                // This unreaped direct child cannot have its PID reused.
                cancelling = true;
                if let Some(pid) = child.id() {
                    unsafe { libc::kill(pid as i32, libc::SIGTERM); }
                }
                deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(8));
            }
            _ = tokio::time::sleep_until(deadline) => {
                if let Some(pid) = child.id() {
                    unsafe { libc::kill(pid as i32, libc::SIGTERM); }
                }
                break Err(EngineError::Timeout("execution result/cleanup unavailable; side effects may have occurred".into()));
            }
        }
    };
    if completed.is_err() {
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), child.wait()).await,
            Ok(Ok(_))
        ) {
            // Cleanup could not be confirmed; do not report a successful result.
            let _ = child.kill().await;
        }
    }
    let ((stdout, stderr), status) = completed?;
    if !status.success() {
        return Err(EngineError::ExecError(
            String::from_utf8_lossy(&stderr).into_owned(),
        ));
    }
    Ok(stdout)
}

#[async_trait]
impl ManagedRuntime for ProcessHandle {
    async fn action(&self, action: &str, payload: &Value) -> Result<Value, EngineError> {
        // A dead desktop control owner may have live detached descendants.
        // Keep its protected record and block recreation until Docker restarts
        // this container. Never guess that root-owner death proves cleanup.
        if action == "stop"
            && payload["service"] == "desktop"
            && payload["pid"].as_u64().unwrap_or(0) > 0
        {
            self.action("alive", payload).await?;
        }
        let mut payload = payload.clone();
        if action == "start" && payload["instance"]["service"] != "desktop" {
            payload["uid"] = json!(PAYLOAD_UID);
        }
        let mut command = python();
        command.env("AGENT_SPACE_LOCAL_RUNTIME", "1");
        command.args([
            "-I",
            "-c",
            include_str!("../../guest/managed.py"),
            action,
            &payload.to_string(),
        ]);
        let output = run_owner(
            command,
            Duration::from_secs(15),
            128 * 1024,
            CancellationToken::new(),
        )
        .await?;
        let value: Value = serde_json::from_slice(&output).map_err(|e| {
            EngineError::Other(format!("managed {action} returned invalid identity: {e}"))
        })?;
        if action == "alive"
            && payload["service"] == "desktop"
            && payload["pid"].as_u64().unwrap_or(0) > 0
            && value == false
        {
            let id = payload["instanceId"]
                .as_str()
                .ok_or_else(|| EngineError::Other("missing desktop identity".into()))?;
            uuid::Uuid::parse_str(id).map_err(|e| EngineError::Other(e.to_string()))?;
            if tokio::fs::try_exists(format!("/tmp/agent-space/{id}/instance.json"))
                .await
                .map_err(|e| EngineError::Other(e.to_string()))?
            {
                return Err(EngineError::Other("CLEANUP_UNCONFIRMED: desktop control owner lost; restart the Standalone container before opening another desktop".into()));
            }
        }
        Ok(value)
    }

    async fn copy_binary(&self, source: &str, destination: &str) -> Result<(), EngineError> {
        tokio::fs::copy(source, destination)
            .await
            .map_err(|e| EngineError::Other(format!("copy helper: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl AgentHandle for ProcessHandle {
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
            CancellationToken::new(),
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
        cancel: CancellationToken,
    ) -> Result<ExecResult, EngineError> {
        if program.is_empty() {
            return Ok(ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                truncated: false,
                output_path: None,
            });
        }
        let spec = json!({"program":program,"args":args,"uid":PAYLOAD_UID,
            "timeout_secs":timeout.map(|t|t.as_secs_f64()),
            "max_lines":max_lines.max(1),"max_bytes":max_bytes.max(1)});
        let mut command = python();
        command.args([
            "-I",
            "-c",
            include_str!("../../guest/exec.py"),
            &spec.to_string(),
        ]);
        let output = run_owner(
            command,
            timeout.unwrap_or(Duration::from_secs(86400)) + Duration::from_secs(8),
            max_bytes.saturating_mul(12).saturating_add(4096),
            cancel,
        )
        .await?;
        ExecResult::from_owned_output(&output)
    }

    async fn stop(&self) -> Result<(), EngineError> {
        for service in [
            ManagedService::Desktop,
            ManagedService::Webdav,
            ManagedService::StaticHttp,
        ] {
            self.reclaim_managed(service).await?;
        }
        Ok(())
    }

    async fn kill(&self) -> Result<(), EngineError> {
        self.stop().await
    }

    async fn status(&self) -> WorkspaceStatus {
        WorkspaceStatus::Running
    }

    fn name(&self) -> &str {
        WORKSPACE
    }

    async fn start_managed(&self, launch: &ManagedLaunch) -> Result<ManagedInstance, EngineError> {
        managed::start(self, launch).await
    }

    async fn managed_alive(&self, instance: &ManagedInstance) -> Result<bool, EngineError> {
        managed::request(self, "alive", &json!(instance)).await
    }

    async fn stop_managed(&self, instance: &ManagedInstance) -> Result<(), EngineError> {
        self.action("stop", &json!(instance)).await?;
        Ok(())
    }

    async fn list_managed(
        &self,
        service: ManagedService,
    ) -> Result<Vec<ManagedInstance>, EngineError> {
        managed::request(self, "list", &json!({"service":service})).await
    }

    async fn connect_guest_tcp(&self, port: u16) -> Result<GuestTcp, EngineError> {
        let stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|e| EngineError::Other(format!("local helper TCP: {e}")))?;
        let (reader, writer) = stream.into_split();
        Ok(GuestTcp {
            reader: Box::new(reader),
            writer: Box::new(writer),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_runtime_has_exactly_one_existing_workspace_and_cannot_delete_it() {
        let engine = ProcessEngine::new();
        assert_eq!(engine.list_workspaces().await.unwrap(), [WORKSPACE]);
        assert_eq!(
            engine.get_sandbox(WORKSPACE).await.unwrap().status().await,
            WorkspaceStatus::Running
        );
        assert!(matches!(
            engine.get_sandbox("another").await,
            Err(EngineError::NotFound(_))
        ));
        assert!(engine.remove_sandbox(WORKSPACE).await.is_err());
        assert!(engine.remove_volume("workspace").await.is_err());
    }

    #[tokio::test]
    async fn dropping_waiter_cancels_owner_and_reaps_foreground_descendants() {
        let path = std::env::temp_dir().join(format!("runcove-owner-{}", uuid::Uuid::new_v4()));
        let spec = json!({"program":"bash","args":["-c",format!("sleep 60 & echo $! > {}; wait",path.display())],"timeout_secs":60,"max_lines":20,"max_bytes":4096});
        let mut command = python();
        command.args([
            "-I",
            "-c",
            include_str!("../../guest/exec.py"),
            &spec.to_string(),
        ]);
        let waiter = tokio::spawn(run_owner(
            command,
            Duration::from_secs(70),
            65536,
            CancellationToken::new(),
        ));
        let pid: i32 = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(pid) = text.trim().parse() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        waiter.abort();
        let _ = waiter.await;
        tokio::time::timeout(Duration::from_secs(8), async {
            while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("abandoned owner must finish descendant cleanup");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn local_foreground_owner_enforces_deadline_and_retains_partial_output() {
        let spec = json!({"program":"bash","args":["-c","echo partial; sleep 20 & wait"],"timeout_secs":0.1,"max_lines":20,"max_bytes":4096});
        let mut command = python();
        command.args([
            "-I",
            "-c",
            include_str!("../../guest/exec.py"),
            &spec.to_string(),
        ]);
        let output = run_owner(
            command,
            Duration::from_secs(6),
            65536,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        match ExecResult::from_owned_output(&output) {
            Err(EngineError::ExecutionEnded {
                output,
                timed_out,
                cleanup_confirmed,
            }) => {
                assert!(timed_out && cleanup_confirmed);
                assert!(output.stdout.contains("partial"));
            }
            other => panic!("unexpected foreground result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn output_eof_does_not_leave_a_timed_out_owner_running() {
        let path = std::env::temp_dir().join(format!("runcove-eof-{}", uuid::Uuid::new_v4()));
        let script = format!(
            "import os,signal,time; open({:?},'w').write(str(os.getpid())); signal.signal(signal.SIGTERM,signal.SIG_IGN); os.close(1); os.close(2); time.sleep(1)",
            path.to_str().unwrap()
        );
        let mut command = python();
        command.args(["-I", "-c", &script]);
        let result = run_owner(
            command,
            Duration::from_millis(200),
            1024,
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(result, Err(EngineError::Timeout(_))));
        let pid: i32 = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        std::fs::remove_file(path).unwrap();
        let running = unsafe { libc::kill(pid, 0) } == 0;
        // The fixture exits independently even if this regression returns early;
        // cleanup never sends a signal to a PID that may already have been reaped.
        assert!(!running, "output EOF must not detach the execution owner");
    }
}
