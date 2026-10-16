use std::collections::HashMap;
use std::sync::Arc;

use agent_engine::config::Config;
use agent_engine::engine::mock::{
    MockEngine, TEST_FAIL_STOP_ONCE, TEST_HOLD_PORT, TEST_STARTED_PIDS, TEST_STOPPED_PIDS,
    TEST_TCP_RELEASE,
};
use agent_engine::engine::WorkspaceOpts;
use agent_engine::session::SessionManager;
use agent_engine::state::StateStore;
use agent_engine::workspace_service::WorkspaceService;
use agent_space_mcp::server::AgentSpaceMcpServer;
use agent_space_mcp::transport;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::json;

fn opts() -> WorkspaceOpts {
    WorkspaceOpts {
        image: "mock".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    }
}

#[tokio::test]
async fn revoked_token_cannot_initialize_or_change_publications() {
    let dir = std::env::temp_dir().join(format!("dynamic-revoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    StateStore::init(&dir).unwrap();
    let state = Arc::new(StateStore::open(&dir).unwrap());
    let config = Arc::new(toml::from_str::<Config>(include_str!("../../config.toml")).unwrap());
    let engine = Arc::new(MockEngine::new());
    let sessions = Arc::new(SessionManager::new(engine, 2, opts()));
    let service =
        WorkspaceService::new(state.clone(), sessions.clone(), "smcp", "http://127.0.0.1");
    let created = service.create_user("alice").unwrap();
    let user = state.authenticate(&created.token).unwrap();
    let session = sessions.get_or_create(&user.sandbox_name).await.unwrap();
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
    let server_state = state.clone();
    let server_sessions = sessions.clone();
    let server_config = config.clone();
    let mcp = StreamableHttpService::new(
        move || {
            Ok(AgentSpaceMcpServer::with_state(
                server_config.clone(),
                server_sessions.clone(),
                Some(server_state.clone()),
            ))
        },
        transport::session_manager(&config.server).into(),
        StreamableHttpServerConfig::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().nest_service("/u/{username}/mcp", mcp),
        )
        .await
        .unwrap();
    });
    state.rotate_token("alice", &user.agent_space_id).unwrap();
    let headers = HashMap::from([(
        http::HeaderName::from_static("authorization"),
        http::HeaderValue::from_str(&format!("Bearer {}", created.token)).unwrap(),
    )]);
    let client = ()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/u/alice/mcp"))
                .custom_headers(headers),
        ))
        .await;
    assert!(client.is_err(), "rotated token must not initialize");
    assert_eq!(session.publication("test").unwrap().port, 3000);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn candidate_helper_is_stopped_when_commit_recheck_fails() {
    let dir = std::env::temp_dir().join(format!("dynamic-candidate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    StateStore::init(&dir).unwrap();
    let state = Arc::new(StateStore::open(&dir).unwrap());
    let config = Arc::new(toml::from_str::<Config>(include_str!("../../config.toml")).unwrap());
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 2, opts()));
    let service =
        WorkspaceService::new(state.clone(), sessions.clone(), "smcp", "http://127.0.0.1");
    let created = service.create_user("alice").unwrap();
    let user = state.authenticate(&created.token).unwrap();
    let session = sessions.get_or_create(&user.sandbox_name).await.unwrap();
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
    TEST_STARTED_PIDS.lock().unwrap().clear();
    TEST_STOPPED_PIDS.lock().unwrap().clear();
    TEST_HOLD_PORT.store(44000, std::sync::atomic::Ordering::SeqCst);
    std::env::set_var("AGENT_SPACE_STATIC_HTTP", "/helper");
    let server_state = state.clone();
    let server_sessions = sessions.clone();
    let server_config = config.clone();
    let mcp = StreamableHttpService::new(
        move || {
            Ok(AgentSpaceMcpServer::with_state(
                server_config.clone(),
                server_sessions.clone(),
                Some(server_state.clone()),
            ))
        },
        transport::session_manager(&config.server).into(),
        StreamableHttpServerConfig::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().nest_service("/u/{username}/mcp", mcp),
        )
        .await
        .unwrap();
    });
    let headers = HashMap::from([(
        http::HeaderName::from_static("authorization"),
        http::HeaderValue::from_str(&format!("Bearer {}", created.token)).unwrap(),
    )]);
    let client = ()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/u/alice/mcp"))
                .custom_headers(headers),
        ))
        .await
        .unwrap();
    let call = CallToolRequestParams::new("web_publish").with_arguments(
        json!({"action":"publish","name":"report","directory":"/workspace/report"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let pending = tokio::spawn(async move { client.call_tool(call).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    state.rotate_token("alice", &user.agent_space_id).unwrap();
    TEST_TCP_RELEASE.notify_waiters();
    let result = pending.await.unwrap();
    assert!(result.is_err() || result.unwrap().is_error == Some(true));
    assert_eq!(session.publication("test").unwrap().port, 3000);
    let started = TEST_STARTED_PIDS.lock().unwrap().clone();
    let stopped = TEST_STOPPED_PIDS.lock().unwrap().clone();
    assert!(
        started.iter().any(|pid| stopped.contains(pid)),
        "started={started:?} stopped={stopped:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn failed_helper_stop_is_retried_before_another_helper_starts() {
    let dir = std::env::temp_dir().join(format!("dynamic-retry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    StateStore::init(&dir).unwrap();
    let state = Arc::new(StateStore::open(&dir).unwrap());
    let config = Arc::new(toml::from_str::<Config>(include_str!("../../config.toml")).unwrap());
    let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 2, opts()));
    let service =
        WorkspaceService::new(state.clone(), sessions.clone(), "smcp", "http://127.0.0.1");
    let created = service.create_user("alice").unwrap();
    let user = state.authenticate(&created.token).unwrap();
    let session = sessions.get_or_create(&user.sandbox_name).await.unwrap();
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
    TEST_STARTED_PIDS.lock().unwrap().clear();
    TEST_STOPPED_PIDS.lock().unwrap().clear();
    TEST_FAIL_STOP_ONCE.store(true, std::sync::atomic::Ordering::SeqCst);
    std::env::set_var("AGENT_SPACE_STATIC_HTTP", "/helper");
    let server_state = state.clone();
    let server_sessions = sessions.clone();
    let server_config = config.clone();
    let mcp = StreamableHttpService::new(
        move || {
            Ok(AgentSpaceMcpServer::with_state(
                server_config.clone(),
                server_sessions.clone(),
                Some(server_state.clone()),
            ))
        },
        transport::session_manager(&config.server).into(),
        StreamableHttpServerConfig::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().nest_service("/u/{username}/mcp", mcp),
        )
        .await
        .unwrap();
    });
    let headers = HashMap::from([(
        http::HeaderName::from_static("authorization"),
        http::HeaderValue::from_str(&format!("Bearer {}", created.token)).unwrap(),
    )]);
    let client = ()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/u/alice/mcp"))
                .custom_headers(headers),
        ))
        .await
        .unwrap();
    let off = CallToolRequestParams::new("web_publish").with_arguments(
        json!({"action":"unpublish","name":"test"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_ne!(client.call_tool(off).await.unwrap().is_error, Some(true));
    assert!(session.publication("test").is_none());
    assert!(
        TEST_STOPPED_PIDS.lock().unwrap().is_empty(),
        "first stop must fail"
    );
    let set = CallToolRequestParams::new("web_publish").with_arguments(
        json!({"action":"publish","name":"report","directory":"/workspace/report"})
            .as_object()
            .unwrap()
            .clone(),
    );
    let set_result = client.call_tool(set).await.unwrap();
    assert_ne!(set_result.is_error, Some(true));
    let encoded = serde_json::to_value(&set_result).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(encoded["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["type"], json!("directory"));
    let get = CallToolRequestParams::new("web_publish").with_arguments(
        json!({"action":"get","name":"report"})
            .as_object()
            .unwrap()
            .clone(),
    );
    session
        .set_last_request(std::time::Instant::now() - std::time::Duration::from_secs(60))
        .await;
    let got = client.call_tool(get).await.unwrap();
    assert!(session.last_request().await.elapsed() >= std::time::Duration::from_secs(30));
    let encoded = serde_json::to_value(&got).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(encoded["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["type"], json!("directory"));
    assert!(
        TEST_STOPPED_PIDS.lock().unwrap().contains(&39000),
        "{:?}",
        TEST_STOPPED_PIDS.lock().unwrap()
    );
    assert_eq!(
        TEST_STARTED_PIDS.lock().unwrap().len(),
        1,
        "retry must not accumulate helpers"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn closing_a_publication_keeps_older_failed_helper_cleanup() {
    let sessions = SessionManager::new(Arc::new(MockEngine::new()), 2, opts());
    let session = sessions.get_or_create("cleanup-two-helpers").await.unwrap();
    TEST_STOPPED_PIDS.lock().unwrap().clear();

    // The previous replacement failed to stop its old helper. Closing the
    // current publication must retain both cleanup handles for retry.
    session.note_helper_cleanup(&agent_engine::engine::mock::managed_instance(39000));
    session
        .publish(
            agent_engine::publication::Publication::new(
                "test".into(),
                ("alice").to_string(),
                ("spc_test").to_string(),
                agent_engine::publication::PublishTarget::Port(44000),
                44000,
                Some(agent_engine::engine::mock::managed_instance(44000)),
                std::time::Duration::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
    TEST_FAIL_STOP_ONCE.store(true, std::sync::atomic::Ordering::SeqCst);
    session.unpublish("test").await;
    assert!(session.helper_cleanup_pending());

    session.retry_helper_cleanup().await.unwrap();
    let stopped = TEST_STOPPED_PIDS.lock().unwrap().clone();
    assert!(stopped.contains(&39000));
    assert!(stopped.contains(&44000));
    assert!(!session.helper_cleanup_pending());
}
