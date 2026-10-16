use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_engine::config::Config;
use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, VolumeSpec, WorkspaceOpts};
use agent_engine::session::SessionManager;
use agent_space_mcp::server::AgentSpaceMcpServer;
use microsandbox::sandbox::SandboxStatus;
use microsandbox::Sandbox;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::json;
use serial_test::serial;

const IMAGE: &str = "nikolaik/python-nodejs:python3.12-nodejs22-slim";

fn config_toml(port: u16, workspace: &str, exec_timeout_secs: u64) -> String {
    format!(
        r#"
resource_prefix = "smcp"
[server]
listen = "127.0.0.1:{port}"
max_sessions = 2

[engine]
type = "msb"

[workspace]
image = "{IMAGE}"
cpus = 1
memory_mib = 512
workspace_size_mib = 256
tmpfs_size_mib = 64
shell = "/bin/bash"
exec_timeout_secs = {exec_timeout_secs}

[lifecycle]
session_timeout_secs = 900
idle_timeout_secs = 3600


[users.alice]
token = "tok-alice-test"
workspace_name = "{workspace}"
"#
    )
}

struct TestServer {
    port: u16,
    task: tokio::task::JoinHandle<()>,
    cancel: tokio_util::sync::CancellationToken,
    sessions: Arc<SessionManager>,
    workspace: String,
}

impl TestServer {
    async fn cleanup(self) -> anyhow::Result<()> {
        self.sessions.remove(&self.workspace).await?;
        self.cancel.cancel();
        self.task.await?;
        Ok(())
    }
}

async fn start_server(workspace: &str, exec_timeout_secs: u64) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(
        toml::from_str::<Config>(&config_toml(port, workspace, exec_timeout_secs)).unwrap(),
    );
    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;
    let opts = WorkspaceOpts {
        image: config
            .workspace
            .image
            .resolve()
            .unwrap()
            .reference()
            .to_owned(),
        cpus: config.workspace.cpus,
        memory_mib: config.workspace.memory_mib,
        shell: config.workspace.shell.clone(),
        volumes: vec![
            VolumeSpec::disk("/workspace", "ws", config.workspace.workspace_size_mib),
            VolumeSpec::tmpfs("/tmp", config.workspace.tmpfs_size_mib),
        ],
        network_access: config.workspace.network_access().unwrap_or_default(),
        outbound_proxy: None,
        max_tcp_connections: config.workspace.max_tcp_connections,
        max_udp_connections: config.workspace.max_udp_connections,
    };
    let sessions = Arc::new(SessionManager::new(
        engine,
        config.server.max_sessions,
        opts,
    ));
    let cancel = tokio_util::sync::CancellationToken::new();
    let server_cancel = cancel.clone();
    let config_for_service = config.clone();
    let sessions_for_service = sessions.clone();
    let sessions_for_shutdown = sessions.clone();
    let service = StreamableHttpService::new(
        move || {
            Ok(AgentSpaceMcpServer::new(
                config_for_service.clone(),
                sessions_for_service.clone(),
            ))
        },
        agent_space_mcp::transport::session_manager(&config.server).into(),
        StreamableHttpServerConfig::default().with_cancellation_token(cancel.child_token()),
    );
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().nest_service("/u/{username}/mcp", service),
        )
        .with_graceful_shutdown(async move {
            server_cancel.cancelled().await;
            sessions_for_shutdown.stop_all().await;
        })
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    TestServer {
        port,
        task,
        cancel,
        sessions,
        workspace: workspace.to_string(),
    }
}

