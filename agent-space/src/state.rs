//! File-backed user and resource ownership.
//!
//! One process holds the directory lock. Mutations are serialized in memory and
//! become visible only after the snapshot is fsynced and atomically replaced.
//! A missing, corrupt, or unsupported snapshot fails closed: it is never treated
//! as an empty user list.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("invalid username")]
    InvalidUsername,
    #[error("user already exists: {0}")]
    AlreadyExists(String),
    #[error("user not found: {0}")]
    NotFound(String),
    #[error("agent space id does not match")]
    SpaceMismatch,
    #[error("user is disabled: {0}")]
    Disabled(String),
    #[error("disabled workspace is still stopping: {0}")]
    StopPending(String),
    #[error("user is being deleted: {0}")]
    Deleting(String),
    #[error("invalid credential")]
    InvalidCredential,
    #[error("DAV token must contain 32..256 ASCII letters, digits, or -._~+/ with optional trailing = padding")]
    InvalidDavToken,
    #[error("state directory is not initialized")]
    NotInitialized,
    #[error("state file is corrupt or unsupported: {0}")]
    Corrupt(String),
    #[error("state directory is locked by another process")]
    Locked,
    #[error("management writes are paused: {0}")]
    WritesPaused(String),
    #[error("state io error: {0}")]
    Io(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserStatus {
    Active,
    Disabled,
    Deleting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    #[serde(rename = "keyId")]
    pub key_id: String,
    #[serde(rename = "secretDigest")]
    pub secret_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cleanup {
    #[serde(rename = "requestedAt")]
    pub requested_at: String,
    #[serde(rename = "lastError", default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserRecord {
    #[serde(default)]
    pub image: Option<crate::image::SpaceImage>,
    pub username: String,
    #[serde(rename = "agentSpaceId")]
    pub agent_space_id: String,
    pub status: UserStatus,
    pub credential: Option<Credential>,
    #[serde(rename = "davCredential")]
    pub dav_credential: Option<Credential>,
    #[serde(rename = "sandboxName")]
    pub sandbox_name: String,
    #[serde(rename = "ownedVolumeNames")]
    pub owned_volume_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<Cleanup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    users: Vec<UserRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedToken {
    pub key_id: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct ImportUser {
    pub image: crate::image::SpaceImage,
    pub username: String,
    pub sandbox_name: String,
    pub owned_volume_names: Vec<String>,
}

pub struct StateStore {
    dir: PathBuf,
    lock: Mutex<Snapshot>,
    writes_paused: Mutex<Option<String>>,
    _lock_file: Option<File>,
    /// Standalone has one read-only in-memory identity, never an account file.
    single_mcp_digest: Option<String>,
    changes: tokio::sync::watch::Sender<()>,
}

#[derive(Clone, Copy)]
enum CredentialKind {
    Mcp,
    Dav,
}
impl CredentialKind {
    fn get(self, user: &UserRecord) -> Option<&Credential> {
        match self {
            Self::Mcp => user.credential.as_ref(),
            Self::Dav => user.dav_credential.as_ref(),
        }
    }
    fn slot(self, user: &mut UserRecord) -> &mut Option<Credential> {
        match self {
            Self::Mcp => &mut user.credential,
            Self::Dav => &mut user.dav_credential,
        }
    }
}

impl StateStore {
    /// Offline, all-or-nothing migration. Resolver reads metadata only; the
    /// exclusive lock prevents racing an active service or administrator.
    pub fn migrate_v2(
        dir: impl AsRef<Path>,
        apply: bool,
        mut resolve: impl FnMut(&str) -> Result<String, StateError>,
    ) -> Result<Vec<String>, StateError> {
        let dir = dir.as_ref();
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(dir.join("state.lock"))
            .map_err(io_err)?;
        lock_exclusive(&lock)?;
        let mut snapshot = load_snapshot(dir)?;
        if snapshot.schema_version != 2 {
            return Err(StateError::Corrupt(
                "migration requires schemaVersion 2".into(),
            ));
        }
        if snapshot.users.iter().any(|user| user.image.is_some()) {
            return Err(StateError::Corrupt(
                "schema2 unexpectedly contains image fields".into(),
            ));
        }
        let mut names = Vec::new();
        for user in &mut snapshot.users {
            let image = crate::image::SpaceImage::Custom {
                reference: resolve(&user.sandbox_name)?,
            };
            image
                .validate()
                .map_err(|error| StateError::Corrupt(error.to_string()))?;
            user.image = Some(image);
            names.push(user.username.clone());
        }
        snapshot.schema_version = SCHEMA_VERSION;
        validate_snapshot(&snapshot)?;
        if apply {
            // Preserve an exact schema2 backup; refuse to overwrite it.
            let old = fs::read(dir.join("state.json")).map_err(io_err)?;
            let mut backup = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(dir.join("state.schema2.backup.json"))
                .map_err(io_err)?;
            backup.write_all(&old).map_err(io_err)?;
            backup.sync_all().map_err(io_err)?;
            File::open(dir)
                .and_then(|file| file.sync_all())
                .map_err(io_err)?;
            commit_snapshot(dir, &snapshot).map_err(CommitError::into_state)?;
        }
        Ok(names)
    }
    /// Create `state.json` only when the directory has no snapshot.
    pub fn init(dir: impl AsRef<Path>) -> Result<(), StateError> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir).map_err(io_err)?;
        let empty = fs::read_dir(dir)
            .map_err(io_err)?
            .next()
            .transpose()
            .map_err(io_err)?
            .is_none();
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(dir.join("state.lock"))
            .map_err(io_err)?;
        lock_exclusive(&lock_file)?;
        let path = dir.join("state.json");
        if path.exists() {
            return Err(StateError::AlreadyExists("state.json".into()));
        }
        if !empty {
            return Err(StateError::Corrupt(
                "state directory is not empty; restore the missing snapshot".into(),
            ));
        }
        commit_snapshot(
            dir,
            &Snapshot {
                schema_version: SCHEMA_VERSION,
                users: Vec::new(),
            },
        )
        .map_err(CommitError::into_state)
    }

    /// Open an initialized directory and hold its exclusive lock.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, StateError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(io_err)?;
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(dir.join("state.lock"))
            .map_err(io_err)?;
        lock_exclusive(&lock_file)?;
        let snapshot = load_snapshot(&dir)?;
        validate_snapshot(&snapshot)?;
        Ok(Self {
            dir,
            lock: Mutex::new(snapshot),
            writes_paused: Mutex::new(None),
            _lock_file: Some(lock_file),
            single_mcp_digest: None,
            changes: tokio::sync::watch::channel(()).0,
        })
    }

    /// Adapt one configured access identity to the shared request contracts.
    /// No directory, lock, account registry or plaintext credential is stored.
    pub fn standalone(
        token: &str,
        dav_token: Option<&str>,
        image: crate::image::SpaceImage,
    ) -> Result<Self, StateError> {
        validate_dav_token(token)?;
        if let Some(dav) = dav_token {
            validate_dav_token(dav)?;
            if constant_eq(token, dav) {
                return Err(StateError::AlreadyExists(
                    "credential already in use".into(),
                ));
            }
        }
        image
            .validate()
            .map_err(|e| StateError::Corrupt(e.to_string()))?;
        if !image.browser() {
            return Err(StateError::Corrupt(
                "Standalone requires its desktop runtime".into(),
            ));
        }
        let credential = |value: &str| Credential {
            key_id: digest_secret(value),
            secret_digest: digest_secret(value),
        };
        let snapshot = Snapshot {
            schema_version: SCHEMA_VERSION,
            users: vec![UserRecord {
                image: Some(image),
                username: "workspace".into(),
                agent_space_id: "spc_00000000-0000-4000-8000-000000000001".into(),
                status: UserStatus::Active,
                credential: Some(credential(token)),
                dav_credential: dav_token.map(credential),
                sandbox_name: crate::engine::process::WORKSPACE.into(),
                owned_volume_names: vec![],
                cleanup: None,
            }],
        };
        validate_snapshot(&snapshot)?;
        Ok(Self {
            dir: PathBuf::new(),
            lock: Mutex::new(snapshot),
            writes_paused: Mutex::new(None),
            _lock_file: None,
            single_mcp_digest: Some(digest_secret(token)),
            changes: tokio::sync::watch::channel(()).0,
        })
    }

    pub fn is_standalone(&self) -> bool {
        self.single_mcp_digest.is_some()
    }

    /// Receivers recheck current authority after durable mutations. This is a
    /// wakeup signal, not a second event log or a copy of credential state.
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes.subscribe()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn list(&self) -> Vec<UserRecord> {
        self.lock.lock().expect("state mutex").users.clone()
    }

    pub fn get(&self, username: &str) -> Result<UserRecord, StateError> {
        self.lock
            .lock()
            .expect("state mutex")
            .users
            .iter()
            .find(|user| user.username == username)
            .cloned()
            .ok_or_else(|| StateError::NotFound(username.to_string()))
    }

    pub fn get_by_space(&self, agent_space_id: &str) -> Option<UserRecord> {
        self.lock
            .lock()
            .expect("state mutex")
            .users
            .iter()
            .find(|user| user.agent_space_id == agent_space_id)
            .cloned()
    }

    /// Reject new admissions when an atomic replacement had uncertain durability.
    pub fn ensure_admission_open(&self) -> Result<(), StateError> {
        if let Some(reason) = self.writes_paused.lock().expect("pause mutex").as_ref() {
            return Err(StateError::WritesPaused(reason.clone()));
        }
        Ok(())
    }

    pub fn check_readable(&self) -> Result<(), StateError> {
        let current = self.lock.lock().expect("state mutex");
        self.ensure_admission_open()?;
        if self.is_standalone() {
            return Ok(());
        }
        let disk = load_snapshot(&self.dir)?;
        validate_snapshot(&disk)?;
        if *current != disk {
            return Err(StateError::Corrupt(
                "state file differs from the active snapshot".into(),
            ));
        }
        Ok(())
    }

    pub fn authenticate(&self, token: &str) -> Result<UserRecord, StateError> {
        self.authenticate_kind(token, CredentialKind::Mcp)
    }
    pub fn authenticate_dav(&self, token: &str) -> Result<UserRecord, StateError> {
        let snapshot = self.lock.lock().expect("state mutex");
        self.ensure_admission_open()?;
        let digest = digest_secret(token);
        let user = snapshot
            .users
            .iter()
            .find(|user| {
                user.dav_credential
                    .as_ref()
                    .is_some_and(|credential| constant_eq(&credential.secret_digest, &digest))
            })
            .ok_or(StateError::InvalidCredential)?;
        match user.status {
            UserStatus::Active => Ok(user.clone()),
            UserStatus::Disabled => Err(StateError::Disabled(user.username.clone())),
            UserStatus::Deleting => Err(StateError::Deleting(user.username.clone())),
        }
    }
    fn authenticate_kind(
        &self,
        token: &str,
        kind: CredentialKind,
    ) -> Result<UserRecord, StateError> {
        if let Some(digest) = &self.single_mcp_digest {
            self.ensure_admission_open()?;
            if !constant_eq(&digest_secret(token), digest) {
                return Err(StateError::InvalidCredential);
            }
            return Ok(self.lock.lock().expect("state mutex").users[0].clone());
        }
        let (key_id, secret) = split_token(token).ok_or(StateError::InvalidCredential)?;
        let snapshot = self.lock.lock().expect("state mutex");
        self.ensure_admission_open()?;
        let user = snapshot
            .users
            .iter()
            .find(|user| {
                kind.get(user)
                    .is_some_and(|credential| credential.key_id == key_id)
            })
            .cloned()
            .ok_or(StateError::InvalidCredential)?;
        let digest = kind
            .get(&user)
            .map(|credential| credential.secret_digest.as_str())
            .unwrap_or("");
        if !constant_eq(digest, &digest_secret(secret)) {
            return Err(StateError::InvalidCredential);
        }
        match user.status {
            UserStatus::Active => Ok(user),
            UserStatus::Disabled => Err(StateError::Disabled(user.username)),
            UserStatus::Deleting => Err(StateError::Deleting(user.username)),
        }
    }

    /// The presented token must still be the credential bound to this session.
    pub fn authorize_bound(
        &self,
        token: &str,
        username: &str,
        agent_space_id: &str,
        key_id: &str,
    ) -> Result<UserRecord, StateError> {
        let user = self.authenticate(token)?;
        if user.username != username
            || user.agent_space_id != agent_space_id
            || user
                .credential
                .as_ref()
                .is_none_or(|credential| credential.key_id != key_id)
        {
            return Err(StateError::InvalidCredential);
        }
        Ok(user)
    }

    /// Recheck a queued session immediately before execution. No token is available here.
    pub fn authorize_bound_current(
        &self,
        username: &str,
        agent_space_id: &str,
        key_id: &str,
    ) -> Result<UserRecord, StateError> {
        self.authorize_current(username, agent_space_id, key_id, CredentialKind::Mcp)
    }
    pub fn authorize_dav_current(
        &self,
        username: &str,
        agent_space_id: &str,
        key_id: &str,
    ) -> Result<UserRecord, StateError> {
        self.authorize_current(username, agent_space_id, key_id, CredentialKind::Dav)
    }
    fn authorize_current(
        &self,
        username: &str,
        agent_space_id: &str,
        key_id: &str,
        kind: CredentialKind,
    ) -> Result<UserRecord, StateError> {
        let snapshot = self.lock.lock().expect("state mutex");
        self.ensure_admission_open()?;
        let user = snapshot
            .users
            .iter()
            .find(|user| user.username == username)
            .cloned()
            .ok_or_else(|| StateError::NotFound(username.to_string()))?;
        if user.agent_space_id != agent_space_id
            || kind
                .get(&user)
                .is_none_or(|credential| credential.key_id != key_id)
        {
            return Err(StateError::InvalidCredential);
        }
        match user.status {
            UserStatus::Active => Ok(user),
            UserStatus::Disabled => Err(StateError::Disabled(user.username)),
            UserStatus::Deleting => Err(StateError::Deleting(user.username)),
        }
    }

    pub fn create_user(
        &self,
        username: &str,
        resource_prefix: &str,
        volume_suffixes: &[&str],
    ) -> Result<(UserRecord, IssuedToken), StateError> {
        self.create_user_with_image(
            username,
            resource_prefix,
            volume_suffixes,
            crate::image::SpaceImage::default(),
        )
    }

    pub fn create_user_with_image(
        &self,
        username: &str,
        resource_prefix: &str,
        volume_suffixes: &[&str],
        image: crate::image::SpaceImage,
    ) -> Result<(UserRecord, IssuedToken), StateError> {
        image
            .validate()
            .map_err(|error| StateError::Corrupt(error.to_string()))?;
        validate_username(username)?;
        let agent_space_id = format!("spc_{}", Uuid::new_v4());
        let sandbox_name = format!("{resource_prefix}-{agent_space_id}");
        validate_sandbox_name(&sandbox_name)?;
        let owned_volume_names = volume_suffixes
            .iter()
            .map(|suffix| format!("{sandbox_name}-{suffix}"))
            .collect::<Vec<_>>();
        let issued = issue_token();
        let record = UserRecord {
            image: Some(image),
            username: username.to_string(),
            agent_space_id,
            status: UserStatus::Active,
            credential: Some(Credential {
                key_id: issued.key_id.clone(),
                secret_digest: digest_secret(issued.token.split_once('.').unwrap().1),
            }),
            dav_credential: None,
            sandbox_name,
            owned_volume_names,
            cleanup: None,
        };
        self.mutate(|snapshot| {
            ensure_insertable(snapshot, &record)?;
            snapshot.users.push(record.clone());
            Ok(())
        })?;
        Ok((record, issued))
    }

    pub fn import_users(&self, users: &[ImportUser]) -> Result<Vec<UserRecord>, StateError> {
        self.mutate(|snapshot| {
            let created = plan_import(snapshot, users)?;
            for record in &created {
                snapshot.users.push(record.clone());
            }
            Ok(created)
        })
    }

    pub fn preview_import_users(&self, users: &[ImportUser]) -> Result<(), StateError> {
        let snapshot = self.lock.lock().expect("state mutex");
        self.ensure_admission_open()?;
        plan_import(&snapshot, users).map(|_| ())
    }

    pub fn set_enabled(
        &self,
        username: &str,
        agent_space_id: &str,
        enabled: bool,
    ) -> Result<UserRecord, StateError> {
        self.mutate(|snapshot| {
            let user = user_mut(snapshot, username)?;
            if user.agent_space_id != agent_space_id {
                return Err(StateError::SpaceMismatch);
            }
            if user.status == UserStatus::Deleting {
                return Err(StateError::Deleting(username.to_string()));
            }
            if enabled && user.status == UserStatus::Disabled && user.cleanup.is_some() {
                return Err(StateError::StopPending(username.to_string()));
            }
            user.status = if enabled {
                UserStatus::Active
            } else {
                UserStatus::Disabled
            };
            if !enabled {
                user.credential = None;
                user.dav_credential = None;
                user.cleanup = Some(Cleanup {
                    requested_at: now_stamp(),
                    last_error: None,
                });
            } else {
                user.cleanup = None;
            }
            Ok(user.clone())
        })
    }

    pub fn clear_stop_pending(
        &self,
        username: &str,
        agent_space_id: &str,
    ) -> Result<(), StateError> {
        self.mutate(|snapshot| {
            let user = user_mut(snapshot, username)?;
            if user.agent_space_id != agent_space_id {
                return Err(StateError::SpaceMismatch);
            }
            if user.status == UserStatus::Disabled {
                user.cleanup = None;
            }
            Ok(())
        })
    }

    pub fn note_cleanup_error(
        &self,
        username: &str,
        agent_space_id: &str,
        error: &str,
    ) -> Result<(), StateError> {
        self.mutate(|snapshot| {
            let user = user_mut(snapshot, username)?;
            if user.agent_space_id != agent_space_id {
                return Err(StateError::SpaceMismatch);
            }
            if let Some(cleanup) = user.cleanup.as_mut() {
                cleanup.last_error = Some(error.to_string());
            }
            Ok(())
        })
    }

    pub fn rotate_token(
        &self,
        username: &str,
        agent_space_id: &str,
    ) -> Result<IssuedToken, StateError> {
        self.rotate_credential(username, agent_space_id, CredentialKind::Mcp)
    }
    pub fn rotate_dav_token(
        &self,
        username: &str,
        agent_space_id: &str,
    ) -> Result<IssuedToken, StateError> {
        self.set_dav_token(username, agent_space_id, &hex::encode(random_bytes(32)))
    }
    /// Set an opaque integration token; retrying the current value is a true no-op.
    pub fn set_dav_token(
        &self,
        username: &str,
        agent_space_id: &str,
        token: &str,
    ) -> Result<IssuedToken, StateError> {
        validate_dav_token(token)?;
        let digest = digest_secret(token);
        self.mutate(|snapshot| {
            let user = user_mut(snapshot, username)?;
            if user.agent_space_id != agent_space_id {
                return Err(StateError::SpaceMismatch);
            }
            match user.status {
                UserStatus::Active => {}
                UserStatus::Disabled => return Err(StateError::Disabled(username.into())),
                UserStatus::Deleting => return Err(StateError::Deleting(username.into())),
            }
            if let Some(current) = &user.dav_credential {
                if constant_eq(&current.secret_digest, &digest) {
                    return Ok(IssuedToken {
                        key_id: current.key_id.clone(),
                        token: token.into(),
                    });
                }
            }
            // Bearer authentication identifies an account without a username.
            // Also forbid reusing an MCP credential as a DAV credential.
            if snapshot.users.iter().any(|user| {
                user.dav_credential
                    .as_ref()
                    .is_some_and(|c| constant_eq(&c.secret_digest, &digest))
                    || user
                        .credential
                        .as_ref()
                        .is_some_and(|c| mcp_token_matches(c, token))
            }) {
                return Err(StateError::AlreadyExists(
                    "credential already in use".into(),
                ));
            }
            let key_id = format!("dav_{}", Uuid::new_v4().simple());
            user_mut(snapshot, username)?.dav_credential = Some(Credential {
                key_id: key_id.clone(),
                secret_digest: digest,
            });
            Ok(IssuedToken {
                key_id,
                token: token.into(),
            })
        })
    }

    fn rotate_credential(
        &self,
        username: &str,
        agent_space_id: &str,
        kind: CredentialKind,
    ) -> Result<IssuedToken, StateError> {
        let issued = issue_token();
        let digest = digest_secret(issued.token.split_once('.').unwrap().1);
        self.mutate(|snapshot| {
            let user = user_mut(snapshot, username)?;
            if user.agent_space_id != agent_space_id {
                return Err(StateError::SpaceMismatch);
            }
            if user.status != UserStatus::Active {
                return Err(match user.status {
                    UserStatus::Disabled => StateError::Disabled(username.to_string()),
                    _ => StateError::Deleting(username.to_string()),
                });
            }
            *kind.slot(user) = Some(Credential {
                key_id: issued.key_id.clone(),
                secret_digest: digest,
            });
            Ok(())
        })?;
        Ok(issued)
    }

    pub fn revoke_dav_token(&self, username: &str, agent_space_id: &str) -> Result<(), StateError> {
        self.mutate(|snapshot| {
            let user = user_mut(snapshot, username)?;
            if user.agent_space_id != agent_space_id {
                return Err(StateError::SpaceMismatch);
            }
            user.dav_credential = None;
            Ok(())
        })
    }

    /// Persist deletion intent and the exact resource inventory first.
    pub fn begin_delete(
        &self,
        username: &str,
        agent_space_id: &str,
    ) -> Result<UserRecord, StateError> {
        self.mutate(|snapshot| {
            let user = user_mut(snapshot, username)?;
            if user.agent_space_id != agent_space_id {
                return Err(StateError::SpaceMismatch);
            }
            user.status = UserStatus::Deleting;
            user.credential = None;
            user.dav_credential = None;
            if user.cleanup.is_none() {
                user.cleanup = Some(Cleanup {
                    requested_at: now_stamp(),
                    last_error: None,
                });
            }
            Ok(user.clone())
        })
    }

    pub fn finish_delete(&self, username: &str, agent_space_id: &str) -> Result<(), StateError> {
        self.mutate(|snapshot| {
            let position = snapshot
                .users
                .iter()
                .position(|user| user.username == username)
                .ok_or_else(|| StateError::NotFound(username.to_string()))?;
            if snapshot.users[position].agent_space_id != agent_space_id {
                return Err(StateError::SpaceMismatch);
            }
            if snapshot.users[position].status != UserStatus::Deleting {
                return Err(StateError::Io(
                    "refusing to remove a user that is not deleting".into(),
                ));
            }
            snapshot.users.remove(position);
            Ok(())
        })
    }

    pub fn deleting_users(&self) -> Vec<UserRecord> {
        self.list()
            .into_iter()
            .filter(|user| user.status == UserStatus::Deleting)
            .collect()
    }

    pub fn pending_stops(&self) -> Vec<UserRecord> {
        self.list()
            .into_iter()
            .filter(|user| user.status == UserStatus::Disabled && user.cleanup.is_some())
            .collect()
    }

    pub fn registered_resources(&self) -> (HashSet<String>, HashSet<String>) {
        let mut sandboxes = HashSet::new();
        let mut volumes = HashSet::new();
        for user in self.list() {
            sandboxes.insert(user.sandbox_name);
            volumes.extend(user.owned_volume_names);
        }
        (sandboxes, volumes)
    }

    fn mutate<T>(
        &self,
        change: impl FnOnce(&mut Snapshot) -> Result<T, StateError>,
    ) -> Result<T, StateError> {
        if self.is_standalone() {
            return Err(StateError::WritesPaused(
                "Standalone identity is read-only".into(),
            ));
        }
        let mut snapshot = self.lock.lock().expect("state mutex");
        if let Some(reason) = self.writes_paused.lock().expect("pause mutex").clone() {
            return Err(StateError::WritesPaused(reason));
        }
        let mut next = snapshot.clone();
        let value = change(&mut next)?;
        validate_snapshot(&next)?;
        if next == *snapshot {
            return Ok(value);
        }
        if let Err(error) = commit_snapshot(&self.dir, &next) {
            if matches!(error, CommitError::AfterReplace(_)) {
                *self.writes_paused.lock().expect("pause mutex") =
                    Some("snapshot durability is uncertain; reload state before serving".into());
                self.changes.send_replace(());
            }
            return Err(error.into_state());
        }
        *snapshot = next;
        self.changes.send_replace(());
        Ok(value)
    }
}

fn plan_import(snapshot: &Snapshot, users: &[ImportUser]) -> Result<Vec<UserRecord>, StateError> {
    if !snapshot.users.is_empty() {
        return Err(StateError::AlreadyExists(
            "state already contains users".into(),
        ));
    }
    let mut planned = snapshot.clone();
    let mut created = Vec::with_capacity(users.len());
    for user in users {
        validate_username(&user.username)?;
        validate_sandbox_name(&user.sandbox_name)?;
        for volume in &user.owned_volume_names {
            validate_resource_name(volume)?;
        }
        let record = UserRecord {
            image: Some(user.image.clone()),
            username: user.username.clone(),
            agent_space_id: format!("spc_{}", Uuid::new_v4()),
            status: UserStatus::Active,
            credential: None,
            dav_credential: None,
            sandbox_name: user.sandbox_name.clone(),
            owned_volume_names: user.owned_volume_names.clone(),
            cleanup: None,
        };
        ensure_insertable(&planned, &record)?;
        planned.users.push(record.clone());
        created.push(record);
    }
    validate_snapshot(&planned)?;
    Ok(created)
}

fn ensure_insertable(snapshot: &Snapshot, record: &UserRecord) -> Result<(), StateError> {
    if snapshot
        .users
        .iter()
        .any(|user| user.username == record.username)
    {
        return Err(StateError::AlreadyExists(record.username.clone()));
    }
    if snapshot.users.iter().any(|user| {
        user.agent_space_id == record.agent_space_id || user.sandbox_name == record.sandbox_name
    }) {
        return Err(StateError::AlreadyExists(record.sandbox_name.clone()));
    }
    let owned: HashSet<&str> = snapshot
        .users
        .iter()
        .flat_map(|user| user.owned_volume_names.iter().map(String::as_str))
        .collect();
    for volume in &record.owned_volume_names {
        if owned.contains(volume.as_str()) {
            return Err(StateError::AlreadyExists(volume.clone()));
        }
    }
    Ok(())
}

fn user_mut<'a>(
    snapshot: &'a mut Snapshot,
    username: &str,
) -> Result<&'a mut UserRecord, StateError> {
    snapshot
        .users
        .iter_mut()
        .find(|user| user.username == username)
        .ok_or_else(|| StateError::NotFound(username.to_string()))
}

