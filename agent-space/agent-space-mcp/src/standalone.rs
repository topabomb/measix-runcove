//! Single-container configuration and service assembly. No VM provisioning.
use crate::{
    desktop, mcp_auth::McpAuthState, origins::OriginPolicy, server::AgentSpaceMcpServer,
    web_publish, webdav,
};
use agent_engine::{
    config::{
        ExecutionConfig, LoggingConfig, ServerConfig, ToolConfig, WebPublishConfig, WebdavConfig,
    },
    engine::{process::ProcessEngine, NetworkAccess, WorkspaceOpts},
    image::{ImageSelection, PresetSelection, DESKTOP_PRESET},
    session::SessionManager,
    state::StateStore,
};
use axum::{http::StatusCode, response::Redirect, routing::get, Router};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StandaloneConfig {
    pub browser: agent_engine::config::BrowserConfig,
    pub server: EntryConfig,
    pub auth: AccessConfig,
    pub execution: ExecutionConfig,
    pub logging: LoggingConfig,
    pub webdav: DavConfig,
    pub web_publish: PublicationConfig,
}

/// Public-port projection belongs to Server's management API; this direct
/// file listener exposes only settings it actually consumes.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DavConfig {
    pub enabled: bool,
    pub listen: String,
    pub public_base_url: Option<String>,
    pub startup_timeout_secs: u64,
    pub io_idle_timeout_secs: u64,
}
impl Default for DavConfig {
    fn default() -> Self {
        let c = WebdavConfig::default();
        Self {
            enabled: false,
            listen: c.listen,
            public_base_url: None,
            startup_timeout_secs: c.startup_timeout_secs,
            io_idle_timeout_secs: c.io_idle_timeout_secs,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EntryConfig {
    pub listen: String,
    pub public_base_url: Option<String>,
    pub allowed_hosts: Vec<String>,
    pub mcp_session_timeout_secs: u64,
}
impl Default for EntryConfig {
    fn default() -> Self {
        let config = ServerConfig::default();
        Self {
            listen: config.listen,
            public_base_url: None,
            allowed_hosts: vec![],
            mcp_session_timeout_secs: config.mcp_session_timeout_secs,
        }
    }
}
impl From<&EntryConfig> for ServerConfig {
    fn from(config: &EntryConfig) -> Self {
        Self {
            listen: config.listen.clone(),
            public_base_url: config
                .public_base_url
                .clone()
                .filter(|s| !s.trim().is_empty()),
            allowed_hosts: config.allowed_hosts.clone(),
            mcp_session_timeout_secs: config.mcp_session_timeout_secs,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccessConfig {
    pub token_env: String,
    pub dav_token_env: String,
}
impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            token_env: "RUNCOVE_TOKEN".into(),
            dav_token_env: "RUNCOVE_DAV_TOKEN".into(),
        }
    }
}

/// Standalone has no VM keep-alive policy. Tool parameters remain compatible.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PublicationConfig {
    pub enabled: bool,
    pub startup_timeout_secs: u64,
    pub io_idle_timeout_secs: u64,
}
impl Default for PublicationConfig {
    fn default() -> Self {
        let config = WebPublishConfig::default();
        Self {
            enabled: config.enabled,
            startup_timeout_secs: config.startup_timeout_secs,
            io_idle_timeout_secs: config.io_idle_timeout_secs,
        }
    }
}
impl From<&PublicationConfig> for WebPublishConfig {
    fn from(config: &PublicationConfig) -> Self {
        Self {
            enabled: config.enabled,
            startup_timeout_secs: config.startup_timeout_secs,
            io_idle_timeout_secs: config.io_idle_timeout_secs,
            ..Self::default()
        }
    }
}

impl StandaloneConfig {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        self.browser.validate()?;
        let listen: std::net::SocketAddr = self.server.listen.parse()?;
        for base in [&self.server.public_base_url, &self.webdav.public_base_url]
            .into_iter()
            .flatten()
        {
            if base.trim().is_empty() {
                continue;
            }
            let url = url::Url::parse(base)?;
            anyhow::ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.host().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && base.trim_end_matches('/') == url.origin().ascii_serialization(),
                "public_base_url must be an origin without credentials or path"
            );
        }
        for name in [&self.auth.token_env, &self.auth.dav_token_env] {
            anyhow::ensure!(
                !name.is_empty() && !name.contains(['=', '\0']),
                "invalid credential environment reference"
            );
        }
        anyhow::ensure!(
            self.execution.exec_timeout_secs > 0
                && self.execution.max_output_lines > 0
                && self.execution.max_output_bytes > 0
                && self.execution.shell == "/bin/bash",
            "execution requires Bash and positive limits"
        );
        anyhow::ensure!(
            self.web_publish.startup_timeout_secs > 0 && self.web_publish.io_idle_timeout_secs > 0,
            "publication timeouts must be positive"
        );
        if self.webdav.enabled {
            let dav: std::net::SocketAddr = self.webdav.listen.parse()?;
            anyhow::ensure!(
                dav.port() != listen.port(),
                "DAV requires a separate listener port"
            );
            anyhow::ensure!(
                self.webdav.startup_timeout_secs > 0 && self.webdav.io_idle_timeout_secs > 0,
                "DAV timeouts must be positive"
            );
            if let Some(base) = self
                .webdav
                .public_base_url
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            {
                let primary = self
                    .server
                    .public_base_url
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| format!("http://127.0.0.1:{}", listen.port()));
                agent_engine::config::validate_dav_origin(base, &primary)?;
            }
        }
        Ok(())
    }
}

