//! Verified process-tree shutdown, ported from the qualified lab supervisor.
use std::{
    collections::{HashMap, HashSet},
    fs,
    time::Duration,
};
use tokio::process::Command;

/// Match the workspace command/file principal. Only Standalone shares its
/// container with a privileged controller; Server commands already own the VM.
pub fn workspace_uid() -> u32 {
    if std::env::var_os("AGENT_SPACE_LOCAL_RUNTIME").is_some() {
        1001
    } else {
        0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub pid: u32,
    ticks: u64,
}
pub fn identity(pid: u32) -> Option<Identity> {
    let data = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = data
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields.first() == Some(&"Z") {
        return None;
    }
    Some(Identity {
        pid,
        ticks: fields.get(19)?.parse().ok()?,
    })
}
pub fn descendants(root: u32) -> Vec<Identity> {
    let mut parents = HashMap::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for p in entries.flatten() {
            let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            if let Ok(stat) = fs::read_to_string(p.path().join("stat")) {
                if let Some(parent) = stat
                    .rsplit_once(')')
                    .and_then(|(_, s)| s.split_whitespace().nth(1))
                    .and_then(|s| s.parse::<u32>().ok())
                {
                    parents.insert(pid, parent);
                }
            }
        }
    }
    let mut owned = HashSet::from([root]);
    loop {
        let additions = parents
            .iter()
            .filter(|(pid, parent)| owned.contains(parent) && !owned.contains(pid))
            .map(|(pid, _)| *pid)
            .collect::<Vec<_>>();
        if additions.is_empty() {
            break;
        }
        owned.extend(additions);
    }
    owned.remove(&root);
    owned.into_iter().filter_map(identity).collect()
}
pub async fn stop_tree(root: Identity) -> anyhow::Result<()> {
    // Keep verified descendants after their parent exits. Never discover a tree
    // under a PID whose start time no longer matches the original owner.
    if identity(root.pid) != Some(root) {
        return Ok(());
    }
    let mut owned = descendants(root.pid);
    owned.push(root);
    for (signal, wait) in [(libc::SIGTERM, 2), (libc::SIGKILL, 1)] {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
        loop {
            let verified = owned
                .iter()
                .copied()
                .filter(|p| identity(p.pid) == Some(*p))
                .collect::<Vec<_>>();
            for record in &verified {
                for child in descendants(record.pid) {
                    if !owned.contains(&child) {
                        owned.push(child);
                    }
                }
            }
            owned.retain(|p| identity(p.pid) == Some(*p));
            if owned.is_empty() {
                return Ok(());
            }
            for record in &owned {
                if identity(record.pid) == Some(*record) {
                    unsafe {
                        libc::kill(record.pid as i32, signal);
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    anyhow::ensure!(
        owned.iter().all(|p| identity(p.pid) != Some(*p)),
        "process cleanup unconfirmed"
    );
    Ok(())
}
pub fn isolate(command: &mut Command, user: Option<u32>) {
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(uid) = user {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(uid) != 0
                    || libc::setuid(uid) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_a_reused_identity() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let current = identity(child.id().unwrap()).unwrap();
        stop_tree(Identity {
            ticks: current.ticks + 1,
            ..current
        })
        .await
        .unwrap();
        assert_eq!(identity(current.pid), Some(current));
        child.kill().await.unwrap();
    }
    #[tokio::test]
    async fn retains_children_when_parent_exits_first() {
        let mut command = Command::new("bash");
        command.args([
            "-c",
            "trap 'exit 0' TERM; (trap '' TERM; exec sleep 30) & wait",
        ]);
        isolate(&mut command, None);
        let mut child = command.spawn().unwrap();
        let root = identity(child.id().unwrap()).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let descendants = loop {
            let children = descendants(root.pid);
            if !children.is_empty() {
                break children;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        stop_tree(root).await.unwrap();
        child.wait().await.unwrap();
        assert!(descendants.iter().all(|p| identity(p.pid) != Some(*p)));
    }
}
