use agent_engine::config::LifecycleConfig;
use agent_engine::engine::mock::{MockEngine, TEST_EXEC_HOLD};
use agent_engine::engine::{AgentEngine, WorkspaceOpts, WorkspaceStatus};
use agent_engine::session::SessionManager;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn test_lifecycle_config(
    session_timeout: u64,
    idle_timeout: u64,
    check_interval: u64,
) -> LifecycleConfig {
    LifecycleConfig {
        session_timeout_secs: session_timeout,
        idle_timeout_secs: idle_timeout,
        check_interval_secs: check_interval,
    }
}

fn default_opts() -> WorkspaceOpts {
    WorkspaceOpts {
        image: "test:latest".to_string(),
        cpus: 1,
        memory_mib: 256,
        volumes: vec![],
        shell: "/bin/bash".to_string(),
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}

fn ws_set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

// ─── LC-01: session timeout ────────────────────────────────────────────────
#[tokio::test]
async fn test_lc01_session_timeout() {
    let engine = Arc::new(MockEngine::new());
    let config = test_lifecycle_config(2, 3600, 60);
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let session = mgr.get_or_create("smcp-001").await.unwrap();

    // 模拟：last_request = 5秒前 → 超过 session_timeout=2s
    let past = Instant::now() - Duration::from_secs(5);
    session.set_last_request(past).await;
    session.set_last_work(past).await;

    // check_and_reclaim 应该 stop sandbox (workspace still in config)
    mgr.check_and_reclaim(&config, &ws_set(&["smcp-001"]))
        .await
        .unwrap();

    let status = session.workspace_status().await;
    assert_ne!(
        status,
        agent_engine::engine::WorkspaceStatus::Running,
        "LC-01: sandbox should be stopped after session timeout"
    );

    // session 仍保留在 map 中
    assert_eq!(
        mgr.session_count().await,
        1,
        "LC-01: session should be retained"
    );
}

// ─── LC-02: idle timeout 不触发 — last_request 远但 last_work 刚刚 ────
// 场景：客户端活着（last_request 远 = 可能已断联），但 last_work 刚刚更新
// 优先级：session_timeout 先检查（req_secs >= session_timeout），不走 idle 分支
// 此测试验证：当 req_secs < session_timeout 且 tc_secs < idle_timeout 时不回收
#[tokio::test]
async fn test_lc02_no_timeout_when_tool_active() {
    let engine = Arc::new(MockEngine::new());
    // session=2s, idle=10s — last_request 5s前超过 session=2s，但 last_work 刚刚
    let config = test_lifecycle_config(2, 10, 60);
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let session = mgr.get_or_create("smcp-002").await.unwrap();

    let now = Instant::now();
    // last_request = 5秒前（超过 session=2s → session timeout 候选）
    session.set_last_request(now - Duration::from_secs(5)).await;
    // last_work = 刚刚（未超过 idle=10s → 不是 idle 候选）
    session.set_last_work(now).await;

    // check_and_reclaim 会收集到 session timeout 候选，但 verify 时重新检查
    // last_request 距今 ≈5s + verify 延迟，仍然 >= session=2s → 应该 stop
    mgr.check_and_reclaim(&config, &ws_set(&["smcp-002"]))
        .await
        .unwrap();

    let status = session.workspace_status().await;
    assert_ne!(
        status,
        agent_engine::engine::WorkspaceStatus::Running,
        "LC-02: should be stopped (session timeout, even though tool active)"
    );
}

// ─── LC-03: idle timeout (last_work 远，last_request 刚刚) ────────────
#[tokio::test]
async fn test_lc03_idle_timeout_tool_idle() {
    let engine = Arc::new(MockEngine::new());
    let config = test_lifecycle_config(900, 2, 60);
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let session = mgr.get_or_create("smcp-003").await.unwrap();

    let now = Instant::now();
    // last_request = 刚刚
    session.set_last_request(now).await;
    // last_work = 5秒前（超过 idle=2s）
    session.set_last_work(now - Duration::from_secs(5)).await;

    mgr.check_and_reclaim(&config, &ws_set(&["smcp-003"]))
        .await
        .unwrap();

    let status = session.workspace_status().await;
    assert_ne!(
        status,
        agent_engine::engine::WorkspaceStatus::Running,
        "LC-03: should be stopped (tool idle)"
    );
}

// ─── LC-04: 未超时，不回收 ─────────────────────────────────────────────────
#[tokio::test]
async fn test_lc04_no_timeout_no_reclaim() {
    let engine = Arc::new(MockEngine::new());
    let config = test_lifecycle_config(900, 3600, 60);
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let session = mgr.get_or_create("smcp-004").await.unwrap();

    // 时间戳 = 刚刚
    let now = Instant::now();
    session.set_last_request(now).await;
    session.set_last_work(now).await;

    mgr.check_and_reclaim(&config, &ws_set(&["smcp-004"]))
        .await
        .unwrap();

    let status = session.workspace_status().await;
    assert_eq!(
        status,
        agent_engine::engine::WorkspaceStatus::Running,
        "LC-04: should still be running"
    );
}

// ─── LC-05: session_timeout < idle_timeout，session 先触发 ─────────────────
#[tokio::test]
async fn test_lc05_session_timeout_before_idle() {
    let engine = Arc::new(MockEngine::new());
    // session=3s, idle=10s
    let config = test_lifecycle_config(3, 10, 60);
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let session = mgr.get_or_create("smcp-005").await.unwrap();

    // 5秒前活动 → 超过 session=3s，但未超过 idle=10s
    let past = Instant::now() - Duration::from_secs(5);
    session.set_last_request(past).await;
    session.set_last_work(past).await;

    mgr.check_and_reclaim(&config, &ws_set(&["smcp-005"]))
        .await
        .unwrap();

    let status = session.workspace_status().await;
    assert_ne!(
        status,
        agent_engine::engine::WorkspaceStatus::Running,
        "LC-05: session timeout should trigger first"
    );
}

// ─── LC-06: stop 后 session 仍在 map 中 ────────────────────────────────────
#[tokio::test]
async fn test_lc06_session_retained_after_stop() {
    let engine = Arc::new(MockEngine::new());
    let config = test_lifecycle_config(2, 3600, 60);
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let _session = mgr.get_or_create("smcp-006").await.unwrap();

    let past = Instant::now() - Duration::from_secs(5);
    mgr.set_session_last_request("smcp-006", past).await;
    mgr.set_session_last_work("smcp-006", past).await;

    mgr.check_and_reclaim(&config, &ws_set(&["smcp-006"]))
        .await
        .unwrap();

    // session 仍保留在 map 中
    assert_eq!(
        mgr.session_count().await,
        1,
        "LC-06: session should be retained"
    );

    // get_or_create 应该恢复
    let session2 = mgr.get_or_create("smcp-006").await;
    assert!(
        session2.is_ok(),
        "LC-06: get_or_create should recover stopped sandbox"
    );
}

// ─── LC-07: exec() 更新请求与工具活动时间 ──────────────────────────────────
#[tokio::test]
async fn test_lc07_exec_updates_both_activity_clocks() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let session = mgr.get_or_create("smcp-007").await.unwrap();

    // 设置旧时间
    let old = Instant::now() - Duration::from_secs(60);
    session.set_last_request(old).await;
    session.set_last_work(old).await;

    // exec 更新两种活动时间
    session
        .exec("echo hi", Duration::from_secs(5))
        .await
        .unwrap();

    let req = session.last_request().await;
    let tc = session.last_work().await;
    let now = Instant::now();

    assert!(
        now.duration_since(req) < Duration::from_secs(2),
        "LC-07: exec is request activity"
    );
    assert!(
        now.duration_since(tc) < Duration::from_secs(2),
        "LC-07: last_work should be updated"
    );
}

