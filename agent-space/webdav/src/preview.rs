use crate::{
    fs::{preview_file_version, RootFs},
    response, ResponseBody,
};
#[path = "../../build-support/preview_protocol.rs"]
mod protocol;
use bytes::Bytes;
use dav_server::davpath::DavPath;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io,
    os::fd::AsRawFd,
    process::Stdio,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command, sync::watch};
use tokio_util::sync::CancellationToken;

const OUTPUT: usize = 1024 * 1024;
const WORKER: &str = include_str!("preview_worker.py");
const ALLOW: &str = protocol::ALLOW;
type ImageResult = Result<Arc<Image>, Error>;

#[derive(Clone, Debug)]
pub(crate) struct Error(StatusCode, &'static str);
impl Error {
    fn invalid() -> Self {
        Self(StatusCode::BAD_REQUEST, "invalid_preview_request")
    }
    fn changed() -> Self {
        Self(StatusCode::PRECONDITION_FAILED, "file_version_conflict")
    }
    fn unavailable() -> Self {
        Self(StatusCode::SERVICE_UNAVAILABLE, "preview_unavailable")
    }
    fn filesystem(error: dav_server::fs::FsError) -> Self {
        let status = match error {
            dav_server::fs::FsError::NotFound => StatusCode::NOT_FOUND,
            dav_server::fs::FsError::Forbidden => StatusCode::FORBIDDEN,
            dav_server::fs::FsError::PathTooLong => StatusCode::BAD_REQUEST,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        Self(
            status,
            match status {
                StatusCode::NOT_FOUND => "file_not_found",
                StatusCode::FORBIDDEN => "file_forbidden",
                StatusCode::BAD_REQUEST => "invalid_preview_request",
                _ => "preview_unavailable",
            },
        )
    }
    fn response(&self, head: bool) -> Response<ResponseBody> {
        let body = serde_json::json!({"type":"about:blank","title":self.1,"status":self.0.as_u16(),"code":self.1}).to_string();
        let mut result = response(self.0, if head { "" } else { &body });
        result.headers_mut().insert(
            "content-type",
            HeaderValue::from_static("application/problem+json"),
        );
        if self.0 == StatusCode::TOO_MANY_REQUESTS {
            result
                .headers_mut()
                .insert("retry-after", HeaderValue::from_static("1"));
        }
        if self.0 == StatusCode::METHOD_NOT_ALLOWED {
            result
                .headers_mut()
                .insert("allow", HeaderValue::from_static(ALLOW));
        }
        result
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    path: String,
    source: String,
    size: u32,
}
struct Image {
    bytes: Bytes,
    etag: String,
}
struct Cached {
    image: Arc<Image>,
    used: Instant,
}
struct Job {
    key: Key,
    waiters: AtomicUsize,
    cancel: CancellationToken,
    result: watch::Sender<Option<ImageResult>>,
}
struct Waiter(Arc<Job>);
impl Drop for Waiter {
    fn drop(&mut self) {
        if self.0.waiters.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.cancel.cancel();
        }
    }
}
#[derive(Default)]
struct State {
    cache: HashMap<Key, Cached>,
    job: Option<Arc<Job>>,
}
#[derive(Clone, Default)]
pub(crate) struct Store(Arc<Mutex<State>>);

pub(crate) fn capability() -> String {
    protocol::CAPABILITY.to_owned()
}

pub(crate) fn strong_etag(value: &str) -> bool {
    value.len() >= 2
        && value.len() <= 1024
        && value.starts_with('"')
        && value.ends_with('"')
        && value.as_bytes()[1..value.len() - 1]
            .iter()
            .all(|byte| *byte >= 0x21 && *byte != b'"' && *byte != 0x7f)
}

fn decode(value: &str) -> Result<String, Error> {
    let bytes = value.as_bytes();
    for (at, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && (at + 2 >= bytes.len()
                || !bytes[at + 1].is_ascii_hexdigit()
                || !bytes[at + 2].is_ascii_hexdigit())
        {
            return Err(Error::invalid());
        }
    }
    percent_encoding::percent_decode_str(&value.replace('+', " "))
        .decode_utf8()
        .map(|text| text.into_owned())
        .map_err(|_| Error::invalid())
}

fn parameters(query: Option<&str>) -> Result<Key, Error> {
    let mut values = HashMap::new();
    for entry in query.ok_or_else(Error::invalid)?.split('&') {
        let (name, value) = entry.split_once('=').ok_or_else(Error::invalid)?;
        let name = decode(name)?;
        if !["path", "size", "expectedEtag"].contains(&name.as_str())
            || values.insert(name, decode(value)?).is_some()
        {
            return Err(Error::invalid());
        }
    }
    let path = values.remove("path").ok_or_else(Error::invalid)?;
    if path.is_empty()
        || path.len() > 4096
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path
            .chars()
            .any(|value| value == '\\' || value.is_control())
    {
        return Err(Error::invalid());
    }
    let source = values.remove("expectedEtag").ok_or_else(Error::invalid)?;
    if !strong_etag(&source) {
        return Err(Error::invalid());
    }
    let size = match values.remove("size").as_deref().unwrap_or("256") {
        "128" => 128,
        "256" => 256,
        "512" => 512,
        _ => return Err(Error::invalid()),
    };
    Ok(Key { path, source, size })
}

fn source_path(path: &str) -> Result<DavPath, Error> {
    let escaped = path
        .split('/')
        .map(|part| {
            percent_encoding::utf8_percent_encode(part, percent_encoding::NON_ALPHANUMERIC)
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("/");
    DavPath::new(&("/".to_owned() + &escaped)).map_err(|_| Error::invalid())
}

// Parse the HTTP list without splitting commas inside quoted opaque tags.
fn condition(headers: &HeaderMap, name: &str) -> Result<Option<Vec<String>>, Error> {
    let values = headers
        .get_all(name)
        .iter()
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| Error::invalid())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.is_empty() {
        return Ok(None);
    }
    let value = values.join(",");
    if value.len() > 8192 {
        return Err(Error::invalid());
    }
    if value.trim() == "*" {
        return Ok(Some(vec!["*".into()]));
    }
    let mut input = value.trim();
    let mut tags = Vec::new();
    loop {
        let weak = input.starts_with("W/");
        let raw = if weak { &input[2..] } else { input };
        if !raw.starts_with('"') {
            return Err(Error::invalid());
        }
        let end = raw[1..].find('"').ok_or_else(Error::invalid)? + 2;
        if !strong_etag(&raw[..end]) {
            return Err(Error::invalid());
        }
        tags.push(format!("{}{}", if weak { "W/" } else { "" }, &raw[..end]));
        input = raw[end..].trim_start();
        if input.is_empty() {
            break;
        }
        input = input
            .strip_prefix(',')
            .ok_or_else(Error::invalid)?
            .trim_start();
        if input.is_empty() {
            return Err(Error::invalid());
        }
    }
    Ok(Some(tags))
}

impl Store {
    pub(crate) async fn handle(
        &self,
        fs: RootFs,
        owner: &str,
        method: &Method,
        query: Option<&str>,
        headers: &HeaderMap,
        cancel: CancellationToken,
    ) -> Response<ResponseBody> {
        let head = method == Method::HEAD;
        let result = self.read(fs, method, query, headers, cancel.clone()).await;
        let mut response = match result {
            Ok((status, image, source)) => {
                let mut response = response(status, "");
                if let Some(image) = image {
                    response
                        .headers_mut()
                        .insert("content-type", HeaderValue::from_static("image/webp"));
                    response.headers_mut().insert(
                        "content-disposition",
                        HeaderValue::from_static("inline; filename=\"thumbnail.webp\""),
                    );
                    response
                        .headers_mut()
                        .insert("etag", HeaderValue::from_str(&image.etag).unwrap());
                    response.headers_mut().insert(
                        "preview-source-etag",
                        HeaderValue::from_str(&source).unwrap(),
                    );
                    if status != StatusCode::NOT_MODIFIED {
                        response.headers_mut().insert(
                            "content-length",
                            HeaderValue::from_str(&image.bytes.len().to_string()).unwrap(),
                        );
                        if !head {
                            *response.body_mut() = crate::bytes_body(image.bytes.clone(), cancel);
                        }
                    }
                } else {
                    let body = capability();
                    response = crate::response(StatusCode::OK, &body);
                    response
                        .headers_mut()
                        .insert("content-type", HeaderValue::from_static("application/json"));
                    response
                        .headers_mut()
                        .insert("allow", HeaderValue::from_static(ALLOW));
                }
                response
            }
            Err(error) => error.response(head),
        };
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
        response.headers_mut().insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
        if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
            if let Ok(value) = HeaderValue::from_str(owner) {
                response.headers_mut().insert("agent-space-id", value);
            }
        }
        response
    }

    async fn read(
        &self,
        fs: RootFs,
        method: &Method,
        query: Option<&str>,
        headers: &HeaderMap,
        cancel: CancellationToken,
    ) -> Result<(StatusCode, Option<Arc<Image>>, String), Error> {
        if method == Method::OPTIONS {
            return if query.is_none() {
                Ok((StatusCode::OK, None, String::new()))
            } else {
                Err(Error::invalid())
            };
        }
        if method != Method::GET && method != Method::HEAD {
            return Err(Error(
                StatusCode::METHOD_NOT_ALLOWED,
                "preview_method_not_allowed",
            ));
        }
        let key = parameters(query)?;
        let if_match = condition(headers, "if-match")?;
        let if_none = condition(headers, "if-none-match")?;
        let path = source_path(&key.path)?;
        let (file, version) = fs.preview_source(&path).map_err(Error::filesystem)?;
        if version != key.source {
            return Err(Error::changed());
        }
        if cancel.is_cancelled() {
            return Err(Error::unavailable());
        }
        let image = self.image(fs.clone(), key.clone(), file, cancel).await?;
        if fs.preview_source(&path).map_err(Error::filesystem)?.1 != key.source {
            return Err(Error::changed());
        }
        if if_match.is_some_and(|tags| !tags.iter().any(|tag| tag == "*" || tag == &image.etag)) {
            return Err(Error::changed());
        }
        let status = if if_none.is_some_and(|tags| {
            tags.iter()
                .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == image.etag)
        }) {
            StatusCode::NOT_MODIFIED
        } else {
            StatusCode::OK
        };
        Ok((status, Some(image), key.source))
    }

    async fn image(
        &self,
        fs: RootFs,
        key: Key,
        file: File,
        cancel: CancellationToken,
    ) -> ImageResult {
        let (job, start) = {
            let mut state = self.0.lock().unwrap();
            state
                .cache
                .retain(|_, item| item.used.elapsed() < Duration::from_secs(600));
            if let Some(cached) = state.cache.get_mut(&key) {
                cached.used = Instant::now();
                return Ok(cached.image.clone());
            }
            if let Some(job) = &state.job {
                if job.key != key || job.cancel.is_cancelled() {
                    return Err(Error(StatusCode::TOO_MANY_REQUESTS, "preview_busy"));
                }
                job.waiters.fetch_add(1, Ordering::Relaxed);
                (job.clone(), false)
            } else {
                let (result, _) = watch::channel(None);
                let job = Arc::new(Job {
                    key: key.clone(),
                    waiters: AtomicUsize::new(1),
                    cancel: CancellationToken::new(),
                    result,
                });
                state.job = Some(job.clone());
                (job, true)
            }
        };
        let _waiter = Waiter(job.clone());
        if start {
            let store = self.clone();
            let work = job.clone();
            tokio::spawn(async move {
                let generation = generate(fs.clone(), &key, file, work.cancel.clone());
                store
                    .finish(&work, &key, generation, Duration::from_secs(10))
                    .await;
            });
        }
        let mut result = job.result.subscribe();
        loop {
            if let Some(value) = result.borrow().clone() {
                return value;
            }
            tokio::select! { biased; _ = cancel.cancelled() => return Err(Error::unavailable()), changed = result.changed() => if changed.is_err() { return Err(Error::unavailable()); } }
        }
    }

    async fn finish(
        &self,
        work: &Arc<Job>,
        key: &Key,
        generation: impl std::future::Future<Output = ImageResult>,
        deadline: Duration,
    ) {
        tokio::pin!(generation);
        let expires = tokio::time::Instant::now() + deadline;
        let result = tokio::select! {
            result = &mut generation => {
                // A synchronous final source check or scheduler delay can make
                // both branches ready. Never cache an outcome past the deadline.
                if tokio::time::Instant::now() >= expires {
                    work.cancel.cancel();
                    Err(Error(StatusCode::SERVICE_UNAVAILABLE, "preview_timeout"))
                } else {
                    result
                }
            },
            _ = tokio::time::sleep_until(expires) => {
                let timeout = Error(StatusCode::SERVICE_UNAVAILABLE, "preview_timeout");
                work.result.send_replace(Some(Err(timeout.clone())));
                work.cancel.cancel();
                // The response deadline does not transfer ownership of children.
                // Retain this task and its generation slot until actual cleanup.
                eprintln!("preview: response deadline expired; retaining decoder cleanup owner");
                let _ = generation.await;
                Err(timeout)
            }
        };
        {
            let mut state = self.0.lock().unwrap();
            if let Ok(image) = &result {
                if !work.cancel.is_cancelled() {
                    state
                        .cache
                        .retain(|other, _| other.path != key.path || other.source == key.source);
                    state.cache.insert(
                        key.clone(),
                        Cached {
                            image: image.clone(),
                            used: Instant::now(),
                        },
                    );
                    while state.cache.len() > 128
                        || state
                            .cache
                            .values()
                            .map(|entry| entry.image.bytes.len())
                            .sum::<usize>()
                            > 32 * 1024 * 1024
                    {
                        let oldest = state
                            .cache
                            .iter()
                            .min_by_key(|(_, entry)| entry.used)
                            .map(|(key, _)| key.clone())
                            .unwrap();
                        state.cache.remove(&oldest);
                    }
                }
            }
            if state
                .job
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, work))
            {
                state.job = None;
            }
        }
        work.result.send_replace(Some(result));
    }
}

