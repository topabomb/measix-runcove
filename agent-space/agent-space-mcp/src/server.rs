use std::sync::Arc;

use crate::browser_params::{ActAction, ActParams, BrowserReadParams, SnapshotParams};
use crate::file_read::ReadParams;
use crate::publish_params::{self, WebPublishParams};
use agent_engine::auth;
use agent_engine::config::{Config, ToolConfig};
use agent_engine::publication::{Publication, PublishTarget};
use agent_engine::session::{Session, SessionManager};
use agent_engine::state::{StateError, StateStore, UserRecord};
use rmcp::{
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext, wrapper::Parameters},
    model::*,
    service::RequestContext,
    tool, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{error, info, instrument, warn};

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct BashParams {
    #[schemars(
        description = "Complete Bash command or script, including arguments. Use pipes, here-documents or redirection for input. Keep reserved Runcove paths intact: /opt/agent-space, /tmp/agent-space, /workspace/.agent-space."
    )]
    cmd: String,
    #[schemars(description = "Execution limit in seconds; omit or null uses the space default.")]
    #[serde(default)]
    #[schemars(range(min = 1))]
    timeout_secs: Option<u64>,
    #[schemars(
        description = "Working directory for this call; default /workspace, the persistent work directory. Prefer an absolute path."
    )]
    #[serde(default)]
    #[serde(deserialize_with = "publish_params::optional_string")]
    workdir: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct WriteParams {
    #[schemars(
        description = "Destination file path in the connected Runcove space. Prefer an absolute path; /workspace stores persistent files."
    )]
    path: String,
    #[schemars(
        description = "Complete nonempty replacement text. Empty content is rejected to prevent accidental file clearing."
    )]
    #[schemars(length(min = 1))]
    content: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct EditParams {
    #[schemars(
        description = "Existing UTF-8 file in the connected Runcove space. Prefer an absolute path."
    )]
    path: String,
    #[schemars(
        description = "Exact text to match, including whitespace and line breaks. Exclude line-number prefixes from read."
    )]
    old_text: String,
    #[schemars(description = "Replacement text; an empty string deletes the match.")]
    new_text: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ScreenView {
    Desktop,
    Page,
}
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct ScreenParams {
    #[schemars(
        description = "Required: \"page\" captures the selected page's complete currently rendered document, including iframe regions as displayed; \"desktop\" captures the entire Runcove desktop, including native dialogs. Independent scroll areas and iframes show their currently displayed contents. Does not load lazy content or expand virtual lists."
    )]
    view: ScreenView,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "For view=\"page\": page ID from browser_snapshot(view='tabs'). Default: the focused tab, or an available tab. Selected once for the sequence; closing it ends capture with an error. Ignored for view=\"desktop\"."
    )]
    page_id: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 1, max = 8))]
    #[schemars(description = "Number of images to capture: 1-8; default 1.")]
    count: Option<usize>,
    #[serde(default)]
    #[schemars(range(min = 0))]
    #[schemars(
        description = "Minimum seconds between capture starts when count>1; default 1, zero captures back to back. Ignored for count=1. Actual intervals may be longer; the sequence must fit within 600 seconds."
    )]
    interval_secs: Option<u64>,
    #[serde(default)]
    #[schemars(
        description = "Also save original images and return their paths in files; default false."
    )]
    save_original: Option<bool>,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "For save_original=true: absolute directory in the connected space; default /workspace/agent-space/artifacts. Creates missing directories without overwriting files. Otherwise ignored."
    )]
    save_dir: Option<String>,
}
#[derive(Clone)]
struct BoundIdentity {
    username: String,
    agent_space_id: String,
    key_id: String,
    sandbox_name: String,
}

pub struct AgentSpaceMcpServer {
    pub config: Arc<ToolConfig>,
    legacy_config: Option<Arc<Config>>,
    pub session_manager: Arc<SessionManager>,
    pub state: Option<Arc<StateStore>>,
    pub session: Mutex<Option<Session>>,
    identity: Mutex<Option<BoundIdentity>>,
    pub tool_router: ToolRouter<Self>,
    browser: std::sync::atomic::AtomicBool,
    feedback: Mutex<crate::browser_feedback::Feedback>,
}

impl AgentSpaceMcpServer {
    pub fn new(config: Arc<Config>, session_manager: Arc<SessionManager>) -> Self {
        Self::with_state(config, session_manager, None)
    }

    pub fn with_state(
        config: Arc<Config>,
        session_manager: Arc<SessionManager>,
        state: Option<Arc<StateStore>>,
    ) -> Self {
        let settings = Arc::new(ToolConfig::from(config.as_ref()));
        let mut server = Self::with_settings(settings, session_manager, state);
        server.legacy_config = Some(config);
        server
    }

    pub fn with_settings(
        config: Arc<ToolConfig>,
        session_manager: Arc<SessionManager>,
        state: Option<Arc<StateStore>>,
    ) -> Self {
        Self {
            config,
            legacy_config: None,
            session_manager,
            state,
            session: Mutex::new(None),
            identity: Mutex::new(None),
            tool_router: Self::tool_router(),
            browser: std::sync::atomic::AtomicBool::new(false),
            feedback: Mutex::new(crate::browser_feedback::Feedback::default()),
        }
    }

    async fn browser_feedback(
        &self,
        result: Result<serde_json::Value, agent_engine::engine::EngineError>,
    ) -> CallToolResult {
        let transport_error = result.is_err();
        let mut value = match result {
            Ok(value) => value,
            Err(error) => {
                let message = match &error {
                    agent_engine::engine::EngineError::Other(message) => message.clone(),
                    _ => error.to_string(),
                };
                serde_json::from_str(&message).unwrap_or_else(
                    |_| json!({"error":browser_error(&message),"side_effects":"unknown"}),
                )
            }
        };
        self.feedback.lock().await.accept(&mut value);
        let mut response = browser_result(Ok(value));
        if transport_error {
            response.is_error = Some(true);
        }
        response
    }

    async fn recheck_bound(&self) -> Result<(), McpError> {
        let Some(identity) = self.identity.lock().await.clone() else {
            return Ok(());
        };
        if let Some(state) = &self.state {
            state
                .authorize_bound_current(
                    &identity.username,
                    &identity.agent_space_id,
                    &identity.key_id,
                )
                .map_err(auth_error)?;
        }
        Ok(())
    }
    async fn ensure_session(&self) -> Result<Session, McpError> {
        self.session.lock().await.clone().ok_or_else(|| {
            McpError::invalid_request(
                "session not initialized",
                Some(json!({"reason": "call initialize first"})),
            )
        })
    }

