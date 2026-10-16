#[path = "../../build-support/bounded_log.rs"]
mod bounded_log;
#[path = "../../build-support/data_process.rs"]
mod data_process;
use percent_encoding::percent_decode_str;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--build-id") {
        println!("{}", env!("AGENT_SPACE_HELPER_BUILD_ID"));
        return;
    }
    data_process::supervise().expect("data-plane supervisor");
    let mode = std::env::args().nth(1).expect("file or directory mode");
    let path = PathBuf::from(std::env::args().nth(2).expect("absolute target path"));
    let root = std::sync::Arc::new(Target::open(&mode, &path).expect("valid published target"));
    bounded_log::install().expect("managed log");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    println!("READY {}", listener.local_addr().expect("addr").port());
    let _ = std::io::stdout().flush();
    accept_loop(listener, root, 32);
}

fn accept_loop(listener: TcpListener, root: std::sync::Arc<Target>, limit: usize) {
    // Keep the published directory inode as the boundary even if its path is
    // renamed or replaced while this helper is running.
    let active = std::sync::Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        if active.load(Ordering::SeqCst) >= limit {
            let _ = write_response(&mut { stream }, 503, "text/plain", b"busy", false);
            continue;
        }
        active.fetch_add(1, Ordering::SeqCst);
        let root = root.clone();
        let active = active.clone();
        std::thread::spawn(move || {
            let _ = serve_root(&root, stream);
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

struct Target {
    root: File,
    file: Option<String>,
}
impl Target {
    fn open(mode: &str, path: &Path) -> std::io::Result<Self> {
        let invalid = || std::io::Error::other("target type or path is invalid");
        match mode {
            "directory" => {
                let root = File::open(fs::canonicalize(path)?)?;
                if !root.metadata()?.is_dir() {
                    return Err(invalid());
                }
                Ok(Self { root, file: None })
            }
            "file" => {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(invalid)?
                    .to_owned();
                let root = File::open(fs::canonicalize(path.parent().ok_or_else(invalid)?)?)?;
                let file = open_entry(&root, &name).map_err(|_| invalid())?;
                if !file.metadata()?.is_file() {
                    return Err(invalid());
                }
                Ok(Self {
                    root,
                    file: Some(name),
                })
            }
            _ => Err(invalid()),
        }
    }

    fn resolve(&self, path: &str) -> Result<Opened, u16> {
        match &self.file {
            Some(name) if path == "/" => readable_file(open_entry(&self.root, name)?),
            Some(_) => Err(404),
            None => open_within(&self.root, path),
        }
    }
}
#[cfg(test)]
fn serve(root: &Path, stream: TcpStream) -> std::io::Result<()> {
    serve_root(&Target::open("directory", root)?, stream)
}

fn serve_root(published: &Target, mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(120)))?;
    let mut buffer = [0u8; 8192];
    let mut count = 0;
    while count < buffer.len() && !buffer[..count].windows(4).any(|part| part == b"\r\n\r\n") {
        let read = stream.read(&mut buffer[count..])?;
        if read == 0 {
            break;
        }
        count += read;
    }
    if !buffer[..count].windows(4).any(|part| part == b"\r\n\r\n") {
        return write_response(&mut stream, 400, "text/plain", b"bad request", false);
    }
    let request = match std::str::from_utf8(&buffer[..count]) {
        Ok(request) => request,
        Err(_) => return write_response(&mut stream, 400, "text/plain", b"bad request", false),
    };
    let line = request.split("\r\n").next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if parts.next().is_some()
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || !target.starts_with('/')
    {
        return write_response(&mut stream, 400, "text/plain", b"bad request", false);
    }
    if !matches!(method, "GET" | "HEAD") {
        return write_response(
            &mut stream,
            405,
            "text/plain",
            b"method not allowed",
            method == "HEAD",
        );
    }
    let (path, query) = target
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((target, None));
    let ranges: Vec<_> = request
        .split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("range"))
        .map(|(_, value)| value.trim())
        .collect();
    let opened = published.resolve(path);
    match opened {
        Ok(Opened::Redirect(location)) => {
            let location = match query {
                Some(query) => format!("{location}?{query}"),
                None => location,
            };
            write_redirect(&mut stream, &location)
        }
        Ok(Opened::File(mut file)) => {
            let range = if method == "HEAD" || ranges.is_empty() {
                None
            } else if ranges.len() == 1 {
                Some(ranges[0])
            } else {
                Some("invalid")
            };
            write_file(&mut stream, &mut file, method == "HEAD", range)
        }
        Ok(Opened::Directory(directory)) => {
            let body = directory_listing(&directory, path)?;
            write_response(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                body.as_bytes(),
                method == "HEAD",
            )
        }
        Err(status) => write_response(
            &mut stream,
            status,
            "text/plain",
            b"not found",
            method == "HEAD",
        ),
    }
}

