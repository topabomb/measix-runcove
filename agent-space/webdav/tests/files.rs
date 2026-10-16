use agent_space_webdav::Service;
use bytes::Bytes;
use http::{Request, StatusCode};
use http_body::Frame;
use http_body_util::{BodyExt, Full, StreamBody};
use std::{
    fs, io,
    os::unix::fs::{symlink, PermissionsExt},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const PREFIX: &str = "/u/alice/dav/";

#[tokio::test]
async fn review_directory_ancestor_overwrite_preserves_source_and_siblings() {
    for method in ["COPY", "MOVE"] {
        for (source, destination) in [("tree/child/", "tree/"), ("tree//child", "%74ree//")] {
            let (dir, service) = setup();
            fs::create_dir_all(dir.path().join("tree/child")).unwrap();
            fs::write(dir.path().join("tree/child/source"), "source").unwrap();
            fs::write(dir.path().join("tree/sibling"), "sibling").unwrap();
            let destination = format!("{PREFIX}{destination}");
            let (status, _, _) = request(
                &service,
                method,
                source,
                &[("destination", &destination)],
                "",
            )
            .await;
            assert_eq!(
                fs::read(dir.path().join("tree/child/source"))
                    .ok()
                    .as_deref(),
                Some(b"source".as_slice()),
                "{method} must preserve source"
            );
            assert_eq!(
                fs::read(dir.path().join("tree/sibling")).unwrap(),
                b"sibling"
            );
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
    }
}

#[tokio::test]
async fn review_move_into_descendant_preserves_existing_destination() {
    for destination in ["tree/child/", "%74ree//child"] {
        let (dir, service) = setup();
        fs::create_dir_all(dir.path().join("tree/child")).unwrap();
        fs::write(dir.path().join("tree/child/keep"), "keep").unwrap();
        let destination = format!("{PREFIX}{destination}");
        let (status, _, _) = request(
            &service,
            "MOVE",
            "tree/",
            &[("destination", &destination)],
            "",
        )
        .await;
        assert_eq!(
            fs::read(dir.path().join("tree/child/keep")).ok().as_deref(),
            Some(b"keep".as_slice()),
            "failed MOVE must not delete its destination first"
        );
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    // A shared textual prefix is not a shared path component.
    for method in ["COPY", "MOVE"] {
        let (dir, service) = setup();
        fs::create_dir(dir.path().join("tree")).unwrap();
        fs::write(dir.path().join("tree/file"), "keep").unwrap();
        let (status, _, _) = request(
            &service,
            method,
            "tree/",
            &[("destination", "/u/alice/dav/tree2/")],
            "",
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(fs::read(dir.path().join("tree2/file")).unwrap(), b"keep");
    }
}

#[tokio::test]
async fn review_malformed_etags_do_not_panic_or_modify_files() {
    let (dir, service) = setup();
    fs::write(dir.path().join("file"), "original").unwrap();
    for value in ["\"", "W/\""] {
        for header in ["if-match", "if-none-match", "if-range"] {
            let (status, _, _) = request(
                &service,
                "GET",
                "file",
                &[(header, value), ("range", "bytes=0-0")],
                "",
            )
            .await;
            assert!(status.is_success() || status.is_client_error());
            if header == "if-match" {
                let (status, _, _) =
                    request(&service, "PUT", "file", &[(header, value)], "replacement").await;
                assert_eq!(status, StatusCode::PRECONDITION_FAILED);
            }
            assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"original");
        }
    }
    assert_eq!(
        request(&service, "GET", "file", &[], "").await.2,
        "original"
    );
}

#[tokio::test]
async fn review_conditional_etags_use_weak_only_for_if_none_match() {
    let (dir, service) = setup();
    fs::write(dir.path().join("file"), "original").unwrap();
    let (_, headers, _) = request(&service, "HEAD", "file", &[], "").await;
    let strong = headers["etag"].to_str().unwrap();
    let weak = format!("W/{strong}");
    for method in ["GET", "HEAD", "PUT"] {
        let (status, _, _) = request(
            &service,
            method,
            "file",
            &[("if-none-match", &weak)],
            if method == "PUT" { "replacement" } else { "" },
        )
        .await;
        assert_eq!(
            status,
            if method == "PUT" {
                StatusCode::PRECONDITION_FAILED
            } else {
                StatusCode::NOT_MODIFIED
            }
        );
        assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"original");
    }
    assert_eq!(
        request(
            &service,
            "PUT",
            "file",
            &[("if-match", &weak)],
            "replacement"
        )
        .await
        .0,
        StatusCode::PRECONDITION_FAILED
    );
    let (status, _, body) = request(
        &service,
        "GET",
        "file",
        &[("if-range", &weak), ("range", "bytes=0-0")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "original");
    for header in ["if-match", "if-range"] {
        let (status, _, body) = request(
            &service,
            "GET",
            "file",
            &[(header, strong), ("range", "bytes=0-0")],
            "",
        )
        .await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(body, "o");
    }
}

fn setup() -> (tempfile::TempDir, Service) {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::new(
        dir.path(),
        "spc_test",
        "instance_test",
        PREFIX,
        Duration::from_secs(2),
    )
    .unwrap();
    (dir, service)
}
async fn request(
    service: &Service,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    data: &str,
) -> (StatusCode, http::HeaderMap, Bytes) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("{PREFIX}{path}"));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let result = service
        .handle(
            req.body(Full::new(Bytes::copy_from_slice(data.as_bytes())))
                .unwrap(),
            CancellationToken::new(),
        )
        .await;
    let (parts, body) = result.into_parts();
    (
        parts.status,
        parts.headers,
        body.collect().await.unwrap().to_bytes(),
    )
}
fn staged(dir: &std::path::Path) -> usize {
    fs::read_dir(dir.join(".agent-space/uploads/instance_test"))
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().file_name() != "boot-id")
        .count()
}
#[tokio::test]
async fn atomic_put_and_protocol_operations() {
    let (dir, service) = setup();
    fs::write(dir.path().join("report.txt"), "old").unwrap();
    fs::set_permissions(
        dir.path().join("report.txt"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let (status, headers, _) = request(&service, "PUT", "report.txt", &[], "new version").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        fs::read(dir.path().join("report.txt")).unwrap(),
        b"new version"
    );
    assert_eq!(
        fs::metadata(dir.path().join("report.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(staged(dir.path()), 0);
    let (_, get_headers, data) =
        request(&service, "GET", "report.txt", &[("range", "bytes=4-6")], "").await;
    assert_eq!(data, "ver");
    assert_eq!(headers.get("etag"), get_headers.get("etag"));
    assert_eq!(get_headers["content-disposition"], "attachment");
    assert_eq!(
        get_headers["content-security-policy"],
        "sandbox allow-downloads"
    );
    assert_eq!(
        request(
            &service,
            "PUT",
            "report.txt",
            &[("if-none-match", "*")],
            "bad"
        )
        .await
        .0,
        StatusCode::PRECONDITION_FAILED
    );
    assert_eq!(
        request(&service, "MKCOL", "folder", &[], "").await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(
            &service,
            "COPY",
            "report.txt",
            &[("destination", "/u/alice/dav/folder/copy.txt")],
            ""
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        request(
            &service,
            "MOVE",
            "folder/copy.txt",
            &[("destination", "/u/alice/dav/moved.txt")],
            ""
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        fs::read(dir.path().join("moved.txt")).unwrap(),
        b"new version"
    );
    assert_eq!(
        request(&service, "DELETE", "moved.txt", &[], "").await.0,
        StatusCode::NO_CONTENT
    );
}
#[tokio::test]
async fn incomplete_or_cancelled_upload_keeps_old_file() {
    let (dir, service) = setup();
    fs::write(dir.path().join("file"), "sentinel").unwrap();
    for length in ["2", "100"] {
        let status = request(
            &service,
            "PUT",
            "file",
            &[("content-length", length)],
            "partial",
        )
        .await
        .0;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"sentinel");
        assert_eq!(staged(dir.path()), 0);
    }
    let body = StreamBody::new(futures::stream::iter(vec![
        Ok(Frame::data(Bytes::from_static(b"partial"))),
        Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "client disconnected",
        )),
    ]));
    let result = service
        .handle(
            Request::put(format!("{PREFIX}file")).body(body).unwrap(),
            CancellationToken::new(),
        )
        .await;
    assert!(!result.status().is_success());
    drop(result);
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"sentinel");
    assert_eq!(staged(dir.path()), 0);
    let cancel = CancellationToken::new();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let body = StreamBody::new(futures::stream::once(async move {
        let _ = tx.send(());
        futures::future::pending::<Result<Frame<Bytes>, io::Error>>().await
    }));
    let future = service.handle(
        Request::put(format!("{PREFIX}file")).body(body).unwrap(),
        cancel.clone(),
    );
    let (result, ()) = tokio::join!(future, async {
        rx.await.unwrap();
        cancel.cancel();
    });
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(result);
    assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"sentinel");
    assert_eq!(staged(dir.path()), 0);
}
#[tokio::test]
async fn symlinks_special_files_and_private_area_are_inaccessible() {
    let (dir, service) = setup();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), "secret").unwrap();
    symlink(outside.path(), dir.path().join("escape")).unwrap();
    symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
    fs::write(dir.path().join("source"), "safe").unwrap();
    let fifo =
        std::ffi::CString::new(dir.path().join("fifo").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    for path in [
        "escape/secret",
        "link",
        "fifo",
        ".agent-space/uploads/owner",
        "%2e%2e/secret",
        "escape%2fsecret",
    ] {
        for method in ["GET", "HEAD", "PUT", "DELETE", "MKCOL", "PROPFIND", "LOCK"] {
            let status = request(
                &service,
                method,
                path,
                &[("depth", "0")],
                if method == "PUT" { "evil" } else { "" },
            )
            .await
            .0;
            assert!(!status.is_success(), "{method} {path}: {status}");
        }
        let destination = format!("{PREFIX}{path}");
        for method in ["COPY", "MOVE"] {
            assert!(!request(
                &service,
                method,
                "source",
                &[("destination", &destination)],
                ""
            )
            .await
            .0
            .is_success());
            assert_eq!(fs::read(dir.path().join("source")).unwrap(), b"safe");
        }
    }
    assert_eq!(fs::read(outside.path().join("secret")).unwrap(), b"secret");
    let (_, _, listing) = request(&service, "PROPFIND", "", &[("depth", "1")], "").await;
    let listing = String::from_utf8(listing.to_vec()).unwrap();
    assert!(!listing.contains(".agent-space-upload"));
    assert!(!listing.contains("escape"));
    assert_eq!(
        request(&service, "DELETE", "", &[], "").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&service, "PROPFIND", "", &[], "").await.0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn lock_conflicts_refresh_and_release() {
    let (_dir, service) = setup();
    let xml = r#"<?xml version="1.0"?><D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockinfo>"#;
    let (status, headers, body) =
        request(&service, "LOCK", "locked", &[("timeout", "Infinite")], xml).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let token = headers["lock-token"].to_str().unwrap();
    assert_eq!(
        request(&service, "PUT", "locked", &[], "fail").await.0,
        StatusCode::LOCKED
    );
    let condition = format!("({token})");
    assert_eq!(
        request(&service, "PUT", "locked", &[("if", &condition)], "ok")
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (refresh_status, _, body) = request(
        &service,
        "LOCK",
        "locked",
        &[("if", &condition), ("timeout", "Infinite")],
        "",
    )
    .await;
    assert_eq!(refresh_status, StatusCode::OK);
    let xml = String::from_utf8_lossy(&body);
    let timeout: u64 = xml
        .split("Second-")
        .nth(1)
        .unwrap()
        .split('<')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(timeout > 0 && timeout <= 1800);
    assert_eq!(
        request(&service, "UNLOCK", "locked", &[("lock-token", token)], "")
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&service, "PUT", "locked", &[], "unlocked").await.0,
        StatusCode::NO_CONTENT
    );
}
#[test]
fn existing_unowned_staging_directory_is_never_adopted() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".agent-space/uploads")).unwrap();
    fs::write(dir.path().join(".agent-space/uploads/user-file"), "keep").unwrap();
    assert!(Service::new(
        dir.path(),
        "spc_test",
        "instance_test",
        PREFIX,
        Duration::from_secs(2)
    )
    .is_err());
    assert_eq!(
        fs::read(dir.path().join(".agent-space/uploads/user-file")).unwrap(),
        b"keep"
    );
}