pub fn validate_username(username: &str) -> Result<(), StateError> {
    let mut chars = username.chars();
    let Some(first) = chars.next() else {
        return Err(StateError::InvalidUsername);
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) || username.chars().count() > 64 {
        return Err(StateError::InvalidUsername);
    }
    if chars.any(|ch| {
        !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '.' | '-'))
    }) {
        return Err(StateError::InvalidUsername);
    }
    Ok(())
}

fn validate_sandbox_name(name: &str) -> Result<(), StateError> {
    #[cfg(feature = "msb")]
    {
        microsandbox::validate_sandbox_name(name).map_err(|error| StateError::Io(error.to_string()))
    }
    #[cfg(not(feature = "msb"))]
    {
        // Standalone never creates SDK resources. Keep its shared state types
        // independent of the optional VM backend.
        validate_resource_name(name)
    }
}

fn validate_resource_name(name: &str) -> Result<(), StateError> {
    if name.is_empty()
        || name.len() > 200
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(StateError::Io(format!("invalid resource name '{name}'")));
    }
    Ok(())
}

fn issue_token() -> IssuedToken {
    let key_id = format!("key_{}", Uuid::new_v4().simple());
    let secret = hex::encode(random_bytes(32));
    IssuedToken {
        token: format!("{key_id}.{secret}"),
        key_id,
    }
}