    fn bearer<'a>(&self, context: &'a RequestContext<RoleServer>) -> Result<&'a str, McpError> {
        let header = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get(http::header::AUTHORIZATION))
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                McpError::invalid_params("missing Authorization: Bearer <token> header", None)
            })?;
        auth::extract_bearer_token(header)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))
    }

    fn legacy_user(&self, token: &str) -> Result<UserRecord, McpError> {
        let config = self
            .legacy_config
            .as_ref()
            .ok_or_else(|| McpError::invalid_request("workspace authority unavailable", None))?;
        let (username, user) = config
            .get_user_by_token(token)
            .ok_or_else(|| McpError::invalid_params("invalid token", None))?;
        Ok(UserRecord {
            image: Some(
                config
                    .workspace
                    .image
                    .resolve()
                    .map_err(|e| McpError::internal_error(compact_error(&e.to_string()), None))?,
            ),
            username: username.clone(),
            agent_space_id: format!("spc_legacy_{}", user.workspace_name),
            status: agent_engine::state::UserStatus::Active,
            credential: Some(agent_engine::state::Credential {
                key_id: format!("spc_legacy_{}", user.workspace_name),
                secret_digest: String::new(),
            }),
            dav_credential: None,
            sandbox_name: user.workspace_name.clone(),
            owned_volume_names: Vec::new(),
            cleanup: None,
        })
    }

    fn authenticate(&self, token: &str) -> Result<UserRecord, McpError> {
        if let Some(state) = &self.state {
            return state.authenticate(token).map_err(auth_error);
        }
        self.legacy_user(token)
    }

    async fn bind(&self, token: &str) -> Result<BoundIdentity, McpError> {
        let user = self.authenticate(token)?;
        let image = user
            .image
            .as_ref()
            .ok_or_else(|| McpError::internal_error("missing space image", None))?;
        self.session_manager
            .register_image(&user.sandbox_name, image.reference())
            .map_err(|e| McpError::internal_error(compact_error(&e.to_string()), None))?;
        self.browser
            .store(image.browser(), std::sync::atomic::Ordering::SeqCst);
        if image.browser() {
            self.session_manager
                .desktop(&user.sandbox_name)
                .await
                .map_err(|e| McpError::internal_error(compact_error(&e.to_string()), None))?;
        }
        let identity = BoundIdentity {
            username: user.username.clone(),
            agent_space_id: user.agent_space_id.clone(),
            key_id: user
                .credential
                .as_ref()
                .map(|c| c.key_id.clone())
                .unwrap_or_default(),
            sandbox_name: user.sandbox_name.clone(),
        };
        let session = self
            .session_manager
            .get_session(&identity.sandbox_name)
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let session = if let Some(state) = &self.state {
            let state = state.clone();
            let bound = identity.clone();
            session.with_admission_check(move || {
                state
                    .authorize_bound_current(&bound.username, &bound.agent_space_id, &bound.key_id)
                    .map(|_| ())
                    .map_err(|error| {
                        agent_engine::engine::EngineError::Other(format!(
                            "workspace authorization failed: {error}"
                        ))
                    })
            })
        } else {
            session
        };
        session.touch().await;
        *self.session.lock().await = Some(session);
        *self.identity.lock().await = Some(identity.clone());
        Ok(identity)
    }

    async fn recheck(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<BoundIdentity, McpError> {
        let token = self.bearer(context)?;
        let bound = self.identity.lock().await.clone().ok_or_else(|| {
            McpError::invalid_request(
                "session not initialized",
                Some(json!({"reason": "call initialize first"})),
            )
        })?;
        let user = if let Some(state) = &self.state {
            state
                .authorize_bound(token, &bound.username, &bound.agent_space_id, &bound.key_id)
                .map_err(auth_error)?
        } else {
            let user = self.legacy_user(token)?;
            if user.sandbox_name != bound.sandbox_name {
                return Err(McpError::invalid_params(
                    "credential does not match this MCP session",
                    None,
                ));
            }
            user
        };
        if user.sandbox_name != bound.sandbox_name {
            return Err(McpError::invalid_params(
                "credential does not match this MCP session",
                None,
            ));
        }
        Ok(bound)
    }

    fn default_exec_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.workspace.exec_timeout_secs)
    }
}

impl AgentSpaceMcpServer {
    fn tool_enabled(&self, name: &str) -> bool {
        !(name.starts_with("browser_") || name == "screen_observe")
            || self.browser.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn disclosed_tool(&self, mut tool: Tool) -> Tool {
        let mut schema = serde_json::Value::Object((*tool.input_schema).clone());
        compact_base_schema(&mut schema);
        match tool.name.as_ref() {
            "bash" => {
                let seconds = self.config.workspace.exec_timeout_secs;
                schema["properties"]["timeout_secs"]["default"] = json!(seconds);
                schema["properties"]["timeout_secs"]["description"] = json!(format!(
                    "Execution limit in seconds; default {seconds}. Omit or null uses this default."
                ));
                schema["properties"]["workdir"]["default"] = json!("/workspace");
            }
            "read" => {
                schema["properties"]["offset"]["default"] = json!(1);
                schema["properties"]["limit"]["default"] = json!(500);
                schema["properties"]["multimodal"]["default"] = json!(false);
            }
            "web_publish" => {
                let seconds = u64::from(self.config.web_publish.default_keep_alive_minutes) * 60;
                let maximum = u64::from(self.config.web_publish.max_keep_alive_minutes) * 60;
                schema["properties"]["keep_alive_secs"]["default"] = json!(seconds);
                schema["properties"]["keep_alive_secs"]["description"] = json!(format!(
                    "Publish only: idle-shutdown protection in seconds, 60-{maximum}; default {seconds}. Omit or null uses this default. Re-publish to extend protection. Expiry ends idle protection; publication lifetime follows the space."
                ));
            }
            "browser_snapshot" => {
                for (key, value) in [
                    ("view", json!("page")),
                    ("diff", json!(false)),
                    ("image", json!(false)),
                    ("text", json!(false)),
                    ("recent_tasks_limit", json!(0)),
                ] {
                    schema["properties"][key]["default"] = value;
                }
            }
            "browser_read" => {
                schema["properties"]["max_chars"]["default"] = json!(4000);
            }
            "browser_act" => {
                for (key, value) in [
                    ("op", json!("execute")),
                    ("wait_secs", json!(30)),
                    ("then", json!("diff")),
                    ("action_timeout_secs", json!(5)),
                    ("timeout_secs", json!(120)),
                ] {
                    schema["properties"][key]["default"] = value;
                }
            }
            "screen_observe" => {
                for (key, value) in [
                    ("count", json!(1)),
                    ("interval_secs", json!(1)),
                    ("save_original", json!(false)),
                ] {
                    schema["properties"][key]["default"] = value;
                }
            }
            _ => {}
        }
        tool.input_schema = Arc::new(schema.as_object().unwrap().clone());
        tool
    }
    async fn desktop(&self) -> Result<std::sync::Arc<agent_engine::desktop::DesktopHub>, McpError> {
        self.recheck_bound().await?;
        if !self.browser.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(McpError::invalid_request(
                "browser tools require the desktop preset",
                None,
            ));
        }
        let session = self.ensure_session().await?;
        self.session_manager
            .desktop(&session.workspace_name)
            .await
            .map_err(|e| McpError::internal_error(compact_error(&e.to_string()), None))
    }
    fn publication_result(
        &self,
        item: &Publication,
        credentials: bool,
        base: &str,
    ) -> serde_json::Value {
        let mut result = json!({"name":item.name,"type":item.target.kind(),
            "url":format!("{}{}", base, crate::web_publish::publication_prefix(self.state.as_deref(), item))});
        if credentials {
            result["username"] = json!(item.access_username);
            result["password"] = json!(item.password);
        }
        result
    }
}