#[tokio::test]
async fn external_write_and_cancelled_waiter_do_not_replace_target() {
    let (dir, service) = setup();
    let target = dir.path().join("race");
    fs::write(&target, "original").unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    let stream = futures::stream::once(async move {
        let _ = started_tx.send(());
        resume_rx.await.unwrap();
        Ok::<_, io::Error>(Frame::data(Bytes::from_static(b"upload")))
    });
    let uploading = service.handle(
        Request::put(format!("{PREFIX}race"))
            .body(StreamBody::new(stream))
            .unwrap(),
        CancellationToken::new(),
    );
    let (result, ()) = tokio::join!(uploading, async {
        started_rx.await.unwrap();
        let (status, _, body) = request(&service, "GET", "race", &[], "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "original");
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            service
                .handle(
                    Request::put(format!("{PREFIX}race"))
                        .body(Full::new(Bytes::from_static(b"cancelled")))
                        .unwrap(),
                    cancel
                )
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        // Different length makes this deterministic even on coarse timestamp filesystems.
        fs::write(&target, "external-new-content").unwrap();
        resume_tx.send(()).unwrap();
    });
    assert_eq!(result.status(), StatusCode::PRECONDITION_FAILED);
    drop(result);
    assert_eq!(fs::read(target).unwrap(), b"external-new-content");
    assert_eq!(staged(dir.path()), 0);
}

#[tokio::test]
async fn large_file_streams_without_preview_body_limit() {
    use sha2::{Digest, Sha256};
    let (dir, service) = setup();
    let chunk = Bytes::from(vec![0x63; 64 * 1024]);
    let expected = {
        let mut digest = Sha256::new();
        for _ in 0..1024 {
            digest.update(&chunk);
        }
        digest.finalize()
    };
    let stream = futures::stream::iter(
        (0..1024).map(move |_| Ok::<_, io::Error>(Frame::data(chunk.clone()))),
    );
    let result = service
        .handle(
            Request::put(format!("{PREFIX}large"))
                .body(StreamBody::new(stream))
                .unwrap(),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(result.status(), StatusCode::CREATED);
    drop(result);
    assert_eq!(
        fs::metadata(dir.path().join("large")).unwrap().len(),
        64 * 1024 * 1024
    );
    let result = service
        .handle(
            Request::get(format!("{PREFIX}large"))
                .body(Full::new(Bytes::new()))
                .unwrap(),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(result.status(), StatusCode::OK);
    let mut body = result.into_body();
    let mut digest = Sha256::new();
    while let Some(frame) = body.frame().await {
        if let Ok(bytes) = frame.unwrap().into_data() {
            digest.update(bytes);
        }
    }
    assert_eq!(digest.finalize(), expected);
    assert_eq!(staged(dir.path()), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_directory_symlink_swaps_never_read_outside_root() {
    let (dir, service) = setup();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), "outside-secret").unwrap();
    fs::create_dir(dir.path().join("swap")).unwrap();
    fs::write(dir.path().join("swap/secret"), "inside").unwrap();
    let root = dir.path().to_owned();
    let ext = outside.path().to_owned();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ended = stop.clone();
    let worker = std::thread::spawn(move || {
        while !ended.load(std::sync::atomic::Ordering::Relaxed) {
            fs::rename(root.join("swap"), root.join("held")).unwrap();
            symlink(&ext, root.join("swap")).unwrap();
            std::thread::yield_now();
            fs::remove_file(root.join("swap")).unwrap();
            fs::rename(root.join("held"), root.join("swap")).unwrap();
        }
    });
    let mut leaked = false;
    for _ in 0..200 {
        let (_, _, data) = request(&service, "GET", "swap/secret", &[], "").await;
        if data == "outside-secret" {
            leaked = true;
            break;
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    worker.join().unwrap();
    assert!(!leaked);
}

#[test]
fn staging_recovery_requires_a_different_boot_and_owned_regular_entries() {
    let (dir, _service) = setup();
    let stage = dir.path().join(".agent-space/uploads");
    let old = stage.join(uuid::Uuid::new_v4().to_string());
    let same = stage.join(uuid::Uuid::new_v4().to_string());
    let unknown = stage.join(uuid::Uuid::new_v4().to_string());
    for path in [&old, &same, &unknown] {
        fs::create_dir(path).unwrap();
    }
    let name = uuid::Uuid::new_v4().to_string();
    fs::write(old.join("boot-id"), format!("{}\n", uuid::Uuid::new_v4())).unwrap();
    fs::write(
        same.join("boot-id"),
        fs::read("/proc/sys/kernel/random/boot_id").unwrap(),
    )
    .unwrap();
    for path in [&old, &same, &unknown] {
        fs::write(path.join(&name), "partial").unwrap();
    }
    agent_space_webdav::fs::RootFs::new(dir.path(), "spc_test", "next").unwrap();
    assert!(!old.exists());
    assert_eq!(fs::read(same.join(&name)).unwrap(), b"partial");
    assert_eq!(fs::read(unknown.join(&name)).unwrap(), b"partial");
    let bad = stage.join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join("boot-id"), format!("{}\n", uuid::Uuid::new_v4())).unwrap();
    let sentinel = dir.path().join("sentinel");
    fs::write(&sentinel, "keep").unwrap();
    symlink(&sentinel, bad.join(uuid::Uuid::new_v4().to_string())).unwrap();
    assert!(agent_space_webdav::fs::RootFs::new(dir.path(), "spc_test", "refused").is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
    assert!(bad.join("boot-id").is_file());
}

#[tokio::test]
async fn recursive_failures_are_reported_and_hardlink_copy_preserves_source() {
    let (dir, service) = setup();
    fs::create_dir_all(dir.path().join("tree/good")).unwrap();
    fs::create_dir_all(dir.path().join("tree/blocked")).unwrap();
    fs::write(dir.path().join("tree/good/file"), "safe").unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("sentinel"), "outside").unwrap();
    symlink(
        outside.path().join("sentinel"),
        dir.path().join("tree/blocked/link"),
    )
    .unwrap();
    let (status, _, body) = request(
        &service,
        "COPY",
        "tree/",
        &[("destination", "/u/alice/dav/copied/")],
        "",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::MULTI_STATUS,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let xml = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        xml.contains("403 Forbidden") && xml.contains("tree/blocked"),
        "{xml}"
    );
    assert_eq!(
        fs::read(dir.path().join("copied/good/file")).unwrap(),
        b"safe"
    );
    assert!(!dir.path().join("copied/blocked/link").exists());
    let (status, _, body) = request(&service, "DELETE", "tree/", &[], "").await;
    assert_eq!(
        status,
        StatusCode::MULTI_STATUS,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(!dir.path().join("tree/good").exists());
    assert!(dir.path().join("tree/blocked/link").is_symlink());
    assert_eq!(
        fs::read(outside.path().join("sentinel")).unwrap(),
        b"outside"
    );
    fs::write(dir.path().join("source"), "do not truncate").unwrap();
    fs::hard_link(dir.path().join("source"), dir.path().join("alias")).unwrap();
    // Exercise the adapter's actual overwrite path too: a hardlink alias must
    // never cause truncation of the source inode.
    use dav_server::{davpath::DavPath, fs::DavFileSystem};
    let adapter = agent_space_webdav::fs::RootFs::new(dir.path(), "spc_test", "copy_test").unwrap();
    assert!(adapter
        .copy(
            &DavPath::new("/source").unwrap(),
            &DavPath::new("/alias").unwrap()
        )
        .await
        .is_err());
    assert_eq!(
        fs::read(dir.path().join("source")).unwrap(),
        b"do not truncate"
    );
    let (status, _, _) = request(
        &service,
        "COPY",
        "source",
        &[("destination", "/u/alice/dav/alias")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        fs::read(dir.path().join("source")).unwrap(),
        b"do not truncate"
    );
}

#[tokio::test]
async fn conditional_writes_ranges_and_overwrite_keep_preconditions() {
    let (dir, service) = setup();
    fs::write(dir.path().join("file"), "original").unwrap();
    let (_, headers, _) = request(&service, "HEAD", "file", &[], "").await;
    let original = headers["etag"].to_str().unwrap();
    for headers in [
        vec![("if-match", "\"wrong\"")],
        vec![("if-unmodified-since", "Sat, 01 Jan 2000 00:00:00 GMT")],
    ] {
        assert_eq!(
            request(&service, "PUT", "file", &headers, "bad").await.0,
            StatusCode::PRECONDITION_FAILED
        );
        assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"original");
        assert_eq!(staged(dir.path()), 0);
    }
    assert_eq!(
        request(
            &service,
            "PUT",
            "file",
            &[("if-match", original)],
            "updated"
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&service, "PUT", "file", &[("if-match", original)], "stale")
            .await
            .0,
        StatusCode::PRECONDITION_FAILED
    );
    let (_, headers, _) = request(&service, "HEAD", "file", &[], "").await;
    let current = headers["etag"].to_str().unwrap();
    assert_ne!(current, original);
    let (status, _, body) =
        request(&service, "GET", "file", &[("if-none-match", current)], "").await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
    let (status, _, body) = request(
        &service,
        "GET",
        "file",
        &[("range", "bytes=1-3"), ("if-range", current)],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, "pda");
    let (status, _, body) = request(
        &service,
        "GET",
        "file",
        &[("range", "bytes=1-3"), ("if-range", original)],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "updated");
    fs::write(dir.path().join("destination"), "keep destination").unwrap();
    for method in ["COPY", "MOVE"] {
        for condition in [("overwrite", "F"), ("if-match", "\"wrong\"")] {
            let (status, _, _) = request(
                &service,
                method,
                "file",
                &[("destination", "/u/alice/dav/destination"), condition],
                "",
            )
            .await;
            assert_eq!(status, StatusCode::PRECONDITION_FAILED);
            assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"updated");
            assert_eq!(
                fs::read(dir.path().join("destination")).unwrap(),
                b"keep destination"
            );
        }
    }
}

#[tokio::test]
async fn modification_time_is_applied_atomically_and_properties_do_not_lie() {
    use std::os::unix::fs::MetadataExt;
    let (dir, service) = setup();
    let (status, headers, _) = request(
        &service,
        "PUT",
        "dated",
        &[("x-oc-mtime", "946684800")],
        "original",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(headers["x-oc-mtime"], "accepted");
    assert_eq!(
        fs::metadata(dir.path().join("dated")).unwrap().mtime(),
        946684800
    );
    for invalid in ["-1", "NaN", "253402300800", "1,2"] {
        assert_eq!(
            request(&service, "PUT", "dated", &[("x-oc-mtime", invalid)], "bad")
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(fs::read(dir.path().join("dated")).unwrap(), b"original");
    let xml = |properties: &str| {
        format!(
            r#"<D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:schemas-microsoft-com:" xmlns:oc="http://owncloud.org/ns"><D:set><D:prop>{properties}</D:prop></D:set></D:propertyupdate>"#
        )
    };
    for property in [
        "<D:getlastmodified>Sun, 01 Jan 2023 00:00:00 GMT</D:getlastmodified>",
        "<Z:Win32LastModifiedTime>Sun, 01 Jan 2023 00:00:00 GMT</Z:Win32LastModifiedTime>",
        "<oc:lastmodified>1672531200</oc:lastmodified>",
    ] {
        let (status, _, body) = request(&service, "PROPPATCH", "dated", &[], &xml(property)).await;
        assert_eq!(status, StatusCode::MULTI_STATUS);
        assert!(
            String::from_utf8_lossy(&body).contains("200 OK"),
            "{body:?}"
        );
        assert_eq!(
            fs::metadata(dir.path().join("dated")).unwrap().mtime(),
            1672531200
        );
    }
    let old = fs::metadata(dir.path().join("dated")).unwrap();
    let (_,_,body)=request(&service,"PROPPATCH","dated",&[],&xml("<D:getlastmodified>Sat, 01 Jan 2000 00:00:00 GMT</D:getlastmodified><Z:Win32FileAttributes>32</Z:Win32FileAttributes>")).await;
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("403 Forbidden") && body.contains("424 Failed Dependency"),
        "{body}"
    );
    assert_eq!(
        fs::metadata(dir.path().join("dated")).unwrap().mtime(),
        old.mtime()
    );
    assert_eq!(
        request(
            &service,
            "PROPPATCH",
            "dated",
            &[("if-match", "\"stale\"")],
            &xml("<oc:lastmodified>946684800</oc:lastmodified>")
        )
        .await
        .0,
        StatusCode::PRECONDITION_FAILED
    );
    let (_, headers, _) = request(&service, "HEAD", "dated", &[], "").await;
    assert_eq!(headers["last-modified"], "Sun, 01 Jan 2023 00:00:00 GMT");
    assert_eq!(
        request(
            &service,
            "COPY",
            "dated",
            &[("destination", "/u/alice/dav/copied-date")],
            ""
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        fs::metadata(dir.path().join("copied-date"))
            .unwrap()
            .mtime(),
        1672531200
    );
}

#[tokio::test]
async fn quota_properties_share_the_volume_accounting_at_root_and_children() {
    let (dir, service) = setup();
    fs::write(dir.path().join("file"), "data").unwrap();
    fs::create_dir(dir.path().join("dir")).unwrap();
    let xml = r#"<D:propfind xmlns:D="DAV:"><D:prop><D:quota-used-bytes/><D:quota-available-bytes/></D:prop></D:propfind>"#;
    let (status, _, body) = request(&service, "PROPFIND", "", &[("depth", "1")], xml).await;
    assert_eq!(status, StatusCode::MULTI_STATUS);
    let tree = xmltree::Element::parse(body.as_ref()).unwrap();
    let mut values = Vec::new();
    for child in &tree.children {
        let Some(response) = child.as_element() else {
            continue;
        };
        let stat = response.get_child("propstat").unwrap();
        assert!(stat
            .get_child("status")
            .unwrap()
            .get_text()
            .unwrap()
            .contains("200 OK"));
        let props = stat.get_child("prop").unwrap();
        let used: u64 = props
            .get_child("quota-used-bytes")
            .unwrap()
            .get_text()
            .unwrap()
            .parse()
            .unwrap();
        let free: u64 = props
            .get_child("quota-available-bytes")
            .unwrap()
            .get_text()
            .unwrap()
            .parse()
            .unwrap();
        assert!(used > 0 && free > 0);
        values.push((used, free));
    }
    assert_eq!(values.len(), 4); // root, file, dir and the visible .agent-space directory
    assert!(values.iter().all(|v| *v == values[0]));
}

#[tokio::test]
async fn unrelated_paths_progress_while_encoded_alias_and_parent_mutations_wait() {
    let (dir, service) = setup();
    fs::create_dir(dir.path().join("dir")).unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let stream = futures::stream::once(async move {
        started_tx.send(()).unwrap();
        release_rx.await.unwrap();
        Ok::<_, io::Error>(Frame::data(Bytes::from_static(b"new")))
    });
    let upload = service.handle(
        Request::put(format!("{PREFIX}dir/file"))
            .body(StreamBody::new(stream))
            .unwrap(),
        CancellationToken::new(),
    );
    let (res, ()) = tokio::join!(upload, async {
        started_rx.await.unwrap();
        for (method, path, body) in [
            ("PUT", "other", "independent"),
            ("MKCOL", "otherdir", ""),
            ("PUT", "dir/sibling", "sibling"),
        ] {
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                request(&service, method, path, &[], body),
            )
            .await
            .expect("unrelated request must finish before releasing upload");
            assert!(result.0.is_success());
        }
        // Poll to pending after reservation; cancellation must remove each waiter.
        for (method, path) in [("DELETE", "dir/"), ("PUT", "dir/%66ile")] {
            let token = CancellationToken::new();
            let blocked = service.handle(
                Request::builder()
                    .method(method)
                    .uri(format!("{PREFIX}{path}"))
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
                token.clone(),
            );
            tokio::pin!(blocked);
            assert!(futures::poll!(&mut blocked).is_pending());
            token.cancel();
            assert_eq!(blocked.await.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        release_tx.send(()).unwrap();
    });
    assert_eq!(res.status(), StatusCode::CREATED);
    drop(res);
    assert_eq!(fs::read(dir.path().join("dir/file")).unwrap(), b"new");
    assert_eq!(
        request(&service, "DELETE", "dir/", &[], "").await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn single_file_overwrite_keeps_target_until_adapter_success() {
    use dav_server::{
        davpath::DavPath,
        fs::{DavFileSystem, DavMetaData, FsError, FsFuture},
        DavHandler,
    };
    #[derive(Clone)]
    struct FailingTransfer(agent_space_webdav::fs::RootFs);
    impl DavFileSystem for FailingTransfer {
        fn open<'a>(
            &'a self,
            path: &'a DavPath,
            options: dav_server::fs::OpenOptions,
        ) -> FsFuture<'a, Box<dyn dav_server::fs::DavFile>> {
            self.0.open(path, options)
        }
        fn read_dir<'a>(
            &'a self,
            path: &'a DavPath,
            meta: dav_server::fs::ReadDirMeta,
        ) -> FsFuture<'a, dav_server::fs::FsStream<Box<dyn dav_server::fs::DavDirEntry>>> {
            self.0.read_dir(path, meta)
        }
        fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
            self.0.metadata(path)
        }
        fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            self.0.remove_file(path)
        }
        fn copy<'a>(&'a self, _: &'a DavPath, _: &'a DavPath) -> FsFuture<'a, ()> {
            Box::pin(async { Err(FsError::InsufficientStorage) })
        }
        fn rename<'a>(&'a self, _: &'a DavPath, _: &'a DavPath) -> FsFuture<'a, ()> {
            Box::pin(async { Err(FsError::GeneralFailure) })
        }
    }
    let (dir, service) = setup();
    fs::write(dir.path().join("source"), "new-source").unwrap();
    fs::write(dir.path().join("target"), "old-target").unwrap();
    let adapter =
        agent_space_webdav::fs::RootFs::new(dir.path(), "spc_test", "failure_test").unwrap();
    let handler = DavHandler::builder()
        .filesystem(Box::new(FailingTransfer(adapter)))
        .strip_prefix(PREFIX)
        .build_handler();
    for (method, expected) in [
        ("COPY", StatusCode::INSUFFICIENT_STORAGE),
        ("MOVE", StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let response = handler
            .handle(
                Request::builder()
                    .method(method)
                    .uri(format!("{PREFIX}source"))
                    .header("destination", format!("{PREFIX}target"))
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), expected);
        response.into_body().collect().await.unwrap();
        assert_eq!(fs::read(dir.path().join("target")).unwrap(), b"old-target");
        assert_eq!(fs::read(dir.path().join("source")).unwrap(), b"new-source");
    }
    for method in ["COPY", "MOVE"] {
        assert_eq!(
            request(
                &service,
                method,
                "source",
                &[("destination", "/u/alice/dav/target"), ("overwrite", "F")],
                ""
            )
            .await
            .0,
            StatusCode::PRECONDITION_FAILED
        );
        assert_eq!(
            request(
                &service,
                method,
                "source",
                &[("destination", "/u/alice/dav/target")],
                ""
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(fs::read(dir.path().join("target")).unwrap(), b"new-source");
    }
    assert!(!dir.path().join("source").exists());
    assert_eq!(staged(dir.path()), 0);
    fs::create_dir(dir.path().join("directory")).unwrap();
    fs::write(dir.path().join("directory/keep"), "keep").unwrap();
    for method in ["COPY", "MOVE"] {
        assert_eq!(
            request(
                &service,
                method,
                "target",
                &[("destination", "/u/alice/dav/directory")],
                ""
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            fs::read(dir.path().join("directory/keep")).unwrap(),
            b"keep"
        );
    }
}

#[tokio::test]
async fn root_aliases_cannot_delete_move_or_replace_workspace_contents() {
    let (dir, service) = setup();
    fs::create_dir(dir.path().join("source")).unwrap();
    fs::write(dir.path().join("source/file"), "source").unwrap();
    fs::write(dir.path().join("sentinel"), "keep").unwrap();
    for suffix in ["", "/", "//"] {
        for method in ["DELETE", "MOVE"] {
            let status = request(
                &service,
                method,
                suffix,
                &[("destination", "/u/alice/dav/destination")],
                "",
            )
            .await
            .0;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} root {suffix:?}");
            assert_eq!(fs::read(dir.path().join("sentinel")).unwrap(), b"keep");
            assert_eq!(fs::read(dir.path().join("source/file")).unwrap(), b"source");
        }
        for method in ["COPY", "MOVE"] {
            let destination = format!("{PREFIX}{suffix}");
            let status = request(
                &service,
                method,
                "source/",
                &[("destination", &destination)],
                "",
            )
            .await
            .0;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} destination {suffix:?}"
            );
            assert_eq!(fs::read(dir.path().join("sentinel")).unwrap(), b"keep");
            assert_eq!(fs::read(dir.path().join("source/file")).unwrap(), b"source");
        }
    }
}