struct Group {
    pid: i32,
    armed: bool,
}
impl Drop for Group {
    fn drop(&mut self) {
        if self.armed {
            unsafe {
                libc::kill(-self.pid, libc::SIGKILL);
            }
        }
    }
}

async fn bounded(stream: impl tokio::io::AsyncRead + Unpin, limit: usize) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    stream.take(limit as u64 + 1).read_to_end(&mut data).await?;
    if data.len() > limit {
        return Err(io::Error::other("preview output limit"));
    }
    Ok(data)
}

async fn terminal_without_reaping(pid: i32) -> io::Result<()> {
    loop {
        let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut information,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { information.si_pid() } == pid {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn reap_descendants(group: i32) -> io::Result<()> {
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(-group, &mut status, libc::WNOHANG) };
        if result > 0 {
            continue;
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                return Ok(());
            }
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(error);
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn run_decoder(
    file: &File,
    size: u32,
    cancel: CancellationToken,
    worker: &str,
    deadline: Duration,
) -> Result<Vec<u8>, Error> {
    if cancel.is_cancelled() {
        return Err(Error::unavailable());
    }
    // This helper owns only its decoder descendants. Adopt orphaned members
    // so cancellation also reaps them rather than leaving zombies at PID 1.
    static SUBREAPER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !SUBREAPER.get_or_init(|| unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) == 0 }) {
        return Err(Error::unavailable());
    }
    let descriptor = file.as_raw_fd();
    let parent = unsafe { libc::getpid() };
    let mut command = Command::new("/usr/bin/python3");
    command
        .args(["-I", "-c", worker, &size.to_string()])
        .env_clear()
        .env("LANG", "C.UTF-8")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0
                || libc::getppid() != parent
            {
                return Err(io::Error::last_os_error());
            }
            if libc::dup2(descriptor, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            for (resource, limit) in [
                (libc::RLIMIT_AS, 768 * 1024 * 1024),
                (libc::RLIMIT_CPU, 8),
                (libc::RLIMIT_CORE, 0),
            ] {
                let mut inherited = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::getrlimit(resource, &mut inherited) < 0 {
                    return Err(io::Error::last_os_error());
                }
                let maximum = (limit as libc::rlim_t).min(inherited.rlim_max);
                let bound = libc::rlimit {
                    rlim_cur: maximum,
                    rlim_max: maximum,
                };
                if libc::setrlimit(resource, &bound) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "preview_dependency_unavailable",
            )
        } else {
            Error::unavailable()
        }
    })?;
    let mut group = Group {
        pid: child.id().unwrap() as i32,
        armed: true,
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let result = {
        let work = async {
            let (out, _) = tokio::try_join!(bounded(stdout, OUTPUT), bounded(stderr, 64 * 1024))?;
            // Keep the leader PID reserved until the process group has been
            // killed: reaping it early would allow unrelated PID/PGID reuse.
            terminal_without_reaping(group.pid).await?;
            Ok::<_, io::Error>(out)
        };
        tokio::select! { biased;
            _ = cancel.cancelled() => Err(Error::unavailable()),
            result = tokio::time::timeout(deadline, work) => match result {
                Err(_) => Err(Error(StatusCode::SERVICE_UNAVAILABLE, "preview_timeout")),
                Ok(Err(_)) => Err(Error(StatusCode::PAYLOAD_TOO_LARGE, "preview_source_limit")),
                Ok(Ok(value)) => Ok(value),
            }
        }
    };
    unsafe {
        libc::kill(-group.pid, libc::SIGKILL);
    }
    // Kill exactly once while the leader PID is still reserved. A later
    // destructor must never signal this group after the leader is reaped.
    group.armed = false;
    let cleanup = async {
        let status = child.wait().await;
        reap_descendants(group.pid).await?;
        status
    };
    tokio::pin!(cleanup);
    let status = tokio::select! {
        status = &mut cleanup => status,
        _ = tokio::time::sleep(Duration::from_secs(1)) => {
            eprintln!("preview: decoder cleanup delayed; retaining generation slot");
            cleanup.await
        }
    }
    .map_err(|_| Error::unavailable())?;
    let bytes = result?;
    if !status.success() {
        return Err(match status.code() {
            Some(41) => Error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "preview_unsupported_media",
            ),
            Some(42) => Error(StatusCode::PAYLOAD_TOO_LARGE, "preview_source_limit"),
            Some(44) => Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "preview_dependency_unavailable",
            ),
            Some(45) | None => Error(StatusCode::SERVICE_UNAVAILABLE, "preview_timeout"),
            _ => Error(StatusCode::UNPROCESSABLE_ENTITY, "preview_decode_failed"),
        });
    }
    Ok(bytes)
}