// ─── LC-08: 双检查竞态消除 ─────────────────────────────────────────────────
#[tokio::test]
async fn test_lc08_double_check_race_condition() {
    let engine = Arc::new(MockEngine::new());
    let config = test_lifecycle_config(2, 3600, 60);
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let session = mgr.get_or_create("smcp-008").await.unwrap();

    // 第一步：设置过期时间
    let past = Instant::now() - Duration::from_secs(5);
    session.set_last_request(past).await;
    session.set_last_work(past).await;

    // 第二步：collect_candidates 应该收集到这个 session
    let candidates = mgr.collect_candidates(&config).await;
    assert_eq!(candidates.len(), 1, "LC-08: should collect 1 candidate");
    assert_eq!(
        candidates[0].0, "smcp-008",
        "LC-08: candidate should be smcp-008"
    );

    // 第三步：模拟新请求到达 → 更新时间戳（竞态发生）
    session.touch().await;

    // 第四步：verify_and_stop 应该跳过停止（因为时间戳已更新，双检查生效）
    mgr.verify_and_stop("smcp-008", &config, candidates[0].1)
        .await
        .unwrap();

    let status = session.workspace_status().await;
    assert_eq!(
        status,
        agent_engine::engine::WorkspaceStatus::Running,
        "LC-08: sandbox should still running (double-check passed)"
    );
}

// ─── LC-09: touch() 只更新 last_request ────────────────────────────────────
#[tokio::test]
async fn test_lc09_touch_only_updates_request() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let session = mgr.get_or_create("smcp-009").await.unwrap();

    let old = Instant::now() - Duration::from_secs(60);
    session.set_last_request(old).await;
    session.set_last_work(old).await;

    session.touch().await;

    let req = session.last_request().await;
    let tc = session.last_work().await;
    let now = Instant::now();

    assert!(
        now.duration_since(req) < Duration::from_secs(2),
        "LC-09: last_request should be updated"
    );
    assert!(
        now.duration_since(tc) > Duration::from_secs(30),
        "LC-09: last_work should NOT be updated"
    );
}