#[tool_router]
impl AgentSpaceMcpServer {
    #[tool(
        description = "Observe current-page elements and panels, including iframes. Use view=tabs only to find or manage pages. Use observed refs unchanged; query browser_read to find a known target. Add image=true only when visual context is needed."
    )]
    async fn browser_snapshot(
        &self,
        Parameters(mut params): Parameters<SnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.view == Some(crate::browser_params::SnapshotView::Tabs) {
            params.page_id = None;
            params.r#ref = None;
            params.diff = None;
            params.image = None;
            params.text = None;
        }
        if params.recent_tasks_limit.is_some_and(|n| n > 10) {
            return Err(McpError::invalid_params(
                "recent_tasks_limit must be 0-10",
                None,
            ));
        }
        if params.r#ref.is_some() && params.diff == Some(true) {
            return Err(McpError::invalid_params(
                "ref selects a subtree snapshot; omit diff or omit ref",
                None,
            ));
        }
        if params
            .r#ref
            .as_deref()
            .is_some_and(|r| !crate::browser_params::is_ref(r))
        {
            return Err(McpError::invalid_params(
                "ref must come from browser_snapshot or browser_read",
                None,
            ));
        }
        let hub = self.desktop().await?;
        let session = self.ensure_session().await?;
        let mut args = json!({"view":params.view,"diff":params.diff.unwrap_or(false),"ref":params.r#ref,"image":params.image.unwrap_or(false),"text":params.text.unwrap_or(false),"recent_tasks_limit":params.recent_tasks_limit.unwrap_or(0)});
        self.feedback
            .lock()
            .await
            .prepare(&mut args, params.page_id.as_deref());
        Ok(self
            .browser_feedback(
                hub.browser(session, "snapshot", args, params.page_id, 60)
                    .await,
            )
            .await)
    }
    #[tool(
        description = "Execute browser or desktop steps in order, with page feedback at the end of the batch. Use op=wait or op=cancel with task_id for an existing task."
    )]
    async fn browser_act(
        &self,
        Parameters(params): Parameters<ActParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.normalize();
        params
            .validate()
            .map_err(|e| McpError::invalid_params(e, None))?;
        let hub = self.desktop().await?;
        match params.op.unwrap_or(ActAction::Execute) {
            ActAction::Wait | ActAction::Cancel => Ok(self
                .browser_feedback(
                    hub.task(
                        params.task_id.as_deref().unwrap(),
                        params.wait_secs.unwrap_or(30),
                        matches!(params.op, Some(ActAction::Cancel)),
                    )
                    .await,
                )
                .await),
            ActAction::Execute => {
                let session = self.ensure_session().await?;
                let mut args = json!({"actions":params.actions.unwrap(),"then":params.then,"action_timeout_secs":params.action_timeout_secs});
                self.feedback
                    .lock()
                    .await
                    .prepare(&mut args, params.page_id.as_deref());
                Ok(self
                    .browser_feedback(
                        hub.browser(
                            session,
                            "act",
                            args,
                            params.page_id,
                            params.timeout_secs.unwrap_or(120),
                        )
                        .await,
                    )
                    .await)
            }
        }
    }
    #[tool(
        description = "Read page text or extract tables, links and fields, including iframes. Use query to find a known target and obtain refs; use ref to scope a read. For page structure and panels, use browser_snapshot."
    )]
    async fn browser_read(
        &self,
        Parameters(params): Parameters<BrowserReadParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.normalize();
        params
            .validate()
            .map_err(|e| McpError::invalid_params(e, None))?;
        let hub = self.desktop().await?;
        let session = self.ensure_session().await?;
        let args = json!({"ref":params.r#ref,"query":params.query,"extract":params.extract,"max_chars":params.max_chars});
        Ok(browser_result(
            hub.browser(session, "read", args, params.page_id, 60).await,
        ))
    }
    #[tool(
        description = "Capture unannotated page or desktop images. Page captures cover the currently rendered full document; use browser_read for long text. For native browser UI use view=desktop. Supports timed sequences and saving originals. Use returned image_id and image-pixel coordinates for actions. While state=running, retrieve the same task with browser_act(op=\"wait\")."
    )]
    async fn screen_observe(
        &self,
        Parameters(mut params): Parameters<ScreenParams>,
    ) -> Result<CallToolResult, McpError> {
        let count = params.count.unwrap_or(1);
        let interval = if count == 1 {
            0
        } else {
            params.interval_secs.unwrap_or(1)
        };
        if !(1..=8).contains(&count) || (count as u64 - 1).saturating_mul(interval) >= 600 {
            return Err(McpError::invalid_params(
                "count must be 1..8 and the capture schedule must fit within 600 seconds",
                None,
            ));
        }
        let save_original = params.save_original.unwrap_or(false);
        if !save_original {
            params.save_dir = None;
        }
        if matches!(params.view, ScreenView::Desktop) {
            params.page_id = None;
        }
        if let Some(directory) = &params.save_dir {
            if !std::path::Path::new(directory).is_absolute() || directory.contains('\0') {
                return Err(McpError::invalid_params(
                    "save_dir must be an absolute directory path",
                    None,
                ));
            }
        }
        let hub = self.desktop().await?;
        let session = self.ensure_session().await?;
        Ok(browser_result(
            hub.screen(
                session,
                agent_engine::desktop::ScreenOptions {
                    page_id: params.page_id,
                    view: match params.view {
                        ScreenView::Desktop => "desktop".into(),
                        ScreenView::Page => "page".into(),
                    },
                    count,
                    interval_ms: interval * 1000,
                    save_original,
                    save_dir: params.save_dir,
                },
            )
            .await,
        ))
    }
    #[tool(
        description = "Publish a file, directory or running HTTP app in the connected Runcove space. Reusing a name preserves its URL and credentials. Give the user the URL and HTTP Basic username/password. Publications end when the space stops or Runcove restarts."
    )]
    async fn web_publish(
        &self,
        Parameters(params): Parameters<WebPublishParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let session = self.ensure_session().await?;
        self.recheck_bound().await?;
        session.refresh_publications().await;
        let identity = self
            .identity
            .lock()
            .await
            .clone()
            .ok_or_else(|| McpError::invalid_request("session not initialized", None))?;
        let base = if let Some(base) = &self.config.server.public_base_url {
            base.trim_end_matches('/').to_string()
        } else {
            let parts = context
                .extensions
                .get::<http::request::Parts>()
                .ok_or_else(|| {
                    McpError::invalid_request("HTTP request entry is unavailable", None)
                })?;
            let mut request = axum::extract::Request::new(axum::body::Body::empty());
            *request.uri_mut() = parts.uri.clone();
            *request.headers_mut() = parts.headers.clone();
            crate::origins::OriginPolicy::new(self.config.server.public_base_url.as_deref())
                .unwrap()
                .resolve(&request)
                .map_err(|_| McpError::invalid_request("invalid HTTP request entry", None))?
                .origin()
                .ascii_serialization()
        };
        let body = match params {
            WebPublishParams::List {} => json!(session
                .publications()
                .iter()
                .map(|item| self.publication_result(item, false, &base))
                .collect::<Vec<_>>()),
            WebPublishParams::Get { name } => {
                publish_params::validate_name(&name)
                    .map_err(|e| McpError::invalid_params(e, None))?;
                let item = session
                    .publication(&name)
                    .ok_or_else(|| McpError::invalid_params("publication not found", None))?;
                self.publication_result(&item, true, &base)
            }
            WebPublishParams::Unpublish { name } => {
                publish_params::validate_name(&name)
                    .map_err(|e| McpError::invalid_params(e, None))?;
                session.unpublish(&name).await;
                json!({"name": name, "status":"unpublished"})
            }
            WebPublishParams::Publish {
                name,
                file,
                directory,
                port,
                keep_alive_secs,
            } => {
                publish_params::validate_name(&name)
                    .map_err(|e| McpError::invalid_params(e, None))?;
                let target = publish_params::target(file, directory, port)
                    .map_err(|e| McpError::invalid_params(e, None))?;
                let settings = &self.config.web_publish;
                if !settings.enabled {
                    return Err(McpError::invalid_request(
                        "web publishing is disabled: enable web_publish.enabled",
                        None,
                    ));
                }
                let seconds = keep_alive_secs
                    .map(u64::from)
                    .unwrap_or(u64::from(settings.default_keep_alive_minutes) * 60);
                let maximum = u64::from(settings.max_keep_alive_minutes) * 60;
                if seconds < 60 || seconds > maximum {
                    return Err(McpError::invalid_params(
                        format!("keep_alive_secs must be 60-{}", maximum),
                        None,
                    ));
                }
                let duration = std::time::Duration::from_secs(seconds);
                let _gate = session.publication_gate().await;
                self.recheck_bound().await?;
                let previous = session.publication(&name);
                if let Some(existing) = previous.as_ref().filter(|item| item.target == target) {
                    // Probe without waking a stopped VM. An old URL never resumes a new runtime.
                    session.probe_guest_tcp(existing.port).await.map_err(|e| {
                        McpError::invalid_params(
                            compact_error(&format!("published service is unavailable: {e}")),
                            None,
                        )
                    })?;
                    self.recheck_bound().await?;
                    let item = session.extend_publication(&name, duration).ok_or_else(|| {
                        McpError::invalid_request("publication ended with its VM", None)
                    })?;
                    self.publication_result(&item, true, &base)
                } else {
                    session.retry_helper_cleanup().await.map_err(|e| {
                        McpError::internal_error(
                            compact_error(&format!("helper cleanup pending: {e}")),
                            None,
                        )
                    })?;
                    let (port, helper) = match &target {
                        PublishTarget::Port(port) => {
                            if session
                                .publications()
                                .iter()
                                .any(|item| item.helper.is_some() && item.port == *port)
                                || session.managed_port(*port)
                            {
                                return Err(McpError::invalid_params(
                                    "a managed service port cannot be published",
                                    None,
                                ));
                            }
                            (*port, None)
                        }
                        PublishTarget::File(path) | PublishTarget::Directory(path) => {
                            let helper = session
                                .start_managed(agent_engine::engine::ManagedLaunch::StaticHttp {
                                    host_binary: std::env::var("AGENT_SPACE_STATIC_HTTP")
                                        .unwrap_or_else(|_| {
                                            "/usr/local/bin/agent-space-static-http".into()
                                        }),
                                    root: path.clone(),
                                    file: matches!(&target, PublishTarget::File(_)),
                                })
                                .await
                                .map_err(|e| {
                                    McpError::invalid_params(compact_error(&e.to_string()), None)
                                })?;
                            (helper.port, Some(helper))
                        }
                    };
                    let result = async {
                        session.probe_guest_tcp(port).await.map_err(|e| {
                            McpError::invalid_params(
                                compact_error(&format!("service is not reachable: {e}")),
                                None,
                            )
                        })?;
                        self.recheck_bound().await?;
                        let mut item = if let Some(previous) = &previous {
                            let mut item = previous.clone();
                            item.target = target;
                            item.port = port;
                            item.helper = helper.clone();
                            item.epoch = Arc::new(tokio_util::sync::CancellationToken::new());
                            item.extend(duration);
                            item
                        } else {
                            Publication::new(
                                name,
                                identity.username,
                                identity.agent_space_id,
                                target,
                                port,
                                helper.clone(),
                                duration,
                            )
                            .map_err(|e| {
                                McpError::internal_error(compact_error(&e.to_string()), None)
                            })?
                        };
                        if let Some(previous) = &previous {
                            let retired = session
                                .replace_publication(item.clone(), previous)
                                .map_err(|e| {
                                    McpError::internal_error(compact_error(&e.to_string()), None)
                                })?;
                            if let Some(helper) = retired.and_then(|old| old.helper) {
                                if session.stop_helper(&helper).await.is_ok() {
                                    session.clear_helper_cleanup(&helper);
                                }
                            }
                        } else {
                            item.reserve_username(&session.publications())
                                .map_err(|e| {
                                    McpError::internal_error(compact_error(&e.to_string()), None)
                                })?;
                            session.publish(item.clone()).map_err(|e| {
                                McpError::internal_error(compact_error(&e.to_string()), None)
                            })?;
                        }
                        Ok::<_, McpError>(self.publication_result(&item, true, &base))
                    }
                    .await;
                    if result.is_err() {
                        if let Some(helper) = &helper {
                            session.note_helper_cleanup(helper);
                            if session.stop_helper(helper).await.is_ok() {
                                session.clear_helper_cleanup(helper);
                            }
                        }
                    }
                    result?
                }
            }
        };
        Ok(CallToolResult::success(vec![Content::text(
            body.to_string(),
        )]))
    }

    #[instrument(skip(self, params), fields(workspace), level = "info")]
    #[tool(
        description = "Run Bash commands in the connected Runcove space. Each call has a separate non-interactive shell. Returns bounded, merged stdout/stderr and an exit code; check the exit code for success. Redirect background-service input from /dev/null and output to a log file. Returns started_at and duration_secs."
    )]
    async fn bash(
        &self,
        Parameters(params): Parameters<BashParams>,
    ) -> Result<CallToolResult, McpError> {
        let timeout_secs = params
            .timeout_secs
            .unwrap_or(self.config.workspace.exec_timeout_secs);
        if timeout_secs == 0 {
            return Err(McpError::invalid_params(
                "timeout_secs must be greater than zero",
                None,
            ));
        }
        let timeout = std::time::Duration::from_secs(timeout_secs);
        // Include the engine's bounded supervision grace in the check. An
        // unrepresentable agent-supplied deadline must fail before queueing work.
        if timeout
            .checked_add(std::time::Duration::from_secs(8))
            .and_then(|budget| std::time::Instant::now().checked_add(budget))
            .is_none()
        {
            return Err(McpError::invalid_params(
                "timeout_secs exceeds the supported execution deadline",
                None,
            ));
        }
        let start = std::time::Instant::now();
        let started_at = time::OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .unwrap()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let workdir = params.workdir.as_deref().unwrap_or("/workspace");
        self.recheck_bound().await?;
        let session = self.ensure_session().await?;
        let cmd = format!(
            "{{ cd {} && {{\n{}\n}}; }} 2>&1",
            shell_escape(workdir),
            params.cmd
        );

        tracing::Span::current().record("workspace", session.workspace_name.as_str());
        info!(workspace = %session.workspace_name, command_bytes = params.cmd.len(), timeout_secs, "bash exec");
        let result: Result<CallToolResult, McpError> = match session.exec(&cmd, timeout).await {
            Ok(result) => {
                info!(
                    workspace = %session.workspace_name,
                    exit_code = result.exit_code,
                    stdout_len = result.stdout.len(),
                    truncated = result.truncated,
                    duration_ms = start.elapsed().as_millis(),
                    "bash done"
                );
                let output = if result.stdout.is_empty() {
                    "(no output)".to_string()
                } else if result.truncated {
                    format!("{}\n(output truncated)", result.stdout)
                } else {
                    result.stdout
                };
                Ok(CallToolResult::success(vec![
                    Content::text(output),
                    Content::text(format!("Exit code: {}", result.exit_code)),
                ]))
            }
            Err(agent_engine::engine::EngineError::ExecutionEnded {
                output,
                timed_out,
                cleanup_confirmed,
            }) => Ok(CallToolResult::error(vec![Content::text(
                serde_json::json!({
                    "error": if timed_out {"EXECUTION_TIMEOUT"} else {"EXECUTION_CANCELLED"},
                    "stdout": output.stdout, "stderr": output.stderr, "truncated": output.truncated,
                    "exit_code": output.exit_code, "cleanup_confirmed": cleanup_confirmed,
                    "side_effects": "may_have_occurred"
                })
                .to_string(),
            )])),
            Err(agent_engine::engine::EngineError::Timeout(message)) => {
                warn!(
                    workspace = %session.workspace_name,
                    command_bytes = params.cmd.len(),
                    timeout_secs,
                    duration_ms = start.elapsed().as_millis(),
                    "bash timed out"
                );
                Ok(CallToolResult::error(vec![
                    Content::text(compact_error(&format!(
                        "(timeout after {timeout_secs}s) {message}"
                    ))),
                    Content::text("Exit code: -1"),
                ]))
            }
            Err(agent_engine::engine::EngineError::SandboxNotRunning(message)) => {
                error!(workspace = %session.workspace_name, tool = "bash", error = %message, "workspace not running during bash exec");
                Ok(CallToolResult::error(vec![Content::text(compact_error(
                    &format!("workspace not running: {message}"),
                ))]))
            }
            Err(error) => {
                error!(workspace = %session.workspace_name, tool = "bash", error = %error, "bash exec failed");
                Ok(CallToolResult::error(vec![Content::text(compact_error(
                    &format!("bash exec failed: {error}"),
                ))]))
            }
        };
        let mut result = result?;
        let mut timing = json!({
            "started_at": started_at,
            "duration_secs": (start.elapsed().as_secs_f64() * 10.0).round() / 10.0
        });
        if result.is_error == Some(true) {
            timing["hint"] = json!("Inspect command state and outputs before retrying; side effects may have occurred.");
        }
        result.content.push(Content::text(timing.to_string()));
        Ok(result)
    }

    #[instrument(skip(self, params, context), fields(workspace), level = "info")]
    #[tool(
        description = "Read numbered UTF-8 text from a file, or set multimodal=true to view an image. Use offset and limit for long text."
    )]
    async fn read(
        &self,
        Parameters(params): Parameters<ReadParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.recheck_bound().await?;
        let session = self.ensure_session().await?;
        tracing::Span::current().record("workspace", session.workspace_name.as_str());
        crate::file_read::read_file(session, params, self.default_exec_timeout(), context.ct).await
    }

    #[instrument(skip(self, params), fields(workspace), level = "info")]
    #[tool(
        description = "Create a UTF-8 text file or replace its entire contents. Creates missing parent directories and returns the bytes written. Empty content is rejected."
    )]
    async fn write(
        &self,
        Parameters(params): Parameters<WriteParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.content.is_empty() {
            return Err(McpError::invalid_params(
                "write requires nonempty content; the file was not changed",
                None,
            ));
        }
        self.recheck_bound().await?;
        let session = self.ensure_session().await?;
        tracing::Span::current().record("workspace", session.workspace_name.as_str());
        let cmd = write_command(&params.path, &params.content);

        match session.exec_file(&cmd, self.default_exec_timeout()).await {
            Ok(result) if result.exit_code == 0 => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Wrote {} bytes to {}",
                    result.stdout.trim(),
                    params.path
                ))]))
            }
            Ok(result) => Ok(CallToolResult::error(vec![Content::text(compact_error(
                &format!("write failed: {}", result.stderr),
            ))])),
            Err(error) => Ok(CallToolResult::error(vec![Content::text(file_error(
                "write", &error,
            ))])),
        }
    }

    #[instrument(skip(self, params), fields(workspace), level = "info")]
    #[tool(
        description = "Replace one exact, unique text passage in a UTF-8 file. A missing or non-unique match leaves the file unchanged."
    )]
    async fn edit(
        &self,
        Parameters(params): Parameters<EditParams>,
    ) -> Result<CallToolResult, McpError> {
        self.recheck_bound().await?;
        let session = self.ensure_session().await?;
        tracing::Span::current().record("workspace", session.workspace_name.as_str());
        let cmd = file_command(
            serde_json::json!({"operation":"edit", "path":params.path, "old_text":params.old_text, "new_text":params.new_text}),
        );

        match session.exec_file(&cmd, self.default_exec_timeout()).await {
            Ok(result) => {
                let output = result.stdout.trim();
                if output == "NOT_FOUND" {
                    Ok(CallToolResult::error(vec![Content::text(compact_error(&format!(
                        "old_text was not found in {}. Use the exact current text, including whitespace and line breaks.",
                        params.path
                    )))]))
                } else if let Some(count) = output.strip_prefix("MULTIPLE:") {
                    Ok(CallToolResult::error(vec![Content::text(compact_error(&format!(
                        "old_text matches {count} locations in {}. Include more surrounding text to identify one occurrence.",
                        params.path
                    )))]))
                } else if output == "OK" && result.exit_code == 0 {
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Replaced 1 occurrence in {}",
                        params.path
                    ))]))
                } else {
                    Ok(CallToolResult::error(vec![Content::text(compact_error(
                        &format!("edit failed: {}", result.stderr),
                    ))]))
                }
            }
            Err(error) => Ok(CallToolResult::error(vec![Content::text(file_error(
                "edit", &error,
            ))])),
        }
    }
}

