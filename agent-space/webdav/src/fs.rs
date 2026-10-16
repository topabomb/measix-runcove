//! File operations are relative to pinned directory descriptors. No user path is
//! resolved through /proc; proc is used only to reopen an already checked inode.
use bytes::{Buf, Bytes};
use dav_server::{davpath::DavPath, fs::*};
use std::{
    ffi::CString,
    fs::{File, Metadata},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub const STAGING: &str = ".agent-space/uploads";
const INTERNAL: &[u8] = b".agent-space";
const UPLOADS: &[u8] = b"uploads";

fn fs_error(e: io::Error) -> FsError {
    match e.raw_os_error() {
        Some(libc::ENOENT) => FsError::NotFound,
        Some(libc::ENOSPC | libc::EDQUOT) => FsError::InsufficientStorage,
        Some(libc::EEXIST | libc::ENOTEMPTY) => FsError::Exists,
        Some(
            libc::EACCES | libc::EPERM | libc::ELOOP | libc::EXDEV | libc::ENOTDIR | libc::EISDIR,
        ) => FsError::Forbidden,
        Some(libc::ENAMETOOLONG) => FsError::PathTooLong,
        _ => FsError::GeneralFailure,
    }
}
fn cstr(bytes: &[u8]) -> FsResult<CString> {
    CString::new(bytes).map_err(|_| FsError::Forbidden)
}
fn syscall_result(value: i32) -> FsResult<()> {
    if value < 0 {
        Err(fs_error(io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
fn open_at(parent: &File, path: &[u8], flags: i32, mode: u32) -> FsResult<File> {
    let path = cstr(path)?;
    let how = OpenHow {
        flags: (flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: mode as u64,
        // RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV.
        resolve: 0x08 | 0x04 | 0x01,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(fs_error(io::Error::last_os_error()));
    }
    let file = unsafe { File::from_raw_fd(fd as i32) };
    let meta = file.metadata().map_err(fs_error)?;
    if !meta.is_dir() && !meta.is_file() {
        return Err(FsError::Forbidden);
    }
    Ok(file)
}
fn reopen(file: &File, write: bool) -> FsResult<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(write)
        .open(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(fs_error)
}
fn mkdir(parent: &File, name: &[u8]) -> FsResult<()> {
    let name = cstr(name)?;
    syscall_result(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) })
}
fn unlink(parent: &File, name: &[u8], dir: bool) -> FsResult<()> {
    let name = cstr(name)?;
    syscall_result(unsafe {
        libc::unlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            if dir { libc::AT_REMOVEDIR } else { 0 },
        )
    })
}
fn rename(from: &File, name: &[u8], to: &File, target: &[u8]) -> FsResult<()> {
    let name = cstr(name)?;
    let target = cstr(target)?;
    syscall_result(unsafe {
        libc::renameat(
            from.as_raw_fd(),
            name.as_ptr(),
            to.as_raw_fd(),
            target.as_ptr(),
        )
    })
}
pub(crate) fn validate_move(from: &DavPath, to: &DavPath) -> FsResult<()> {
    // Validate before dav-server's overwrite handling can delete a destination.
    if relative(from)? == INTERNAL || relative(to)? == INTERNAL {
        return Err(FsError::Forbidden);
    }
    Ok(())
}

fn relative(path: &DavPath) -> FsResult<Vec<u8>> {
    let bytes = path.as_bytes();
    let mut parts = Vec::new();
    for part in bytes.split(|b| *b == b'/').filter(|s| !s.is_empty()) {
        if part == b"." || part == b".." || part.contains(&b'\\') || part.contains(&0) {
            return Err(FsError::Forbidden);
        }
        parts.push(part);
    }
    if parts.starts_with(&[INTERNAL, UPLOADS]) {
        return Err(FsError::Forbidden);
    }
    Ok(if parts.is_empty() {
        b".".to_vec()
    } else {
        parts.join(&b'/')
    })
}

// The volatile process record cannot survive a guest reboot. A persistent boot
// marker proves that a staged upload belongs to a boot whose processes ended.
// Same-boot recovery remains the typed launcher's responsibility.
fn recover_previous_boots(staging_root: &File, boot: &str) -> FsResult<()> {
    for entry in std::fs::read_dir(format!("/proc/self/fd/{}", staging_root.as_raw_fd()))
        .map_err(fs_error)?
    {
        let name = entry.map_err(fs_error)?.file_name();
        if name
            .to_str()
            .and_then(|name| uuid::Uuid::parse_str(name).ok())
            .is_none()
        {
            continue;
        }
        let dir = open_at(
            staging_root,
            name.as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        let marker = match open_at(&dir, b"boot-id", libc::O_PATH, 0) {
            Ok(marker) => marker,
            Err(FsError::NotFound) => continue, // No proof: preserve older/unknown contents.
            Err(error) => return Err(error),
        };
        if !marker.metadata().map_err(fs_error)?.is_file() {
            return Err(FsError::Forbidden);
        }
        let mut recorded = String::new();
        use std::io::Read;
        reopen(&marker, false)?
            .take(64)
            .read_to_string(&mut recorded)
            .map_err(fs_error)?;
        if uuid::Uuid::parse_str(recorded.trim_end()).is_err() {
            return Err(FsError::Forbidden);
        }
        if recorded.trim_end() == boot.trim_end() {
            continue;
        }
        // Preflight the entire directory before deleting anything. No recursive
        // deletion and no following symlinks, special files or mounted children.
        let mut files = Vec::new();
        for entry in
            std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd())).map_err(fs_error)?
        {
            let child = entry.map_err(fs_error)?.file_name();
            if child == "boot-id" {
                continue;
            }
            if child
                .to_str()
                .and_then(|name| uuid::Uuid::parse_str(name).ok())
                .is_none()
            {
                return Err(FsError::Forbidden);
            }
            let file = open_at(&dir, child.as_bytes(), libc::O_PATH, 0)?;
            if !file.metadata().map_err(fs_error)?.is_file() {
                return Err(FsError::Forbidden);
            }
            files.push(child);
        }
        for file in files {
            unlink(&dir, file.as_bytes(), false)?;
        }
        unlink(&dir, b"boot-id", false)?;
        unlink(staging_root, name.as_bytes(), true)?;
        staging_root.sync_all().map_err(fs_error)?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct RootFs {
    root: Arc<File>,
    staging: Arc<File>,
    // Present only in the filesystem clone for one PUT/LOCK request.
    upload: Option<Arc<Mutex<Option<Upload>>>>,
    report_inaccessible: bool,
    modified: Option<SystemTime>,
}
impl RootFs {
    pub fn new(root: &Path, owner: &str, instance: &str) -> FsResult<Self> {
        if owner.is_empty()
            || instance.is_empty()
            || !owner
                .bytes()
                .chain(instance.bytes())
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(FsError::Forbidden);
        }
        let root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)
            .map_err(fs_error)?;
        // Fail closed if the kernel cannot enforce the requested resolution rules.
        open_at(&root, b".", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        let internal_name = cstr(INTERNAL)?;
        match syscall_result(unsafe {
            libc::mkdirat(root.as_raw_fd(), internal_name.as_ptr(), 0o755)
        }) {
            Ok(()) | Err(FsError::Exists) => (),
            Err(e) => return Err(e),
        }
        let internal = open_at(&root, INTERNAL, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        if matches!(
            open_at(&internal, UPLOADS, libc::O_PATH, 0),
            Err(FsError::NotFound)
        ) {
            if let Ok(legacy) = open_at(
                &root,
                b".agent-space-upload",
                libc::O_RDONLY | libc::O_DIRECTORY,
                0,
            ) {
                if let Ok(marker) = open_at(&legacy, b"owner", libc::O_PATH, 0) {
                    use std::io::Read;
                    let mut recorded = String::new();
                    if reopen(&marker, false)?
                        .take(256)
                        .read_to_string(&mut recorded)
                        .is_ok()
                        && recorded == format!("agent-space-upload-v1:{owner}\n")
                    {
                        rename(&root, b".agent-space-upload", &internal, UPLOADS)?;
                        internal.sync_all().map_err(fs_error)?;
                        root.sync_all().map_err(fs_error)?;
                    }
                }
            }
        }
        match mkdir(&internal, UPLOADS) {
            Ok(()) => {
                let dir = open_at(&internal, UPLOADS, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                let mut marker = open_at(
                    &dir,
                    b"owner",
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                    0o600,
                )?;
                use std::io::Write;
                marker
                    .write_all(format!("agent-space-upload-v1:{owner}\n").as_bytes())
                    .map_err(fs_error)?;
                marker.sync_all().map_err(fs_error)?;
                dir.sync_all().map_err(fs_error)?;
            }
            Err(FsError::Exists) => {}
            Err(e) => return Err(e),
        }
        let dir = open_at(&internal, UPLOADS, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        let marker_inode = open_at(&dir, b"owner", libc::O_PATH, 0)?;
        if !marker_inode.metadata().map_err(fs_error)?.is_file() {
            return Err(FsError::Forbidden);
        }
        let mut marker = reopen(&marker_inode, false)?;
        use std::io::Read;
        let mut content = String::new();
        std::io::Read::by_ref(&mut marker)
            .take(256)
            .read_to_string(&mut content)
            .map_err(fs_error)?;
        if content != format!("agent-space-upload-v1:{owner}\n") {
            return Err(FsError::Forbidden);
        }
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(fs_error)?;
        if uuid::Uuid::parse_str(boot.trim_end()).is_err() {
            return Err(FsError::Forbidden);
        }
        recover_previous_boots(&dir, &boot)?;
        // Existing instance directories are not reused or deleted without proof
        // that the previous owner is gone. The host gives every launch a new ID.
        mkdir(&dir, instance.as_bytes())?;
        let staging = open_at(
            &dir,
            instance.as_bytes(),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        let mut marker = open_at(
            &staging,
            b"boot-id",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        use std::io::Write;
        marker.write_all(boot.as_bytes()).map_err(fs_error)?;
        marker.sync_all().map_err(fs_error)?;
        staging.sync_all().map_err(fs_error)?;
        dir.sync_all().map_err(fs_error)?;
        Ok(Self {
            root: Arc::new(root),
            staging: Arc::new(staging),
            upload: None,
            report_inaccessible: false,
            modified: None,
        })
    }
    pub fn for_recursive_mutation(&self) -> Self {
        Self {
            report_inaccessible: true,
            ..self.clone()
        }
    }
    pub fn for_upload(&self) -> Self {
        Self {
            upload: Some(Arc::new(Mutex::new(None))),
            ..self.clone()
        }
    }
    pub fn with_modified(mut self, modified: Option<SystemTime>) -> Self {
        self.modified = modified;
        self
    }
    fn parent(&self, path: &DavPath) -> FsResult<(File, Vec<u8>)> {
        let path = relative(path)?;
        if path == b"." {
            return Err(FsError::Forbidden);
        }
        let split = path.iter().rposition(|b| *b == b'/');
        let (parent, name) = match split {
            Some(i) => (&path[..i], path[i + 1..].to_vec()),
            None => (b".".as_slice(), path),
        };
        Ok((
            open_at(&self.root, parent, libc::O_RDONLY | libc::O_DIRECTORY, 0)?,
            name,
        ))
    }
    fn checked(&self, path: &DavPath) -> FsResult<File> {
        open_at(&self.root, &relative(path)?, libc::O_PATH, 0)
    }
    fn prepare(&self, path: &DavPath, options: &OpenOptions) -> FsResult<File> {
        let context = self.upload.as_ref().ok_or(FsError::Forbidden)?;
        let mut slot = context.lock().unwrap();
        if slot.is_some() || options.append {
            return Err(FsError::Forbidden);
        }
        let (parent, name) = self.parent(path)?;
        let original = match open_at(&parent, &name, libc::O_PATH, 0) {
            Ok(file) => {
                let meta = file.metadata().map_err(fs_error)?;
                if !meta.is_file() {
                    return Err(FsError::Forbidden);
                }
                if options.create_new {
                    return Err(FsError::Exists);
                }
                Some(meta)
            }
            Err(FsError::NotFound) if options.create || options.create_new => None,
            Err(e) => return Err(e),
        };
        let temp_name = uuid::Uuid::new_v4().to_string().into_bytes();
        let file = open_at(
            &self.staging,
            &temp_name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o644,
        )?;
        // Install cleanup ownership before any fallible operation on the file.
        *slot = Some(Upload {
            staging: self.staging.clone(),
            name: temp_name,
            file,
            parent,
            target: name,
            original,
        });
        let pending = slot.as_ref().unwrap();
        if let Some(meta) = &pending.original {
            pending
                .file
                .set_permissions(std::fs::Permissions::from_mode(meta.mode() & 0o777))
                .map_err(fs_error)?;
        }
        pending.file.try_clone().map_err(fs_error)
    }
    pub fn commit(&self) -> Result<Option<Meta>, CommitError> {
        let Some(context) = &self.upload else {
            return Ok(None);
        };
        let mut slot = context.lock().unwrap();
        let Some(pending) = slot.take() else {
            return Ok(None);
        };
        if let Some(modified) = self.modified {
            set_modified(&pending.file, modified).map_err(CommitError::Fs)?;
        }
        pending
            .file
            .sync_all()
            .map_err(|e| CommitError::Fs(fs_error(e)))?;
        let now = match open_at(&pending.parent, &pending.target, libc::O_PATH, 0) {
            Ok(file) => Some(file.metadata().map_err(|e| CommitError::Fs(fs_error(e)))?),
            Err(FsError::NotFound) => None,
            Err(e) => return Err(CommitError::Fs(e)),
        };
        if pending.original.as_ref().map(fingerprint) != now.as_ref().map(fingerprint) {
            return Err(CommitError::Changed);
        }
        rename(
            &pending.staging,
            &pending.name,
            &pending.parent,
            &pending.target,
        )
        .map_err(CommitError::Fs)?;
        // A failure after rename means the result may already be visible. Never
        // attempt rollback or replay; the caller returns an error to the client.
        pending
            .parent
            .sync_all()
            .map_err(|e| CommitError::Fs(fs_error(e)))?;
        Ok(Some(Meta(
            pending
                .file
                .metadata()
                .map_err(|e| CommitError::Fs(fs_error(e)))?,
        )))
    }
}
#[derive(Debug)]
pub enum CommitError {
    Changed,
    Fs(FsError),
}
fn fingerprint(m: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
struct Upload {
    staging: Arc<File>,
    name: Vec<u8>,
    file: File,
    parent: File,
    target: Vec<u8>,
    original: Option<Metadata>,
}
impl Drop for Upload {
    fn drop(&mut self) {
        let _ = unlink(&self.staging, &self.name, false);
    }
}

impl DavFileSystem for RootFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            let file = if options.write {
                self.prepare(path, &options)?
            } else {
                let file = self.checked(path)?;
                if !file.metadata().map_err(fs_error)?.is_file() {
                    return Err(FsError::Forbidden);
                }
                reopen(&file, false)?
            };
            Ok(Box::new(OpenFile(tokio::fs::File::from_std(file))) as Box<dyn DavFile>)
        })
    }
    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            Ok(
                Box::new(Meta(self.checked(path)?.metadata().map_err(fs_error)?))
                    as Box<dyn DavMetaData>,
            )
        })
    }
    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            let dir = self.checked(path)?;
            if !dir.metadata().map_err(fs_error)?.is_dir() {
                return Err(FsError::Forbidden);
            }
            let mut entries: Vec<FsResult<Box<dyn DavDirEntry>>> = Vec::new();
            for entry in
                std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd())).map_err(fs_error)?
            {
                let entry = entry.map_err(fs_error)?;
                let name = entry.file_name().as_bytes().to_vec();
                if relative(path)? == INTERNAL && name == UPLOADS {
                    continue;
                }
                match open_at(&dir, &name, libc::O_PATH, 0) {
                    Ok(file) => entries.push(Ok(Box::new(Entry {
                        name,
                        meta: Some(Meta(file.metadata().map_err(fs_error)?)),
                    }))),
                    // Browsing hides inaccessible entries. Recursive mutations
                    // must report failure instead of silently claiming a full copy.
                    Err(FsError::Forbidden) if self.report_inaccessible => {
                        entries.push(Ok(Box::new(Entry { name, meta: None })));
                    }
                    Err(FsError::Forbidden | FsError::NotFound) => continue,
                    Err(e) => return Err(e),
                }
            }
            Ok(Box::pin(futures::stream::iter(entries)) as FsStream<Box<dyn DavDirEntry>>)
        })
    }
    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let (parent, name) = self.parent(path)?;
            let name = cstr(&name)?;
            syscall_result(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) })
        })
    }
    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let _ = self.checked(path)?;
            let (parent, name) = self.parent(path)?;
            unlink(&parent, &name, false)
        })
    }
    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let _ = self.checked(path)?;
            let (parent, name) = self.parent(path)?;
            unlink(&parent, &name, true)
        })
    }
    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            // Renaming the staging ancestor would move live upload paths outside
            // their fixed internal location. Other internal files remain ordinary DAV files.
            validate_move(from, to)?;
            let source = self.checked(from)?.metadata().map_err(fs_error)?;
            match self.checked(to) {
                Ok(target) => {
                    let target = target.metadata().map_err(fs_error)?;
                    if source.dev() == target.dev() && source.ino() == target.ino() {
                        return Err(FsError::Forbidden);
                    }
                }
                Err(FsError::NotFound) => {}
                Err(e) => return Err(e),
            }
            let (a, an) = self.parent(from)?;
            let (b, bn) = self.parent(to)?;
            rename(&a, &an, &b, &bn)?;
            a.sync_all().map_err(fs_error)?;
            b.sync_all().map_err(fs_error)
        })
    }
    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let source = self.checked(from)?;
            let original = source.metadata().map_err(fs_error)?;
            if !original.is_file() {
                return Err(FsError::Forbidden);
            }
            match self.checked(to) {
                Ok(target) => {
                    let meta = target.metadata().map_err(fs_error)?;
                    if original.dev() == meta.dev() && original.ino() == meta.ino() {
                        return Err(FsError::Forbidden);
                    }
                }
                Err(FsError::NotFound) => {}
                Err(error) => return Err(error),
            }
            let staged = self
                .for_upload()
                .with_modified(Some(original.modified().map_err(fs_error)?));
            let options = OpenOptions {
                write: true,
                create: true,
                truncate: true,
                ..Default::default()
            };
            let target = staged.prepare(to, &options)?;
            let mut reader = tokio::fs::File::from_std(reopen(&source, false)?);
            let mut writer = tokio::fs::File::from_std(target);
            tokio::io::copy(&mut reader, &mut writer)
                .await
                .map_err(fs_error)?;
            writer.flush().await.map_err(fs_error)?;
            if fingerprint(&source.metadata().map_err(fs_error)?) != fingerprint(&original) {
                return Err(FsError::GeneralFailure);
            }
            staged.commit().map(|_| ()).map_err(|e| match e {
                CommitError::Fs(error) => error,
                CommitError::Changed => FsError::GeneralFailure,
            })
        })
    }
    fn get_quota(&self) -> FsFuture<'_, (u64, Option<u64>)> {
        Box::pin(async move {
            let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
            syscall_result(unsafe { libc::fstatvfs(self.root.as_raw_fd(), stat.as_mut_ptr()) })?;
            let stat = unsafe { stat.assume_init() };
            let used = stat
                .f_blocks
                .saturating_sub(stat.f_bfree)
                .saturating_mul(stat.f_frsize);
            let available = stat.f_bavail.saturating_mul(stat.f_frsize);
            // dav-server expects (used, total), not (used, free). Exclude reserved blocks.
            Ok((used, Some(used.saturating_add(available))))
        })
    }
    fn have_props<'a>(
        &'a self,
        _: &'a DavPath,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn get_props<'a>(&'a self, _: &'a DavPath, _: bool) -> FsFuture<'a, Vec<DavProp>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn patch_props<'a>(
        &'a self,
        path: &'a DavPath,
        patch: Vec<(bool, DavProp)>,
    ) -> FsFuture<'a, Vec<(http::StatusCode, DavProp)>> {
        Box::pin(async move {
            use http::StatusCode;
            let mut modified = None;
            let mut results = Vec::new();
            for (set, prop) in patch {
                let supported = set
                    && matches!(
                        (prop.namespace.as_deref(), prop.name.as_str()),
                        (Some("DAV:"), "getlastmodified")
                            | (Some("urn:schemas-microsoft-com:"), "Win32LastModifiedTime")
                            | (Some("http://owncloud.org/ns"), "lastmodified")
                    );
                let status = if supported {
                    let time = prop
                        .xml
                        .as_ref()
                        .and_then(|xml| xmltree::Element::parse(xml.as_slice()).ok())
                        .filter(|node| {
                            !node
                                .children
                                .iter()
                                .any(|child| matches!(child, xmltree::XMLNode::Element(_)))
                        })
                        .and_then(|node| node.get_text().map(|text| text.into_owned()))
                        .and_then(|text| {
                            if prop.namespace.as_deref() == Some("http://owncloud.org/ns") {
                                parse_mtime(text.trim())
                            } else {
                                httpdate::parse_http_date(text.trim()).ok()
                            }
                        });
                    match time {
                        Some(time) => {
                            modified = Some(time);
                            StatusCode::OK
                        }
                        None => StatusCode::CONFLICT,
                    }
                } else {
                    StatusCode::FORBIDDEN
                };
                results.push((status, prop));
            }
            // PROPPATCH is all-or-nothing: preflight every property before changing the inode.
            if results.iter().any(|(status, _)| *status != StatusCode::OK) {
                for (status, _) in &mut results {
                    if *status == StatusCode::OK {
                        *status = StatusCode::FAILED_DEPENDENCY;
                    }
                }
                return Ok(results);
            }
            if let Some(modified) = modified {
                let file = reopen(&self.checked(path)?, false)?;
                set_modified(&file, modified)?;
                file.sync_all().map_err(fs_error)?;
            }
            Ok(results)
        })
    }
}