pub async fn serve(config: StandaloneConfig) -> anyhow::Result<()> {
    config.validate()?;
    anyhow::ensure!(
        unsafe { libc::geteuid() } == 0,
        "Standalone controller must start as root; user execution runs as UID1001"
    );
    let token = std::env::var(&config.auth.token_env)
        .map_err(|_| anyhow::anyhow!("access credential environment variable is missing"))?;
    let dav_token = if config.webdav.enabled {
        Some(
            std::env::var(&config.auth.dav_token_env)
                .map_err(|_| anyhow::anyhow!("DAV credential environment variable is missing"))?,
        )
    } else {
        None
    };
    let image = ImageSelection::Preset(PresetSelection {
        preset: DESKTOP_PRESET.into(),
    })
    .resolve()?;
    let authority = Arc::new(StateStore::standalone(
        &token,
        dav_token.as_deref(),
        image.clone(),
    )?);
    drop((token, dav_token));
    // Provisioning fields are unused by the existing-container adapter. No
    // VM, mount, resource allocation or reclamation loop is created here.
    let opts = WorkspaceOpts {
        image: image.reference().into(),
        cpus: 0,
        memory_mib: 0,
        shell: config.execution.shell.clone(),
        volumes: vec![],
        network_access: NetworkAccess::Unrestricted,
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    };
    let manager = Arc::new(
        SessionManager::new(Arc::new(ProcessEngine::new()), 1, opts)
            .with_browser(config.browser.clone())
            .with_limits(
                config.execution.max_output_lines,
                config.execution.max_output_bytes,
            ),
    );
    let server = ServerConfig::from(&config.server);
    let tools = Arc::new(ToolConfig {
        server: server.clone(),
        workspace: config.execution.clone(),
        web_publish: WebPublishConfig::from(&config.web_publish),
    });
    let auth = McpAuthState::with_authority(server.clone(), Some(authority.clone()), None);
    let shutdown = CancellationToken::new();
    let mut transport = StreamableHttpServerConfig::default()
        .with_stateful_mode(true)
        .with_cancellation_token(shutdown.child_token());
    transport = if server.allowed_hosts.is_empty() {
        transport.disable_allowed_hosts()
    } else {
        transport.with_allowed_hosts(&server.allowed_hosts)
    };
    let (tool_manager, tool_authority) = (manager.clone(), authority.clone());
    let mcp = StreamableHttpService::new(
        move || {
            Ok(AgentSpaceMcpServer::with_settings(
                tools.clone(),
                tool_manager.clone(),
                Some(tool_authority.clone()),
            ))
        },
        auth.session_manager().into(),
        transport,
    );
    let mcp = Router::new()
        .nest_service("/mcp", mcp)
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::mcp_auth::middleware,
        ));
    let ui = std::env::var("AGENT_SPACE_DESKTOP_UI").unwrap_or_else(|_| "desktop-ui/dist".into());
    let health_authority = authority.clone();
    let app = Router::new()
        .route("/", get(|| async { Redirect::temporary("desktop/") }))
        .route(
            "/healthz",
            get(move || {
                let authority = health_authority.clone();
                async move {
                    if authority.check_readable().is_ok() {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }
            }),
        )
        .merge(mcp)
        .merge(desktop::router(desktop::DesktopState::new(
            manager.clone(),
            authority.clone(),
            ui,
        )))
        .layer(axum::middleware::from_fn_with_state(
            web_publish::PublishState {
                sessions: manager.clone(),
                state: Some(authority.clone()),
                origin: url::Url::parse(
                    server
                        .public_base_url
                        .as_deref()
                        .unwrap_or("http://127.0.0.1"),
                )?,
                enabled: config.web_publish.enabled,
                startup_timeout: Duration::from_secs(config.web_publish.startup_timeout_secs),
                io_idle_timeout: Duration::from_secs(config.web_publish.io_idle_timeout_secs),
            },
            web_publish::dispatch,
        ))
        .layer(axum::middleware::from_fn_with_state(
            OriginPolicy::new(server.public_base_url.as_deref())?,
            crate::origins::origin_boundary,
        ));
    let listener = tokio::net::TcpListener::bind(&server.listen).await?;
    let mut dav_task = None;
    if config.webdav.enabled {
        let dav_listener = tokio::net::TcpListener::bind(&config.webdav.listen).await?;
        let app = webdav::router(webdav::DavState {
            sessions: manager.clone(),
            state: authority.clone(),
            origin: url::Url::parse(
                config
                    .webdav
                    .public_base_url
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("http://127.0.0.1"),
            )?,
            helper_binary: std::env::var("AGENT_SPACE_WEBDAV")
                .unwrap_or_else(|_| "/usr/local/bin/agent-space-webdav".into()),
            startup_timeout: Duration::from_secs(config.webdav.startup_timeout_secs),
            io_idle_timeout: Duration::from_secs(config.webdav.io_idle_timeout_secs),
        });
        let stopped = shutdown.clone();
        dav_task = Some(tokio::spawn(async move {
            axum::serve(dav_listener, app)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
        }));
    }
    let stopped = shutdown.clone();
    let signal_manager = manager.clone();
    let signal = async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("TERM handler");
        tokio::select! {_=term.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
        stopped.cancel();
        signal_manager.stop_all().await;
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(signal)
        .await?;
    shutdown.cancel();
    if let Some(task) = dav_task {
        task.await??;
    }
    Ok(())
}

use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_limit_uses_shared_defaults_and_validation() {
        let config: StandaloneConfig = toml::from_str("").unwrap();
        assert_eq!(config.browser.max_pages, 4);
        let config: StandaloneConfig = toml::from_str("[browser]\nmax_pages=2").unwrap();
        config.validate().unwrap();
        assert_eq!(config.browser.max_pages, 2);
        let config: StandaloneConfig = toml::from_str("[browser]\nmax_pages=0").unwrap();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("browser.max_pages"));
        assert!(toml::from_str::<StandaloneConfig>("[browser]\nmax_tabs=4").is_err());
    }
    #[test]
    fn dav_defaults_off_and_preserves_common_private_or_https_origin_boundary() {
        let config: StandaloneConfig = toml::from_str("[webdav]").unwrap();
        assert!(!config.webdav.enabled);
        for base in [
            "http://127.0.0.1:8933",
            "http://192.168.1.2:8933",
            "https://files.example",
        ] {
            let mut config = StandaloneConfig::default();
            config.webdav.enabled = true;
            config.webdav.public_base_url = Some(base.into());
            config.validate().unwrap();
        }
        for base in [
            "http://127.0.0.1:8932",
            "http://files.example",
            "https://files.example/path",
        ] {
            let mut config = StandaloneConfig::default();
            config.webdav.enabled = true;
            config.webdav.public_base_url = Some(base.into());
            assert!(config.validate().is_err(), "{base}");
        }
        assert!(toml::from_str::<StandaloneConfig>("[webdav]\npublic_port=9000").is_err());
    }

    #[test]
    fn single_space_configuration_rejects_management_vm_and_unused_keepalive_fields() {
        for input in [
            "[users.alice]\ntoken='secret'",
            "[spaces.work]\ntoken='secret'",
            "[state]\ndir='/data'",
            "[engine]\ntype='msb'",
            "[server]\nmax_running_spaces=2",
            "[web_publish]\ndefault_keep_alive_minutes=10",
        ] {
            assert!(
                toml::from_str::<StandaloneConfig>(input).is_err(),
                "{input}"
            );
        }
        let config: StandaloneConfig = toml::from_str("").unwrap();
        config.validate().unwrap();
        assert_eq!(config.server.listen, ServerConfig::default().listen);
    }
}