impl ServerHandler for AgentSpaceMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                format!("Agent Space MCP — persistent workspace with command execution.\n\
                 Built-in tools: bash, read, write, edit, web_publish.\n\
                 Command and file tool execution defaults to workspace.exec_timeout_secs; bash.timeout_secs may override it per call. Web publication transfers use separate startup and I/O idle timeouts.\n\
                 Host-managed services are documented in /opt/agent-space/README.md when installed; do not modify their reserved files or stop them manually. web_publish publishes selected files, directories or running HTTP services; it does not control DAV.{}",if self.browser.load(std::sync::atomic::Ordering::SeqCst) {"\nThis space also provides browser_snapshot, browser_act, browser_read and screen_observe for one shared visible browser. Use browser_act op=wait/cancel for all browser and screenshot tasks. Task deadlines include preparation; polling does not extend them. Tool descriptions contain all required usage guidance."} else {""}),
            )
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        let token = self.bearer(&context)?;
        let identity = self.bind(token).await?;
        info!(user = %identity.username, workspace = %identity.sandbox_name, "user initialized with workspace");
        Ok(self.get_info())
    }

    async fn ping(&self, context: RequestContext<RoleServer>) -> Result<(), McpError> {
        self.recheck(&context).await?;
        self.ensure_session().await?.touch().await;
        Ok(())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.recheck(&context).await?;
        self.ensure_session().await?.touch().await;
        Ok(ListToolsResult {
            tools: self
                .tool_router
                .list_all()
                .into_iter()
                .filter(|t| self.tool_enabled(&t.name))
                .map(|tool| self.disclosed_tool(tool))
                .collect(),
            next_cursor: None,
            meta: None,
        })
    }

    #[instrument(skip_all, fields(tool = %request.name), level = "info")]
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.recheck(&context).await?;
        if !self.tool_enabled(&request.name) {
            return Err(McpError::invalid_request(
                "browser tools are not enabled for this space image",
                None,
            ));
        }
        let request_id = context.id.clone();
        let started = std::time::Instant::now();
        let result = self
            .tool_router
            .call(ToolCallContext::new(self, request, context))
            .await
            .map(|result| budget_result(result, &request_id));
        info!(
            duration_ms = started.elapsed().as_millis(),
            protocol_error = result.is_err(),
            "tool completed"
        );
        result
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        if !self.tool_enabled(name) {
            return None;
        }
        self.tool_router.get(name).cloned()
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![],
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        _request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        Err(McpError::resource_not_found("not found", None))
    }
}

