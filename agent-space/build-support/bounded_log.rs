//! Drain stderr in the helper itself, retaining at most 1 MiB per instance.
//! This does not impose RLIMIT_FSIZE (which would also limit DAV user files).
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, Write},
    os::fd::{AsRawFd, FromRawFd},
};
const LIMIT: usize = 1024 * 1024;
pub fn install() -> io::Result<()> {
    // Legacy markers alone use the legacy stderr file; no new runtime directory exists.
    if std::env::var_os("AGENT_SPACE_HELPER_SERVICE").is_none() {
        return Ok(());
    }
    let Ok(instance) = std::env::var("AGENT_SPACE_HELPER_INSTANCE") else {
        return Ok(());
    };
    if instance.len() != 32 || !instance.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::other("invalid managed instance ID"));
    }
    let path = format!("/tmp/agent-space/{instance}/log");
    let log = OpenOptions::new().create(true).append(true).open(path)?;
    let mut pipe = [0; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let read = unsafe { File::from_raw_fd(pipe[0]) };
    let write = unsafe { File::from_raw_fd(pipe[1]) };
    if unsafe { libc::dup2(write.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    drop(write);
    std::thread::Builder::new()
        .name("managed-log".into())
        .spawn(move || {
            let _ = drain(read, log);
        })?;
    Ok(())
}
pub fn drain(mut source: impl Read, mut log: File) -> io::Result<()> {
    let mut size = log.metadata()?.len() as usize;
    let mut buffer = [0; 8192];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        if size + count > LIMIT {
            log.set_len(0)?;
            log.rewind()?;
            size = 0;
        }
        log.write_all(&buffer[..count])?;
        size += count;
    }
}
