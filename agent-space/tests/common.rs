// This file is a shared test utility module, compiled as part of multiple test binaries.
// Each test binary uses a different subset of helpers, so dead_code is expected.
#![allow(dead_code)]

use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, VolumeKind, WorkspaceOpts};
use std::path::PathBuf;

#[path = "../build-support/helper.rs"]
pub mod helper_build;

pub fn msb_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("MSB_BIN") {
        return PathBuf::from(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".local/bin/msb");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("msb")
}

pub fn static_helper_binary() -> String {
    if let Some(path) = option_env!("CARGO_BIN_EXE_agent-space-static-http") {
        return path.to_owned();
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .unwrap_or_else(|| root.join("target"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let binary = target.join(profile).join("agent-space-static-http");
    assert!(
        binary.is_file(),
        "missing {}; build agent-space-static-http with --locked before this targeted VM test",
        binary.display()
    );
    verify_helper_identity(&binary, &root, "static-http");
    binary.to_string_lossy().into_owned()
}

pub fn verify_helper_identity(
    binary: &std::path::Path,
    workspace: &std::path::Path,
    package: &str,
) {
    let expected = helper_build::fingerprint(workspace, package).expect("read helper build inputs");
    let output = std::process::Command::new(binary)
        .arg("--build-id")
        .output()
        .expect("read helper binary identity");
    assert!(
        output.status.success(),
        "{} does not support --build-id; rebuild the current helper",
        binary.display()
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected,
        "{} is built from different source inputs; rebuild with --locked",
        binary.display()
    );
}

/// Stop and remove all test sandboxes and their volumes.
/// Called at the start of each test to catch orphans from previous failed tests.
/// Uses msb CLI with --force for fast cleanup.
pub async fn cleanup_test_sandboxes() {
    // Step 1: Force-stop all test sandboxes via msb CLI
    let test_names = list_test_sandboxes_cli().await;
    for name in &test_names {
        let _ = std::process::Command::new(msb_binary())
            .args(["stop", "--force", name])
            .output();
    }

    // Step 2: Remove all test sandboxes via msb CLI
    for name in &test_names {
        let _ = std::process::Command::new(msb_binary())
            .args(["remove", name])
            .output();
    }

    // Step 3: Also remove any test sandboxes visible to SDK (belt and suspenders)
    if let Ok(list) = MsbEngine::new().list_running().await {
        for name in list {
            if name.starts_with("test-") {
                let _ = microsandbox::Sandbox::remove(&name).await;
            }
        }
    }

    // Step 4: Remove orphaned volumes
    cleanup_orphaned_volumes().await;
}

/// List test sandboxes via msb CLI (works even if SDK can't connect).
async fn list_test_sandboxes_cli() -> Vec<String> {
    let Ok(output) = std::process::Command::new(msb_binary())
        .args(["list"])
        .output()
    else {
        return vec![];
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| {
            let name = line.split_whitespace().next()?;
            if name.starts_with("test-") {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Remove all test-prefixed volumes.
pub async fn cleanup_orphaned_volumes() {
    if let Ok(volumes) = microsandbox::Volume::list().await {
        for vol in volumes {
            let vol_name = vol.name().to_string();
            if vol_name.starts_with("test-") {
                let _ = vol.remove().await;
            }
        }
    }
}

/// Clean up a single sandbox and its associated disk volumes.
/// Uses msb remove --force to instantly kill + remove.
pub async fn cleanup_sandbox(_engine: &MsbEngine, name: &str, opts: &WorkspaceOpts) {
    // Remove via CLI with --force (instant kill + remove, no graceful stop)
    let _ = std::process::Command::new(msb_binary())
        .args(["remove", "--force", name])
        .output();

    // Remove disk volumes
    for vol in &opts.volumes {
        if vol.kind == VolumeKind::Disk {
            let vol_name = vol.name.as_deref().unwrap_or("vol");
            let scoped = format!("{}-{}", name, vol_name);
            let _ = microsandbox::Volume::remove(&scoped).await;
        }
    }
}