#[tokio::test]
async fn only_exact_upload_staging_is_private_other_internal_and_similar_paths_work() {
    let (dir, service) = setup();
    for method in ["GET", "PUT", "PROPFIND", "DELETE"] {
        assert!(
            !request(&service, method, ".agent-space/uploads/owner", &[], "x")
                .await
                .0
                .is_success()
        );
    }
    for folder in [
        ".agent-space/notes",
        ".agent-space/uploads-backup",
        ".agent-space-backup",
        "project/.agent-space/uploads",
        ".agent-space-upload",
    ] {
        fs::create_dir_all(dir.path().join(folder)).unwrap();
        let path = format!("{folder}/file.txt");
        assert!(
            request(&service, "PUT", &path, &[], "ordinary")
                .await
                .0
                .is_success(),
            "{path}"
        );
        assert_eq!(
            request(&service, "GET", &path, &[], "").await.2.as_ref(),
            b"ordinary"
        );
    }
    let (_, _, listing) =
        request(&service, "PROPFIND", ".agent-space/", &[("depth", "1")], "").await;
    let listing = String::from_utf8(listing.to_vec()).unwrap();
    assert!(listing.contains("notes"));
    assert!(!listing.contains("/.agent-space/uploads/"));
    let (_, _, listing) = request(&service, "PROPFIND", "", &[("depth", "1")], "").await;
    assert!(String::from_utf8(listing.to_vec())
        .unwrap()
        .contains(".agent-space/"));
    let (_, _, listing) = request(
        &service,
        "PROPFIND",
        "project/.agent-space/",
        &[("depth", "1")],
        "",
    )
    .await;
    assert!(String::from_utf8(listing.to_vec())
        .unwrap()
        .contains("/uploads"));
    assert!(!request(
        &service,
        "MOVE",
        ".agent-space",
        &[("destination", &format!("{PREFIX}renamed"))],
        ""
    )
    .await
    .0
    .is_success());
}

