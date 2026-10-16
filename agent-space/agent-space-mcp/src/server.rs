use std::sync::Arc;

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
        description = "Bash command or script to execute. Supply input through arguments, files, or shell redirection."
    )]
    command: String,
    #[schemars(description = "Execution timeout in seconds; defaults to the workspace setting.")]
    #[serde(default)]
    #[schemars(range(min = 1))]
    timeout_secs: Option<u64>,
    #[schemars(description = "Working directory; defaults to /workspace.")]
    #[serde(default)]
    #[serde(deserialize_with = "publish_params::optional_string")]
    workdir: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct ReadParams {
    #[schemars(description = "Path of the text file to read in Runcove's remote workspace.")]
    path: String,
    #[schemars(
        description = "First line to return, counting from 1. Defaults to 1; use a positive integer."
    )]
    #[serde(default)]
    #[schemars(range(min = 1))]
    offset: Option<u64>,
    #[schemars(
        description = "Maximum number of lines to return. Defaults to 500; use a positive integer."
    )]
    #[serde(default)]
    #[schemars(range(min = 1))]
    limit: Option<u64>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct WriteParams {
    #[schemars(description = "Destination file path in Runcove's remote workspace.")]
    path: String,
    #[schemars(description = "Complete text contents to save as UTF-8.")]
    content: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct EditParams {
    #[schemars(description = "Path of the text file to edit in Runcove's remote workspace.")]
    path: String,
    #[schemars(
        description = "Exact text to replace, including whitespace and line breaks; must occur once."
    )]
    old_text: String,
    #[schemars(description = "Replacement text; an empty string removes the matched text.")]
    new_text: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum BrowserView {
    Dom,
    Page,
    Desktop,
}
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct ObserveParams {
    #[serde(default)]
    #[schemars(
        description = "Optional target tab. Usually omit this parameter. Use a page_id from tabs only when targeting a specific tab. Omit or use JSON null to select the focused tab, falling back to an available tab. Applies to dom and page views; desktop captures the full desktop regardless of page_id."
    )]
    #[serde(deserialize_with = "publish_params::optional_string")]
    page_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Choose the JSON string \"dom\" for semantic page content, \"page\" for a webpage screenshot, or \"desktop\" for the full desktop, including browser controls and native dialogs. Omit to use \"dom\"."
    )]
    #[serde(deserialize_with = "publish_params::optional_choice")]
    view: Option<BrowserView>,
    #[serde(default)]
    #[schemars(
        description = "Number of recent task summaries, newest first; defaults to 3, range 1-10."
    )]
    #[schemars(range(min = 1, max = 10))]
    recent_tasks_limit: Option<usize>,
}
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct RunParams {
    #[schemars(
        description = "JavaScript async function body or async function to run in Runcove's remote workspace, up to 65,536 UTF-8 bytes. The script receives page, context, desktop, output, and artifacts. artifacts is the default artifact directory path; use it to construct file paths. Import Node.js modules with await import(...), await asynchronous operations, and return a JSON-serializable value."
    )]
    code: String,
    #[serde(default)]
    #[schemars(
        description = "Optional target tab. Usually omit this parameter. Use a page_id from tabs only when targeting a specific tab. Omit or use JSON null to select the focused tab, falling back to an available tab. Selecting a tab does not bring it to the foreground."
    )]
    #[serde(deserialize_with = "publish_params::optional_string")]
    page_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Total task deadline in seconds, including preparation and execution. Defaults to 60; accepts 1-600. Waiting for a result leaves this deadline unchanged."
    )]
    #[schemars(range(min = 1, max = 600))]
    timeout_secs: Option<u64>,
}
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TaskAction {
    Wait,
    Cancel,
}
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct TaskParams {
    #[schemars(
        description = "ID returned when a browser task was accepted in Runcove's remote workspace."
    )]
    task_id: String,
    #[serde(default)]
    #[schemars(
        description = "Choose the JSON string \"wait\" to retrieve status or wait for completion, or \"cancel\" to stop remaining execution. Omit to use \"wait\"."
    )]
    #[serde(deserialize_with = "publish_params::optional_choice")]
    action: Option<TaskAction>,
    #[serde(default)]
    #[schemars(
        description = "Maximum wait for this call in seconds; defaults to 20, range 0-20. Use 0 for an immediate status check."
    )]
    #[schemars(range(min = 0, max = 20))]
    wait_secs: Option<u64>,
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
        }
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
        !name.starts_with("browser_") || self.browser.load(std::sync::atomic::Ordering::SeqCst)
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
        description = "Inspect the shared browser in Runcove's remote workspace, starting the desktop when needed. Call with {} to inspect the current browser and list open tabs; no page_id is required. Returns page IDs, browser health, recent task summaries, and semantic content or a screenshot while preserving the current page and focus. Use browser_run to interact with the observed page and browser_task to check progress, retrieve results, or cancel a task. Browser operations and workspace tools share files under /workspace."
    )]
    async fn browser_observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, McpError> {
        let hub = self.desktop().await?;
        let view = match params.view.unwrap_or(BrowserView::Dom) {
            BrowserView::Dom => "dom",
            BrowserView::Page => "page",
            BrowserView::Desktop => "desktop",
        };
        Ok(browser_result(
            hub.observe(params.page_id, view, params.recent_tasks_limit.unwrap_or(3))
                .await,
        ))
    }
    #[tool(
        description = "Run an asynchronous JavaScript task with the provided Playwright page and context. Provide code; usually omit page_id. Navigate the selected tab with await page.goto(url). Reuse it whenever possible. Open another tab with await context.newPage() only when the task requires keeping multiple pages open; use await tab.bringToFront() when it needs to be visible. Close tabs you created when no longer needed, preserving pre-existing tabs and unfinished work. Ordinary browser tasks create a blank tab when none are open. The final observation describes the initially selected tab; if it was closed, it reports the remaining tabs without substituting another page. Use desktop helpers for native dialogs or an unresponsive page. The service manages the shared browser and allows one browser task at a time while AI control is active. Returns the completed result or a task_id while execution continues. Keep each task focused on a clear outcome. Prefer waiting for a relevant element, URL, or event over fixed delays. After a timeout or partial failure, inspect the current state before retrying; earlier actions may already have completed.\n\nUpload workspace files with locator.setInputFiles(path), or register a filechooser listener before the triggering action. For downloads, register the download listener before triggering the download. Use await output.download(download) to save it in the workspace and return its file metadata. For a specific destination, save it under /workspace and register it with await output.file(path). Return an existing workspace file with await output.file(path), a PNG with await output.image(buffer), or a desktop screenshot with await output.image(await desktop.screenshot()). Save an MHTML snapshot with await output.saveMhtml(page, path).\n\nFor desktop coordinates, use the image_id and original pixel coordinates from a desktop screenshot captured within the last 60 seconds while the same window remains in the foreground. Use desktop.click, desktop.drag, and desktop.scroll for pointer actions, desktop.type(text) for text, and desktop.press(key) for keys."
    )]
    async fn browser_run(
        &self,
        Parameters(params): Parameters<RunParams>,
    ) -> Result<CallToolResult, McpError> {
        let hub = self.desktop().await?;
        let session = self.ensure_session().await?;
        Ok(browser_result(
            hub.run(
                session,
                params.code,
                params.page_id,
                params.timeout_secs.unwrap_or(60),
            )
            .await,
        ))
    }
    #[tool(
        description = "Retrieve a browser task's status and available result, optionally waiting for completion, or cancel its remaining execution. Failed tasks return a concise error and any available outputs. Cancellation stops remaining execution; website actions already submitted retain their effects. Results remain available for 30 minutes after completion within the current host session."
    )]
    async fn browser_task(
        &self,
        Parameters(params): Parameters<TaskParams>,
    ) -> Result<CallToolResult, McpError> {
        let hub = self.desktop().await?;
        Ok(browser_result(
            hub.task(
                &params.task_id,
                params.wait_secs.unwrap_or(20),
                matches!(params.action, Some(TaskAction::Cancel)),
            )
            .await,
        ))
    }
    #[tool(
        description = "Publish a file, directory, or running HTTP service from Runcove's remote workspace through an authenticated web URL. Publishing under an existing name updates its target while preserving the URL and credentials. Share the returned URL and HTTP Basic username and password with the user. Access remains available while the publication exists and the workspace is running."
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
                keep_alive_minutes,
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
                let minutes = keep_alive_minutes.unwrap_or(settings.default_keep_alive_minutes);
                if minutes == 0 || minutes > settings.max_keep_alive_minutes {
                    return Err(McpError::invalid_params(
                        format!(
                            "keep_alive_minutes must be 1-{}",
                            settings.max_keep_alive_minutes
                        ),
                        None,
                    ));
                }
                let duration = std::time::Duration::from_secs(u64::from(minutes) * 60);
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
        description = "Run Bash commands in Runcove's remote workspace. Use /workspace for persistent files, read to inspect text, write to create or replace files, edit for precise changes, and web_publish to share files or running HTTP services. Each call runs in a separate, non-interactive shell and returns the exit code and bounded output. Set workdir for the working directory. When starting a background service, redirect its standard input and output streams. Keep Runcove-managed files in /opt/agent-space, /tmp/agent-space, and /workspace/.agent-space intact. Runcove handles managed service lifecycle and internal cleanup."
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
        let workdir = params.workdir.as_deref().unwrap_or("/workspace");
        self.recheck_bound().await?;
        let session = self.ensure_session().await?;
        let cmd = format!(
            "{{ cd {} && {{\n{}\n}}; }} 2>&1",
            shell_escape(workdir),
            params.command
        );

        tracing::Span::current().record("workspace", session.workspace_name.as_str());
        info!(workspace = %session.workspace_name, command_bytes = params.command.len(), timeout_secs, "bash exec");
        let start = std::time::Instant::now();

        match session.exec(&cmd, timeout).await {
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
                    command_bytes = params.command.len(),
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
        }
    }

    #[instrument(skip(self, params), fields(workspace), level = "info")]
    #[tool(
        description = "Read a text file and return its contents with line numbers. Use offset and limit to read a selected range or continue through a large file."
    )]
    async fn read(
        &self,
        Parameters(params): Parameters<ReadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.recheck_bound().await?;
        let session = self.ensure_session().await?;
        tracing::Span::current().record("workspace", session.workspace_name.as_str());
        let offset = params.offset.unwrap_or(1);
        let limit = params.limit.unwrap_or(500);
        if offset == 0 || limit == 0 {
            return Err(McpError::invalid_params(
                "offset and limit must be positive integers",
                None,
            ));
        }
        let cmd = file_command(
            serde_json::json!({"operation":"read", "path":params.path, "offset":offset, "limit":limit}),
        );

        match session.exec_file(&cmd, self.default_exec_timeout()).await {
            Ok(result) => match result.stdout.trim() {
                "FILE_NOT_FOUND" => Ok(CallToolResult::error(vec![Content::text(compact_error(&format!(
                    "File not found: {}",
                    params.path
                )))])),
                "NOT_A_FILE" => Ok(CallToolResult::error(vec![Content::text(
                    "The path refers to a directory or another non-regular file. Provide the path of a text file.",
                )])),
                "EMPTY" => Ok(CallToolResult::success(vec![Content::text("(empty)")])),
                "NO_CONTENT" => Ok(CallToolResult::success(vec![Content::text(
                    "(no content in range)",
                )])),
                _ if result.exit_code != 0 => Ok(CallToolResult::error(vec![Content::text(
                    compact_error(&format!("read failed: {}", result.stderr)),
                )])),
                _ => {
                    let output = if result.truncated {
                        format!("{}\n[output truncated] Request a smaller line range using offset and limit.", result.stdout)
                    } else { result.stdout };
                    Ok(CallToolResult::success(vec![Content::text(output)]))
                },
            },
            Err(error) => Ok(CallToolResult::error(vec![Content::text(file_error("read", &error))])),
        }
    }

    #[instrument(skip(self, params), fields(workspace), level = "info")]
    #[tool(
        description = "Create a UTF-8 text file or atomically replace its complete contents. Creates parent directories as needed and returns the number of bytes written."
    )]
    async fn write(
        &self,
        Parameters(params): Parameters<WriteParams>,
    ) -> Result<CallToolResult, McpError> {
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
        description = "Replace one exact text occurrence in a file and commit the change atomically. Choose a unique passage, preserving its whitespace and line breaks."
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
                 Host-managed services are documented in /opt/agent-space/README.md when installed; do not modify their reserved files or stop them manually. web_publish publishes selected files, directories or running HTTP services; it does not control DAV.{}",if self.browser.load(std::sync::atomic::Ordering::SeqCst) {"\nThis space also provides browser_observe, browser_run and browser_task for one shared visible browser. Browser task deadlines include preparation; polling does not extend them. Tool descriptions contain all required usage guidance."} else {""}),
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
                .collect(),
            next_cursor: None,
            meta: None,
        })
    }

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
        self.tool_router
            .call(ToolCallContext::new(self, request, context))
            .await
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
fn compact_error(message: &str) -> String {
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

fn file_error(operation: &str, error: &agent_engine::engine::EngineError) -> String {
    use agent_engine::engine::EngineError;
    match error {
        EngineError::ExecutionEnded { timed_out, cleanup_confirmed, .. } => compact_error(&format!(
            "{operation} {}. Cleanup confirmed: {cleanup_confirmed}. Inspect the current file contents before retrying.",
            if *timed_out { "timed out" } else { "was cancelled" }
        )),
        _ => compact_error(&format!("{operation} failed: {error}")),
    }
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
            return CallToolResult::error(vec![Content::text(json!({"error":"BROWSER_ERROR","message":compact_error(&message),"side_effects":"unknown"}).to_string())]);
        }
    };
    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
        value["error"] = json!(compact_error(error));
    }
    let mut images = vec![];
    for path in ["", "/result", "/observation"] {
        let container = if path.is_empty() {
            Some(&mut value)
        } else {
            value.pointer_mut(path)
        };
        if let Some(container) = container {
            if let Some(items) = container
                .as_object_mut()
                .and_then(|o| o.remove("images"))
                .and_then(|v| v.as_array().cloned())
            {
                for image in items {
                    if let (Some(data), Some(mime)) =
                        (image["data"].as_str(), image["mimeType"].as_str())
                    {
                        images.push(Content::image(data, mime));
                    }
                }
            }
        }
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("window");
        object.remove("nativeWindow");
    }
    if let Some(object) = value.get_mut("observation").and_then(|v| v.as_object_mut()) {
        object.remove("window");
        object.remove("nativeWindow");
    }
    let failed = matches!(
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

fn auth_error(error: StateError) -> McpError {
    match error {
        StateError::InvalidCredential | StateError::Disabled(_) | StateError::Deleting(_) => {
            McpError::invalid_params("credential is no longer valid for this workspace", None)
        }
        other => McpError::internal_error(other.to_string(), None),
    }
}

fn file_command(spec: serde_json::Value) -> String {
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
            "value":{"partial":true},"files":[{"path":"/workspace/result.txt"}],
            "images":[{"data":"AA==","mimeType":"image/png"}]})));
        assert_eq!(result.is_error, Some(true));
        let wire = serde_json::to_value(result).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error"].as_str().unwrap().chars().count(), 2048);
        assert_eq!(body["side_effects"], "may_have_occurred");
        assert_eq!(body["cleanup_confirmed"], false);
        assert_eq!(body["value"]["partial"], true);
        assert_eq!(body["files"][0]["path"], "/workspace/result.txt");
        assert_eq!(wire["content"][1]["type"], "image");
        let success = browser_result(Ok(json!({"state":"succeeded","value":long})));
        let wire = serde_json::to_value(success).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["value"].as_str().unwrap().len(), 8000);
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
            json!({"command":" echo hi ","workdir":"", "timeout_secs":null,"unused":1}),
        )
        .unwrap();
        assert!(bash.workdir.is_none());
        assert!(bash.timeout_secs.is_none());
        assert_eq!(bash.command, " echo hi ");
        let run: RunParams =
            serde_json::from_value(json!({"code":" return ''; ","page_id":" ","script_path":null}))
                .unwrap();
        assert!(run.page_id.is_none());
        assert_eq!(run.code, " return ''; ");
        let observe: ObserveParams = serde_json::from_value(
            json!({"page_id":"","view":null,"recent_tasks_limit":null,"unused":false}),
        )
        .unwrap();
        assert!(observe.page_id.is_none());
        assert!(observe.view.is_none());
        let task: TaskParams =
            serde_json::from_value(json!({"task_id":"id","action":null,"wait_secs":0,"unused":0}))
                .unwrap();
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
        assert!(serde_json::from_value::<RunParams>(json!({"code":null})).is_err());
        assert!(
            serde_json::from_value::<BashParams>(json!({"command":"x","timeout_secs":"bad"}))
                .is_err()
        );
        assert!(serde_json::from_value::<WriteParams>(json!({"content":"x","extra":1})).is_err());
    }

    #[test]
    fn optional_choices_default_on_blank_and_validate_nonempty_values() {
        for placeholder in [json!(null), json!(""), json!(" \n\t ")] {
            let observe: ObserveParams = serde_json::from_value(
                json!({"view":placeholder,"extra":{"anything":[1,null,false]}}),
            )
            .unwrap();
            assert!(observe.view.is_none());
            let task: TaskParams = serde_json::from_value(
                json!({"task_id":"task","action":placeholder,"wait_secs":null}),
            )
            .unwrap();
            assert!(task.action.is_none());
        }
        let observe: ObserveParams = serde_json::from_value(json!({"view":" desktop "})).unwrap();
        assert!(matches!(observe.view, Some(BrowserView::Desktop)));
        let task: TaskParams =
            serde_json::from_value(json!({"task_id":"task","action":" cancel "})).unwrap();
        assert!(matches!(task.action, Some(TaskAction::Cancel)));
        for invalid in [
            json!("unknown"),
            json!(false),
            json!([]),
            json!({}),
            json!(1),
        ] {
            assert!(serde_json::from_value::<ObserveParams>(json!({"view":invalid})).is_err());
            assert!(serde_json::from_value::<TaskParams>(
                json!({"task_id":"task","action":invalid})
            )
            .is_err());
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
