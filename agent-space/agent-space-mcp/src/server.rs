use std::sync::Arc;

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
        description = "Bash command or script. Put command arguments in this string; provide stdin with a pipe, here-document, or redirection."
    )]
    cmd: String,
    #[schemars(
        description = "Optional command execution limit in seconds; positive integer. Omit or pass JSON null to use the configured default."
    )]
    #[serde(default)]
    #[schemars(range(min = 1))]
    timeout_secs: Option<u64>,
    #[schemars(
        description = "Optional working directory, used only for this call; defaults to /workspace. Absolute paths are recommended. An invalid directory prevents command execution."
    )]
    #[serde(default)]
    #[serde(deserialize_with = "publish_params::optional_string")]
    workdir: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct WriteParams {
    #[schemars(
        description = "Destination file path in the connected Runcove space. Absolute paths are recommended; /workspace is the persistent work directory."
    )]
    path: String,
    #[schemars(
        description = "Complete new text, saved as UTF-8; an empty string writes an empty file."
    )]
    content: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct EditParams {
    #[schemars(
        description = "Path of the existing UTF-8 file in the connected Runcove space. Absolute paths are recommended."
    )]
    path: String,
    #[schemars(
        description = "Exact text to match, including whitespace and line breaks. Exclude the line-number prefixes displayed by read."
    )]
    old_text: String,
    #[schemars(
        description = "Text to insert in place of old_text; an empty string deletes the match."
    )]
    new_text: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum BrowserView {
    Dom,
    Ax,
    Page,
    Desktop,
}
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct ObserveParams {
    #[serde(default)]
    #[schemars(
        description = "Optional tab ID from tabs[].page_id. Omit or pass JSON null to choose the focused tab, falling back to an available tab. Invalid IDs return PAGE_NOT_FOUND without choosing another tab. Ignored for compatibility view=\"desktop\"."
    )]
    #[serde(deserialize_with = "publish_params::optional_string")]
    page_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional content view: \"dom\" (default) returns visible text, controls, and current values; \"ax\" returns the accessibility tree. Use screen_observe for screenshots. Compatibility values \"page\" and \"desktop\" return that tool's screenshot-task result."
    )]
    #[serde(deserialize_with = "publish_params::optional_choice")]
    view: Option<BrowserView>,
    #[serde(default)]
    #[schemars(
        description = "Optional number of recent task summaries, newest first: 0-10, default 0 (no history)."
    )]
    #[schemars(range(min = 0, max = 10))]
    recent_tasks_limit: Option<usize>,
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
        description = "Required target: \"desktop\" captures the desktop of the connected Runcove space, including native dialogs; \"page\" captures a tab's viewport in its browser."
    )]
    view: ScreenView,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "Optional tab ID for view=\"page\", from browser_observe's tabs[].page_id. Omit or pass JSON null to choose the focused tab, falling back to an available tab. Selected once for the sequence; invalid or closed targets cause an error without switching tabs. Ignored for view=\"desktop\"."
    )]
    page_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional number of screenshots, from 1 to 8. Defaults to 1.",
        range(min = 1, max = 8)
    )]
    count: Option<usize>,
    #[serde(default)]
    #[schemars(
        description = "Optional minimum time between capture starts, in milliseconds; default 1000. Used only when count exceeds 1. Zero captures back to back; actual gaps may be longer. The whole observation, including preparation and waiting, must fit within 600 seconds.",
        range(min = 0)
    )]
    interval_ms: Option<u64>,
    #[serde(default)]
    #[schemars(
        description = "Optional: save original PNG files in addition to returned images; default false. Saved paths appear in files in capture order."
    )]
    save_original: bool,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "Optional absolute directory for original PNGs; requires save_original=true. Defaults to /workspace/agent-space/artifacts. Uses the same filesystem as this space's command and file tools. Creates missing directories if permitted and never overwrites files. Choose a path under /workspace for downloadable files."
    )]
    save_dir: Option<String>,
}
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct RunParams {
    #[schemars(
        description = r#"JavaScript async function body or async function, 1-65,536 UTF-8 bytes. Receives page (the selected Playwright tab), context (the shared browser context), desktop (desktop controls), output (explicit outputs), and artifacts (the output directory path, /workspace/agent-space/artifacts). Await async calls, import modules with await import(...), and return a JSON-serializable value. Variables do not persist between calls.

Browser: reuse page; navigate with await page.goto(url). Use await context.newPage() when additional tabs are needed, and close temporary tabs you create. Do not launch another browser, create another context, or close the shared browser or context. Wait for elements, URLs, or events. Use await page.evaluate(...) for code that runs inside the web page. Before desktop input to a tab, bring it forward with await page.bringToFront(). If the task starts with a native dialog or an unresponsive tab, page/context are unavailable for that task; recover with desktop controls, then observe again.

Files use absolute paths in this space, not paths on the caller's computer. Prefer await locator.setInputFiles(path) for uploads. For a file chooser, register before clicking:
const chooser = page.waitForEvent('filechooser');
await locator.click();
await (await chooser).setFiles(path);
If a native upload dialog is already open, observe it with screen_observe(view='desktop'), use await desktop.press('ctrl+l') and await desktop.type(absolutePath), then click Open using a fresh desktop capture. Verify the website received the file; a selected filename alone does not confirm an upload.
For downloads, register before triggering:
const pending = page.waitForEvent('download');
await locator.click();
const file = await output.download(await pending);
output.download saves under artifacts and returns registered file metadata.

Outputs: await output.file(path) registers an existing regular file under /workspace and returns metadata, not its contents. await output.image(bufferOrCapture) emits a PNG, JPEG, or WebP image from a Buffer or desktop Capture. await output.saveMhtml(page, path) saves and registers MHTML under /workspace. Use artifacts for generated files. Per task: 16 registered files and 4 images totaling 8 MiB of image data. JSON return values over 32 KiB are saved as files. Already emitted outputs survive a later error, timeout, or cancellation.

Desktop: await desktop.screenshot() returns a Capture; await output.image(capture) emits its image. Pointer calls: await desktop.click({image_id,x,y,button?}), await desktop.drag({image_id,x,y,to_x,to_y}), await desktop.scroll({image_id,x,y,delta}). Use image_id from a screen_observe desktop frame or a desktop Capture, with coordinates in the returned image's pixels. The capture must be under 60 seconds old with the same foreground window; otherwise capture again. Page screenshots cannot supply desktop coordinates. Click buttons: 1=left (default), 2=middle, 3=right. Drag holds the left button. Scroll delta: integer -30..30 wheel steps, positive down, negative up.

Other controls: await desktop.type(text), await desktop.press(key), await desktop.setWindowMode('normal'|'maximized'|'fullscreen'). Key examples: 'Enter', 'Escape', 'ctrl+a'. Window mode changes require page/context to be available."#
    )]
    code: String,
    #[serde(default)]
    #[schemars(
        description = "Optional tab ID from browser_observe's tabs[].page_id. Omit or pass JSON null to choose the focused tab, falling back to an available tab. Creates a blank tab if automatic selection finds none and no native dialog is open. Invalid IDs return PAGE_NOT_FOUND without switching tabs. Selection does not bring the tab to the foreground."
    )]
    #[serde(deserialize_with = "publish_params::optional_string")]
    page_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional total task deadline in seconds, including preparation and execution: 1-600, default 60."
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
        description = "ID of a task started by browser_run or screen_observe in the connected Runcove space."
    )]
    task_id: String,
    #[serde(default)]
    #[schemars(
        description = "Optional action: \"wait\" (default) retrieves status or waits for completion; \"cancel\" requests cancellation. The call can return before cancellation finishes."
    )]
    #[serde(deserialize_with = "publish_params::optional_choice")]
    action: Option<TaskAction>,
    #[serde(default)]
    #[schemars(
        description = "Optional maximum wait in seconds for either action: 0-20, default 20. Zero returns current status immediately. Does not extend the task deadline."
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
        !(name.starts_with("browser_") || name == "screen_observe")
            || self.browser.load(std::sync::atomic::Ordering::SeqCst)
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
        description = "Read content from a tab in the shared browser of the connected Runcove space. Returns content when available, tab details, open tabs, browser status, and control mode. Does not navigate, change focus, or take control; available during human control."
    )]
    async fn browser_observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.recent_tasks_limit.is_some_and(|limit| limit > 10) {
            return Err(McpError::invalid_params(
                "recent_tasks_limit must be 0..10",
                None,
            ));
        }
        let hub = self.desktop().await?;
        let view = match params.view.unwrap_or(BrowserView::Dom) {
            BrowserView::Dom => "dom",
            BrowserView::Ax => "ax",
            BrowserView::Page => "page",
            BrowserView::Desktop => "desktop",
        };
        Ok(browser_result(
            hub.observe(params.page_id, view, params.recent_tasks_limit.unwrap_or(0))
                .await,
        ))
    }
    #[tool(
        description = "Run JavaScript to automate the browser and desktop of the connected Runcove space. Requires AI control; one browser_run task at a time. Returns the script's value and explicit outputs, without an automatic page observation or screenshot. If still running after up to 8 seconds, use the returned task_id with browser_task. After an error or lost response, inspect current state before repeating actions."
    )]
    async fn browser_run(
        &self,
        Parameters(params): Parameters<RunParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.code.is_empty()
            || params.code.len() > 65536
            || params
                .timeout_secs
                .is_some_and(|timeout| !(1..=600).contains(&timeout))
        {
            return Err(McpError::invalid_params(
                "code must be 1..65536 UTF-8 bytes and timeout_secs must be 1..600",
                None,
            ));
        }
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
        description = "Capture one or more screenshots without navigating, changing focus, scrolling, or taking control. Can run during human control or browser_run. Returns progress while state=\"running\", then captured images in order with timing and dimensions, including partial results after failure or cancellation. Images may be resized or compressed. If still running after up to 8 seconds, use the returned task_id with browser_task."
    )]
    async fn screen_observe(
        &self,
        Parameters(params): Parameters<ScreenParams>,
    ) -> Result<CallToolResult, McpError> {
        let count = params.count.unwrap_or(1);
        let interval = params.interval_ms.unwrap_or(1000);
        if !(1..=8).contains(&count) || (count as u64 - 1).saturating_mul(interval) >= 600000 {
            return Err(McpError::invalid_params(
                "count must be 1..8 and the capture schedule must fit within 600 seconds",
                None,
            ));
        }
        if let Some(directory) = &params.save_dir {
            if !params.save_original
                || !std::path::Path::new(directory).is_absolute()
                || directory.contains('\0')
            {
                return Err(McpError::invalid_params(
                    "save_dir requires save_original=true and an absolute directory path",
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
                    interval_ms: interval,
                    save_original: params.save_original,
                    save_dir: params.save_dir,
                },
            )
            .await,
        ))
    }
    #[tool(
        description = "Check, wait for, or request cancellation of an automation or screenshot task. state=\"running\" means work is ongoing; \"succeeded\", \"failed\", \"timed_out\", and \"cancelled\" are final states. Cancellation preserves available outputs and does not undo website actions. Results remain available for up to 30 minutes after completion and are lost on service restart. If a task is unavailable, inspect the page and files before repeating actions."
    )]
    async fn browser_task(
        &self,
        Parameters(params): Parameters<TaskParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.wait_secs.is_some_and(|wait| wait > 20) {
            return Err(McpError::invalid_params("wait_secs must be 0..20", None));
        }
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
        description = "Publish and manage authenticated web access to files, directories, or running HTTP services in the connected Runcove space. Updating an existing name preserves the URL and credentials. Share the returned URL, HTTP Basic username, and password with the user. Publications end when the space stops or the Runcove service restarts."
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
        description = "Run Bash commands in the connected Runcove space. Each call uses a separate non-interactive shell; variables and directory changes do not carry over. Use /workspace for persistent files. Returns the exit code and size-limited, merged stdout/stderr; check the exit code to determine command success. For background services, redirect stdin and send stdout/stderr to a log file. After a timeout or lost response, inspect state before retrying. Keep reserved paths /opt/agent-space, /tmp/agent-space, and /workspace/.agent-space intact."
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
            params.cmd
        );

        tracing::Span::current().record("workspace", session.workspace_name.as_str());
        info!(workspace = %session.workspace_name, command_bytes = params.cmd.len(), timeout_secs, "bash exec");
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
        }
    }

    #[instrument(skip(self, params, context), fields(workspace), level = "info")]
    #[tool(
        description = "Read text lines or view a supported image from a file. Text is decoded as UTF-8 with line numbers; invalid bytes become replacement characters. Text output is bounded; request a smaller line range if truncated."
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
        description = "Create a UTF-8 text file or replace its full contents. Creates missing parent directories and returns the number of bytes written. After a timeout or lost response, read the file before retrying."
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
        description = "Replace one exact text passage in an existing UTF-8 file. A missing or non-unique match returns an error without changing the file. Returns a replacement confirmation on success. After a timeout or lost response, read the file before retrying."
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
                 Host-managed services are documented in /opt/agent-space/README.md when installed; do not modify their reserved files or stop them manually. web_publish publishes selected files, directories or running HTTP services; it does not control DAV.{}",if self.browser.load(std::sync::atomic::Ordering::SeqCst) {"\nThis space also provides browser_observe, screen_observe, browser_run and browser_task for one shared visible browser. Browser task deadlines include preparation; polling does not extend them. Tool descriptions contain all required usage guidance."} else {""}),
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
            let value = serde_json::from_str::<serde_json::Value>(&message).unwrap_or_else(
                |_| json!({"error":browser_error(&message),"side_effects":"unknown"}),
            );
            return CallToolResult::error(vec![Content::text(value.to_string())]);
        }
    };
    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
        value["error"] = browser_error(error);
    }
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
    // Capture ownership stays in the host cache; callers need only the public
    // identity and dimensions used for pointer actions.
    if let Some(captures) = value["captures"].as_array_mut() {
        for capture in captures {
            if let Some(object) = capture.as_object_mut() {
                object.remove("window");
                object.remove("at");
            }
        }
    }
    if value.get("frames").is_some() {
        value.as_object_mut().unwrap().remove("captures");
    }
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
        assert_eq!(
            body["error"]["message"].as_str().unwrap().chars().count(),
            2048
        );
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
            json!({"cmd":" echo hi ","workdir":"", "timeout_secs":null,"unused":1}),
        )
        .unwrap();
        assert!(bash.workdir.is_none());
        assert!(bash.timeout_secs.is_none());
        assert_eq!(bash.cmd, " echo hi ");
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
            serde_json::from_value::<BashParams>(json!({"cmd":"x","timeout_secs":"bad"})).is_err()
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