/// Bound diagnostic text, not command output, file contents, or structured results.
pub(crate) fn compact_error(message: &str) -> String {
    let mut clean = String::new();
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if !c.is_control() || matches!(c, '\n' | '\t') {
            clean.push(c);
        }
    }
    let clean = clean.trim();
    const LIMIT: usize = 2048;
    const HEAD: usize = 1536;
    const MARK: &str = "\n... [error truncated] ...\n";
    let count = clean.chars().count();
    if count <= LIMIT {
        return clean.to_owned();
    }
    clean
        .chars()
        .take(HEAD)
        .chain(MARK.chars())
        .chain(
            clean
                .chars()
                .skip(count - (LIMIT - HEAD - MARK.chars().count())),
        )
        .collect()
}

pub(crate) fn file_error(operation: &str, error: &agent_engine::engine::EngineError) -> String {
    use agent_engine::engine::EngineError;
    match error {
        EngineError::ExecutionEnded { timed_out, cleanup_confirmed, .. } => compact_error(&format!(
            "{operation} {}. Cleanup confirmed: {cleanup_confirmed}. Inspect the current file contents before retrying.",
            if *timed_out { "timed out" } else { "was cancelled" }
        )),
        _ => compact_error(&format!("{operation} failed: {error}")),
    }
}

