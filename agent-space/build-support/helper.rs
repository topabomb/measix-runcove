//! Shared by the helper build scripts and VM-test preflight. Identity is based
//! on source content, not file timestamps or the placeholder package version.
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};

fn source_files(path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            source_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}
pub fn inputs(workspace: &Path, package: &str) -> io::Result<Vec<PathBuf>> {
    let mut files = vec![
        workspace.join("Cargo.toml"),
        workspace.join("Cargo.lock"),
        workspace.join("build-support/helper.rs"),
        workspace.join(package).join("Cargo.toml"),
        workspace.join(package).join("build.rs"),
    ];
    source_files(&workspace.join(package).join("src"), &mut files)?;
    source_files(&workspace.join("build-support"), &mut files)?;
    if package == "webdav" {
        files.push(workspace.join("vendor/dav-server/Cargo.toml"));
        source_files(&workspace.join("vendor/dav-server/src"), &mut files)?;
    }
    files.sort();
    files.dedup();
    Ok(files)
}
pub fn fingerprint(workspace: &Path, package: &str) -> io::Result<String> {
    let mut digest = Sha256::new();
    for path in inputs(workspace, package)? {
        let relative = path.strip_prefix(workspace).unwrap().to_string_lossy();
        let bytes = std::fs::read(&path)?;
        digest.update((relative.len() as u64).to_le_bytes());
        digest.update(relative.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    Ok(format!("{:x}", digest.finalize()))
}
