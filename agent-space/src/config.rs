use crate::engine::NetworkAccess;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default, skip_serializing)]
    pub engine: EngineConfig,
    #[serde(default)]
    pub workspace: WorkspaceDefaults,
    #[serde(default)]
    pub lifecycle: LifecycleConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub users: HashMap<String, UserConfig>,
    /// Resource prefix for MSB sandboxes/volumes (default: "smcp")
    #[serde(default = "default_resource_prefix")]
    pub resource_prefix: String,
    /// Dynamic user state and administrator credential source.
    #[serde(default)]
    pub state: StateConfig,
    /// Administrator credential source. The secret is read from the environment.
    #[serde(default)]
    pub admin: AdminConfig,
    #[serde(default)]
    pub webdav: WebdavConfig,
    #[serde(default)]
    pub web_publish: WebPublishConfig,
}

/// Settings consumed by workspace tools, independent of VM provisioning.
#[derive(Clone, Debug)]
pub struct ToolConfig {
    pub server: ServerConfig,
    pub workspace: ExecutionConfig,
    pub web_publish: WebPublishConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionConfig {
    pub shell: String,
    pub exec_timeout_secs: u64,
    pub max_output_lines: usize,
    pub max_output_bytes: usize,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            shell: "/bin/bash".into(),
            exec_timeout_secs: default_exec_timeout_secs(),
            max_output_lines: default_max_output_lines(),
            max_output_bytes: default_max_output_bytes(),
        }
    }
}

impl From<&Config> for ToolConfig {
    fn from(config: &Config) -> Self {
        Self {
            server: config.server.clone(),
            workspace: ExecutionConfig {
                shell: config.workspace.shell.clone(),
                exec_timeout_secs: config.workspace.exec_timeout_secs,
                max_output_lines: config.workspace.max_output_lines,
                max_output_bytes: config.workspace.max_output_bytes,
            },
            web_publish: config.web_publish.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebdavConfig {
    pub enabled: bool,
    pub listen: String,
    /// External DAV port for request-relative links (defaults to the listener port).
    pub public_port: Option<u16>,
    #[serde(deserialize_with = "optional_origin")]
    pub public_base_url: Option<String>,
    pub startup_timeout_secs: u64,
    pub io_idle_timeout_secs: u64,
}
impl Default for WebdavConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: "0.0.0.0:8933".into(),
            public_port: None,
            public_base_url: None,
            startup_timeout_secs: 60,
            io_idle_timeout_secs: 120,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebPublishConfig {
    pub enabled: bool,
    pub default_keep_alive_minutes: u32,
    pub max_keep_alive_minutes: u32,
    pub startup_timeout_secs: u64,
    pub io_idle_timeout_secs: u64,
}
impl Default for WebPublishConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_keep_alive_minutes: 10,
            max_keep_alive_minutes: 1440,
            startup_timeout_secs: 60,
            io_idle_timeout_secs: 120,
        }
    }
}

fn optional_origin<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.filter(|value| !value.trim().is_empty()))
}

fn default_resource_prefix() -> String {
    "smcp".to_string()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateConfig {
    #[serde(default)]
    pub dir: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    #[serde(default)]
    pub token_env: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Optional canonical link origin; empty means use the actual request entry.
    #[serde(default, deserialize_with = "optional_origin")]
    pub public_base_url: Option<String>,
    #[serde(
        default = "default_max_sessions",
        rename = "max_running_spaces",
        alias = "max_sessions"
    )]
    pub max_sessions: u32,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Protocol inactivity expiry, independent of VM reclamation. Zero disables
    /// expiry: clients may stay idle while their workspace sleeps.
    #[serde(default)]
    pub mcp_session_timeout_secs: u64,
}

impl ServerConfig {
    pub fn bind_addr(&self) -> String {
        self.listen.clone()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            public_base_url: None,
            max_sessions: default_max_sessions(),
            allowed_hosts: vec![],
            mcp_session_timeout_secs: 0,
        }
    }
}

fn default_listen() -> String {
    "0.0.0.0:8932".to_string()
}

fn default_max_sessions() -> u32 {
    2
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    #[serde(default = "default_engine_type")]
    pub r#type: String,
}

fn default_engine_type() -> String {
    "msb".to_string()
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            r#type: default_engine_type(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceDefaults {
    pub image: crate::image::ImageSelection,
    pub cpus: u32,
    pub memory_mib: u32,
    pub workspace_size_mib: u32,
    #[serde(default = "default_tmpfs_size_mib")]
    pub tmpfs_size_mib: u32,
    pub shell: String,
    #[serde(default = "default_exec_timeout_secs")]
    pub exec_timeout_secs: u64,
    #[serde(default = "default_max_output_lines")]
    pub max_output_lines: usize,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Network egress policy applied to every sandbox.
    /// Values: "unrestricted" (default), "public-only", "private-and-public", "none".
    #[serde(default = "default_network_policy")]
    pub network_policy: String,
    /// Native host-side SOCKS4/5 proxy. Passwords reference an environment
    /// variable; SDK launch resolution keeps them out of guest configuration.
    #[serde(default)]
    pub outbound_proxy: Option<OutboundProxyConfig>,
    /// Optional per-sandbox concurrent connection caps. Zero means unlimited.
    #[serde(default)]
    pub max_tcp_connections: Option<usize>,
    #[serde(default)]
    pub max_udp_connections: Option<usize>,
}