async fn generate(fs: RootFs, key: &Key, file: File, cancel: CancellationToken) -> ImageResult {
    let bytes = run_decoder(&file, key.size, cancel, WORKER, Duration::from_secs(10)).await?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err(Error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "preview_decode_failed",
        ));
    }
    if preview_file_version(&file).map_err(Error::filesystem)? != key.source
        || fs
            .preview_source(&source_path(&key.path)?)
            .map_err(Error::filesystem)?
            .1
            != key.source
    {
        return Err(Error::changed());
    }
    Ok(Arc::new(Image {
        etag: format!("\"{:x}\"", Sha256::digest(&bytes)),
        bytes: Bytes::from(bytes),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generation_ready_after_deadline_is_not_cached() {
        let store = Store::default();
        let key = Key {
            path: "image.png".into(),
            source: "\"source\"".into(),
            size: 128,
        };
        let (sender, _) = watch::channel(None);
        let job = Arc::new(Job {
            key: key.clone(),
            waiters: AtomicUsize::new(1),
            cancel: CancellationToken::new(),
            result: sender,
        });
        store.0.lock().unwrap().job = Some(job.clone());
        store
            .finish(
                &job,
                &key,
                async {
                    std::thread::sleep(Duration::from_millis(80));
                    Ok(Arc::new(Image {
                        bytes: Bytes::from_static(b"late"),
                        etag: "\"late\"".into(),
                    }))
                },
                Duration::from_millis(10),
            )
            .await;
        assert_eq!(
            job.result
                .borrow()
                .as_ref()
                .unwrap()
                .as_ref()
                .err()
                .unwrap()
                .1,
            "preview_timeout"
        );
        assert!(job.cancel.is_cancelled());
        assert!(store.0.lock().unwrap().cache.is_empty());
    }

    #[tokio::test]
    async fn response_deadline_retains_cleanup_slot_and_discards_late_success() {
        let store = Store::default();
        let key = Key {
            path: "image.png".into(),
            source: "\"source\"".into(),
            size: 128,
        };
        let (sender, mut result) = watch::channel(None);
        let job = Arc::new(Job {
            key: key.clone(),
            waiters: AtomicUsize::new(1),
            cancel: CancellationToken::new(),
            result: sender,
        });
        store.0.lock().unwrap().job = Some(job.clone());
        let (release, held) = tokio::sync::oneshot::channel();
        let owner = store.clone();
        let active = job.clone();
        let task = tokio::spawn(async move {
            owner
                .finish(
                    &active,
                    &key,
                    async {
                        held.await.unwrap();
                        Ok(Arc::new(Image {
                            bytes: Bytes::from_static(b"late"),
                            etag: "\"late\"".into(),
                        }))
                    },
                    Duration::from_millis(40),
                )
                .await;
        });
        tokio::time::timeout(Duration::from_secs(1), result.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.borrow().as_ref().unwrap().as_ref().err().unwrap().1,
            "preview_timeout"
        );
        assert!(job.cancel.is_cancelled());
        assert!(
            store.0.lock().unwrap().job.is_some(),
            "unreaped generation released its slot"
        );
        release.send(()).unwrap();
        task.await.unwrap();
        let state = store.0.lock().unwrap();
        assert!(state.job.is_none());
        assert!(state.cache.is_empty(), "late success entered cache");
    }

    #[tokio::test]
    async fn decoder_reaps_descendant_after_leader_exits_with_open_pipes() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("child.pid");
        let input = temporary.path().join("input");
        std::fs::write(&input, marker.to_str().unwrap()).unwrap();
        let file = File::open(&input).unwrap();
        let worker = "import os,pathlib,time\npath=os.read(3,4096).decode()\nif os.fork()==0:\n pathlib.Path(path).write_text(str(os.getpid()))\n time.sleep(60)\nelse:\n os._exit(0)\n";
        let error = run_decoder(
            &file,
            128,
            CancellationToken::new(),
            worker,
            Duration::from_millis(250),
        )
        .await
        .unwrap_err();
        assert_eq!(error.1, "preview_timeout");
        let pid: i32 = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
        let process = std::path::PathBuf::from(format!("/proc/{pid}"));
        let leaked = process.exists();
        if std::fs::read_link(process.join("fd/3")).is_ok_and(|path| path == input) {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        assert!(!leaked, "decoder descendant survived its request owner");
    }

    #[tokio::test]
    async fn decoder_output_and_memory_are_bounded() {
        let file = File::open("/dev/null").unwrap();
        let error = run_decoder(
            &file,
            128,
            CancellationToken::new(),
            "import os\nwhile True: os.write(2,b'x'*8192)\n",
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::PAYLOAD_TOO_LARGE);
        let error = run_decoder(
            &file,
            128,
            CancellationToken::new(),
            "import sys\ntry: data=bytearray(2*1024*1024*1024)\nexcept MemoryError: sys.exit(42)\n",
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn decoder_reaps_background_descendant_even_after_successful_leader() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("child.pid");
        let input = temporary.path().join("input");
        std::fs::write(&input, marker.to_str().unwrap()).unwrap();
        let file = File::open(&input).unwrap();
        let worker = "import os,pathlib,time\npath=pathlib.Path(os.read(3,4096).decode())\nif os.fork()==0:\n os.close(1); os.close(2); path.write_text(str(os.getpid())); time.sleep(60)\nelse:\n while not path.exists(): time.sleep(.001)\n os._exit(0)\n";
        assert!(run_decoder(
            &file,
            128,
            CancellationToken::new(),
            worker,
            Duration::from_secs(2)
        )
        .await
        .unwrap()
        .is_empty());
        let pid: i32 = std::fs::read_to_string(marker).unwrap().parse().unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }

    #[tokio::test]
    async fn decoder_cancellation_reaps_the_whole_live_group() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("children.json");
        let input = temporary.path().join("input");
        std::fs::write(&input, marker.to_str().unwrap()).unwrap();
        let file = File::open(&input).unwrap();
        let worker="import os,pathlib,time,json\npath=pathlib.Path(os.read(3,4096).decode()); parent=os.getpid()\nif os.fork()==0:\n path.write_text(json.dumps([parent,os.getpid()])); time.sleep(60)\nelse:\n time.sleep(60)\n";
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task = tokio::spawn(async move {
            run_decoder(&file, 128, token, worker, Duration::from_secs(10)).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
        assert_eq!(task.await.unwrap().unwrap_err().1, "preview_unavailable");
        let pids: Vec<i32> =
            serde_json::from_str(&std::fs::read_to_string(marker).unwrap()).unwrap();
        for pid in pids {
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
    }

    #[tokio::test]
    async fn decoder_cpu_limit_is_applied_to_the_actual_worker() {
        let file = File::open("/dev/null").unwrap();
        let begun = Instant::now();
        let error = run_decoder(
            &file,
            128,
            CancellationToken::new(),
            "while True: pass",
            Duration::from_secs(12),
        )
        .await
        .unwrap_err();
        assert_eq!(error.1, "preview_timeout");
        assert!(begun.elapsed() < Duration::from_secs(11));
    }
}