enum Opened {
    File(File),
    Directory(File),
    Redirect(String),
}
fn open_within(root: &File, url_path: &str) -> Result<Opened, u16> {
    let decoded = percent_decode_str(url_path)
        .decode_utf8()
        .map_err(|_| 404u16)?;
    if !decoded.starts_with('/') || decoded.contains(['\\', '\0']) {
        return Err(404);
    }
    let segments: Vec<&str> = decoded.split('/').filter(|part| !part.is_empty()).collect();
    if segments.iter().any(|part| matches!(*part, "." | "..")) {
        return Err(404);
    }
    let mut current = root.try_clone().map_err(|_| 404u16)?;
    for (index, segment) in segments.iter().enumerate() {
        let next = open_entry(&current, segment)?;
        let metadata = next.metadata().map_err(|_| 404u16)?;
        if index + 1 < segments.len() && !metadata.is_dir() {
            return Err(404);
        }
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(404);
        }
        current = next;
    }
    if current.metadata().map_err(|_| 404u16)?.is_dir() {
        if !url_path.ends_with('/') {
            // A Location beginning with // would navigate to another host.
            let path = segments
                .iter()
                .map(|part| encode_segment(part))
                .collect::<Vec<_>>()
                .join("/");
            return Ok(Opened::Redirect(format!("/{path}/")));
        }
        match open_entry(&current, "index.html") {
            Ok(index) => current = index,
            Err(_) => return Ok(Opened::Directory(current)),
        }
    }
    readable_file(current)
}

fn readable_file(current: File) -> Result<Opened, u16> {
    if !current.metadata().map_err(|_| 404u16)?.is_file() {
        return Err(404);
    }
    // The proc fd link names this already checked inode, so renames and
    // symlink swaps cannot redirect the final readable handle.
    File::open(format!("/proc/self/fd/{}", current.as_raw_fd()))
        .map(Opened::File)
        .map_err(|_| 404)
}

fn open_entry(parent: &File, name: &str) -> Result<File, u16> {
    let name = CString::new(name).map_err(|_| 404u16)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(404);
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if file
        .metadata()
        .map_err(|_| 404u16)?
        .file_type()
        .is_symlink()
    {
        return Err(404);
    }
    Ok(file)
}

fn fd_path(file: &File) -> Result<PathBuf, u16> {
    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(|_| 404)
}

fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("txt" | "log" | "md") => "text/plain; charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("wasm") => "application/wasm",
        Some("mp4") => "video/mp4",
        Some("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

fn write_redirect(stream: &mut TcpStream, location: &str) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

fn encode_segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}
fn html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn directory_listing(directory: &File, path: &str) -> std::io::Result<String> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // Do not advertise symlinks or special files that the reader denies.
        let Ok(file) = open_entry(directory, &name) else {
            continue;
        };
        let metadata = file.metadata()?;
        if !metadata.is_dir() && !metadata.is_file() {
            continue;
        }
        entries.push((name, metadata.is_dir()));
    }
    entries.sort();
    let mut body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{}</title><h1>{}</h1><ul>",
        html(path),
        html(path)
    );
    if path != "/" {
        body.push_str("<li><a href=\"../\">../</a></li>");
    }
    for (name, directory) in entries {
        let suffix = if directory { "/" } else { "" };
        body.push_str(&format!(
            "<li><a href=\"{}{suffix}\">{}{suffix}</a></li>",
            encode_segment(&name),
            html(&name)
        ));
    }
    body.push_str("</ul>");
    Ok(body)
}

