use agent_space_webdav::Service;
use bytes::Bytes;
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Full};
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const LENGTH: u64 = 1u64 << 32;
const PREFIX: &str = "/u/alice/dav/";

use dav_server::{davpath::DavPath, fs::*, DavHandler};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Clone)]
struct ObservedFs {
    inner: agent_space_webdav::fs::RootFs,
    reads: Arc<AtomicUsize>,
    open: Arc<AtomicUsize>,
    fail_seek: usize,
}
impl DavFileSystem for ObservedFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            let file = DavFileSystem::open(&self.inner, path, options).await?;
            self.open.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(ObservedFile {
                file,
                owner: self.clone(),
                seeks: 0,
            }) as Box<dyn DavFile>)
        })
    }
    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        DavFileSystem::metadata(&self.inner, path)
    }
    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        meta: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        DavFileSystem::read_dir(&self.inner, path, meta)
    }
}
struct ObservedFile {
    file: Box<dyn DavFile>,
    owner: ObservedFs,
    seeks: usize,
}
impl std::fmt::Debug for ObservedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObservedFile")
    }
}
impl Drop for ObservedFile {
    fn drop(&mut self) {
        self.owner.open.fetch_sub(1, Ordering::SeqCst);
    }
}
impl DavFile for ObservedFile {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        self.file.metadata()
    }
    fn write_buf(&mut self, buf: Box<dyn bytes::Buf + Send>) -> FsFuture<'_, ()> {
        self.file.write_buf(buf)
    }
    fn write_bytes(&mut self, buf: Bytes) -> FsFuture<'_, ()> {
        self.file.write_bytes(buf)
    }
    fn flush(&mut self) -> FsFuture<'_, ()> {
        self.file.flush()
    }
    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        self.owner.reads.fetch_add(1, Ordering::SeqCst);
        // Legal nonempty short reads must not be treated as EOF.
        self.file.read_bytes(count.min(3))
    }
    fn seek(&mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        self.seeks += 1;
        if self.seeks == self.owner.fail_seek {
            return Box::pin(async { Err(FsError::GeneralFailure) });
        }
        self.file.seek(pos)
    }
}