#[derive(Debug, Clone)]
pub struct Meta(Metadata);
impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn is_dir(&self) -> bool {
        self.0.is_dir()
    }
    fn modified(&self) -> FsResult<SystemTime> {
        self.0.modified().map_err(fs_error)
    }
    fn etag(&self) -> Option<String> {
        Some(format!(
            "{:x}-{:x}-{:x}-{:x}-{:x}-{:x}",
            self.0.ino(),
            self.0.len(),
            self.0.mtime(),
            self.0.mtime_nsec(),
            self.0.ctime(),
            self.0.ctime_nsec()
        ))
    }
    fn accessed(&self) -> FsResult<SystemTime> {
        self.0.accessed().map_err(fs_error)
    }
    fn created(&self) -> FsResult<SystemTime> {
        self.0.created().map_err(fs_error)
    }
    fn executable(&self) -> FsResult<bool> {
        Ok(self.0.mode() & 0o111 != 0)
    }
}
struct Entry {
    name: Vec<u8>,
    meta: Option<Meta>,
}
impl DavDirEntry for Entry {
    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }
    fn metadata(&'_ self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async {
            self.meta
                .clone()
                .map(|meta| Box::new(meta) as Box<dyn DavMetaData>)
                .ok_or(FsError::Forbidden)
        })
    }
}
#[derive(Debug)]
struct OpenFile(tokio::fs::File);
impl DavFile for OpenFile {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async {
            Ok(Box::new(Meta(self.0.metadata().await.map_err(fs_error)?)) as Box<dyn DavMetaData>)
        })
    }
    fn write_buf(&'_ mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async move {
            while buf.has_remaining() {
                let chunk = buf.chunk();
                let len = chunk.len();
                self.0.write_all(chunk).await.map_err(fs_error)?;
                buf.advance(len);
            }
            Ok(())
        })
    }
    fn write_bytes(&'_ mut self, buf: Bytes) -> FsFuture<'_, ()> {
        Box::pin(async move { self.0.write_all(&buf).await.map_err(fs_error) })
    }
    fn read_bytes(&'_ mut self, count: usize) -> FsFuture<'_, Bytes> {
        Box::pin(async move {
            let mut buf = vec![0; count.min(256 * 1024)];
            let n = self.0.read(&mut buf).await.map_err(fs_error)?;
            buf.truncate(n);
            Ok(Bytes::from(buf))
        })
    }
    fn seek(&'_ mut self, pos: io::SeekFrom) -> FsFuture<'_, u64> {
        Box::pin(async move { self.0.seek(pos).await.map_err(fs_error) })
    }
    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        Box::pin(async move { self.0.flush().await.map_err(fs_error) })
    }
}

fn set_modified(file: &File, modified: SystemTime) -> FsResult<()> {
    let old = file
        .metadata()
        .map_err(fs_error)?
        .modified()
        .map_err(fs_error)?;
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .map_err(fs_error)?;
    // Some filesystems clamp out-of-range timestamps without returning an error.
    if file
        .metadata()
        .map_err(fs_error)?
        .modified()
        .map_err(fs_error)?
        != modified
    {
        file.set_times(std::fs::FileTimes::new().set_modified(old))
            .map_err(fs_error)?;
        return Err(FsError::GeneralFailure);
    }
    Ok(())
}

/// Unix seconds accepted by X-OC-Mtime; bounded to HTTP-date's representable years.
pub fn parse_mtime(value: &str) -> Option<SystemTime> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let seconds: u64 = value.parse().ok()?;
    if seconds > 253_402_300_799 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}