pub(crate) fn internal_error(error: impl std::fmt::Display) -> McpError {
    McpError::internal_error(compact_error(&error.to_string()), None)
}
pub(crate) fn media_error(code: &str, message: &str) -> CallToolResult {
    let (code, message) = message
        .split_once(": ")
        .filter(|(code, _)| {
            !code.is_empty() && code.bytes().all(|c| c.is_ascii_uppercase() || c == b'_')
        })
        .unwrap_or((code, message));
    CallToolResult::error(vec![Content::text(
        json!({"error":{"code":code,"message":message}}).to_string(),
    )])
}

const TOOL_RESULT_BUDGET: usize = 12 * 1024 * 1024;
// Reserve space for JSON-RPC and SSE framing, including the request ID.
fn budget_result(result: CallToolResult, request_id: &RequestId) -> CallToolResult {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes += bytes.len();
            if self.bytes > self.limit {
                return Err(std::io::ErrorKind::FileTooLarge.into());
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let framing = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":request_id,"result":{}}))
        .unwrap()
        .len()
        + 4096;
    let mut counter = Counter {
        bytes: 0,
        limit: TOOL_RESULT_BUDGET.saturating_sub(framing),
    };
    if serde_json::to_writer(&mut counter, &result).is_err() {
        return media_error("TOOL_RESULT_TOO_LARGE", "Tool result exceeds the 12 MiB transport budget; request fewer images or smaller output");
    }
    tracing::info!(
        result_bytes = counter.bytes,
        is_error = result.is_error.unwrap_or(false),
        "tool result"
    );
    result
}

fn browser_result(
    result: Result<serde_json::Value, agent_engine::engine::EngineError>,
) -> CallToolResult {
    let mut value = match result {
        Ok(value) => value,
        Err(error) => {
            let message = match &error {
                agent_engine::engine::EngineError::Other(message) => message.clone(),
                _ => error.to_string(),
            };
            let mut value = serde_json::from_str::<serde_json::Value>(&message).unwrap_or_else(
                |_| json!({"error":browser_error(&message),"side_effects":"unknown"}),
            );
            compact_browser_feedback(&mut value);
            crate::browser_feedback::project(&mut value);
            return CallToolResult::error(vec![Content::text(value.to_string())]);
        }
    };
    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
        value["error"] = browser_error(error);
    }
    compact_browser_feedback(&mut value);
    let mut images = vec![];
    if let Some(items) = value
        .as_object_mut()
        .and_then(|object| object.remove("images"))
        .and_then(|items| items.as_array().cloned())
    {
        for item in items {
            if let (Some(data), Some(mime)) = (item["data"].as_str(), item["mimeType"].as_str()) {
                images.push(Content::image(data.to_owned(), mime.to_owned()));
            }
        }
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("window");
        object.remove("nativeWindow");
    }
    // Capture geometry stays in the host cache; callers use the image identity.
    if let Some(object) = value.as_object_mut() {
        object.remove("captures");
    }
    crate::browser_feedback::project(&mut value);
    let failed = value.get("error").is_some()
        || matches!(
            value["state"].as_str(),
            Some("failed" | "timed_out" | "cancelled")
        );
    let mut content = vec![Content::text(value.to_string())];
    content.extend(images);
    if failed {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}
fn compact_base_schema(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            object.remove("title");
            object.remove("$schema");
            if matches!(
                object.get("format").and_then(|v| v.as_str()),
                Some("uint" | "uint32" | "uint64" | "int32" | "int64" | "double" | "float")
            ) {
                object.remove("format");
            }
            if object
                .get("default")
                .is_some_and(serde_json::Value::is_null)
            {
                object.remove("default");
            }
            for child in object.values_mut() {
                compact_base_schema(child);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                compact_base_schema(child);
            }
        }
        _ => {}
    }
}
fn compact_browser_feedback(value: &mut serde_json::Value) {
    if let Some(epoch) = value.get("started_at").and_then(|v| v.as_i64()) {
        if let Ok(time) = time::OffsetDateTime::from_unix_timestamp(epoch) {
            if let Ok(text) = time.format(&time::format_description::well_known::Rfc3339) {
                value["started_at"] = json!(text);
            }
        }
    }

    if let Some(frames) = value.get_mut("frames").and_then(|v| v.as_array_mut()) {
        for frame in frames {
            if let Some(object) = frame.as_object_mut() {
                if let Some(ms) = object.remove("elapsed_ms").and_then(|v| v.as_u64()) {
                    object.insert(
                        "elapsed_secs".into(),
                        json!((ms as f64 / 100.0).round() / 10.0),
                    );
                }
            }
        }
    }
    // Browser downloads can register image files as well as captured originals.
    // Normalize only these owned lists; page fields may legitimately have the
    // same names as image metadata.
    if let Some(items) = value.get_mut("files") {
        compact_image_files(items);
    }
    if let Some(items) = value.pointer_mut("/value/downloads") {
        compact_image_files(items);
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("phase");
        object.remove("logs");
    }
    if value["state"] == "succeeded" {
        value.as_object_mut().unwrap().remove("side_effects");
    }
    if value["error"]["code"] == "PAGE_LIMIT_REACHED" {
        let reason = value["error"]["message"]
            .as_str()
            .unwrap_or("Page limit reached")
            .split(". ")
            .next()
            .unwrap();
        value["error"]["message"] = json!(format!(
            "{reason}. Use browser_snapshot(view='tabs'), then reuse or close existing pages."
        ));
    }
    if value["error"]["code"] == "PAGE_NOT_FOUND" {
        value["error"]["message"] = json!("Page not found. Use browser_snapshot(view='tabs') to select an open page, or tab/new to open one.");
    }
    if value["error"]["code"] == "HUMAN_CONTROL" {
        value["error"]["message"] = json!(
            "The user controls the desktop. Observation remains available; resume actions after control is returned."
        );
    }
}
fn compact_image_files(value: &mut serde_json::Value) {
    let Some(items) = value.as_array_mut() else {
        return;
    };
    for item in items {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        let format = match object.get("mime_type").and_then(|v| v.as_str()) {
            Some("image/png") => "PNG",
            Some("image/jpeg") => "JPEG",
            Some("image/webp") => "WebP",
            Some("image/gif") => "GIF",
            _ => continue,
        };
        if let Some(bytes) = object.remove("bytes").and_then(|v| v.as_u64()) {
            object.insert(
                "size".into(),
                json!(agent_engine::media::size_label(bytes as usize)),
            );
        }
        object.insert("format".into(), json!(format));
        object.remove("mime_type");
        if object.contains_key("path") {
            object.remove("name");
        }
    }
}
fn browser_error(message: &str) -> serde_json::Value {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(message) {
        if value.get("error").is_some() {
            return value["error"].clone();
        }
    }
    let message = compact_error(message);
    if !message.is_empty() && message.bytes().all(|c| c.is_ascii_uppercase() || c == b'_') {
        return json!({"code":message,"message":message});
    }
    let (code, detail) = message
        .split_once(": ")
        .filter(|(code, _)| {
            !code.is_empty() && code.bytes().all(|c| c.is_ascii_uppercase() || c == b'_')
        })
        .unwrap_or(("BROWSER_ERROR", &message));
    json!({"code":code,"message":detail})
}

