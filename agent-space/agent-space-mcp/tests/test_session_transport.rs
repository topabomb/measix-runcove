use agent_engine::{
    config::Config,
    engine::{mock::MockEngine, WorkspaceOpts, WorkspaceStatus},
    session::SessionManager,
    state::StateStore,
};
use agent_space_mcp::{mcp_auth, server::AgentSpaceMcpServer, transport};
use rmcp::{
    model::CallToolRequestParams,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig,
        streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService},
        StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

#[tokio::test]
async fn modern_protocol_versions_negotiate_and_discover_without_starting_vm() -> anyhow::Result<()>
{
    let config = Arc::new(toml::from_str::<Config>(include_str!("../../config.toml"))?);
    let dir = tempfile::tempdir()?;
    StateStore::init(dir.path())?;
    let state = Arc::new(StateStore::open(dir.path())?);
    let (_, credential) = state.create_user("alice", "smcp", &["ws"])?;
    let engine = Arc::new(MockEngine::new());
    let manager = Arc::new(SessionManager::new(
        engine.clone(),
        1,
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
        },
    ));
    let server_manager = manager.clone();
    let server_config = config.clone();
    let auth = mcp_auth::McpAuthState::new(config.clone(), Some(state.clone()));
    let ct = tokio_util::sync::CancellationToken::new();
    let service = StreamableHttpService::new(
        move || {
            Ok(AgentSpaceMcpServer::with_state(
                server_config.clone(),
                server_manager.clone(),
                Some(state.clone()),
            ))
        },
        auth.session_manager().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let uri = format!("http://{}/u/alice/mcp", listener.local_addr()?);
    let shutdown = ct.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new()
                .nest_service("/u/{username}/mcp", service)
                .layer(axum::middleware::from_fn_with_state(
                    auth,
                    mcp_auth::middleware,
                )),
        )
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    fn response_json(raw: &str, id: u64) -> anyhow::Result<serde_json::Value> {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
            assert_eq!(value["id"], id);
            assert!(value.get("error").is_none(), "{value}");
            return Ok(value);
        }
        for line in raw.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                    if value["id"] == id {
                        assert!(value.get("error").is_none(), "{value}");
                        return Ok(value);
                    }
                }
            }
        }
        anyhow::bail!("no JSON-RPC result")
    }
    for version in ["2025-11-25", "2025-06-18", "2025-03-26"] {
        let response = client.post(&uri).bearer_auth(&credential.token)
            .header("accept", "application/json, text/event-stream")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":version,"capabilities":{},"clientInfo":{"name":"protocol-test","version":"1"}}}))
            .send().await?;
        assert!(response.status().is_success());
        let session = response
            .headers()
            .get("mcp-session-id")
            .unwrap()
            .to_str()?
            .to_string();
        let result = response_json(&response.text().await?, 1)?;
        assert_eq!(result["result"]["protocolVersion"], version);
        assert!(result["result"]["capabilities"]["tools"].is_object());
        let notification = client
            .post(&uri)
            .bearer_auth(&credential.token)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", version)
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await?;
        assert_eq!(notification.status(), reqwest::StatusCode::ACCEPTED);
        assert!(notification.bytes().await?.is_empty());
        let response = client
            .post(&uri)
            .bearer_auth(&credential.token)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", version)
            .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .send()
            .await?;
        assert!(response.status().is_success());
        let result = response_json(&response.text().await?, 2)?;
        for tool in result["result"]["tools"].as_array().unwrap() {
            assert!(!tool["description"].as_str().unwrap().is_empty());
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(tool["inputSchema"]["properties"].is_object());
        }
        let mut names = result["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["bash", "edit", "read", "web_publish", "write"]);
        let ping = client
            .post(&uri)
            .bearer_auth(&credential.token)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", version)
            .json(&json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
            .send()
            .await?;
        assert!(ping.status().is_success());
        assert!(response_json(&ping.text().await?, 3)?["result"].is_object());
        let invalid = client
            .post(&uri)
            .bearer_auth(&credential.token)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", "1900-01-01")
            .json(&json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}))
            .send()
            .await?;
        assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
        assert!(client
            .delete(&uri)
            .bearer_auth(&credential.token)
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", version)
            .send()
            .await?
            .status()
            .is_success());
    }
    assert_eq!(engine.create_count.load(Ordering::SeqCst), 0);
    ct.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await???;
    manager.stop_all().await;
    Ok(())
}

