use microsandbox::{Sandbox, Volume};
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

unsafe extern "C" { fn sqlite3_libversion() -> *const std::ffi::c_char; }

fn inspect(label: &str) {
    let pid = std::process::id();
    let locks = std::fs::read_to_string("/proc/locks").unwrap_or_default();
    let own: Vec<_> = locks.lines().filter(|line| line.split_whitespace().nth(4) == Some(&pid.to_string())).collect();
    let mut mappings = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc/self/fd") {
        for entry in entries.flatten() {
            if let Ok(target) = std::fs::read_link(entry.path()) {
                if target.to_string_lossy().contains("msb.db-shm") { mappings.push(target.display().to_string()); }
            }
        }
    }
    println!("INSPECT {label} pid={pid} locks={own:?} shm={mappings:?}");
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let version = unsafe { std::ffi::CStr::from_ptr(sqlite3_libversion()) }.to_string_lossy();
    println!("SDK=0.7.2 SQLITE={version} MODE={}", args.get(1).map(String::as_str).unwrap_or("parent"));
    std::io::stdout().flush()?;
    if args.get(1).is_some_and(|arg| arg == "writer") {
        let name = &args[2];
        let scoped: std::sync::Arc<dyn microsandbox::backend::Backend> = std::sync::Arc::new(microsandbox::backend::LocalBackend::new().await?);
        microsandbox::backend::with_backend(scoped.clone(), async {
            match Volume::builder(name).create().await {
                Ok(volume) => println!("CREATED {}", volume.name()),
                Err(error) => println!("CREATE_ERROR {error}"),
            }
        }).await;
        println!("READY");
        std::io::stdout().flush()?;
        if args.get(3).is_some_and(|arg| arg == "hold") {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
        }
        drop(scoped);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        println!("DROPPED_CHILD_BACKEND");
        return Ok(());
    }
    match Sandbox::list().await {
        Ok(items) => println!("INITIAL_LIST count={}", items.sandboxes.len()),
        Err(error) => println!("INITIAL_LIST_ERROR {error}"),
    }
    inspect("after-sdk-list");
    let exe = std::env::current_exe()?;
    let first = Command::new(&exe).args(["writer", "child-first", "exit"]).output()?;
    print!("{}", String::from_utf8_lossy(&first.stdout));
    println!("FIRST_CHILD_EXIT {}", first.status);
    inspect("after-first-child-exit");
    match Volume::builder("parent-write").create().await {
        Ok(volume) => println!("PARENT_CREATED {}", volume.name()),
        Err(error) => println!("PARENT_CREATE_ERROR {error}"),
    }
    let mut second = Command::new(&exe).args(["writer", "child-second", "hold"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?;
    let mut output = std::io::BufReader::new(second.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        if output.read_line(&mut line)? == 0 { break; }
        print!("{line}");
        if line.trim() == "READY" { break; }
    }
    inspect("while-second-child-alive");
    match Sandbox::list().await {
        Ok(items) => println!("FINAL_SANDBOX_LIST count={}", items.sandboxes.len()),
        Err(error) => println!("FINAL_SANDBOX_LIST_ERROR {error}"),
    }
    match Volume::list().await {
        Ok(items) => println!("FINAL_VOLUME_LIST count={}", items.len()),
        Err(error) => println!("FINAL_VOLUME_LIST_ERROR {error}"),
    }
    second.stdin.as_mut().unwrap().write_all(b"\n")?;
    second.wait()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(run())
}