fn split_token(token: &str) -> Option<(&str, &str)> {
    let (key_id, secret) = token.split_once('.')?;
    if key_id.is_empty() || secret.len() < 64 || secret.chars().any(|ch| !ch.is_ascii_hexdigit()) {
        return None;
    }
    Some((key_id, secret))
}

fn mcp_token_matches(credential: &Credential, token: &str) -> bool {
    split_token(token).is_some_and(|(key, secret)| {
        credential.key_id == key && constant_eq(&credential.secret_digest, &digest_secret(secret))
    })
}
fn validate_dav_token(token: &str) -> Result<(), StateError> {
    if !(32..=256).contains(&token.len())
        || token.trim_end_matches('=').is_empty()
        || !token.trim_end_matches('=').bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
        })
    {
        return Err(StateError::InvalidDavToken);
    }
    Ok(())
}

fn digest_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn constant_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).expect("os random");
    bytes
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn load_snapshot(dir: &Path) -> Result<Snapshot, StateError> {
    let mut content = String::new();
    let mut file = File::open(dir.join("state.json")).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            StateError::NotInitialized
        } else {
            io_err(error)
        }
    })?;
    file.read_to_string(&mut content).map_err(io_err)?;
    // Typed decoding rejects duplicate fields and ambiguous resource ownership.
    serde_json::from_str(&content).map_err(|error| StateError::Corrupt(error.to_string()))
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<(), StateError> {
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err(StateError::Corrupt("unsupported schema".into()));
    }
    let mut names = HashSet::new();
    let mut spaces = HashSet::new();
    let mut resources = HashSet::new();
    let mut key_ids = HashSet::new();
    let mut dav_digests = HashSet::new();
    for user in &snapshot.users {
        user.image
            .as_ref()
            .ok_or_else(|| StateError::Corrupt(format!("missing image for {}", user.username)))?
            .validate()
            .map_err(|error| StateError::Corrupt(error.to_string()))?;
        validate_username(&user.username)?;
        if user
            .agent_space_id
            .strip_prefix("spc_")
            .and_then(|value| Uuid::parse_str(value).ok())
            .is_none_or(|uuid| uuid.get_version() != Some(uuid::Version::Random))
        {
            return Err(StateError::Corrupt(format!(
                "invalid space id for {}",
                user.username
            )));
        }
        validate_sandbox_name(&user.sandbox_name)
            .map_err(|error| StateError::Corrupt(error.to_string()))?;
        if (user.status == UserStatus::Active && user.cleanup.is_some())
            || (user.status == UserStatus::Deleting && user.cleanup.is_none())
            || user
                .cleanup
                .as_ref()
                .is_some_and(|c| c.requested_at.is_empty())
        {
            return Err(StateError::Corrupt(format!(
                "invalid cleanup intent for {}",
                user.username
            )));
        }
        if user.status != UserStatus::Active
            && (user.credential.is_some() || user.dav_credential.is_some())
        {
            return Err(StateError::Corrupt(format!(
                "inactive user {} retains a credential",
                user.username
            )));
        }
        if user
            .dav_credential
            .as_ref()
            .is_some_and(|c| !dav_digests.insert(c.secret_digest.clone()))
        {
            return Err(StateError::Corrupt(
                "ambiguous DAV credential ownership".into(),
            ));
        }
        for credential in [&user.credential, &user.dav_credential]
            .into_iter()
            .flatten()
        {
            if credential.key_id.is_empty()
                || credential.secret_digest.len() != 64
                || !credential
                    .secret_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || !key_ids.insert(credential.key_id.clone())
            {
                return Err(StateError::Corrupt(format!(
                    "invalid credential for {}",
                    user.username
                )));
            }
        }
        if !names.insert(user.username.clone())
            || !spaces.insert(user.agent_space_id.clone())
            || !resources.insert(user.sandbox_name.clone())
        {
            return Err(StateError::Corrupt("overlapping user ownership".into()));
        }
        for volume in &user.owned_volume_names {
            validate_resource_name(volume)
                .map_err(|error| StateError::Corrupt(error.to_string()))?;
            if !resources.insert(volume.clone()) {
                return Err(StateError::Corrupt(format!("overlapping volume {volume}")));
            }
        }
    }
    Ok(())
}