#[tokio::test]
async fn same_http_session_survives_idle_and_workspace_reclaim() -> anyhow::Result<()> {
    let mut config: Config = toml::from_str(
        &(include_str!("../../config.toml").to_owned()
            + "\n[users.alice]\ntoken = \"tok-alice-dev\"\nworkspace_name = \"smcp-alice\"\n"),
    )?;
    config.state.dir = None;
    config.webdav.enabled = false;
    assert_eq!(config.server.mcp_session_timeout_secs, 0);
    assert!(transport::session_manager(&config.server)
        .session_config
        .keep_alive
        .is_none());
    let config = Arc::new(config);
    let engine = Arc::new(MockEngine::new());
    let manager = Arc::new(SessionManager::new(
        engine.clone(),
        1,
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
        },
    ));
    let server_manager = manager.clone();
    let server_config = config.clone();
    let ct = tokio_util::sync::CancellationToken::new();
    let service = StreamableHttpService::new(
        move || {
            Ok(AgentSpaceMcpServer::with_state(
                server_config.clone(),
                server_manager.clone(),
                None,
            ))
        },
        transport::session_manager(&config.server).into(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let uri = format!("http://{}/u/alice/mcp", listener.local_addr()?);
    let shutdown = ct.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().nest_service("/u/{username}/mcp", service),
        )
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
    });
    let headers = HashMap::from([(
        http::HeaderName::from_static("authorization"),
        http::HeaderValue::from_static("Bearer tok-alice-dev"),
    )]);
    let client = ()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(uri).custom_headers(headers),
        ))
        .await?;
    client.list_all_tools().await?;
    assert_eq!(
        engine.create_count.load(Ordering::SeqCst),
        0,
        "handshake/discovery must be independent of VM capacity"
    );
    let call = || {
        CallToolRequestParams::new("bash")
            .with_arguments(json!({"cmd": "echo hello"}).as_object().unwrap().clone())
    };
    assert_ne!(client.call_tool(call()).await?.is_error, Some(true));
    let session = manager.get_session("smcp-alice").await?;
    manager.stop_workspace("smcp-alice").await?;
    assert_eq!(session.workspace_status().await, WorkspaceStatus::Stopped);
    client.list_all_tools().await?;
    client
        .send_request(rmcp::model::ClientRequest::PingRequest(Default::default()))
        .await?;
    assert_eq!(session.workspace_status().await, WorkspaceStatus::Stopped);

    // Cross rmcp 1.7's former 300-second inactivity expiry without a real wait.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::time::resume();
    assert_ne!(client.call_tool(call()).await?.is_error, Some(true));
    assert_eq!(session.workspace_status().await, WorkspaceStatus::Running);
    assert_eq!(engine.create_count.load(Ordering::SeqCst), 1);

    // A UTF-8 boundary in the log preview used to panic before executing.
    let long_command = "中".repeat(100);
    let unicode = CallToolRequestParams::new("bash")
        .with_arguments(json!({"cmd": long_command}).as_object().unwrap().clone());
    assert_ne!(client.call_tool(unicode).await?.is_error, Some(true));
    client.cancel().await?;
    ct.cancel();
    server.await??;
    manager.stop_all().await;
    Ok(())
}