fn client_config(port: u16, token: Option<&str>) -> StreamableHttpClientTransportConfig {
    let mut config = StreamableHttpClientTransportConfig::with_uri(format!(
        "http://127.0.0.1:{port}/u/alice/mcp"
    ));
    if let Some(token) = token {
        let mut headers = HashMap::new();
        headers.insert(
            http::HeaderName::from_static("authorization"),
            http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        config = config.custom_headers(headers);
    }
    config
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    arguments: serde_json::Value,
) -> anyhow::Result<rmcp::model::CallToolResult> {
    Ok(client
        .call_tool(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(arguments.as_object().cloned().unwrap_or_default()),
        )
        .await?)
}

async fn pause(workspace: &str) -> anyhow::Result<()> {
    let handle = Sandbox::get(workspace).await?;
    handle.pause().await?;
    assert_eq!(
        Sandbox::get(workspace).await?.status_snapshot(),
        SandboxStatus::Paused
    );
    Ok(())
}

async fn assert_running(workspace: &str) -> anyhow::Result<()> {
    assert_eq!(
        Sandbox::get(workspace).await?.status_snapshot(),
        SandboxStatus::Running
    );
    Ok(())
}

#[tokio::test]
#[serial]
async fn authentication_and_builtin_tool_contract() -> anyhow::Result<()> {
    let server = start_server("smcp-e2e-contract", 30).await;
    let port = server.port;
    let no_token = ()
        .serve(StreamableHttpClientTransport::from_config(client_config(
            port, None,
        )))
        .await;
    assert!(no_token.is_err());

    let client = ()
        .serve(StreamableHttpClientTransport::from_config(client_config(
            port,
            Some("tok-alice-test"),
        )))
        .await?;
    let tools = client.list_all_tools().await?;
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<HashSet<_>>();
    assert_eq!(
        names,
        HashSet::from(["bash", "read", "write", "edit", "web_publish"])
    );

    let bash = tools.iter().find(|tool| tool.name == "bash").unwrap();
    let schema = serde_json::to_value(&bash.input_schema)?;
    assert!(schema["properties"].get("timeout_secs").is_some());
    assert!(schema["properties"].get("timeout").is_none());
    assert!(schema["properties"]["timeout_secs"]["description"]
        .as_str()
        .unwrap()
        .contains("seconds"));

    client.cancel().await?;
    server.cleanup().await
}

#[tokio::test]
#[serial]
async fn configured_default_and_manual_bash_timeout_are_seconds() -> anyhow::Result<()> {
    let server = start_server("smcp-e2e-timeout", 1).await;
    let port = server.port;
    let client = ()
        .serve(StreamableHttpClientTransport::from_config(client_config(
            port,
            Some("tok-alice-test"),
        )))
        .await?;

    let started = Instant::now();
    let defaulted = call(&client, "bash", json!({"command": "sleep 2"})).await?;
    assert_eq!(defaulted.is_error, Some(true));
    assert!(started.elapsed() < Duration::from_secs(4));

    let overridden = call(
        &client,
        "bash",
        json!({"command": "sleep 1.2; printf ok", "timeout_secs": 3}),
    )
    .await?;
    assert_ne!(overridden.is_error, Some(true));

    let legacy = call(
        &client,
        "bash",
        json!({"command": "sleep 2", "timeout": 1000}),
    )
    .await?;
    assert_eq!(
        legacy.is_error,
        Some(true),
        "unknown timeout field is ignored; the configured one-second timeout still applies"
    );

    let zero = call(
        &client,
        "bash",
        json!({"command": "printf unsafe", "timeout_secs": 0}),
    )
    .await;
    assert!(zero.is_err(), "zero-second manual timeout must be rejected");

    let oversized = call(
        &client,
        "bash",
        json!({"command": "touch /workspace/oversized-timeout", "timeout_secs": u64::MAX}),
    )
    .await;
    assert!(oversized.is_err(), "overflowing deadline must be rejected");
    let intact = call(
        &client,
        "bash",
        json!({"command": "test ! -e /workspace/oversized-timeout && printf alive", "workdir":"", "timeout_secs":null,"extra":{"ignored":true}}),
    )
    .await?;
    assert_ne!(intact.is_error, Some(true));
    assert!(serde_json::to_string(&intact)?.contains("alive"));

    let missing_workdir = call(
        &client,
        "bash",
        json!({"command":"printf must-not-run", "workdir":"/workspace/missing-workdir-for-contract"}),
    )
    .await?;
    let text = serde_json::to_string(&missing_workdir)?;
    assert!(text.contains("missing-workdir-for-contract"));
    assert!(text.contains("Exit code: 1"));
    assert!(!text.contains("must-not-run"));

    client.cancel().await?;
    server.cleanup().await
}

#[tokio::test]
#[serial]
async fn every_builtin_tool_resumes_a_paused_workspace() -> anyhow::Result<()> {
    let workspace = "smcp-e2e-resume";
    let server = start_server(workspace, 30).await;
    let port = server.port;
    let client = ()
        .serve(StreamableHttpClientTransport::from_config(client_config(
            port,
            Some("tok-alice-test"),
        )))
        .await?;

    assert_ne!(
        call(
            &client,
            "write",
            json!({"path": "/workspace/a.txt", "content": "old"})
        )
        .await?
        .is_error,
        Some(true)
    );

    pause(workspace).await?;
    assert_ne!(
        call(&client, "bash", json!({"command": "true"}))
            .await?
            .is_error,
        Some(true)
    );
    assert_running(workspace).await?;

    pause(workspace).await?;
    assert_ne!(
        call(&client, "read", json!({"path": "/workspace/a.txt"}))
            .await?
            .is_error,
        Some(true)
    );
    assert_running(workspace).await?;

    pause(workspace).await?;
    assert_ne!(
        call(
            &client,
            "write",
            json!({"path": "/workspace/b.txt", "content": "data"})
        )
        .await?
        .is_error,
        Some(true)
    );
    assert_running(workspace).await?;

    pause(workspace).await?;
    assert_ne!(
        call(
            &client,
            "edit",
            json!({"path": "/workspace/a.txt", "old_text": "old", "new_text": "new"})
        )
        .await?
        .is_error,
        Some(true)
    );
    assert_running(workspace).await?;

    client.cancel().await?;
    server.cleanup().await
}

#[tokio::test]
#[serial]
async fn tool_call_starts_a_stopped_workspace_and_keeps_data() -> anyhow::Result<()> {
    let workspace = "smcp-e2e-wake";
    let server = start_server(workspace, 30).await;
    let port = server.port;
    let client = ()
        .serve(StreamableHttpClientTransport::from_config(client_config(
            port,
            Some("tok-alice-test"),
        )))
        .await?;
    assert_ne!(
        call(
            &client,
            "write",
            json!({"path": "/workspace/persist.txt", "content": "kept"})
        )
        .await?
        .is_error,
        Some(true)
    );

    Sandbox::get(workspace).await?.stop().await?;
    assert_ne!(
        call(&client, "read", json!({"path": "/workspace/persist.txt"}))
            .await?
            .is_error,
        Some(true)
    );
    assert_running(workspace).await?;

    client.cancel().await?;
    server.cleanup().await
}
