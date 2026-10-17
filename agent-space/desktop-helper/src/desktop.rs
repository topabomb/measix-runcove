use crate::process::{self, Identity};
use std::{
    fs,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    net::TcpStream,
    process::{Child, Command},
};
pub const ROOT: &str = "/workspace/.agent-space/desktop";
pub const ARTIFACTS: &str = "/workspace/agent-space/artifacts";
pub const NODE: &str = "/opt/agent-space/desktop";
/// Node sources travel with the helper's verified binary, not an independently
/// editable release directory. Replacing a helper replaces one matched bundle.
pub fn install_sources() -> anyhow::Result<()> {
    fs::create_dir_all(NODE)?;
    anyhow::ensure!(
        fs::symlink_metadata(NODE)?.is_dir(),
        "invalid worker directory"
    );
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    for (name, source) in [
        (
            "browser-worker.cjs",
            include_str!("../../guest/desktop/browser-worker.cjs"),
        ),
        (
            "observe-worker.cjs",
            include_str!("../../guest/desktop/observe-worker.cjs"),
        ),
        (
            "observation.cjs",
            include_str!("../../guest/desktop/observation.cjs"),
        ),
        ("script.cjs", include_str!("../../guest/desktop/script.cjs")),
        (
            "artifacts.cjs",
            include_str!("../../guest/desktop/artifacts.cjs"),
        ),
    ] {
        let path = Path::new(NODE).join(name);
        let temp = path.with_extension("cjs.tmp");
        // Reject symlinks at the fixed staging location before writing.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        use std::io::Write;
        (&file).write_all(source.as_bytes())?;
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
        fs::rename(temp, path)?;
    }
    Ok(())
}
fn migrate_artifacts(old: &Path, new: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(new)?;
    anyhow::ensure!(
        fs::symlink_metadata(new)?.is_dir(),
        "artifact destination is not a directory"
    );
    match fs::symlink_metadata(old) {
        Ok(meta) => anyhow::ensure!(meta.is_dir(), "legacy artifact location is not a directory"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    let entries = match fs::read_dir(old) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        // Generated artifacts are regular files. Leave unexpected user entries intact.
        if !entry.file_type()?.is_file() {
            continue;
        }
        let target = new.join(entry.file_name());
        match fs::hard_link(entry.path(), target) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                use std::os::unix::fs::MetadataExt;
                let source = entry.metadata()?;
                let target = fs::symlink_metadata(new.join(entry.file_name()))?;
                if source.dev() == target.dev() && source.ino() == target.ino() {
                    fs::remove_file(entry.path())?;
                    continue;
                }
                fs::hard_link(
                    entry.path(),
                    new.join(format!(
                        "{}-{}",
                        uuid::Uuid::new_v4(),
                        entry.file_name().to_string_lossy()
                    )),
                )?;
            }
            Err(e) => return Err(e.into()),
        }
        fs::remove_file(entry.path())?;
    }
    match fs::remove_dir(old) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub fn prepare_workspace() -> anyhow::Result<()> {
    let uid = process::workspace_uid();
    if let Err(error) = migrate_artifacts(
        Path::new(&format!("{ROOT}/artifacts")),
        Path::new(ARTIFACTS),
    ) {
        eprintln!("Legacy artifacts retained: {error}");
    }
    for path in [
        ROOT.to_owned(),
        format!("{ROOT}/profile"),
        format!("{ROOT}/profile/Default"),
        ARTIFACTS.to_owned(),
        "/tmp/runtime-desktop".into(),
    ] {
        fs::create_dir_all(&path)?;
        anyhow::ensure!(
            fs::symlink_metadata(&path)?.is_dir(),
            "reserved directory is not a directory"
        );
        let c = std::ffi::CString::new(path.clone())?;
        unsafe {
            anyhow::ensure!(
                (libc::geteuid() != 0 || libc::chown(c.as_ptr(), uid, uid) == 0),
                "directory ownership failed"
            );
        }
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let preferences = format!("{ROOT}/profile/Default/Preferences");
    if let Ok(bytes) = fs::read(&preferences) {
        if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if value["download"]["default_directory"] == format!("{ROOT}/artifacts") {
                value["download"]["default_directory"] = ARTIFACTS.into();
                let temporary = format!("{preferences}.agent-space-migrate");
                fs::write(&temporary, serde_json::to_vec(&value)?)?;
                fs::rename(temporary, &preferences)?;
                let c = std::ffi::CString::new(preferences.clone())?;
                anyhow::ensure!(
                    unsafe { libc::chown(c.as_ptr(), uid, uid) } == 0,
                    "preferences ownership failed"
                );
            }
        }
    }
    if !Path::new(&preferences).exists() {
        fs::write(&preferences,serde_json::json!({"download":{"default_directory":ARTIFACTS,"prompt_for_download":false},"browser":{"check_default_browser":false}}).to_string())?;
        let c = std::ffi::CString::new(preferences)?;
        unsafe {
            if libc::geteuid() == 0 {
                anyhow::ensure!(
                    libc::chown(c.as_ptr(), uid, uid) == 0,
                    "preferences ownership failed"
                );
            }
        }
    }
    Ok(())
}