#[tokio::test]
async fn another_valid_token_cannot_reuse_a_bound_mcp_session() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!("mcp-bind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    StateStore::init(&dir)?;
    let state = Arc::new(StateStore::open(&dir)?);
    let (alice, alice_token) = state.create_user("alice", "smcp", &["ws"])?;
    let (_bob, bob_token) = state.create_user("bob", "smcp", &["ws"])?;
    let config: Config = toml::from_str(include_str!("../../config.toml"))?;
    let config = Arc::new(config);
    let engine = Arc::new(MockEngine::new());
    let manager = Arc::new(SessionManager::new(
        engine.clone(),
        2,
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
        },
    ));
    let server_state = state.clone();
    let server_manager = manager.clone();
    let server_config = config.clone();
    let auth_state = mcp_auth::McpAuthState::new(config.clone(), Some(state.clone()));
    let service = StreamableHttpService::new(
        move || {
            Ok(AgentSpaceMcpServer::with_state(
                server_config.clone(),
                server_manager.clone(),
                Some(server_state.clone()),
            ))
        },
        auth_state.session_manager().into(),
        Default::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new()
                .nest_service("/u/{username}/mcp", service)
                .layer(axum::middleware::from_fn_with_state(
                    auth_state,
                    mcp_auth::middleware,
                )),
        )
        .await
    });
    let client = reqwest::Client::new();
    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}
    });
    let response = client
        .post(format!("http://{address}/u/alice/mcp"))
        .bearer_auth(&alice_token.token)
        .header("accept", "application/json, text/event-stream")
        .json(&init)
        .send()
        .await?;
    assert!(response.status().is_success(), "{}", response.status());
    let session = response
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()?
        .to_string();
    let tools = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
    let stolen = client
        .post(format!("http://{address}/u/alice/mcp"))
        .bearer_auth(&bob_token.token)
        .header("mcp-session-id", &session)
        .header("accept", "application/json, text/event-stream")
        .json(&tools)
        .send()
        .await?;
    assert!(
        !stolen.status().is_success() || stolen.text().await?.contains("error"),
        "bob must not reuse alice session"
    );
    let stolen_get = client
        .get(format!("http://{address}/u/alice/mcp"))
        .bearer_auth(&bob_token.token)
        .header("mcp-session-id", &session)
        .header("accept", "text/event-stream")
        .send()
        .await?;
    assert_eq!(stolen_get.status(), reqwest::StatusCode::FORBIDDEN);
    let stolen_delete = client
        .delete(format!("http://{address}/u/alice/mcp"))
        .bearer_auth(&bob_token.token)
        .header("mcp-session-id", &session)
        .send()
        .await?;
    assert_eq!(stolen_delete.status(), reqwest::StatusCode::FORBIDDEN);
    let own_path_with_stolen_session = client
        .post(format!("http://{address}/u/bob/mcp"))
        .bearer_auth(&bob_token.token)
        .header("mcp-session-id", &session)
        .header("accept", "application/json, text/event-stream")
        .json(&tools)
        .send()
        .await?;
    assert_eq!(
        own_path_with_stolen_session.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let alice_token_on_bob_path = client
        .post(format!("http://{address}/u/bob/mcp"))
        .bearer_auth(&alice_token.token)
        .header("accept", "application/json, text/event-stream")
        .json(&tools)
        .send()
        .await?;
    assert_eq!(
        alice_token_on_bob_path.status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let no_token_delete = client
        .delete(format!("http://{address}/u/alice/mcp"))
        .header("mcp-session-id", &session)
        .send()
        .await?;
    assert_eq!(no_token_delete.status(), reqwest::StatusCode::UNAUTHORIZED);
    let still_alive = client
        .post(format!("http://{address}/u/alice/mcp"))
        .bearer_auth(&alice_token.token)
        .header("mcp-session-id", &session)
        .header("accept", "application/json, text/event-stream")
        .json(&tools)
        .send()
        .await?;
    assert!(still_alive.status().is_success());
    state.rotate_token("alice", &alice.agent_space_id)?;
    let rotated = client
        .post(format!("http://{address}/u/alice/mcp"))
        .bearer_auth(&alice_token.token)
        .header("mcp-session-id", &session)
        .header("accept", "application/json, text/event-stream")
        .json(&tools)
        .send()
        .await?;
    let rotated_status = rotated.status();
    let rotated_body = rotated.text().await?;
    assert!(
        rotated_status == reqwest::StatusCode::UNAUTHORIZED
            || rotated_body.contains("error")
            || rotated_body.contains("no longer valid"),
        "{rotated_body}"
    );
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}