enum CommitError {
    BeforeReplace(StateError),
    AfterReplace(StateError),
}

impl CommitError {
    fn into_state(self) -> StateError {
        match self {
            Self::BeforeReplace(error) | Self::AfterReplace(error) => error,
        }
    }
}

fn commit_snapshot(dir: &Path, snapshot: &Snapshot) -> Result<(), CommitError> {
    let encoded = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| CommitError::BeforeReplace(StateError::Io(error.to_string())))?;
    let temporary = dir.join(format!("state.json.{}.tmp", std::process::id()));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|error| CommitError::BeforeReplace(io_err(error)))?;
        file.write_all(&encoded)
            .map_err(|error| CommitError::BeforeReplace(io_err(error)))?;
        file.sync_all()
            .map_err(|error| CommitError::BeforeReplace(io_err(error)))?;
    }
    fs::rename(&temporary, dir.join("state.json"))
        .map_err(|error| CommitError::BeforeReplace(io_err(error)))?;
    let directory = File::open(dir).map_err(|error| CommitError::AfterReplace(io_err(error)))?;
    directory
        .sync_all()
        .map_err(|error| CommitError::AfterReplace(io_err(error)))?;
    Ok(())
}

fn lock_exclusive(file: &File) -> Result<(), StateError> {
    use std::os::unix::io::AsRawFd;
    let flock = libc::flock {
        l_type: libc::F_WRLCK as _,
        l_whence: libc::SEEK_SET as _,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    // Open-file-description locks conflict across handles, including a second
    // handle in this process. Classic fcntl locks would not.
    const F_OFD_SETLK: libc::c_int = 37;
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), F_OFD_SETLK, &flock) };
    if rc == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock
        || matches!(error.raw_os_error(), Some(11 | 35))
    {
        Err(StateError::Locked)
    } else {
        Err(StateError::Io(error.to_string()))
    }
}