#[tokio::test]
async fn head_short_reads_seek_errors_and_drop_release_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file"), b"abcdefghij").unwrap();
    let inner = agent_space_webdav::fs::RootFs::new(dir.path(), "test", "instance").unwrap();
    for fail_seek in [0, 1, 2] {
        let fs = ObservedFs {
            inner: inner.clone(),
            reads: Arc::default(),
            open: Arc::default(),
            fail_seek,
        };
        let dav = DavHandler::builder()
            .filesystem(Box::new(fs.clone()))
            .build_handler();
        let response = dav
            .handle(
                Request::builder()
                    .method("HEAD")
                    .uri("/file")
                    .header("range", "bytes=1-2")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-length"], "10");
        assert_eq!(fs.reads.load(Ordering::SeqCst), 0);
        assert_eq!(fs.open.load(Ordering::SeqCst), 0);
        let mut response = dav
            .handle(
                Request::builder()
                    .uri("/file")
                    .header("range", "bytes=0-3,7-9")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await;
        if fail_seek == 1 {
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        } else {
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            let result = response.body_mut().collect().await;
            if fail_seek == 2 {
                assert!(result.is_err(), "seek failure was skipped");
            } else {
                let data = result.unwrap().to_bytes();
                assert!(data.windows(4).any(|w| w == b"abcd"));
                assert!(data.windows(3).any(|w| w == b"hij"));
            }
        }
        drop(response);
        assert_eq!(fs.open.load(Ordering::SeqCst), 0);
        let mut response = dav
            .handle(
                Request::builder()
                    .uri("/file")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await;
        response.body_mut().frame().await.unwrap().unwrap();
        assert_eq!(fs.open.load(Ordering::SeqCst), 1);
        drop(response);
        assert_eq!(
            fs.open.load(Ordering::SeqCst),
            0,
            "client drop retained file"
        );
    }
}

fn fixture() -> (tempfile::TempDir, Service) {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::new(
        dir.path(),
        "test",
        "instance",
        PREFIX,
        Duration::from_secs(2),
    )
    .unwrap();
    (dir, service)
}

async fn get(
    service: &Service,
    method: &str,
    headers: &[(&str, &str)],
) -> http::Response<agent_space_webdav::ResponseBody> {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("{PREFIX}file"));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    service
        .handle(
            request.body(Full::new(Bytes::new())).unwrap(),
            CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn truncated_get_and_range_fail_without_inventing_bytes() {
    for range in [None, Some("bytes=7-131071")] {
        let (dir, service) = fixture();
        std::fs::write(dir.path().join("file"), vec![0x59; 256 * 1024]).unwrap();
        let headers = range.map(|r| vec![("range", r)]).unwrap_or_default();
        let mut response = get(&service, "GET", &headers).await;
        assert_eq!(
            response.status(),
            if range.is_some() {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            }
        );
        let first = response
            .body_mut()
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        assert!(!first.is_empty());
        assert!(first.iter().all(|byte| *byte == 0x59));
        // The lazy body is suspended after this frame. Truncate the same inode
        // before polling again, without a race against a network producer.
        File::options()
            .write(true)
            .open(dir.path().join("file"))
            .unwrap()
            .set_len(0)
            .unwrap();
        loop {
            match response.body_mut().frame().await {
                Some(Err(error)) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
                    break;
                }
                Some(Ok(frame)) => assert!(
                    frame.into_data().unwrap().iter().all(|b| *b == 0x59),
                    "fabricated bytes"
                ),
                None => panic!("truncated transfer reported success"),
            }
        }
    }
}

#[tokio::test]
async fn four_gib_sparse_ranges_and_version_preconditions() {
    let (dir, service) = fixture();
    let mut file = File::create(dir.path().join("file")).unwrap();
    file.set_len(LENGTH).unwrap();
    let middle: Vec<u8> = (1..=16).collect();
    let tail: Vec<u8> = (128..192).collect();
    file.seek(SeekFrom::Start((1 << 31) - 8)).unwrap();
    file.write_all(&middle).unwrap();
    file.seek(SeekFrom::Start(LENGTH - 64)).unwrap();
    file.write_all(&tail).unwrap();
    let head = get(&service, "HEAD", &[]).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()["content-length"], LENGTH.to_string());
    let etag = head.headers()["etag"].to_str().unwrap().to_owned();
    assert!(head
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .is_empty());
    for (range, start, expected) in [
        ("bytes=2147483640-2147483655", (1 << 31) - 8, middle),
        ("bytes=4294967295-4294967295", LENGTH - 1, vec![191]),
        ("bytes=4294967232-4294967295", LENGTH - 64, tail.clone()),
        ("bytes=4294967232-", LENGTH - 64, tail.clone()),
        ("bytes=-64", LENGTH - 64, tail.clone()),
        ("bytes=4294967232-18446744073709551615", LENGTH - 64, tail),
    ] {
        let response = get(&service, "GET", &[("range", range), ("if-match", &etag)]).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(response.headers()["etag"], etag);
        assert_eq!(
            response.headers()["content-length"],
            expected.len().to_string()
        );
        assert_eq!(
            response.headers()["content-range"],
            format!(
                "bytes {start}-{}/{LENGTH}",
                start + expected.len() as u64 - 1
            )
        );
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            expected
        );
    }
    for range in [
        "bytes=4294967296-",
        "bytes=4294967296-4294967300",
        "bytes=-0",
    ] {
        let response = get(&service, "GET", &[("range", range)]).await;
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response.headers()["content-range"],
            format!("bytes */{LENGTH}")
        );
    }
    std::fs::write(dir.path().join("replacement"), b"new").unwrap();
    std::fs::rename(dir.path().join("replacement"), dir.path().join("file")).unwrap();
    let response = get(
        &service,
        "GET",
        &[("range", "bytes=0-1"), ("if-match", &etag)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    assert!(response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .is_empty());
    for range in ["bytes=-99", "bytes=0-18446744073709551615"] {
        let response = get(&service, "GET", &[("range", range)]).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()["content-range"], "bytes 0-2/3");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "new"
        );
    }
    for range in ["bytes=+1-2", "bytes=0-+2", "bytes=-+2"] {
        let response = get(&service, "GET", &[("range", range)]).await;
        assert_eq!(response.status(), StatusCode::OK, "{range}");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "new"
        );
    }
    std::fs::write(dir.path().join("file"), b"").unwrap();
    for range in ["bytes=-1", "bytes=0-", "bytes=0-0"] {
        let response = get(&service, "GET", &[("range", range)]).await;
        assert_eq!(response.status(), StatusCode::OK, "{range}");
        assert_eq!(response.headers()["content-length"], "0");
        assert!(!response.headers().contains_key("content-range"));
        assert!(response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty());
    }
}
