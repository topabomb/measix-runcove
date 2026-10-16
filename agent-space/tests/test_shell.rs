mod common;

use std::sync::Arc;
use std::time::Duration;

use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, VolumeSpec, WorkspaceOpts};
use agent_engine::session::SessionManager;

const TEST_IMAGE: &str = "nikolaik/python-nodejs:python3.12-nodejs22-slim";
const TIMEOUT_SECS: u64 = 30;

fn make_opts() -> WorkspaceOpts {
    WorkspaceOpts {
        image: TEST_IMAGE.to_string(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".to_string(),
        volumes: vec![
            VolumeSpec::disk("/workspace", "test-ws", 256),
            VolumeSpec::tmpfs("/tmp", 64),
        ],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}

fn timeout() -> Duration {
    Duration::from_secs(TIMEOUT_SECS)
}

fn unique_name(prefix: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{}", prefix, ts)
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Read file using the exact same shell command as the server's read tool
async fn read_file(
    session: &agent_engine::session::Session,
    path: &str,
    offset: u64,
    limit: u64,
) -> (String, i32) {
    let end = offset + limit - 1;
    let cmd = format!(
        r#"if [ ! -f {} ]; then echo 'FILE_NOT_FOUND'; exit 1; fi; if [ ! -s {} ]; then echo 'EMPTY'; exit 0; fi; lines=$(nl -ba {} | sed -n '{},{}p'); if [ -z "$lines" ]; then echo 'NO_CONTENT'; else echo "$lines"; fi"#,
        shell_escape(path),
        shell_escape(path),
        shell_escape(path),
        offset,
        end
    );
    let r = session.exec(&cmd, timeout()).await.unwrap();
    (r.stdout, r.exit_code)
}

/// Edit file using the exact same python3 command as the server's edit tool
async fn edit_file(
    session: &agent_engine::session::Session,
    path: &str,
    old: &str,
    new: &str,
) -> (String, i32) {
    let cmd = format!(
        r#"OLD_TEXT={} NEW_TEXT={} python3 -c "
import os, sys
content = open(sys.argv[1], 'r').read()
old = os.environ['OLD_TEXT']
new = os.environ['NEW_TEXT']
count = content.count(old)
if count == 0:
    print('NOT_FOUND')
    sys.exit(1)
if count > 1:
    print('MULTIPLE:' + str(count))
    sys.exit(2)
content = content.replace(old, new, 1)
open(sys.argv[1], 'w').write(content)
print('OK')
" {}"#,
        shell_escape(old),
        shell_escape(new),
        shell_escape(path)
    );
    let r = session.exec(&cmd, timeout()).await.unwrap();
    (r.stdout, r.exit_code)
}

// ─── Shell command tests (each uses its own sandbox, run in parallel) ──────

// S-01~S-03, S-05: Basic shell commands (echo, exit code, stderr, small output)
#[tokio::test]
async fn test_shell_echo_exit_code_stderr() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-basic");
    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();

    // S-01: echo
    let r = handle
        .exec("bash", &["-c", "echo hello"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("hello"));

    // S-02: exit code
    let r = handle
        .exec("bash", &["-c", "exit 42"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(r.exit_code, 42);

    // S-03: stderr
    let r = handle
        .exec("bash", &["-c", "echo out && echo err >&2"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("out"));

    // S-05: small output
    let r = handle
        .exec("bash", &["-c", "seq 1 100"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(!r.truncated, "100 lines should not be truncated");
    assert!(r.stdout.contains("100"));

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}
#[tokio::test]
async fn test_shell_timeout_kills_and_sandbox_recovers() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-timeout");
    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();

    // S-07: timeout kills process
    let short = Duration::from_secs(2);
    let r = handle.exec("bash", &["-c", "sleep 60"], Some(short)).await;
    assert!(r.is_err(), "should timeout");
    assert!(matches!(
        r.unwrap_err(),
        agent_engine::engine::EngineError::ExecutionEnded {
            timed_out: true,
            cleanup_confirmed: true,
            ..
        }
    ));

    // The next command must work; transport errors are not recovery evidence.
    let recovered = handle
        .exec("bash", &["-c", "echo alive"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(recovered.exit_code, 0);
    assert_eq!(recovered.stdout.trim(), "alive");

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

// S-09: Custom workdir
#[tokio::test]
async fn test_shell_custom_workdir() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-workdir");
    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();

    handle
        .exec(
            "bash",
            &[
                "-c",
                "mkdir -p /workspace/subdir && echo sub > /workspace/subdir/f.txt",
            ],
            Some(timeout()),
        )
        .await
        .unwrap();
    let r = handle
        .exec(
            "bash",
            &["-c", "cd /workspace/subdir && cat f.txt"],
            Some(timeout()),
        )
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(
        r.stdout.contains("sub"),
        "should read from subdir: {}",
        r.stdout
    );

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

// Pipe chains commands, redirect writes to file
#[tokio::test]
async fn test_shell_pipe_and_redirect() {
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = unique_name("test-pipe");
    let handle = engine.create_sandbox(&name, &make_opts()).await.unwrap();

    // Pipe
    let r = handle
        .exec("bash", &["-c", "echo hello | tr a-z A-Z"], Some(timeout()))
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("HELLO"));

    // Redirect
    let r = handle
        .exec(
            "bash",
            &["-c", "echo test123 > /tmp/r.txt && cat /tmp/r.txt"],
            Some(timeout()),
        )
        .await
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("test123"));

    common::cleanup_sandbox(&engine, &name, &make_opts()).await;
}

// S-04: Session truncates output when exceeding line limit
#[tokio::test]
async fn test_session_truncates_output_over_limit() {
    common::cleanup_test_sandboxes().await;
    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;
    let mgr = SessionManager::new(engine, 2, make_opts()).with_limits(50, 1000);
    let sb_name = unique_name("test-trunc");
    let session = mgr.get_or_create(&sb_name).await.unwrap();

    let r = session.exec("seq 1 100", timeout()).await.unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.truncated, "100 lines should be truncated with limit=50");
    assert!(
        r.stdout.contains("100"),
        "truncated output should contain tail: {}",
        r.stdout
    );

    let _ = mgr.remove(&sb_name).await;
}

// Q-02~Q-05: Command queue serial execution, results don't mix
#[tokio::test]
async fn test_session_command_queue_serial_execution() {
    common::cleanup_test_sandboxes().await;
    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;
    let mgr = SessionManager::new(engine, 2, make_opts());
    let sb_name = unique_name("test-queue");
    let session = mgr.get_or_create(&sb_name).await.unwrap();

    // Q-02: Concurrent commands — results don't mix
    let mut handles = Vec::new();
    for i in 0..5 {
        let s = session.clone();
        handles.push(tokio::spawn(async move {
            s.exec(&format!("echo msg-{}", i), Duration::from_secs(10))
                .await
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        let r = h.await.unwrap().unwrap();
        assert_eq!(r.exit_code, 0);
        assert!(
            r.stdout.contains(&format!("msg-{}", i)),
            "result {} should contain 'msg-{}', got: {}",
            i,
            i,
            r.stdout
        );
    }

    // Q-04: FIFO ordering
    let s1 = session.clone();
    let s2 = session.clone();
    let (r1, r2) = tokio::join!(
        tokio::spawn(async move {
            s1.exec("sleep 1 && echo slept", Duration::from_secs(10))
                .await
        }),
        tokio::spawn(async move { s2.exec("echo instant", Duration::from_secs(10)).await }),
    );
    let r1 = r1.unwrap().unwrap();
    let r2 = r2.unwrap().unwrap();
    assert_eq!(r1.exit_code, 0);
    assert!(r1.stdout.contains("slept"));
    assert_eq!(r2.exit_code, 0);
    assert!(r2.stdout.contains("instant"));

    // Q-05: Drop session handle
    let r = session.exec("echo before-drop", timeout()).await.unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("before-drop"));
    drop(session);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let session2 = mgr.get_or_create(&sb_name).await.unwrap();
    let r = session2.exec("echo after-drop", timeout()).await.unwrap();
    assert_eq!(r.exit_code, 0);
    assert!(r.stdout.contains("after-drop"));

    let _ = mgr.remove(&sb_name).await;
}

// ─── File tools: grouped by sandbox to minimize creation overhead ──────────

// F-01, F-02, F-03, F-10: write + read basics + special chars (one sandbox)
#[tokio::test]
async fn test_file_write_and_read_basics() {
    common::cleanup_test_sandboxes().await;
    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;
    let mgr = SessionManager::new(engine, 2, make_opts());
    let sb_name = unique_name("test-fwr");
    let session = mgr.get_or_create(&sb_name).await.unwrap();

    // F-01: write creates file, returns byte count
    let cmd = r#"mkdir -p $(dirname '/workspace/test-write.txt') && printf '%s' 'hello world' > /workspace/test-write.txt && wc -c < /workspace/test-write.txt"#;
    let r = session.exec(cmd, timeout()).await.unwrap();
    assert_eq!(r.exit_code, 0);
    let bytes: u64 = r.stdout.trim().parse().unwrap();
    assert_eq!(
        bytes, 11,
        "expected 11 bytes for 'hello world', got: {}",
        bytes
    );

    // F-02: write overwrites existing file
    session
        .exec(
            r#"printf '%s' 'original' > /workspace/overwrite.txt"#,
            timeout(),
        )
        .await
        .unwrap();
    let cmd = r#"mkdir -p $(dirname '/workspace/overwrite.txt') && printf '%s' 'new content here' > /workspace/overwrite.txt && wc -c < /workspace/overwrite.txt"#;
    let r = session.exec(cmd, timeout()).await.unwrap();
    assert_eq!(r.exit_code, 0);
    let bytes: u64 = r.stdout.trim().parse().unwrap();
    assert_eq!(bytes, 16, "expected 16 bytes, got: {}", bytes);
    let r = session
        .exec("cat /workspace/overwrite.txt", timeout())
        .await
        .unwrap();
    assert_eq!(r.stdout.trim(), "new content here");

    // F-03: read returns file content with line numbers
    session
        .exec(
            r#"printf 'line1\nline2\nline3' > /workspace/read-test.txt"#,
            timeout(),
        )
        .await
        .unwrap();
    let (output, code) = read_file(&session, "/workspace/read-test.txt", 1, 500).await;
    assert_eq!(code, 0);
    assert!(output.contains("line1"), "should contain line1: {}", output);
    assert!(output.contains("line2"), "should contain line2: {}", output);
    assert!(output.contains("line3"), "should contain line3: {}", output);
    assert!(
        output.contains("1\t") || output.contains("1 "),
        "should have line numbers: {}",
        output
    );

    // F-10: write with special characters
    let content = "hello <>&\"' world\nnewline";
    let cmd = format!(
        r#"mkdir -p $(dirname '/workspace/special.txt') && printf '%s' {} > /workspace/special.txt && wc -c < /workspace/special.txt"#,
        shell_escape(content)
    );
    let r = session.exec(&cmd, timeout()).await.unwrap();
    assert_eq!(r.exit_code, 0);
    let r = session
        .exec("cat /workspace/special.txt", timeout())
        .await
        .unwrap();
    assert!(
        r.stdout.contains("hello <>&"),
        "should preserve special chars: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("newline"),
        "should preserve newline: {}",
        r.stdout
    );

    let _ = mgr.remove(&sb_name).await;
}

// F-04, F-05, F-06: read edge cases (empty, not found, pagination) — one sandbox
#[tokio::test]
async fn test_file_read_edge_cases() {
    common::cleanup_test_sandboxes().await;
    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;
    let mgr = SessionManager::new(engine, 2, make_opts());
    let sb_name = unique_name("test-fre");
    let session = mgr.get_or_create(&sb_name).await.unwrap();

    // F-04: read returns EMPTY for empty file
    session
        .exec("touch /workspace/empty.txt", timeout())
        .await
        .unwrap();
    let (output, code) = read_file(&session, "/workspace/empty.txt", 1, 500).await;
    assert_eq!(code, 0);
    assert!(
        output.contains("EMPTY"),
        "expected EMPTY for empty file, got: {}",
        output
    );

    // F-05: read returns FILE_NOT_FOUND for missing file
    let (output, code) = read_file(&session, "/workspace/nonexistent.txt", 1, 500).await;
    assert_ne!(code, 0, "should fail for missing file");
    assert!(
        output.contains("FILE_NOT_FOUND"),
        "expected FILE_NOT_FOUND, got: {}",
        output
    );

    // F-06: read with offset/limit pagination
    session
        .exec(
            r#"printf 'a\nb\nc\nd\ne' > /workspace/paginate.txt"#,
            timeout(),
        )
        .await
        .unwrap();
    let (output, code) = read_file(&session, "/workspace/paginate.txt", 2, 3).await;
    assert_eq!(code, 0);
    assert!(
        output.contains("b"),
        "should contain line 2 (b): {}",
        output
    );
    assert!(
        output.contains("c"),
        "should contain line 3 (c): {}",
        output
    );
    assert!(
        output.contains("d"),
        "should contain line 4 (d): {}",
        output
    );
    assert!(
        !output.contains("\ta") && !output.contains("1\ta"),
        "should NOT contain line 1 (a): {}",
        output
    );
    assert!(
        !output.contains("\te") && !output.contains("5\te"),
        "should NOT contain line 5 (e): {}",
        output
    );

    let _ = mgr.remove(&sb_name).await;
}

// F-07, F-08, F-09: edit operations (success, not found, multiple) — one sandbox
#[tokio::test]
async fn test_file_edit_operations() {
    common::cleanup_test_sandboxes().await;
    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;
    let mgr = SessionManager::new(engine, 2, make_opts());
    let sb_name = unique_name("test-fe");
    let session = mgr.get_or_create(&sb_name).await.unwrap();

    // F-07: edit replaces exact text (single occurrence)
    session
        .exec(r#"printf 'hello world' > /workspace/edit.txt"#, timeout())
        .await
        .unwrap();
    let (output, code) = edit_file(&session, "/workspace/edit.txt", "hello", "goodbye").await;
    assert_eq!(code, 0);
    assert!(output.contains("OK"), "expected OK, got: {}", output);
    let r = session
        .exec("cat /workspace/edit.txt", timeout())
        .await
        .unwrap();
    assert_eq!(r.stdout.trim(), "goodbye world");

    // F-08: edit fails when old_text not found
    session
        .exec(
            r#"printf 'hello world' > /workspace/edit-nf.txt"#,
            timeout(),
        )
        .await
        .unwrap();
    let (output, code) = edit_file(
        &session,
        "/workspace/edit-nf.txt",
        "nonexistent",
        "replacement",
    )
    .await;
    assert_ne!(code, 0, "should fail for not found");
    assert!(
        output.contains("NOT_FOUND"),
        "expected NOT_FOUND, got: {}",
        output
    );

    // F-09: edit fails when old_text appears multiple times
    session
        .exec(
            r#"printf 'hello hello world' > /workspace/edit-m.txt"#,
            timeout(),
        )
        .await
        .unwrap();
    let (output, code) = edit_file(&session, "/workspace/edit-m.txt", "hello", "goodbye").await;
    assert_ne!(code, 0, "should fail for multiple occurrences");
    assert!(
        output.contains("MULTIPLE:"),
        "expected MULTIPLE:, got: {}",
        output
    );

    let _ = mgr.remove(&sb_name).await;
}