struct Owned {
    child: Child,
    identity: Identity,
    critical: bool,
    browser: bool,
    display: bool,
}
pub struct Desktop {
    children: Vec<Owned>,
    pub last_used: Instant,
    generation: u64,
    ready: bool,
}
impl Default for Desktop {
    fn default() -> Self {
        Self {
            children: vec![],
            last_used: Instant::now(),
            generation: 0,
            ready: false,
        }
    }
}
impl Desktop {
    pub fn display_alive(&mut self) -> bool {
        self.ready
            && self
                .children
                .iter_mut()
                .any(|p| p.display && matches!(p.child.try_wait(), Ok(None)))
    }
    pub fn alive(&mut self) -> bool {
        self.ready
            && self.children_alive()
            && self
                .children
                .iter_mut()
                .any(|p| p.browser && matches!(p.child.try_wait(), Ok(None)))
    }
    fn children_alive(&mut self) -> bool {
        !self.children.is_empty()
            && self
                .children
                .iter_mut()
                .all(|p| p.browser || !p.critical || matches!(p.child.try_wait(), Ok(None)))
    }
    async fn spawn(&mut self, name: &str, args: &[&str], critical: bool) -> anyhow::Result<()> {
        let mut command = Command::new(args[0]);
        process::workspace_env(&mut command);
        command
            .args(&args[1..])
            .env("DISPLAY", ":1")
            .env("XDG_RUNTIME_DIR", "/tmp/runtime-desktop")
            .env_remove("AGENT_SPACE_HELPER")
            .env_remove("AGENT_SPACE_HELPER_INSTANCE")
            .env_remove("AGENT_SPACE_HELPER_SERVICE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        process::isolate(&mut command, Some(process::workspace_uid()));
        let mut child = command.spawn()?;
        let identity = process::identity(
            child
                .id()
                .ok_or_else(|| anyhow::anyhow!("missing child PID"))?,
        )
        .ok_or_else(|| anyhow::anyhow!("child exited before identity"))?;
        let log_dir = std::env::var("AGENT_SPACE_HELPER_INSTANCE").unwrap_or_default();
        for (label, pipe) in [
            (
                "out",
                child
                    .stdout
                    .take()
                    .map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
            ),
            (
                "err",
                child
                    .stderr
                    .take()
                    .map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
            ),
        ] {
            if let Some(mut pipe) = pipe {
                let path = format!("/tmp/agent-space/{log_dir}/desktop-{name}-{label}.log");
                tokio::spawn(async move {
                    let mut total = 0;
                    let mut buf = [0; 8192];
                    if let Ok(mut file) = tokio::fs::File::create(path).await {
                        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
                        while let Ok(n) = pipe.read(&mut buf).await {
                            if n == 0 {
                                break;
                            }
                            if total + n > 1024 * 1024 {
                                let _ = file.set_len(0).await;
                                let _ = file.seek(std::io::SeekFrom::Start(0)).await;
                                total = 0;
                            }
                            if file.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                            total += n;
                        }
                    } else {
                        while let Ok(n) = pipe.read(&mut buf).await {
                            if n == 0 {
                                break;
                            }
                        }
                    }
                });
            }
        }
        self.children.push(Owned {
            child,
            identity,
            critical,
            browser: name == "browser",
            display: name == "vnc",
        });
        Ok(())
    }
    pub async fn start_display(&mut self) -> anyhow::Result<()> {
        if self.display_alive() {
            return Ok(());
        }
        self.stop().await?;
        if std::env::var_os("AGENT_SPACE_LOCAL_RUNTIME").is_some() {
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("--prepare-workspace")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            process::workspace_env(&mut command);
            process::isolate(&mut command, Some(process::workspace_uid()));
            let output = tokio::time::timeout(Duration::from_secs(15), command.output()).await??;
            anyhow::ensure!(
                output.status.success(),
                "workspace preparation: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        } else {
            prepare_workspace()?;
        }
        let result = async {
            self.spawn(
                "vnc",
                &[
                    "Xtigervnc",
                    ":1",
                    "-SecurityTypes",
                    "None",
                    "-localhost",
                    "yes",
                    "-AlwaysShared",
                    "-geometry",
                    "1440x900",
                    "-depth",
                    "24",
                ],
                true,
            )
            .await?;
            self.ready(5901, true, Duration::from_secs(5)).await?;
            self.spawn("desktop", &["dbus-run-session", "--", "startlxqt"], true)
                .await?;
            self.spawn("terminal", &["qterminal"], false).await?;
            self.ready = true;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if result.is_err() {
            self.stop().await?;
        }
        result
    }
    pub async fn start(&mut self) -> anyhow::Result<()> {
        if self.ready && !self.children_alive() {
            self.stop().await?;
        }
        self.start_display().await?;
        if self.alive() {
            return Ok(());
        }
        let mut index = 0;
        while index < self.children.len() {
            if self.children[index].browser {
                let mut owned = self.children.remove(index);
                process::stop_tree(owned.identity).await?;
                let _ = owned.child.wait().await;
            } else {
                index += 1;
            }
        }
        let profile = format!("--user-data-dir={ROOT}/profile");
        // No launch URL: wake restores tabs without appending about:blank.
        let mut args = vec![
            "/opt/agent-tools/browsers/chromium-1243/chrome-linux64/chrome",
            "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=9222",
            &profile,
            "--no-first-run",
            "--disable-infobars",
            "--no-default-browser-check",
            "--password-store=basic",
            "--restore-last-session",
        ];
        // Server's VM is the isolation boundary. Chromium refuses root without
        // this flag; Standalone retains its unprivileged Chromium sandbox.
        if process::workspace_uid() == 0 {
            args.push("--no-sandbox");
        }
        self.spawn("browser", &args, true).await?;
        self.ready(9222, false, Duration::from_secs(15)).await?;
        self.generation += 1;
        self.last_used = Instant::now();
        node(serde_json::json!({"maximize":true})).await?;
        Ok(())
    }
    async fn ready(&mut self, port: u16, rfb: bool, timeout: Duration) -> anyhow::Result<()> {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            anyhow::ensure!(self.children_alive(), "critical desktop child exited");
            if let Ok(Ok(mut tcp)) = tokio::time::timeout(
                Duration::from_millis(500),
                TcpStream::connect(("127.0.0.1", port)),
            )
            .await
            {
                if !rfb {
                    return Ok(());
                }
                let mut header = [0; 12];
                if matches!(
                    tokio::time::timeout(Duration::from_millis(500), tcp.read_exact(&mut header))
                        .await,
                    Ok(Ok(_))
                ) && &header == b"RFB 003.008\n"
                {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("desktop readiness deadline exceeded")
    }
    pub async fn stop(&mut self) -> anyhow::Result<()> {
        let has_browser = self.children.iter().any(|p| p.browser);
        self.ready = false;
        if has_browser {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                node(serde_json::json!({"action":"close"})),
            )
            .await;
            if let Some(browser) = self.children.last_mut() {
                let _ = tokio::time::timeout(Duration::from_secs(5), browser.child.wait()).await;
            }
        }
        let mut failures = vec![];
        for mut owned in self.children.drain(..).rev() {
            if let Err(e) = process::stop_tree(owned.identity).await {
                failures.push(e.to_string());
            }
            let _ = tokio::time::timeout(Duration::from_secs(1), owned.child.wait()).await;
        }
        // Daemonized LXQt children are adopted by this subreaper. A desktop is
        // stopped only when no browser task is running; these remain its tree.
        for child in process::descendants(std::process::id()) {
            if let Err(e) = process::stop_tree(child).await {
                failures.push(e.to_string());
            }
        }
        loop {
            let pid = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
            if pid <= 0 {
                break;
            }
        }
        anyhow::ensure!(
            failures.is_empty(),
            "desktop cleanup unconfirmed: {}",
            failures.join("; ")
        );
        Ok(())
    }
}
pub async fn node(value: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let spec = serde_json::json!({"program":"node","args":[format!("{NODE}/observe-worker.cjs"),value.to_string()],"timeout_secs":5,"max_bytes":agent_engine::media::REPORT_LIMIT,"max_lines":10000,"uid":process::workspace_uid()});
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
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = command.output().await?;
    anyhow::ensure!(output.status.success(), "observation owner failed");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(
        result["truncated"] != true && result["stdout"].is_string(),
        "INVALID_OBSERVATION_REPORT: truncated or incomplete report"
    );
    anyhow::ensure!(
        result["exit_code"] == 0,
        "{}",
        result["stderr"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Observation failed without diagnostic output")
            .trim()
    );
    Ok(serde_json::from_str(
        result["stdout"].as_str().unwrap_or_default(),
    )?)
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    #[test]
    fn artifacts_migrate_without_overwrite_and_keep_unknown_entries() {
        let base =
            std::env::temp_dir().join(format!("artifact-migration-{}", uuid::Uuid::new_v4()));
        let old = base.join("old");
        let new = base.join("new");
        fs::create_dir_all(old.join("user-directory")).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("a.txt"), "old").unwrap();
        fs::write(new.join("a.txt"), "new").unwrap();
        fs::write(old.join("b.txt"), "second").unwrap();
        migrate_artifacts(&old, &new).unwrap();
        assert_eq!(fs::read(new.join("a.txt")).unwrap(), b"new");
        assert_eq!(fs::read(new.join("b.txt")).unwrap(), b"second");
        assert_eq!(fs::read_dir(&new).unwrap().count(), 3);
        assert!(old.join("user-directory").is_dir());
        migrate_artifacts(&old, &new).unwrap();
        assert_eq!(fs::read_dir(&new).unwrap().count(), 3);
        fs::remove_dir_all(base).unwrap();
    }
}