impl Default for WorkspaceDefaults {
    fn default() -> Self {
        Self {
            image: crate::image::ImageSelection::Preset(crate::image::PresetSelection {
                preset: crate::image::DESKTOP_PRESET.into(),
            }),
            cpus: 2,
            memory_mib: 3072,
            workspace_size_mib: 8192,
            tmpfs_size_mib: default_tmpfs_size_mib(),
            shell: "/bin/bash".into(),
            exec_timeout_secs: default_exec_timeout_secs(),
            max_output_lines: default_max_output_lines(),
            max_output_bytes: default_max_output_bytes(),
            network_policy: default_network_policy(),
            outbound_proxy: None,
            max_tcp_connections: None,
            max_udp_connections: None,
        }
    }
}

fn default_tmpfs_size_mib() -> u32 {
    512
}
fn default_exec_timeout_secs() -> u64 {
    30
}

fn default_max_output_lines() -> usize {
    2000
}
fn default_max_output_bytes() -> usize {
    51200
}

fn default_network_policy() -> String {
    "unrestricted".to_string()
}

/// Durable proxy settings without linking the common workspace core to MSB.
/// Secrets remain host-side references, resolved only by the MSB adapter.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "lowercase", deny_unknown_fields)]
pub enum OutboundProxyConfig {
    Socks4 {
        address: std::net::SocketAddr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_id: Option<String>,
    },
    Socks5 {
        address: std::net::SocketAddr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credentials: Option<ProxyCredentials>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyCredentials {
    username: String,
    password: ProxySecretSource,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxySecretSource {
    Env { var: String },
    Store { reference: String },
}

impl OutboundProxyConfig {
    pub fn address(&self) -> std::net::SocketAddr {
        match self {
            Self::Socks4 { address, .. } | Self::Socks5 { address, .. } => *address,
        }
    }

    #[cfg(feature = "msb")]
    pub fn to_msb(&self) -> Result<microsandbox::OutboundProxy, serde_json::Error> {
        serde_json::from_value(serde_json::to_value(self)?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleConfig {
    #[serde(rename = "request_idle_timeout_secs", alias = "session_timeout_secs")]
    pub session_timeout_secs: u64,
    #[serde(rename = "work_idle_timeout_secs", alias = "idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_check_interval")]
    pub check_interval_secs: u64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            session_timeout_secs: 900,
            idle_timeout_secs: 3600,
            check_interval_secs: default_check_interval(),
        }
    }
}

fn default_check_interval() -> u64 {
    60
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_format")]
    pub format: String,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default = "default_log_max_days")]
    pub max_days: u32,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
            dir: None,
            max_days: default_log_max_days(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "text".to_string()
}

fn default_log_max_days() -> u32 {
    7
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    pub token: String,
    pub workspace_name: String,
}

impl WorkspaceDefaults {
    /// Parse the `network_policy` string into a `NetworkAccess`.
    pub fn network_access(&self) -> Result<NetworkAccess, anyhow::Error> {
        match self.network_policy.trim().to_lowercase().as_str() {
            "unrestricted" => Ok(NetworkAccess::Unrestricted),
            "public-only" | "public" => Ok(NetworkAccess::PublicOnly),
            "private-and-public" | "private" => Ok(NetworkAccess::PrivateAndPublic),
            "none" | "off" => Ok(NetworkAccess::None),
            other => anyhow::bail!(
                "workspace.network_policy '{}' is invalid; expected one of: unrestricted, public-only, private-and-public, none",
                other
            ),
        }
    }
}

impl Config {
    /// Convert the pre-split schema3 configuration without touching its source
    /// or runtime state. Old implicit defaults are materialized, never replaced
    /// by the defaults for a new installation.
    pub fn convert_file(
        input: &std::path::Path,
        output: Option<&std::path::Path>,
        apply: bool,
    ) -> anyhow::Result<serde_json::Value> {
        use sha2::{Digest, Sha256};
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let original = std::fs::read_to_string(input)?;
        let config = Self::parse(&original, true)?;
        anyhow::ensure!(config.users.is_empty(),"import historical static users before converting a Server configuration; Standalone never accepts a user roster");
        let encoded = toml::to_string_pretty(&config)?;
        if apply {
            let destination =
                output.ok_or_else(|| anyhow::anyhow!("--output is required with --apply"))?;
            let destination = if destination.is_absolute() {
                destination.to_path_buf()
            } else {
                std::env::current_dir()?.join(destination)
            };
            anyhow::ensure!(!destination.exists(),"configuration destination already exists; source and existing files are never overwritten");
            let parent = destination
                .parent()
                .ok_or_else(|| anyhow::anyhow!("invalid destination"))?;
            let temporary = parent.join(format!(".runcove-config-{}.tmp", uuid::Uuid::new_v4()));
            let result = (|| -> anyhow::Result<()> {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temporary)?;
                file.write_all(encoded.as_bytes())?;
                file.sync_all()?;
                // Atomic no-overwrite publication, including a concurrent
                // creation of the destination after the preflight above.
                std::fs::hard_link(&temporary, &destination)?;
                std::fs::File::open(parent)?.sync_all()?;
                Ok(())
            })();
            let _ = std::fs::remove_file(&temporary);
            result?;
        }
        Ok(serde_json::json!({"target":"server","applied":apply,
            "sourceSha256":hex::encode(Sha256::digest(original.as_bytes())),
            "outputSha256":hex::encode(Sha256::digest(encoded.as_bytes())),
            "stateMigrationRequired":false,"credentialsRotated":false}))
    }

    pub fn load(path: &str) -> Result<Self, anyhow::Error> {
        Self::load_with_mode(path, false)
    }

    /// State initialization and import run offline, before the static roster
    /// is removed and before the admin HTTP credential is needed.
    pub fn load_for_state_command(path: &str) -> Result<Self, anyhow::Error> {
        Self::load_with_mode(path, true)
    }

    fn load_with_mode(path: &str, state_command: bool) -> Result<Self, anyhow::Error> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content, state_command)
    }

