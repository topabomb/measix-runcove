use agent_engine::{image::SpaceImage, state::StateStore};
use std::{fs, path::PathBuf};

fn legacy() -> (PathBuf, serde_json::Value) {
    let dir = std::env::temp_dir().join(format!("image-migrate-{}", uuid::Uuid::new_v4()));
    StateStore::init(&dir).unwrap();
    let state = StateStore::open(&dir).unwrap();
    state.create_user("alice", "test-img", &["ws"]).unwrap();
    drop(state);
    let mut raw: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("state.json")).unwrap()).unwrap();
    raw["schemaVersion"] = 2.into();
    raw["users"][0].as_object_mut().unwrap().remove("image");
    fs::write(dir.join("state.json"), serde_json::to_vec(&raw).unwrap()).unwrap();
    (dir, raw)
}

#[test]
fn migration_is_explicit_atomic_and_preserves_identity() {
    let (dir, original) = legacy();
    assert!(StateStore::open(&dir).is_err());
    let before = fs::read(dir.join("state.json")).unwrap();
    StateStore::migrate_v2(&dir, false, |_| Ok("old-image:1".into())).unwrap();
    assert_eq!(fs::read(dir.join("state.json")).unwrap(), before);
    assert!(!dir.join("state.schema2.backup.json").exists());
    StateStore::migrate_v2(&dir, true, |_| Ok("old-image:1".into())).unwrap();
    assert_eq!(
        fs::read(dir.join("state.schema2.backup.json")).unwrap(),
        before
    );
    let state = StateStore::open(&dir).unwrap();
    let user = state.get("alice").unwrap();
    assert_eq!(user.agent_space_id, original["users"][0]["agentSpaceId"]);
    assert_eq!(
        serde_json::to_value(&user.credential).unwrap(),
        original["users"][0]["credential"]
    );
    assert_eq!(
        user.image,
        Some(SpaceImage::Custom {
            reference: "old-image:1".into()
        })
    );
    assert!(StateStore::migrate_v2(&dir, true, |_| Ok("other".into())).is_err());
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_resolver_never_changes_state() {
    let (dir, _) = legacy();
    let before = fs::read(dir.join("state.json")).unwrap();
    assert!(StateStore::migrate_v2(&dir, true, |_| Err(
        agent_engine::state::StateError::Corrupt("metadata unavailable".into())
    ))
    .is_err());
    assert_eq!(before, fs::read(dir.join("state.json")).unwrap());
    assert!(!dir.join("state.schema2.backup.json").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn active_owner_blocks_migration_before_resolving_metadata() {
    let (dir, original) = legacy();
    // The previous process holds the same OS lock, even with schema2.
    use std::os::fd::AsRawFd;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("state.lock"))
        .unwrap();
    let spec = libc::flock {
        l_type: libc::F_WRLCK as _,
        l_whence: libc::SEEK_SET as _,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    assert_eq!(unsafe { libc::fcntl(lock.as_raw_fd(), 37, &spec) }, 0);
    let error = StateStore::migrate_v2(&dir, true, |_| {
        panic!("must not read metadata while an owner is active")
    })
    .unwrap_err();
    assert!(error.to_string().contains("lock"));
    let after: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(original, after);
    drop(lock);
    fs::remove_dir_all(dir).unwrap();
}