#[test]
fn legacy_owned_upload_staging_moves_once_without_overwriting_user_directories() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join(".agent-space-upload");
    fs::create_dir(&old).unwrap();
    fs::write(old.join("owner"), "agent-space-upload-v1:spc_test\n").unwrap();
    agent_space_webdav::fs::RootFs::new(dir.path(), "spc_test", "first").unwrap();
    assert!(!old.exists());
    assert!(dir.path().join(".agent-space/uploads/owner").is_file());
    fs::create_dir(&old).unwrap();
    fs::write(old.join("user"), "keep").unwrap();
    agent_space_webdav::fs::RootFs::new(dir.path(), "spc_test", "second").unwrap();
    assert_eq!(fs::read(old.join("user")).unwrap(), b"keep");
}

#[tokio::test]
async fn forbidden_upload_ancestor_move_never_deletes_source_or_destination() {
    for internal in [".agent-space", "%2Eagent-space//"] {
        for destination_kind in ["file", "directory"] {
            let (dir, service) = setup();
            fs::write(dir.path().join(".agent-space/notes"), "internal-data").unwrap();
            let keep = if destination_kind == "directory" {
                fs::create_dir(dir.path().join("target")).unwrap();
                dir.path().join("target/keep")
            } else {
                dir.path().join("target")
            };
            fs::write(&keep, "user-data").unwrap();
            let destination = format!("{PREFIX}target");
            let (status, _, _) = request(
                &service,
                "MOVE",
                internal,
                &[("destination", &destination)],
                "",
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(fs::read(&keep).unwrap(), b"user-data");
            assert_eq!(
                fs::read(dir.path().join(".agent-space/notes")).unwrap(),
                b"internal-data"
            );

            fs::create_dir(dir.path().join("source")).unwrap();
            fs::write(dir.path().join("source/keep"), "source-data").unwrap();
            let destination = format!("{PREFIX}{internal}");
            let (status, _, _) = request(
                &service,
                "MOVE",
                "source",
                &[("destination", &destination)],
                "",
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_eq!(
                fs::read(dir.path().join("source/keep")).unwrap(),
                b"source-data"
            );
            assert_eq!(
                fs::read(dir.path().join(".agent-space/notes")).unwrap(),
                b"internal-data"
            );
        }
    }
}