fn auth_error(error: StateError) -> McpError {
    match error {
        StateError::InvalidCredential | StateError::Disabled(_) | StateError::Deleting(_) => {
            McpError::invalid_params("credential is no longer valid for this workspace", None)
        }
        other => McpError::internal_error(other.to_string(), None),
    }
}

pub(crate) fn file_command(spec: serde_json::Value) -> String {
    format!(
        "python3 -I -c {} {}",
        shell_escape(include_str!("../../guest/file.py")),
        shell_escape(&spec.to_string())
    )
}

fn write_command(path: &str, content: &str) -> String {
    file_command(serde_json::json!({"operation":"write", "path":path, "content":content}))
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn browser_feedback_preserves_transport_failure_even_without_an_error_field() {
        use agent_engine::engine::{mock::MockEngine, EngineError, WorkspaceOpts};
        let config = Arc::new(toml::from_str::<Config>(include_str!("../../config.toml")).unwrap());
        let sessions = Arc::new(SessionManager::new(
            Arc::new(MockEngine::new()),
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
        let server = AgentSpaceMcpServer::new(config, sessions);
        let response = server
            .browser_feedback(Err(EngineError::Other(
                json!({"value":{"completed":1,"of":2}}).to_string(),
            )))
            .await;
        assert_eq!(response.is_error, Some(true));
        let body: serde_json::Value = serde_json::from_str(
            serde_json::to_value(response).unwrap()["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["completed_steps"], 1);
    }

    #[test]
    fn tool_result_budget_counts_base64_json_and_framing_without_truncation() {
        use base64::Engine;
        let bytes = vec![137; 8 * 1024 * 1024];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let result = budget_result(
            CallToolResult::success(vec![
                Content::text("complete"),
                Content::image(encoded.clone(), "image/png"),
            ]),
            &RequestId::Number(7),
        );
        assert_ne!(result.is_error, Some(true));
        let wire = serde_json::to_value(result).unwrap();
        assert_eq!(wire["content"][1]["data"], encoded);
        assert!(
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":7,"result":wire}))
                .unwrap()
                .len()
                < TOOL_RESULT_BUDGET
        );
        let oversized = budget_result(
            CallToolResult::success(vec![Content::text("x".repeat(TOOL_RESULT_BUDGET))]),
            &RequestId::Number(7),
        );
        assert_eq!(oversized.is_error, Some(true));
        assert!(serde_json::to_string(&oversized)
            .unwrap()
            .contains("TOOL_RESULT_TOO_LARGE"));
    }

    #[test]
    fn browser_timing_and_success_metadata_are_compact_without_losing_failures() {
        let mut value = json!({"state":"succeeded","started_at":1792495384u64,"duration_secs":2.1,"phase":"done","side_effects":"may_have_occurred","value":{"completed":1}});
        compact_browser_feedback(&mut value);
        let start = value["started_at"].as_str().unwrap();
        assert_eq!(start.len(), 20);
        assert!(start.ends_with('Z'));
        assert_eq!(value["duration_secs"], 2.1);
        assert!(value.get("phase").is_none());
        assert!(value.get("side_effects").is_none());
        let mut failed = json!({"state":"failed","side_effects":"may_have_occurred","cleanup_confirmed":false,"failed_step":{"step_number":2},"error":{"code":"PAGE_LIMIT_REACHED","message":"full"}});
        compact_browser_feedback(&mut failed);
        assert_eq!(failed["side_effects"], "may_have_occurred");
        assert_eq!(failed["cleanup_confirmed"], false);
        assert_eq!(failed["failed_step"]["step_number"], 2);
        assert!(failed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("view='tabs'"));
    }

    #[test]
    fn error_text_is_bounded_without_dropping_the_cause_or_tail() {
        assert_eq!(
            compact_error("\x1b[31mPermission denied\x1b[0m\r\n"),
            "Permission denied"
        );
        assert_eq!(
            compact_error("\x1b]8;;https://example.test\x1b\\link\x1b]8;;\x07"),
            "link"
        );
        let message = format!("SyntaxError: {}\nlast cause", "中🙂".repeat(3000));
        let short = compact_error(&message);
        assert_eq!(short.chars().count(), 2048);
        assert!(short.starts_with("SyntaxError: "));
        assert!(short.ends_with("last cause"));
        assert!(short.contains("[error truncated]"));
    }

    #[test]
    fn browser_errors_preserve_effects_cleanup_and_available_outputs() {
        let long = "x".repeat(8000);
        let result = browser_result(Ok(json!({"task_id":"id","state":"failed",
            "error":long,"side_effects":"may_have_occurred","cleanup_confirmed":false,
            "value":{"partial":true,"downloads":[{"path":"/workspace/image.png","name":"image.png","bytes":1536,"mime_type":"image/png","url":"https://example.test/image.png"}],"fields":[{"name":"width","value":"3000"}]},
            "files":[{"path":"/workspace/result.txt"},{"path":"/workspace/image.png","name":"image.png","bytes":1536,"mime_type":"image/png"}],
            "images":[{"data":"AA==","mimeType":"image/png"}]})));
        assert_eq!(result.is_error, Some(true));
        let wire = serde_json::to_value(result).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            body["error"]["message"].as_str().unwrap().chars().count(),
            2048
        );
        assert_eq!(body["side_effects"], "may_have_occurred");
        assert_eq!(body["cleanup_confirmed"], false);
        assert_eq!(body["partial"], true);
        assert_eq!(body["files"][0]["path"], "/workspace/result.txt");
        assert_eq!(
            body["files"][1],
            json!({"path":"/workspace/image.png","size":"1.5 KiB","format":"PNG"})
        );
        assert_eq!(
            body["downloads"][0],
            json!({"path":"/workspace/image.png","size":"1.5 KiB","format":"PNG","url":"https://example.test/image.png"})
        );
        assert_eq!(body["fields"][0]["value"], "3000");
        assert_eq!(wire["content"][1]["type"], "image");
        let success = browser_result(Ok(
            json!({"state":"succeeded","value":{"observation":long}}),
        ));
        let wire = serde_json::to_value(success).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["observation"].as_str().unwrap().len(), 8000);
    }

    #[test]
    fn browser_feedback_keeps_actionable_errors_without_worker_logs() {
        let mut value = json!({"error":{"code":"TIMEOUT","message":"not found"},
            "logs":{"stderr":"TIMEOUT: not found\n","stdout":"useful"},
            "value":{"completed":1,"failed_step":2},"side_effects":"may_have_occurred"});
        compact_browser_feedback(&mut value);
        assert!(value.get("logs").is_none());
        assert_eq!(value["value"]["completed"], 1);
        assert_eq!(value["side_effects"], "may_have_occurred");
        value["logs"] = json!({"stderr":"additional cause"});
        compact_browser_feedback(&mut value);
        assert!(value.get("logs").is_none());
        assert_eq!(value["error"]["message"], "not found");
        let mut value = json!({"error":{"code":"HUMAN_CONTROL","message":"HUMAN_CONTROL"},
            "logs":{"stderr":"HUMAN_CONTROL: HUMAN_CONTROL"}});
        compact_browser_feedback(&mut value);
        assert!(value.get("logs").is_none());
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Observation remains available"));
    }

    #[test]
    fn publication_targets_report_the_actual_invalid_input() {
        assert!(publish_params::target(None, None, None)
            .unwrap_err()
            .contains("exactly one"));
        assert!(
            publish_params::target(Some("/workspace/a".into()), None, Some(8123))
                .unwrap_err()
                .contains("exactly one")
        );
        assert!(publish_params::target(None, None, Some(80))
            .unwrap_err()
            .contains("1024"));
        assert!(publish_params::target(None, None, Some(8080))
            .unwrap_err()
            .contains("reserved"));
        assert!(publish_params::target(None, None, Some(8123)).is_ok());
    }

    #[test]
    fn optional_selectors_and_unknown_fields_are_tolerated_without_changing_payloads() {
        let bash: BashParams = serde_json::from_value(
            json!({"cmd":" echo hi ","workdir":"", "timeout_secs":null,"unused":1}),
        )
        .unwrap();
        assert!(bash.workdir.is_none());
        assert!(bash.timeout_secs.is_none());
        assert_eq!(bash.cmd, " echo hi ");
        let observe: SnapshotParams =
            serde_json::from_value(json!({"page_id":"","image":null,"recent_tasks_limit":null}))
                .unwrap();
        assert!(observe.page_id.is_none());
        let task: ActParams =
            serde_json::from_value(json!({"task_id":"id","op":"wait","wait_secs":0})).unwrap();
        assert_eq!(task.wait_secs, Some(0));
        let write: WriteParams =
            serde_json::from_value(json!({"path":"/workspace/a","content":"","unused":null}))
                .unwrap();
        assert_eq!(write.content, "");
        let edit: EditParams = serde_json::from_value(
            json!({"path":"/workspace/a","old_text":" x ","new_text":"","unused":1}),
        )
        .unwrap();
        assert_eq!(edit.old_text, " x ");
        assert_eq!(edit.new_text, "");
        assert!(serde_json::from_value::<ActParams>(json!({"code":null})).is_err());
        assert!(
            serde_json::from_value::<BashParams>(json!({"cmd":"x","timeout_secs":"bad"})).is_err()
        );
        assert!(serde_json::from_value::<WriteParams>(json!({"content":"x","extra":1})).is_err());
    }

    #[test]
    fn optional_choices_default_on_blank_and_validate_nonempty_values() {
        for placeholder in [json!(null), json!(""), json!(" \n\t ")] {
            let p: ActParams = serde_json::from_value(json!({"op":placeholder})).unwrap();
            assert!(p.op.is_none());
        }
        let p: ActParams = serde_json::from_value(json!({"op":" cancel ","task_id":"id"})).unwrap();
        assert!(matches!(p.op, Some(ActAction::Cancel)));
        for invalid in [
            json!("unknown"),
            json!(false),
            json!([]),
            json!({}),
            json!(1),
        ] {
            assert!(serde_json::from_value::<ActParams>(json!({"op":invalid})).is_err());
        }
        for image in [json!(true), json!(false), json!(null)] {
            assert!(serde_json::from_value::<SnapshotParams>(json!({"image":image})).is_ok());
        }
        for image in [json!(""), json!("marks"), json!(1), json!({})] {
            assert!(serde_json::from_value::<SnapshotParams>(json!({"image":image})).is_err());
        }
        let read: ReadParams = serde_json::from_value(
            json!({"path":" file \n", "offset":null,"limit":null,"unexpected":[1,{}]}),
        )
        .unwrap();
        assert_eq!(read.path, " file \n");
        assert!(read.offset.is_none() && read.limit.is_none());
        assert!(serde_json::from_value::<ReadParams>(json!({"path":"a","limit":"500"})).is_err());
        assert!(serde_json::from_value::<WriteParams>(json!({"path":"a","content":null})).is_err());
        assert!(serde_json::from_value::<EditParams>(
            json!({"path":"a","old_text":null,"new_text":""})
        )
        .is_err());
    }

    #[test]
    fn write_preserves_path_and_content_without_shell_expansion() {
        let dir = tempfile::tempdir().unwrap();
        let content = "it's $HOME `false` $(false)\nsecond line\n";
        for path in [
            "a b/file.txt",
            "trailing\n/file.txt",
            "quote's/*.txt",
            "-option/file",
            "-file",
            "unicode-中文/result",
        ] {
            let result = std::process::Command::new("bash")
                .args(["-c", &write_command(path, content)])
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{path:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join(path)).unwrap(),
                content
            );
            assert_eq!(
                String::from_utf8(result.stdout)
                    .unwrap()
                    .trim()
                    .parse::<usize>()
                    .unwrap(),
                content.len()
            );
        }
    }
}