// ─── LC-10: stop_workspace 停但不删 ──────────────────────────────────────────
#[tokio::test]
async fn test_lc10_stop_workspace_no_remove() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    let session = mgr.get_or_create("smcp-010").await.unwrap();

    // stop_workspace 停止 sandbox 但保留 session
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_test").to_string(),
                agent_engine::publication::PublishTarget::Port(3000),
                3000,
                Some(agent_engine::engine::mock::managed_instance(39000)),
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    mgr.stop_workspace("smcp-010").await.unwrap();
    assert!(
        session.publication("test").is_none(),
        "preview closes even after stop"
    );

    // sandbox 停止（通过 session 引用检查）
    let status = session.workspace_status().await;
    assert_ne!(
        status,
        agent_engine::engine::WorkspaceStatus::Running,
        "LC-10: sandbox should be stopped"
    );

    // session 仍在 map
    assert_eq!(
        mgr.session_count().await,
        1,
        "LC-10: session should be retained"
    );

    // get_or_create 应该能恢复
    let session2 = mgr.get_or_create("smcp-010").await;
    assert!(
        session2.is_ok(),
        "LC-10: get_or_create should recover stopped sandbox"
    );
}

// ─── LC-11: an absent recorded VM is already stopped (no resurrection) ────
#[tokio::test]
async fn test_lc11_stop_workspace_not_found() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    mgr.stop_workspace("nonexistent").await.unwrap();
    assert!(engine.list_running().await.unwrap().is_empty());
}

// ─── LC-12: 并发 get_or_create 竞态 ─────────────────────────────────────────
#[tokio::test]
async fn test_lc12_concurrent_get_or_create() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));
    let mgr2 = mgr.clone();

    // 两个并发任务同时为同一用户创建 session
    let handle1 = tokio::spawn(async move { mgr.get_or_create("sb-concurrent").await });
    let handle2 = tokio::spawn(async move { mgr2.get_or_create("sb-concurrent").await });

    let (r1, r2) = tokio::join!(handle1, handle2);
    let r1 = r1.unwrap();
    let r2 = r2.unwrap();

    // 至少一个应该成功
    assert!(
        r1.is_ok() && r2.is_ok(),
        "LC-12: both connections must succeed"
    );

    // session 数量应该为 1（不重复）
    assert_eq!(
        engine.list_running().await.unwrap().len(),
        1,
        "LC-12: should have exactly 1 sandbox"
    );
}

// ─── LC-13: workspace 不在 config → 完全清理 ──────────────────────────────
#[tokio::test]
async fn test_lc13_workspace_removed_full_cleanup() {
    let engine = Arc::new(MockEngine::new());
    let config = test_lifecycle_config(2, 3600, 60);
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    // 创建 session
    let _session = mgr.get_or_create("sb-removed").await.unwrap();
    assert_eq!(mgr.session_count().await, 1, "LC-13: should have 1 session");

    // 模拟超时
    let past = Instant::now() - Duration::from_secs(5);
    mgr.set_session_last_request("sb-removed", past).await;
    mgr.set_session_last_work("sb-removed", past).await;

    // Absence from a static roster no longer destroys disks. Expiry only stops.
    let ws_names = HashSet::new();
    mgr.check_and_reclaim(&config, &ws_names).await.unwrap();
    assert_eq!(
        mgr.session_count().await,
        1,
        "LC-13: idle reclaim must retain the session and its disks"
    );
    assert!(
        engine
            .list_workspaces()
            .await
            .unwrap()
            .contains(&"sb-removed".to_string()),
        "LC-13: sandbox must survive roster absence"
    );
}

// ─── LC-14: 未登记资源只报告，不删除 ─────────────────────────────────────
#[tokio::test]
async fn test_lc14_reports_unregistered_without_deleting() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));
    let _session1 = mgr.get_or_create("smcp-user-keep").await.unwrap();
    let _session2 = mgr.get_or_create("smcp-user-unknown").await.unwrap();
    let registered = ws_set(&["smcp-user-keep"]);
    let reported = mgr
        .report_unregistered_resources(&registered, &ws_set(&["smcp-user-keep-ws"]))
        .await
        .unwrap();
    assert!(reported.contains(&"smcp-user-unknown".to_string()));
    assert!(reported.contains(&"smcp-user-unknown-ws".to_string()));
    let running = engine.list_running().await.unwrap();
    assert!(running.contains(&"smcp-user-unknown".to_string()));
    assert!(running.contains(&"smcp-user-keep".to_string()));
}