    fn parse(content: &str, state_command: bool) -> Result<Self, anyhow::Error> {
        let mut value: toml::Value = toml::from_str(content)?;
        let legacy = value.get("engine").is_some()
            || value
                .get("server")
                .is_some_and(|v| v.get("max_sessions").is_some())
            || value.get("lifecycle").is_some_and(|v| {
                v.get("session_timeout_secs").is_some() || v.get("idle_timeout_secs").is_some()
            });
        if legacy {
            let table = value
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("configuration must be a table"))?;
            let server = table
                .entry("server")
                .or_insert_with(|| toml::Value::Table(Default::default()))
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("server must be a table"))?;
            server
                .entry("listen")
                .or_insert_with(|| toml::Value::String("0.0.0.0:3100".into()));
            if !server.contains_key("max_sessions") && !server.contains_key("max_running_spaces") {
                server.insert("max_running_spaces".into(), toml::Value::Integer(12));
            }
        }
        let config: Config = value.try_into()?;
        if state_command {
            config.validate_with_mode(true)?;
        } else {
            config.validate()?;
        }
        Ok(config)
    }

    fn validate(&self) -> Result<(), anyhow::Error> {
        self.validate_with_mode(false)
    }

    fn validate_with_mode(&self, state_command: bool) -> Result<(), anyhow::Error> {
        anyhow::ensure!(
            self.server.max_sessions > 0,
            "server.max_sessions must be greater than zero"
        );
        anyhow::ensure!(self.engine.r#type == "msb", "engine.type must be 'msb'");
        anyhow::ensure!(
            (1..=u8::MAX as u32).contains(&self.workspace.cpus),
            "workspace.cpus must be between 1 and 255"
        );
        anyhow::ensure!(
            self.workspace.memory_mib > 0 && self.workspace.workspace_size_mib > 0,
            "workspace memory and disk sizes must be positive"
        );
        anyhow::ensure!(
            self.workspace.max_output_lines > 0 && self.workspace.max_output_bytes > 0,
            "workspace output limits must be positive"
        );
        self.workspace.image.validate()?;
        anyhow::ensure!(
            self.state.dir.is_some()
                || matches!(
                    self.workspace.image,
                    crate::image::ImageSelection::Custom(_)
                ),
            "desktop preset requires dynamic state"
        );
        let prefix = format!("{}-", self.resource_prefix);
        for (key, user) in &self.users {
            crate::state::validate_username(key)?;
            if !user.workspace_name.starts_with(&prefix) {
                anyhow::bail!(
                    "users.{}.workspace_name '{}' must start with resource_prefix '{}'",
                    key,
                    user.workspace_name,
                    prefix
                );
            }
        }
        if self.lifecycle.idle_timeout_secs <= self.lifecycle.session_timeout_secs {
            anyhow::bail!(
                "lifecycle.idle_timeout_secs ({}) must be greater than lifecycle.session_timeout_secs ({})",
                self.lifecycle.idle_timeout_secs, self.lifecycle.session_timeout_secs
            );
        }
        anyhow::ensure!(
            self.workspace.exec_timeout_secs > 0,
            "workspace.exec_timeout_secs must be greater than zero"
        );
        // Validate network_policy string (parse to confirm it is one of the known values).
        self.workspace.network_access()?;
        if self.state.dir.is_some() && !state_command {
            anyhow::ensure!(
                self.users.is_empty(),
                "users cannot remain configured after state.dir is enabled"
            );
            let name = self.admin.token_env.as_deref().unwrap_or("");
            anyhow::ensure!(
                !name.is_empty() && !name.contains('='),
                "admin.token_env is required when state.dir is configured"
            );
            anyhow::ensure!(
                !std::env::var(name).unwrap_or_default().is_empty(),
                "admin token environment variable is empty"
            );
        }
        if let Some(base) = &self.server.public_base_url {
            let url = url::Url::parse(base)
                .map_err(|error| anyhow::anyhow!("server.public_base_url: {error}"))?;
            anyhow::ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.host().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && url.path() == "/"
                    && base.trim_end_matches('/') == url.origin().ascii_serialization(),
                "server.public_base_url must be an http(s) origin without credentials, path, query, or fragment"
            );
        }
        anyhow::ensure!(self.web_publish.default_keep_alive_minutes > 0
            && self.web_publish.default_keep_alive_minutes <= self.web_publish.max_keep_alive_minutes
            && self.web_publish.max_keep_alive_minutes <= 1440
            && self.web_publish.startup_timeout_secs > 0 && self.web_publish.io_idle_timeout_secs > 0,
            "web_publish requires positive timeouts and 1 <= default_keep_alive_minutes <= max_keep_alive_minutes <= 1440");
        if self.webdav.enabled {
            anyhow::ensure!(
                self.state_dir().is_some() && self.users.is_empty(),
                "webdav requires dynamic state without static users"
            );
            anyhow::ensure!(
                self.webdav.startup_timeout_secs > 0 && self.webdav.io_idle_timeout_secs > 0,
                "webdav timeouts must be greater than zero"
            );
            let primary: std::net::SocketAddr = self.server.listen.parse()?;
            let dav: std::net::SocketAddr = self.webdav.listen.parse()?;
            anyhow::ensure!(
                primary.port() != dav.port() || primary.port() == 0,
                "webdav.listen must use a separate port from server.listen"
            );
            anyhow::ensure!(
                self.webdav.public_port != Some(0),
                "webdav.public_port must be nonzero"
            );
            if let Some(base) = &self.webdav.public_base_url {
                validate_dav_origin(base, &self.public_base_url())?;
            }
        }
        if let Some(proxy) = &self.workspace.outbound_proxy {
            anyhow::ensure!(
                proxy.address().port() != 0,
                "workspace.outbound_proxy address must have a nonzero port"
            );
        }
        Ok(())
    }

    pub fn get_user_by_token(&self, token: &str) -> Option<(&String, &UserConfig)> {
        self.users.iter().find(|(_, u)| u.token == token)
    }

    pub fn state_dir(&self) -> Option<&str> {
        self.state.dir.as_deref()
    }
    pub fn admin_token(&self) -> Option<String> {
        self.admin
            .token_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .filter(|value| !value.is_empty())
    }
    pub fn public_base_url(&self) -> String {
        if let Some(base) = &self.server.public_base_url {
            return base.trim_end_matches('/').to_string();
        }
        // A wildcard listener is never a client destination. The fallback is
        // for same-host development; deployments set public_base_url.
        let listen = self.server.listen.parse::<std::net::SocketAddr>();
        match listen {
            Ok(address) if address.ip().is_unspecified() => {
                format!("http://127.0.0.1:{}", address.port())
            }
            _ => format!("http://{}", self.server.listen),
        }
    }

    pub fn dav_base_url(&self) -> Option<&str> {
        self.webdav
            .enabled
            .then_some(self.webdav.public_base_url.as_deref())
            .flatten()
            .map(|base| base.trim_end_matches('/'))
    }

    pub fn dav_public_port(&self) -> u16 {
        self.webdav.public_port.unwrap_or_else(|| {
            self.webdav
                .listen
                .parse::<std::net::SocketAddr>()
                .map(|a| a.port())
                .unwrap_or(8933)
        })
    }

    pub fn mcp_url(&self, username: &str) -> String {
        format!("{}/u/{username}/mcp", self.public_base_url())
    }
}