fn byte_range(value: Option<&str>, length: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.strip_prefix("bytes=") else {
        return Err(());
    };
    // Multipart ranges are not implemented; an ordinary full response is allowed.
    if value.contains(',') {
        return Ok(None);
    }
    let (first, last) = value.split_once('-').ok_or(())?;
    if length == 0 {
        return Err(());
    }
    if first.is_empty() {
        let suffix = last.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        return Ok(Some((length.saturating_sub(suffix), length - 1)));
    }
    let first = first.parse::<u64>().map_err(|_| ())?;
    let last = if last.is_empty() {
        length - 1
    } else {
        last.parse::<u64>().map_err(|_| ())?.min(length - 1)
    };
    if first >= length || first > last {
        return Err(());
    }
    Ok(Some((first, last)))
}
fn write_file(
    stream: &mut TcpStream,
    file: &mut File,
    head: bool,
    range: Option<&str>,
) -> std::io::Result<()> {
    let length = file.metadata()?.len();
    let range = match byte_range(range, length) {
        Ok(value) => value,
        Err(_) => return write!(stream, "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{length}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    };
    let (start, count) = range
        .map(|(start, end)| (start, end - start + 1))
        .unwrap_or((0, length));
    let status = if range.is_some() {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let path = fd_path(file).unwrap_or_default();
    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {count}\r\nAccept-Ranges: bytes\r\nContent-Disposition: inline; filename*=UTF-8''{}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n", mime_of(&path), encode_segment(filename))?;
    if let Some((start, end)) = range {
        write!(stream, "Content-Range: bytes {start}-{end}/{length}\r\n")?;
    }
    write!(stream, "\r\n")?;
    if !head {
        file.seek(SeekFrom::Start(start))?;
        // Never leak appended bytes beyond the advertised body or range.
        std::io::copy(&mut file.take(count), stream)?;
    }
    Ok(())
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
    head: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Not Found",
    };
    write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    if !head {
        stream.write_all(body)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(mode: &str, path: &Path, request: &[u8]) -> Vec<u8> {
        let target = Target::open(mode, path).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_root(&target, stream).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(request).unwrap();
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).unwrap();
        worker.join().unwrap();
        bytes
    }
    fn body(response: &[u8]) -> &[u8] {
        let split = response.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
        &response[split + 4..]
    }
    #[test]
    fn file_mode_scopes_one_live_file_and_supports_ranges_and_head() {
        let root = std::env::temp_dir().join(format!("published-file-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("report.pdf");
        fs::write(&file, b"0123456789").unwrap();
        fs::write(root.join("secret"), b"private").unwrap();
        let full = request("file", &file, b"GET / HTTP/1.1\r\nHost: test\r\n\r\n");
        assert_eq!(body(&full), b"0123456789");
        assert!(String::from_utf8_lossy(&full).contains("Content-Type: application/pdf"));
        assert!(String::from_utf8_lossy(&full).contains("filename*=UTF-8''report%2Epdf"));
        for (range, expected) in [
            ("bytes=2-4", b"234".as_slice()),
            ("bytes=7-", b"789"),
            ("bytes=-3", b"789"),
            ("bytes=0-99", b"0123456789"),
        ] {
            let response = request(
                "file",
                &file,
                format!("GET / HTTP/1.1\r\nHost: test\r\nRange: {range}\r\n\r\n").as_bytes(),
            );
            assert!(response.starts_with(b"HTTP/1.1 206"));
            assert_eq!(body(&response), expected);
        }
        let invalid = request(
            "file",
            &file,
            b"GET / HTTP/1.1\r\nHost: test\r\nRange: bytes=10-\r\n\r\n",
        );
        assert!(invalid.starts_with(b"HTTP/1.1 416"));
        assert!(String::from_utf8_lossy(&invalid).contains("Content-Range: bytes */10"));
        let head = request(
            "file",
            &file,
            b"HEAD / HTTP/1.1\r\nHost: test\r\nRange: bytes=2-4\r\n\r\n",
        );
        assert!(head.starts_with(b"HTTP/1.1 200"));
        assert!(body(&head).is_empty());
        assert!(String::from_utf8_lossy(&head).contains("Content-Length: 10\r\n"));
        for path in [
            "/secret",
            "/../secret",
            "/%2e%2e/secret",
            "/report.pdf",
            "//",
        ] {
            assert!(request(
                "file",
                &file,
                format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n").as_bytes()
            )
            .starts_with(b"HTTP/1.1 404"));
        }
        let target = Target::open("file", &file).unwrap();
        fs::write(root.join("new"), b"updated").unwrap();
        fs::rename(root.join("new"), &file).unwrap();
        let Opened::File(mut updated) =
            open_within(&target.root, "/report.pdf").unwrap_or_else(|_| panic!("file"))
        else {
            panic!("file")
        };
        let mut value = String::new();
        updated.read_to_string(&mut value).unwrap();
        assert_eq!(value, "updated");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn single_file_does_not_expand_when_replaced_by_a_directory() {
        let root =
            std::env::temp_dir().join(format!("published-file-replaced-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("report");
        fs::write(&path, b"selected").unwrap();
        let target = Target::open("file", &path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("index.html"), b"must not become public").unwrap();
        assert!(matches!(target.resolve("/"), Err(404)));
        assert!(matches!(target.resolve("/index.html"), Err(404)));
        fs::remove_dir_all(&path).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", &path).unwrap();
        assert!(matches!(target.resolve("/"), Err(404)));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn directory_redirect_stays_on_its_origin_with_repeated_slashes() {
        let root = std::env::temp_dir().join(format!("published-redirect-{}", std::process::id()));
        fs::create_dir_all(root.join("other.example")).unwrap();
        for path in ["//other.example", "/%2fother.example", "///other.example"] {
            let response = request(
                "directory",
                &root,
                format!("GET {path}?a=1 HTTP/1.1\r\nHost: test\r\n\r\n").as_bytes(),
            );
            let headers = String::from_utf8_lossy(&response);
            assert!(
                headers.contains("Location: /other%2Eexample/?a=1\r\n"),
                "{headers}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_listing_escapes_names_and_links_and_hides_symlinks() {
        let root = std::env::temp_dir().join(format!("published-listing-{}", std::process::id()));
        fs::create_dir_all(root.join("folder")).unwrap();
        fs::write(root.join("<script> &中文.txt"), b"file").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("secret-link")).unwrap();
        let response = request("directory", &root, b"GET / HTTP/1.1\r\nHost: test\r\n\r\n");
        let html = String::from_utf8_lossy(body(&response));
        assert!(html.contains("&lt;script&gt; &amp;中文.txt"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("%3Cscript%3E%20%26"));
        assert!(!html.contains("secret-link"));
        assert!(html.contains("href=\"folder/\""));
        fs::write(root.join("index.html"), b"index wins").unwrap();
        assert_eq!(
            body(&request(
                "directory",
                &root,
                b"GET / HTTP/1.1\r\nHost: test\r\n\r\n"
            )),
            b"index wins"
        );
        assert!(Target::open("file", &root).is_err());
        assert!(Target::open("directory", &root.join("index.html")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn encoded_traversal_and_symlinks_cannot_escape_root() {
        use std::os::unix::fs::symlink;
        let parent =
            std::env::temp_dir().join(format!("static-http-boundary-{}", std::process::id()));
        let _ = fs::remove_dir_all(&parent);
        let root = parent.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(parent.join("secret.txt"), b"secret").unwrap();
        fs::write(root.join("ok.txt"), b"ok").unwrap();
        symlink(parent.join("secret.txt"), root.join("link.txt")).unwrap();
        let root_handle = File::open(&root).unwrap();
        assert!(matches!(
            open_within(&root_handle, "/ok.txt"),
            Ok(Opened::File(_))
        ));
        for path in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/%2e%2e%2fsecret.txt",
            "/link.txt",
        ] {
            assert!(
                matches!(open_within(&root_handle, path), Err(404)),
                "{path}"
            );
        }
        let _ = fs::remove_dir_all(parent);
    }
    #[test]
    fn directory_without_slash_redirects_and_index_stays_inside_root() {
        let root = std::env::temp_dir().join(format!("static-http-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/index.html"), b"inside").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = serve(&root, stream);
            }
        });
        let mut first = TcpStream::connect(address).unwrap();
        first
            .write_all(b"GET /nested?view=1 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        first.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 307"), "{response}");
        assert!(
            response.contains("Location: /nested/?view=1\r\n"),
            "{response}"
        );
        let mut second = TcpStream::connect(address).unwrap();
        second
            .write_all(b"GET /nested/ HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut body = String::new();
        second.read_to_string(&mut body).unwrap();
        assert!(body.contains("inside"), "{body}");
        let mut outside = TcpStream::connect(address).unwrap();
        outside
            .write_all(b"GET /../outside HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut denied = String::new();
        outside.read_to_string(&mut denied).unwrap();
        assert!(denied.starts_with("HTTP/1.1 404"), "{denied}");
    }
}

#[test]
fn large_file_head_matches_streamed_get() {
    let root = std::env::temp_dir().join(format!("static-http-large-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let payload = vec![b'x'; 20 * 1024];
    fs::write(root.join("blob.bin"), &payload).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve(&root, stream);
        }
    });
    let mut head = TcpStream::connect(address).unwrap();
    head.write_all(b"HEAD /blob.bin HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut head_response = String::new();
    head.read_to_string(&mut head_response).unwrap();
    assert!(
        head_response.contains("Content-Length: 20480\r\n"),
        "{head_response}"
    );
    assert!(!head_response.contains('x'), "{head_response}");
    let mut get = TcpStream::connect(address).unwrap();
    get.write_all(b"GET /blob.bin HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut bytes = Vec::new();
    get.read_to_end(&mut bytes).unwrap();
    let split = bytes
        .windows(4)
        .position(|item| item == b"\r\n\r\n")
        .unwrap();
    assert_eq!(&bytes[split + 4..], payload);
}

#[test]
fn busy_helper_rejects_without_starting_another_request() {
    let root = std::env::temp_dir().join(format!("static-http-busy-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("index.html"), b"ok").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        accept_loop(
            listener,
            std::sync::Arc::new(Target::open("directory", &root).unwrap()),
            1,
        )
    });
    let held = TcpStream::connect(address).unwrap();
    // An incomplete header holds one worker without using a test-only
    // production switch or a long-running file transfer.
    std::thread::sleep(Duration::from_millis(50));
    let mut busy = TcpStream::connect(address).unwrap();
    busy.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    busy.write_all(b"GET /index.html HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = Vec::new();
    let mut buf = [0u8; 256];
    while let Ok(n) = busy.read(&mut buf) {
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buf[..n]);
    }
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 503"), "{response}");
    drop(held);
}