// ─── LC-15: 精确 deleting 清单会删沙箱和卷，未登记资源不动 ──────────────
#[tokio::test]
async fn test_lc15_recorded_delete_does_not_touch_other_resources() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));
    mgr.get_or_create("smcp-delete").await.unwrap();
    mgr.get_or_create("manual-sandbox").await.unwrap();
    mgr.destroy_recorded_resources("smcp-delete", &["smcp-delete-ws".into()])
        .await
        .unwrap();
    let running = engine.list_running().await.unwrap();
    assert!(!running.contains(&"smcp-delete".to_string()));
    assert!(running.contains(&"manual-sandbox".to_string()));
    assert!(!engine
        .list_volumes()
        .await
        .unwrap()
        .contains(&"smcp-delete-ws".to_string()));
    assert!(engine
        .list_volumes()
        .await
        .unwrap()
        .contains(&"manual-sandbox-ws".to_string()));
}

// ─── LC-16: 卷删除失败时沙箱记录保持，下次只补删残留卷 ──────────────────
#[tokio::test]
async fn test_lc16_volume_failure_is_retryable() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));
    mgr.get_or_create("smcp-partial").await.unwrap();
    engine
        .fail_volume_removal
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let failed = mgr
        .destroy_recorded_resources("smcp-partial", &["smcp-partial-ws".into()])
        .await;
    assert!(failed.is_err(), "LC-16: volume failure must stay visible");
    assert!(!engine
        .list_running()
        .await
        .unwrap()
        .contains(&"smcp-partial".to_string()));
    assert!(engine
        .list_volumes()
        .await
        .unwrap()
        .contains(&"smcp-partial-ws".to_string()));
    engine
        .fail_volume_removal
        .store(false, std::sync::atomic::Ordering::SeqCst);
    mgr.destroy_recorded_resources("smcp-partial", &["smcp-partial-ws".into()])
        .await
        .unwrap();
    assert!(!engine
        .list_volumes()
        .await
        .unwrap()
        .contains(&"smcp-partial-ws".to_string()));
}

// ─── LC-17: stop_all 停止所有 session 并清空 map ─────────────────────────
#[tokio::test]
async fn test_lc17_stop_all_stops_and_clears_sessions() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine.clone(), 6, default_opts()));

    // 创建 3 个 session
    let s1 = mgr.get_or_create("smcp-stop-1").await.unwrap();
    let s2 = mgr.get_or_create("smcp-stop-2").await.unwrap();
    let s3 = mgr.get_or_create("smcp-stop-3").await.unwrap();
    assert_eq!(
        mgr.session_count().await,
        3,
        "LC-17: should have 3 sessions"
    );

    // 验证所有 session 的 handle 是 Running
    assert_eq!(s1.workspace_status().await, WorkspaceStatus::Running);
    assert_eq!(s2.workspace_status().await, WorkspaceStatus::Running);
    assert_eq!(s3.workspace_status().await, WorkspaceStatus::Running);

    // stop_all
    mgr.stop_all().await;

    // session map 应该为空
    assert_eq!(
        mgr.session_count().await,
        0,
        "LC-17: session map should be empty after stop_all"
    );

    // 之前获取的 handle 应该已经被停止（handle 是 Arc 共享的）
    assert_eq!(
        s1.workspace_status().await,
        WorkspaceStatus::Stopped,
        "LC-17: s1 should be stopped"
    );
    assert_eq!(
        s2.workspace_status().await,
        WorkspaceStatus::Stopped,
        "LC-17: s2 should be stopped"
    );
    assert_eq!(
        s3.workspace_status().await,
        WorkspaceStatus::Stopped,
        "LC-17: s3 should be stopped"
    );

    // Shutdown permanently closes this manager and all existing clients.
    assert!(mgr.get_or_create("smcp-stop-1").await.is_err());
    assert!(s1
        .exec("echo should-not-run", Duration::from_secs(1))
        .await
        .is_err());
    assert_eq!(mgr.session_count().await, 0);
}

#[tokio::test]
async fn deleting_cancels_a_running_command_and_rejects_the_next_one() {
    let engine = Arc::new(MockEngine::new());
    let mgr = Arc::new(SessionManager::new(engine, 2, default_opts()));
    let session = mgr.get_or_create("smcp-cancel").await.unwrap();
    TEST_EXEC_HOLD.store(true, std::sync::atomic::Ordering::SeqCst);
    let running = tokio::spawn(async move {
        session
            .exec("sleep 30", std::time::Duration::from_secs(30))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while TEST_EXEC_HOLD.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("mock command must enter its in-flight hold before deletion");
    let started = std::time::Instant::now();
    mgr.destroy_recorded_resources("smcp-cancel", &["smcp-cancel-ws".into()])
        .await
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    let error = running.await.unwrap().unwrap_err().to_string();
    assert!(error.contains("cancelled"), "{error}");
}