/// Shared origin policy for the separate authenticated file listener.
pub fn validate_dav_origin(base: &str, primary: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(base)?;
    let local = match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
        None => false,
    };
    anyhow::ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && local),
        "webdav requires HTTPS except HTTP on loopback or private LAN IP addresses"
    );
    anyhow::ensure!(
        url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && base.trim_end_matches('/') == url.origin().ascii_serialization(),
        "webdav.public_base_url must be an origin without credentials, path, query, or fragment"
    );
    let primary = url::Url::parse(primary)?;
    anyhow::ensure!(
        url.host() != primary.host()
            || (url.port() != primary.port()
                && url.port_or_known_default() != primary.port_or_known_default()),
        "webdav and MCP/admin require separate host or port"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TOML: &str = r#"
[webdav]
enabled = false

[server]
listen = "127.0.0.1:8932"
max_sessions = 6

[engine]
type = "msb"

[workspace]
image = "nikolaik/python-nodejs:python3.12-nodejs22-slim"
cpus = 1
memory_mib = 512
workspace_size_mib = 1024
tmpfs_size_mib = 128
shell = "/bin/bash"
exec_timeout_secs = 30

[lifecycle]
session_timeout_secs = 900
idle_timeout_secs = 3600

[users.alice]
token = "tok-alice"
workspace_name = "smcp-alice"
"#;

    #[test]
    fn legacy_conversion_preserves_effective_values_without_overwriting_files() {
        use std::os::unix::fs::PermissionsExt;
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Directory(
            std::env::temp_dir().join(format!("runcove-config-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&dir.0).unwrap();
        let input = dir.0.join("old.toml");
        let output = dir.0.join("canonical.toml");
        let original = VALID_TOML
            .split("[users.alice]")
            .next()
            .unwrap()
            .replace("listen = \"127.0.0.1:8932\"\n", "")
            .replace("max_sessions = 6\n", "");
        std::fs::write(&input, &original).unwrap();
        let before = Config::load_for_state_command(input.to_str().unwrap()).unwrap();
        assert_eq!(before.server.listen, "0.0.0.0:3100");
        assert_eq!(before.server.max_sessions, 12);
        let preview = Config::convert_file(&input, Some(&output), false).unwrap();
        assert!(!output.exists());
        assert!(!preview.to_string().contains("tok-"));
        let result = Config::convert_file(&input, Some(&output), true).unwrap();
        assert_eq!(result["outputSha256"], preview["outputSha256"]);
        assert_eq!(std::fs::read_to_string(&input).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let converted = std::fs::read_to_string(&output).unwrap();
        assert!(!converted.contains("[engine]"));
        assert!(!converted.contains("max_sessions"));
        let after = Config::load_for_state_command(output.to_str().unwrap()).unwrap();
        assert_eq!(
            toml::to_string(&before).unwrap(),
            toml::to_string(&after).unwrap()
        );
        assert!(Config::convert_file(&input, Some(&output), true).is_err());
        assert!(Config::convert_file(&input, Some(&input), true).is_err());
        assert_eq!(
            Config::convert_file(&output, None, false).unwrap()["outputSha256"],
            preview["outputSha256"]
        );
        let conflicting =
            original.replace("[server]", "[server]\nmax_sessions=4\nmax_running_spaces=3");
        std::fs::write(&input, conflicting).unwrap();
        assert!(Config::convert_file(&input, None, false).is_err());
        assert_eq!(std::fs::read_to_string(&output).unwrap(), converted);
    }

    #[test]
    fn canonical_conversion_preserves_new_installation_defaults() {
        let directory =
            std::env::temp_dir().join(format!("runcove-canonical-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let input = directory.join("input.toml");
        let output = directory.join("output.toml");
        std::fs::write(&input, "[state]\ndir='/data/state'\n").unwrap();
        let before = Config::load_for_state_command(input.to_str().unwrap()).unwrap();
        Config::convert_file(&input, Some(&output), true).unwrap();
        let after = Config::load_for_state_command(output.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(before.server.listen, after.server.listen);
        assert_eq!(before.server.max_sessions, after.server.max_sessions);
    }

    #[test]
    fn invalid_workspace_resources_fail_before_provisioning_or_conversion() {
        for (field, value) in [
            ("cpus", 0),
            ("cpus", 256),
            ("cpus", 257),
            ("memory_mib", 0),
            ("workspace_size_mib", 0),
            ("max_output_lines", 0),
            ("max_output_bytes", 0),
        ] {
            let mut input: toml::Value = toml::from_str(VALID_TOML).unwrap();
            input["workspace"]
                .as_table_mut()
                .unwrap()
                .insert(field.to_owned(), toml::Value::Integer(value));
            assert!(
                Config::parse(&toml::to_string(&input).unwrap(), true).is_err(),
                "{field}={value}"
            );
        }
    }

    #[test]
    fn omitted_tmpfs_capacity_uses_512_mib() {
        let source = VALID_TOML.replace("tmpfs_size_mib = 128\n", "");
        let config: Config = toml::from_str(&source).unwrap();
        assert_eq!(config.workspace.tmpfs_size_mib, 512);
        assert_eq!(
            config.workspace.memory_mib, 512,
            "memory remains explicit configuration"
        );
    }

    #[test]
    fn webdav_allows_empty_origin_with_separate_listener_and_positive_timeouts() {
        let mut config: Config = toml::from_str(VALID_TOML).unwrap();
        assert!(!config.webdav.enabled);
        assert_eq!(config.dav_base_url(), None);
        config.users.clear();
        config.state.dir = Some("/tmp/test-state".into());
        config.webdav.enabled = true;
        config.validate_with_mode(true).unwrap();
        for base in [
            "https://files.example",
            "http://127.0.0.1:8933",
            "http://[::1]:8933",
            "http://localhost:8933/",
            "http://10.0.0.1:8934",
            "http://172.16.0.1:8934",
            "http://192.168.50.10:8934",
            "http://169.254.1.1:8934",
            "http://[fd00::1]:8934",
            "http://[fe80::1]:8934",
        ] {
            config.webdav.public_base_url = Some(base.into());
            config.validate_with_mode(true).unwrap();
        }
        for base in [
            "http://files.example",
            "http://8.8.8.8:8934",
            "http://172.32.0.1:8934",
            "http://[2001:4860:4860::8888]:8934",
            "http://127.0.0.1:8932",
            "https://files.example/path",
            "https://files.example?query",
            "https://files.example/#fragment",
            "https://user:pass@files.example",
            "ftp://files.example",
        ] {
            config.webdav.public_base_url = Some(base.into());
            assert!(config.validate_with_mode(true).is_err(), "{base}");
        }
        config.server.public_base_url = Some("http://files.example".into());
        config.webdav.public_base_url = Some("https://files.example".into());
        assert!(
            config.validate_with_mode(true).is_err(),
            "scheme-only split cannot be routed from Host"
        );
        config.server.public_base_url = None;
        config.webdav.public_base_url = Some("https://files.example".into());
        config.webdav.io_idle_timeout_secs = 0;
        assert!(config.validate_with_mode(true).is_err());
        config.webdav.io_idle_timeout_secs = 120;
        config.webdav.startup_timeout_secs = 0;
        assert!(config.validate_with_mode(true).is_err());
        config.webdav.startup_timeout_secs = 60;
        config.state.dir = None;
        assert!(config.validate_with_mode(true).is_err());
    }

    #[test]
    fn lan_webdav_still_requires_a_distinct_authority() {
        let mut config: Config = toml::from_str(VALID_TOML).unwrap();
        config.users.clear();
        config.state.dir = Some("/tmp/test-state".into());
        config.server.public_base_url = Some("http://192.168.50.10:8933".into());
        config.webdav.enabled = true;
        config.webdav.public_base_url = Some("http://192.168.50.10:8934".into());
        config.validate_with_mode(true).unwrap();

        config.webdav.public_base_url = Some("http://192.168.50.10:8933".into());
        assert!(config.validate_with_mode(true).is_err());
        config.webdav.public_base_url = Some("https://192.168.50.10:8933".into());
        assert!(config.validate_with_mode(true).is_err());
    }

    #[test]
    fn blank_origins_are_optional_and_dav_ports_are_validated() {
        let source = VALID_TOML
            .replace("[webdav]\nenabled = false\n", "")
            .replace(
                "max_sessions = 6",
                "max_sessions = 6\npublic_base_url = \"\"",
            );
        let mut config: Config =
            toml::from_str(&format!("{source}\n[webdav]\npublic_base_url = \"\"\n")).unwrap();
        assert!(config.server.public_base_url.is_none());
        assert!(config.webdav.public_base_url.is_none());
        config.users.clear();
        config.state.dir = Some("/tmp/test-state".into());
        config.webdav.enabled = true;
        config.validate_with_mode(true).unwrap();
        config.webdav.public_port = Some(0);
        assert!(config.validate_with_mode(true).is_err());
        config.webdav.public_port = Some(18933);
        assert_eq!(config.dav_public_port(), 18933);
        config.webdav.listen = config.server.listen.clone();
        assert!(config.validate_with_mode(true).is_err());
    }

    #[test]
    fn state_directory_typo_is_rejected_and_mcp_url_is_reachable() {
        let typo = format!("state_dir = \"/tmp/state\"\n{VALID_TOML}");
        assert!(toml::from_str::<Config>(&typo).is_err());

        let wildcard: Config =
            toml::from_str(&VALID_TOML.replace("127.0.0.1:8932", "0.0.0.0:8932")).unwrap();
        assert_eq!(
            wildcard.mcp_url("alice"),
            "http://127.0.0.1:8932/u/alice/mcp"
        );

        let public = VALID_TOML.replace(
            "max_sessions = 6",
            "max_sessions = 6\npublic_base_url = \"https://workspace.example\"",
        );
        let config: Config = toml::from_str(&public).unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.mcp_url("alice"),
            "https://workspace.example/u/alice/mcp"
        );
    }

    #[test]
    fn test_valid_toml() {
        let config: Config = toml::from_str(VALID_TOML).unwrap();
        assert_eq!(config.server.listen, "127.0.0.1:8932");
        assert_eq!(config.server.max_sessions, 6);
        assert_eq!(config.engine.r#type, "msb");
        assert_eq!(
            config.workspace.image,
            "nikolaik/python-nodejs:python3.12-nodejs22-slim".into()
        );
        assert_eq!(config.workspace.cpus, 1);
        assert_eq!(config.workspace.memory_mib, 512);
        assert_eq!(config.workspace.shell, "/bin/bash");
        assert_eq!(config.users.len(), 1);
        assert_eq!(config.users["alice"].token, "tok-alice");
    }

    #[test]
    fn test_missing_optional_fields_use_defaults() {
        let minimal = r#"
[server]
listen = "0.0.0.0:3100"
max_sessions = 4

[engine]

[workspace]
image = "test:latest"
cpus = 1
memory_mib = 256
workspace_size_mib = 512
tmpfs_size_mib = 64
shell = "/bin/sh"

[lifecycle]
session_timeout_secs = 300
idle_timeout_secs = 600
"#;
        let config: Config = toml::from_str(minimal).unwrap();
        assert!(config.webdav.enabled); // DAV enabled even when the section is omitted
        assert_eq!(config.webdav.listen, "0.0.0.0:8933");
        assert!(config.webdav.public_base_url.is_none());
        assert_eq!(config.engine.r#type, "msb"); // default
        assert_eq!(config.workspace.max_output_lines, 2000); // default
        assert_eq!(config.workspace.max_output_bytes, 51200); // default
        assert_eq!(config.workspace.exec_timeout_secs, 30);
        assert_eq!(config.lifecycle.check_interval_secs, 60); // default
        assert_eq!(config.logging.level, "info"); // default
        assert!(config.users.is_empty()); // no users defined
    }

    #[test]
    fn removed_bridge_configuration_is_rejected() {
        for removed in [
            "bridge_enabled = false",
            "bridge_binary = \"/usr/local/bin/mcp-bridge\"",
        ] {
            let source =
                VALID_TOML.replace("type = \"msb\"", &format!("type = \"msb\"\n{removed}"));
            let error = toml::from_str::<Config>(&source).unwrap_err();
            assert!(error
                .to_string()
                .contains(removed.split(' ').next().unwrap()));
        }
    }

    #[test]
    fn test_invalid_toml() {
        let result: Result<Config, _> = toml::from_str("not valid toml {{{");
        assert!(result.is_err());
    }

    #[test]
    fn test_get_user_by_token() {
        let config: Config = toml::from_str(VALID_TOML).unwrap();
        let (key, user) = config.get_user_by_token("tok-alice").unwrap();
        assert_eq!(key.as_str(), "alice");
        assert_eq!(user.workspace_name, "smcp-alice");
        assert!(config.get_user_by_token("nonexistent").is_none());
    }

    #[test]
    fn test_logging_defaults() {
        let config: Config = toml::from_str(VALID_TOML).unwrap();
        assert_eq!(config.logging.level, "info");
        assert_eq!(config.logging.format, "text");
        assert_eq!(config.logging.dir, None);
        assert_eq!(config.logging.max_days, 7);
    }

    #[test]
    fn test_logging_with_dir() {
        let toml = r#"
[server]
listen = "127.0.0.1:8932"
max_sessions = 6

[engine]
type = "msb"

[workspace]
image = "test:latest"
cpus = 1
memory_mib = 256
workspace_size_mib = 512
tmpfs_size_mib = 64
shell = "/bin/sh"
exec_timeout_secs = 10

[lifecycle]
session_timeout_secs = 300
idle_timeout_secs = 600

[logging]
level = "debug"
format = "json"
dir = "/var/log/agent-space"
max_days = 14
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.logging.level, "debug");
        assert_eq!(config.logging.format, "json");
        assert_eq!(config.logging.dir, Some("/var/log/agent-space".to_string()));
        assert_eq!(config.logging.max_days, 14);
    }

    #[test]
    fn test_logging_partial_override() {
        let toml = r#"
[server]
listen = "127.0.0.1:8932"
max_sessions = 6

[engine]
type = "msb"

[workspace]
image = "test:latest"
cpus = 1
memory_mib = 256
workspace_size_mib = 512
tmpfs_size_mib = 64
shell = "/bin/sh"
exec_timeout_secs = 10

[lifecycle]
session_timeout_secs = 300
idle_timeout_secs = 600

[logging]
dir = "/tmp/logs"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.logging.level, "info"); // default
        assert_eq!(config.logging.format, "text"); // default
        assert_eq!(config.logging.dir, Some("/tmp/logs".to_string()));
        assert_eq!(config.logging.max_days, 7); // default
    }

    #[test]
    fn test_validation_workspace_name_must_have_prefix() {
        let toml = r#"
[server]
listen = "127.0.0.1:8932"
max_sessions = 6

[engine]
type = "msb"

[workspace]
image = "test:latest"
cpus = 1
memory_mib = 256
workspace_size_mib = 512
tmpfs_size_mib = 64
shell = "/bin/sh"
exec_timeout_secs = 10

[lifecycle]
session_timeout_secs = 300
idle_timeout_secs = 600

[users.alice]
token = "tok-alice"
workspace_name = "sb-alice"
"#;
        let result: Result<Config, _> = toml::from_str(toml);
        let config = result.unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("must start with resource_prefix"));
    }

    #[test]
    fn test_validation_idle_must_exceed_session() {
        let toml = r#"
[server]
listen = "127.0.0.1:8932"
max_sessions = 6

[engine]
type = "msb"

[workspace]
image = "test:latest"
cpus = 1
memory_mib = 256
workspace_size_mib = 512
tmpfs_size_mib = 64
shell = "/bin/sh"
exec_timeout_secs = 10

[lifecycle]
session_timeout_secs = 3600
idle_timeout_secs = 900

[users.alice]
token = "tok-alice"
workspace_name = "smcp-alice"
"#;
        let result: Result<Config, _> = toml::from_str(toml);
        let config = result.unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("must be greater than"));
    }

    #[test]
    fn test_validation_requires_positive_session_capacity() {
        let config: Config =
            toml::from_str(&VALID_TOML.replace("max_sessions = 6", "max_sessions = 0")).unwrap();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("max_sessions"));
    }

    #[test]
    fn test_validation_rejects_unknown_engine_type() {
        let config: Config =
            toml::from_str(&VALID_TOML.replace("type = \"msb\"", "type = \"other\"")).unwrap();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("engine.type"));
    }

    #[test]
    fn test_network_policy_default_unrestricted() {
        let config: Config = toml::from_str(VALID_TOML).unwrap();
        assert_eq!(config.workspace.network_policy, "unrestricted");
        assert!(config.workspace.outbound_proxy.is_none());
        assert_eq!(
            config.workspace.network_access().unwrap(),
            crate::engine::NetworkAccess::Unrestricted
        );
    }

    #[test]
    fn test_network_policy_parsing() {
        use crate::engine::NetworkAccess;

        let toml = r#"
[server]
listen = "127.0.0.1:8932"
max_sessions = 6

[engine]
type = "msb"

[workspace]
image = "test:latest"
cpus = 1
memory_mib = 256
workspace_size_mib = 512
tmpfs_size_mib = 64
shell = "/bin/sh"
exec_timeout_secs = 10
network_policy = "public-only"

[lifecycle]
session_timeout_secs = 300
idle_timeout_secs = 600
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.workspace.network_access().unwrap(),
            NetworkAccess::PublicOnly
        );

        // Case-insensitive + aliases
        let t2 = toml.replace("public-only", "NONE");
        let c2: Config = toml::from_str(&t2).unwrap();
        assert_eq!(c2.workspace.network_access().unwrap(), NetworkAccess::None);

        let t3 = toml.replace("public-only", "private");
        let c3: Config = toml::from_str(&t3).unwrap();
        assert_eq!(
            c3.workspace.network_access().unwrap(),
            NetworkAccess::PrivateAndPublic
        );
    }

    #[test]
    fn test_network_policy_invalid() {
        let toml = r#"
[server]
listen = "127.0.0.1:8932"
max_sessions = 6

[engine]
type = "msb"

[workspace]
image = "test:latest"
cpus = 1
memory_mib = 256
workspace_size_mib = 512
tmpfs_size_mib = 64
shell = "/bin/sh"
exec_timeout_secs = 10
network_policy = "bogus"

[lifecycle]
session_timeout_secs = 300
idle_timeout_secs = 600
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(
            config.validate().is_err(),
            "invalid network_policy should fail validation"
        );
    }

    #[cfg(feature = "msb")]
    #[test]
    fn test_to_profiles_mapping() {
        use crate::engine::NetworkAccess;
        assert_eq!(NetworkAccess::Unrestricted.to_profiles(), None);
        assert_eq!(
            NetworkAccess::PublicOnly.to_profiles(),
            Some(vec![microsandbox::NetworkProfile::Public])
        );
        assert_eq!(
            NetworkAccess::PrivateAndPublic.to_profiles(),
            Some(vec![
                microsandbox::NetworkProfile::Public,
                microsandbox::NetworkProfile::Private,
            ])
        );
        assert_eq!(NetworkAccess::None.to_profiles(), Some(vec![]));
    }

    #[test]
    fn native_proxy_config_uses_host_secret_reference() {
        let source = VALID_TOML.replace(
            "[lifecycle]",
            r#"
[workspace.outbound_proxy]
protocol = "socks5"
address = "127.0.0.1:1080"
[workspace.outbound_proxy.credentials]
username = "workspace"
password = { kind = "env", var = "MSB_PROXY_PASSWORD" }
[lifecycle]"#,
        );
        let config: Config = toml::from_str(&source).unwrap();
        config.validate().unwrap();
        let json = serde_json::to_value(config.workspace.outbound_proxy).unwrap();
        assert_eq!(json["credentials"]["password"]["var"], "MSB_PROXY_PASSWORD");
        assert!(toml::from_str::<Config>(
            &source.replace("127.0.0.1:1080", "socks5://localhost:1080")
        )
        .is_err());
        assert!(toml::from_str::<Config>(&source.replace("socks5", "http")).is_err());
        let zero: Config = toml::from_str(&source.replace(":1080", ":0")).unwrap();
        assert!(zero.validate().is_err());
    }

    #[test]
    fn public_base_url_builds_user_routes_without_secrets() {
        let raw = VALID_TOML.replace(
            "max_sessions = 6",
            "max_sessions = 6\npublic_base_url = \"https://workspace.example/\"",
        );
        let config: Config = toml::from_str(&raw).unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.mcp_url("alice"),
            "https://workspace.example/u/alice/mcp"
        );
        assert!(config.web_publish.enabled);
        assert_eq!(config.web_publish.default_keep_alive_minutes, 10);
        assert_eq!(config.web_publish.max_keep_alive_minutes, 1440);
        let mut invalid = config.clone();
        invalid.web_publish.max_keep_alive_minutes = 1441;
        assert!(invalid.validate().is_err());
        assert!(toml::from_str::<Config>(
            &(raw.clone() + "\n[web_publish]\npublic_base_url = \"https://old.example\"")
        )
        .is_err());
        let disabled: Config =
            toml::from_str(&(raw.clone() + "\n[web_publish]\nenabled = false")).unwrap();
        disabled.validate().unwrap();
        assert!(!disabled.web_publish.enabled);
        let bad = VALID_TOML.replace(
            "max_sessions = 6",
            "max_sessions = 6\npublic_base_url = \"https://user:secret@workspace.example\"",
        );
        assert!(toml::from_str::<Config>(&bad).unwrap().validate().is_err());
        let old = VALID_TOML.replace(
            "max_sessions = 6",
            "max_sessions = 6\npublic_mcp_url = \"https://workspace.example/mcp\"",
        );
        assert!(toml::from_str::<Config>(&old).is_err());
        let mixed = VALID_TOML.to_string()
            + "[state]\ndir = \"/tmp/state\"\n[admin]\ntoken_env = \"AGENT_SPACE_ADMIN_TOKEN\"\n";
        std::env::set_var("AGENT_SPACE_ADMIN_TOKEN", "admin-secret");
        assert!(toml::from_str::<Config>(&mixed)
            .unwrap()
            .validate()
            .is_err());
        std::env::remove_var("AGENT_SPACE_ADMIN_TOKEN");
    }
}
