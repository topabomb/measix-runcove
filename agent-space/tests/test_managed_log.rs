#[allow(dead_code)]
#[path = "../build-support/bounded_log.rs"]
mod bounded_log;

#[test]
fn managed_log_is_bounded_and_retains_latest_output() {
    use std::io::Read;
    let path = std::env::temp_dir().join(format!("managed-log-{}", uuid::Uuid::new_v4()));
    let mut log = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .append(true)
        .open(&path)
        .unwrap();
    // Unlink while open: no test artifact is left even if the assertion fails.
    std::fs::remove_file(path).unwrap();
    let mut output = vec![b'x'; 3 * 1024 * 1024 + 17];
    output.extend_from_slice(b"latest diagnostic\n");
    bounded_log::drain(output.as_slice(), log.try_clone().unwrap()).unwrap();
    assert!(log.metadata().unwrap().len() <= 1024 * 1024);
    std::io::Seek::rewind(&mut log).unwrap();
    let mut retained = Vec::new();
    log.read_to_end(&mut retained).unwrap();
    assert!(retained.ends_with(b"latest diagnostic\n"));
}
