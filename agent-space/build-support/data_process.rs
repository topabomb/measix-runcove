//! Keep the verified helper control process root-owned while its file-serving
//! child runs as the workspace user. No SYS_PTRACE capability is required to
//! inspect the root-owned control process through /proc.
use std::{
    io,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

pub fn supervise() -> io::Result<()> {
    let Some(uid) = std::env::var_os("AGENT_SPACE_DATA_UID") else {
        return Ok(());
    };
    if uid != "1001" || unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::other("invalid data-plane identity"));
    }
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(std::env::args_os().skip(1))
        .env_remove("AGENT_SPACE_DATA_UID")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(1001) != 0
                || libc::setuid(1001) != 0
            {
                return Err(io::Error::last_os_error());
            }
            // A UID transition clears PDEATHSIG, so install it afterward.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                || libc::getppid() != parent
            {
                return Err(io::Error::other("data-plane parent no longer owns child"));
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let mut stopping = None;
    loop {
        if let Some(status) = child.try_wait()? {
            std::process::exit(status.code().unwrap_or(1));
        }
        if STOP.load(Ordering::Relaxed) && stopping.is_none() {
            // The direct child is unreaped, so its PID cannot be reused.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            stopping = Some(Instant::now());
        }
        if stopping.is_some_and(|at| at.elapsed() > Duration::from_secs(1)) {
            child.kill()?;
            let _ = child.wait()?;
            std::process::exit(0);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
