#[path = "../../tests/common.rs"]
mod common;

#[test]
fn binary_identity_ignores_timestamps_but_rejects_different_sources() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let copy = tempfile::tempdir().unwrap();
    for source in common::helper_build::inputs(workspace, "webdav").unwrap() {
        let destination = copy.path().join(source.strip_prefix(workspace).unwrap());
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(&source, &destination).unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&destination)
            .unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600)),
        )
        .unwrap();
    }
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_agent-space-webdav"));
    common::verify_helper_identity(binary, copy.path(), "webdav");
    for name in [
        "webdav/src/main.rs",
        "vendor/dav-server/src/handle_props.rs",
        "vendor/dav-server/Cargo.toml",
    ] {
        let source = copy.path().join(name);
        let original = std::fs::read(&source).unwrap();
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&source)
            .unwrap()
            .write_all(b"\n# changed input\n")
            .unwrap();
        assert!(std::panic::catch_unwind(|| common::verify_helper_identity(
            binary,
            copy.path(),
            "webdav"
        ))
        .is_err());
        std::fs::write(source, original).unwrap();
        common::verify_helper_identity(binary, copy.path(), "webdav");
    }
}