fn io_err(error: std::io::Error) -> StateError {
    StateError::Io(error.to_string())
}

pub fn unregistered<'a>(
    found: impl IntoIterator<Item = &'a str>,
    registered: &HashSet<String>,
) -> Vec<String> {
    let mut unknown: Vec<String> = found
        .into_iter()
        .filter(|name| !registered.contains(*name))
        .map(str::to_string)
        .collect();
    unknown.sort();
    unknown
}

#[cfg(test)]
mod standalone_tests {
    use super::*;

    fn image() -> crate::image::SpaceImage {
        crate::image::ImageSelection::Preset(crate::image::PresetSelection {
            preset: crate::image::DESKTOP_PRESET.into(),
        })
        .resolve()
        .unwrap()
    }

    #[test]
    fn one_configured_identity_uses_no_account_file_and_keeps_credentials_separate() {
        let token = "standalone-access-credential-00000000000000000000";
        let dav = "standalone-dav-credential-00000000000000000000000";
        let store = StateStore::standalone(token, Some(dav), image()).unwrap();
        assert!(store.is_standalone());
        assert!(store.dir().as_os_str().is_empty());
        store.check_readable().unwrap();
        assert_eq!(store.list().len(), 1);
        let user = store.authenticate(token).unwrap();
        assert_eq!(user.sandbox_name, crate::engine::process::WORKSPACE);
        assert!(user.image.as_ref().unwrap().browser());
        assert!(store.authenticate(dav).is_err());
        assert!(store.authenticate_dav(token).is_err());
        assert_eq!(
            store.authenticate_dav(dav).unwrap().agent_space_id,
            user.agent_space_id
        );
        assert!(store
            .authorize_bound_current(
                "another",
                &user.agent_space_id,
                &user.credential.as_ref().unwrap().key_id
            )
            .is_err());
        assert!(store
            .rotate_token(&user.username, &user.agent_space_id)
            .is_err());
        assert!(store
            .begin_delete(&user.username, &user.agent_space_id)
            .is_err());
        assert!(StateStore::standalone(token, Some(token), image()).is_err());
        let replacement = StateStore::standalone(dav, None, image()).unwrap();
        assert!(replacement.authenticate(token).is_err());
        assert!(replacement
            .authorize_bound_current(
                &user.username,
                &user.agent_space_id,
                &user.credential.unwrap().key_id
            )
            .is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "agent-space-state-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn impossible_cleanup_states_fail_closed_without_rewriting_snapshot() {
        let path = dir();
        StateStore::init(&path).unwrap();
        let store = StateStore::open(&path).unwrap();
        store.create_user("alice", "smcp", &["ws"]).unwrap();
        drop(store);
        let original = fs::read(path.join("state.json")).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["users"][0]["cleanup"] = serde_json::json!({"requestedAt":"now"});
        let corrupt = serde_json::to_vec(&value).unwrap();
        fs::write(path.join("state.json"), &corrupt).unwrap();
        assert!(matches!(
            StateStore::open(&path),
            Err(StateError::Corrupt(_))
        ));
        assert_eq!(fs::read(path.join("state.json")).unwrap(), corrupt);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn dav_keys_are_independent_and_all_invalidation_paths_are_persistent() {
        let path = dir();
        StateStore::init(&path).unwrap();
        let store = StateStore::open(&path).unwrap();
        let (user, mcp) = store.create_user("alice", "smcp", &["ws"]).unwrap();
        assert!(user.dav_credential.is_none());
        let mut changes = store.subscribe_changes();
        let first = store
            .rotate_dav_token("alice", &user.agent_space_id)
            .unwrap();
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        assert!(store.authenticate_dav(&first.token).is_ok());
        assert!(store.authenticate(&first.token).is_err());
        assert!(store.authenticate_dav(&mcp.token).is_err());
        assert!(store.authenticate(&mcp.token).is_ok());
        assert!(store
            .authorize_dav_current("alice", "spc_other", &first.key_id)
            .is_err());
        let second = store
            .rotate_dav_token("alice", &user.agent_space_id)
            .unwrap();
        assert!(store.authenticate_dav(&first.token).is_err());
        assert!(store
            .authorize_dav_current("alice", &user.agent_space_id, &first.key_id)
            .is_err());
        assert!(store.authenticate(&mcp.token).is_ok());
        let mcp2 = store.rotate_token("alice", &user.agent_space_id).unwrap();
        assert!(store.authenticate_dav(&second.token).is_ok());
        assert!(store.authenticate(&mcp2.token).is_ok());
        let raw = fs::read_to_string(path.join("state.json")).unwrap();
        assert!(!raw.contains(&second.token));
        drop(store);
        let store = StateStore::open(&path).unwrap();
        assert!(store.authenticate_dav(&second.token).is_ok());
        assert!(matches!(
            store.revoke_dav_token("alice", "spc_other"),
            Err(StateError::SpaceMismatch)
        ));
        store
            .revoke_dav_token("alice", &user.agent_space_id)
            .unwrap();
        store
            .revoke_dav_token("alice", &user.agent_space_id)
            .unwrap();
        assert!(store.authenticate_dav(&second.token).is_err());
        assert!(store.authenticate(&mcp2.token).is_ok());
        let third = store
            .rotate_dav_token("alice", &user.agent_space_id)
            .unwrap();
        store
            .set_enabled("alice", &user.agent_space_id, false)
            .unwrap();
        assert!(store.authenticate_dav(&third.token).is_err());
        assert!(store.authenticate(&mcp2.token).is_err());
        assert!(store
            .rotate_dav_token("alice", &user.agent_space_id)
            .is_err());
        store
            .clear_stop_pending("alice", &user.agent_space_id)
            .unwrap();
        store
            .set_enabled("alice", &user.agent_space_id, true)
            .unwrap();
        assert!(store.get("alice").unwrap().dav_credential.is_none());
        let fourth = store
            .rotate_dav_token("alice", &user.agent_space_id)
            .unwrap();
        store.begin_delete("alice", &user.agent_space_id).unwrap();
        assert!(store.authenticate_dav(&fourth.token).is_err());
        store.finish_delete("alice", &user.agent_space_id).unwrap();
        let (replacement, _) = store.create_user("alice", "smcp", &["ws"]).unwrap();
        assert_ne!(replacement.agent_space_id, user.agent_space_id);
        assert!(store
            .authorize_dav_current("alice", &user.agent_space_id, &fourth.key_id)
            .is_err());
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn custom_dav_tokens_are_persistent_unique_and_retry_safe() {
        let path = dir();
        StateStore::init(&path).unwrap();
        let store = StateStore::open(&path).unwrap();
        let (alice, mcp) = store.create_user("alice", "smcp", &["ws"]).unwrap();
        let (bob, _) = store.create_user("bob", "smcp", &["ws"]).unwrap();
        let custom = "0123456789abcdef0123456789abcdef-._~+/=";
        let first = store
            .set_dav_token("alice", &alice.agent_space_id, custom)
            .unwrap();
        assert_eq!(first.token, custom);
        assert!(first.key_id.starts_with("dav_"));
        assert!(store.authenticate_dav(custom).is_ok());
        assert!(store.authenticate(custom).is_err());
        let before = fs::read(path.join("state.json")).unwrap();
        assert!(!String::from_utf8_lossy(&before).contains(custom));
        let watch = store.subscribe_changes();
        let retry = store
            .set_dav_token("alice", &alice.agent_space_id, custom)
            .unwrap();
        assert_eq!(retry, first);
        assert!(!watch.has_changed().unwrap());
        assert_eq!(fs::read(path.join("state.json")).unwrap(), before);
        assert!(matches!(
            store.set_dav_token("bob", &bob.agent_space_id, custom),
            Err(StateError::AlreadyExists(_))
        ));
        assert!(matches!(
            store.set_dav_token("alice", &alice.agent_space_id, &mcp.token),
            Err(StateError::AlreadyExists(_))
        ));
        for bad in [
            "".to_string(),
            "a".repeat(31),
            "a".repeat(257),
            "=".repeat(32),
            format!("{}=middle", "a".repeat(32)),
            " ".repeat(32),
            ":".repeat(32),
            "\n".repeat(32),
            "中".repeat(32),
        ] {
            assert!(matches!(
                store.set_dav_token("alice", &alice.agent_space_id, &bad),
                Err(StateError::InvalidDavToken)
            ));
        }
        assert!(matches!(
            store.set_dav_token("alice", "spc_stale", custom),
            Err(StateError::SpaceMismatch)
        ));
        assert_eq!(fs::read(path.join("state.json")).unwrap(), before);
        drop(store);
        let store = StateStore::open(&path).unwrap();
        assert_eq!(store.authenticate_dav(custom).unwrap().username, "alice");
        let replacement = "b".repeat(64);
        let second = store
            .set_dav_token("alice", &alice.agent_space_id, &replacement)
            .unwrap();
        assert_ne!(first.key_id, second.key_id);
        assert!(store.authenticate_dav(custom).is_err());
        assert!(store
            .authorize_dav_current("alice", &alice.agent_space_id, &first.key_id)
            .is_err());
        assert!(
            store
                .authenticate_dav(&format!("{}.{}", second.key_id, replacement))
                .is_err(),
            "DAV authenticates the complete token, not a keyId-prefixed secret"
        );
        let generated = store
            .rotate_dav_token("alice", &alice.agent_space_id)
            .unwrap();
        assert!(store.authenticate_dav(&replacement).is_err());
        assert!(store.authenticate_dav(&generated.token).is_ok());
        assert_eq!(
            store
                .set_dav_token("alice", &alice.agent_space_id, &generated.token)
                .unwrap(),
            generated
        );
        assert!(matches!(
            store.set_dav_token("bob", &bob.agent_space_id, &generated.token),
            Err(StateError::AlreadyExists(_))
        ));
        for length in [32, 256] {
            let token = "z".repeat(length);
            store
                .set_dav_token("alice", &alice.agent_space_id, &token)
                .unwrap();
            assert!(store.authenticate_dav(&token).is_ok());
        }
        store
            .revoke_dav_token("alice", &alice.agent_space_id)
            .unwrap();
        assert!(store.authenticate_dav(&"z".repeat(256)).is_err());
        assert!(store.authenticate(&mcp.token).is_ok());
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn missing_state_is_not_an_empty_user_list() {
        let path = dir();
        assert!(matches!(
            StateStore::open(&path),
            Err(StateError::NotInitialized)
        ));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn create_persists_digest_and_rejects_duplicate() {
        let path = dir();
        StateStore::init(&path).unwrap();
        let store = StateStore::open(&path).unwrap();
        let (user, token) = store.create_user("alice", "smcp", &["ws"]).unwrap();
        let uuid = Uuid::parse_str(user.agent_space_id.strip_prefix("spc_").unwrap()).unwrap();
        assert_eq!(uuid.get_version(), Some(uuid::Version::Random));
        assert_eq!(user.agent_space_id, format!("spc_{uuid}"));
        assert_eq!(user.credential.unwrap().secret_digest.len(), 64);
        assert!(store.create_user("alice", "smcp", &["ws"]).is_err());
        drop(store);
        let reopened = StateStore::open(&path).unwrap();
        let authed = reopened.authenticate(&token.token).unwrap();
        assert_eq!(authed.agent_space_id, user.agent_space_id);
        let raw = fs::read_to_string(path.join("state.json")).unwrap();
        assert!(!raw.contains(&token.token));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn corrupt_snapshot_refuses_startup() {
        let path = dir();
        StateStore::init(&path).unwrap();
        for content in [
            "{",
            r#"{"schemaVersion":999,"users":[]}"#,
            r#"{"schemaVersion":2,"schemaVersion":2,"users":[]}"#,
        ] {
            fs::write(path.join("state.json"), content).unwrap();
            assert!(matches!(
                StateStore::open(&path),
                Err(StateError::Corrupt(_))
            ));
            assert_eq!(
                fs::read_to_string(path.join("state.json")).unwrap(),
                content
            );
        }
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn readiness_rejects_a_removed_state_file() {
        let path = dir();
        StateStore::init(&path).unwrap();
        let store = StateStore::open(&path).unwrap();
        fs::remove_file(path.join("state.json")).unwrap();
        assert!(matches!(
            store.check_readable(),
            Err(StateError::NotInitialized)
        ));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn init_does_not_replace_a_missing_snapshot_with_empty_users() {
        let path = dir();
        StateStore::init(&path).unwrap();
        fs::remove_file(path.join("state.json")).unwrap();
        assert!(matches!(
            StateStore::init(&path),
            Err(StateError::Corrupt(_))
        ));
        assert!(!path.join("state.json").exists());
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn rotation_and_space_id_guard_delete() {
        let path = dir();
        StateStore::init(&path).unwrap();
        let store = StateStore::open(&path).unwrap();
        let (user, first) = store.create_user("bob", "smcp", &["ws"]).unwrap();
        let second = store.rotate_token("bob", &user.agent_space_id).unwrap();
        assert!(matches!(
            store.authenticate(&first.token),
            Err(StateError::InvalidCredential)
        ));
        assert!(store.authenticate(&second.token).is_ok());
        assert!(matches!(
            store.begin_delete("bob", "spc_other"),
            Err(StateError::SpaceMismatch)
        ));
        store.begin_delete("bob", &user.agent_space_id).unwrap();
        assert!(matches!(
            store.authenticate(&second.token),
            Err(StateError::InvalidCredential)
        ));
        store.finish_delete("bob", &user.agent_space_id).unwrap();
        assert!(matches!(store.get("bob"), Err(StateError::NotFound(_))));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn second_process_cannot_take_the_lock() {
        let path = dir();
        StateStore::init(&path).unwrap();
        let _store = StateStore::open(&path).unwrap();
        assert!(matches!(StateStore::open(&path), Err(StateError::Locked)));
        assert!(matches!(StateStore::init(&path), Err(StateError::Locked)));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn disable_revokes_token_until_stop_is_cleared() {
        let path = dir();
        StateStore::init(&path).unwrap();
        let store = StateStore::open(&path).unwrap();
        let (user, token) = store.create_user("cara", "smcp", &["ws"]).unwrap();
        store
            .set_enabled("cara", &user.agent_space_id, false)
            .unwrap();
        assert!(matches!(
            store.authenticate(&token.token),
            Err(StateError::InvalidCredential)
        ));
        assert!(store
            .set_enabled("cara", &user.agent_space_id, true)
            .is_err());
        store
            .clear_stop_pending("cara", &user.agent_space_id)
            .unwrap();
        store
            .set_enabled("cara", &user.agent_space_id, true)
            .unwrap();
        assert!(store.rotate_token("cara", &user.agent_space_id).is_ok());
        let _ = fs::remove_dir_all(path);
    }
}
