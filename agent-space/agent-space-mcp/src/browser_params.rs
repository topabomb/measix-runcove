//! Public structured browser contract. Execution and task ownership stay in DesktopHub.
use crate::publish_params;
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotParams {
    #[serde(default, deserialize_with = "publish_params::optional_choice")]
    #[schemars(
        description = "Default page observes the selected page; tabs lists open pages only, ignoring page-specific options."
    )]
    pub view: Option<SnapshotView>,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "Page ID from browser_snapshot(view='tabs'). Default: focused tab, or an available tab."
    )]
    pub page_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Compare with this page's last unscoped snapshot or action feedback; default false. A new document returns a full snapshot."
    )]
    pub diff: Option<bool>,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "Element ref from an observation. Scope the observation to its subtree. Default: whole page. Images remain page-wide. Excludes diff=true."
    )]
    pub r#ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Include a page image annotated with element refs; default false. For unannotated images or saved originals, use screen_observe."
    )]
    pub image: Option<bool>,
    #[serde(default)]
    #[schemars(description = "Include long page text excerpts in the observation; default false.")]
    pub text: Option<bool>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 10))]
    #[schemars(
        description = "Number of recent browser and screenshot task summaries, newest first: 0-10; default 0."
    )]
    pub recent_tasks_limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotView {
    Page,
    Tabs,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TabAction {
    New,
    Close,
    Focus,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NavigateMode {
    Open,
    Back,
    Forward,
    Reload,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActionTarget {
    #[default]
    Page,
    Desktop,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowMode {
    Normal,
    Maximized,
    Fullscreen,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "do", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActStep {
    /// Click an element ref, or a point in a returned page or desktop image. Prefer ref when available.
    Click {
        #[schemars(description = "Element ref from an observation.")]
        #[serde(default)]
        r#ref: Option<String>,
        #[serde(default)]
        #[schemars(
            description = "For coordinates: image_id from browser_snapshot or screen_observe; copy unchanged."
        )]
        image_id: Option<String>,
        #[serde(default)]
        #[schemars(
            range(min = 0),
            description = "Horizontal coordinate in returned image pixels, less than image width."
        )]
        x: Option<f64>,
        #[serde(default)]
        #[schemars(
            range(min = 0),
            description = "Vertical coordinate in returned image pixels, less than image height."
        )]
        y: Option<f64>,
        #[serde(default)]
        #[schemars(
            description = "Mouse button: \"left\", \"right\" or \"middle\"; default \"left\"."
        )]
        button: Option<MouseButton>,
        #[serde(default)]
        #[schemars(description = "Double-click instead of a single click; default false.")]
        double: Option<bool>,
    },
    /// Replace the text of an editable page element.
    Fill {
        #[schemars(description = "Editable element ref from an observation.")]
        r#ref: String,
        #[schemars(description = "Replacement text; an empty string clears the element.")]
        text: String,
        #[serde(default)]
        #[schemars(description = "Press Enter after filling; default false.")]
        submit: Option<bool>,
    },
    /// Select one option in a native HTML select element.
    Select {
        #[schemars(description = "Native select element ref from an observation.")]
        r#ref: String,
        #[schemars(
            description = "Exact option value or label. Value matches take precedence; the match must be unique."
        )]
        value: String,
    },
    /// Set the checked state of a checkable page element.
    Check {
        #[schemars(description = "Checkable element ref from an observation.")]
        r#ref: String,
        #[serde(default)]
        #[schemars(description = "Desired checked state; default true.")]
        checked: Option<bool>,
    },
    /// Send a key or chord to the selected page, an element ref, or the focused desktop control.
    Press {
        #[serde(default)]
        #[schemars(
            description = "Input target; default page. desktop uses the focused desktop control and ignores ref."
        )]
        target: Option<ActionTarget>,
        #[schemars(length(min = 1))]
        #[schemars(description = "Browser key or chord, such as \"Enter\" or \"Control+a\".")]
        key: String,
        #[serde(default)]
        #[schemars(
            description = "Element ref from an observation. When omitted, send keys to the selected page's focused element."
        )]
        r#ref: Option<String>,
    },
    /// Move the pointer over the referenced page element.
    Hover {
        #[schemars(description = "Element ref from an observation.")]
        r#ref: String,
    },
    /// Drag between element refs, or between points in one returned image. Page image endpoints must fit in one viewport.
    Drag {
        #[serde(default)]
        #[schemars(
            description = "Source element ref; when set, use to_ref and ignore image coordinates."
        )]
        r#ref: Option<String>,
        #[serde(default)]
        #[schemars(description = "Destination element ref.")]
        to_ref: Option<String>,
        #[serde(default)]
        #[schemars(
            description = "For coordinates: image_id from browser_snapshot or screen_observe; copy unchanged."
        )]
        image_id: Option<String>,
        #[serde(default)]
        #[schemars(
            range(min = 0),
            description = "Start horizontal coordinate in returned image pixels."
        )]
        x: Option<f64>,
        #[serde(default)]
        #[schemars(
            range(min = 0),
            description = "Start vertical coordinate in returned image pixels."
        )]
        y: Option<f64>,
        #[serde(default)]
        #[schemars(
            range(min = 0),
            description = "Destination horizontal coordinate in the same image pixels."
        )]
        to_x: Option<f64>,
        #[serde(default)]
        #[schemars(
            range(min = 0),
            description = "Destination vertical coordinate in the same image pixels."
        )]
        to_y: Option<f64>,
    },
    /// Upload files from the connected Runcove space through a page's file input or file chooser.
    Upload {
        #[schemars(
            description = "Observed ref of a file input or an element that opens a file chooser."
        )]
        r#ref: String,
        #[schemars(length(min = 1, max = 20))]
        #[schemars(
            description = "1-20 absolute paths to readable regular files in the connected Runcove space, using the same filesystem as bash/read/write."
        )]
        paths: Vec<String>,
    },
    /// Click a page element to start a download, save the file in the Runcove space, and return its file information.
    Download {
        #[schemars(description = "Observed ref of the element that starts the download.")]
        r#ref: String,
        #[serde(default)]
        #[schemars(range(min = 1, max = 60))]
        #[schemars(
            description = "Seconds to wait for the download event and, separately, save it: 1-60; default 30."
        )]
        timeout_secs: Option<u64>,
    },
    /// Scroll an element into view, scroll to find text, or send vertical wheel input to the selected page.
    Scroll {
        #[serde(default)]
        #[schemars(
            description = "Scroll target; default page. desktop requires image_id, x, y and delta; ignores page scroll options."
        )]
        target: Option<ActionTarget>,
        #[serde(default)]
        #[schemars(description = "For desktop: image_id from screen_observe(view=desktop).")]
        image_id: Option<String>,
        #[serde(default)]
        #[schemars(
            range(min = 0),
            description = "For desktop: horizontal coordinate in returned image pixels."
        )]
        x: Option<f64>,
        #[serde(default)]
        #[schemars(
            range(min = 0),
            description = "For desktop: vertical coordinate in returned image pixels."
        )]
        y: Option<f64>,
        #[serde(default)]
        #[schemars(range(min = -30, max = 30), description = "For desktop: wheel steps, -30..30; positive scrolls down.")]
        delta: Option<i32>,
        #[serde(default)]
        #[schemars(
            description = "Element ref returned by browser_snapshot, browser_read or browser_act; copy unchanged to scroll it into view. Ignores other scroll options."
        )]
        r#ref: Option<String>,
        #[serde(default)]
        #[schemars(length(min = 1))]
        #[schemars(
            description = "Nonempty text to find by scrolling, checking the selected page and its iframes. Uses substring matching unless exact=true."
        )]
        until_text: Option<String>,
        #[serde(default)]
        #[schemars(range(min = -20000, max = 20000))]
        #[schemars(
            description = "Vertical wheel distance per scroll in CSS pixels: -20000..20000; default 800. Positive values scroll down."
        )]
        dy: Option<i32>,
        #[serde(default)]
        #[schemars(
            description = "For until_text only: require an exact text match; default false."
        )]
        exact: Option<bool>,
        #[serde(default)]
        #[schemars(range(min = 1, max = 30))]
        #[schemars(
            description = "For until_text only: maximum number of scrolls, 1-30; default 15."
        )]
        max: Option<u32>,
    },
    /// Wait for visible text or an element name, including iframes, or pause. Provide one of text, gone or secs. Element interactions already wait for the target to be actionable.
    Wait {
        #[serde(default)]
        #[schemars(length(min = 1))]
        #[schemars(
            description = "Wait for visible text or an accessible element name to appear. Case-insensitive substring, as in observations."
        )]
        text: Option<String>,
        #[serde(default)]
        #[schemars(length(min = 1))]
        #[schemars(
            description = "Wait until no visible text or accessible element name matches this case-insensitive substring."
        )]
        gone: Option<String>,
        #[serde(default)]
        #[schemars(range(min = 0, max = 60))]
        #[schemars(
            description = "Fixed delay in seconds, 0-60, independent of page state. Ignores timeout_secs."
        )]
        secs: Option<u64>,
        #[serde(default)]
        #[schemars(range(min = 1, max = 60))]
        #[schemars(
            description = "Page-condition timeout in seconds, 1-60; default 60. Returns as soon as the condition is met."
        )]
        timeout_secs: Option<u64>,
    },
    /// Open a URL, go back or forward, or reload the selected page.
    Navigate {
        #[serde(default)]
        #[schemars(description = "Navigation mode: open (default), back, forward or reload.")]
        mode: Option<NavigateMode>,
        #[serde(default)]
        #[schemars(
            description = "Required for open: absolute http, https, file, about or data URL. Other modes ignore url. File URLs use the connected space's filesystem."
        )]
        url: Option<String>,
    },
    /// Create, close or focus a browser tab. Creating or focusing a tab selects it for subsequent page actions in this batch.
    Tab {
        #[schemars(
            description = "\"new\" creates a tab; \"close\" closes a tab; \"focus\" brings a tab to the foreground."
        )]
        action: TabAction,
        #[serde(default)]
        #[schemars(
            description = "For close/focus only: tab ID from browser_snapshot(view='tabs'). Default: the currently selected tab."
        )]
        page_id: Option<String>,
        #[serde(default)]
        #[schemars(
            description = "For new only: initial absolute URL using http, https, file, about or data. Default: a blank page. File URLs refer to the connected Runcove space's filesystem."
        )]
        url: Option<String>,
    },
    /// Set the policy for JS alert, confirm or prompt dialogs opened by later page steps in this batch. Place this step before the triggering action. Without a policy, dialogs are dismissed and reported. For an already-open native dialog, use a desktop image and desktop actions.
    Dialog {
        #[schemars(description = "true accepts subsequent dialogs; false dismisses them.")]
        accept: bool,
        #[serde(default)]
        #[schemars(description = "Text supplied when accepting a prompt dialog.")]
        text: Option<String>,
    },
    /// Enter text into the currently focused control on the Runcove desktop, including native dialogs.
    DesktopType {
        #[schemars(description = "Text to enter; at most 65536 UTF-8 bytes.")]
        text: String,
    },
    /// Change the state of the browser window containing the selected tab.
    Window {
        #[schemars(
            description = "Browser window state: \"normal\", \"maximized\" or \"fullscreen\"."
        )]
        mode: WindowMode,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActAction {
    Execute,
    Wait,
    Cancel,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActThen {
    Diff,
    Snapshot,
    None,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActParams {
    #[serde(default, deserialize_with = "publish_params::optional_choice")]
    #[schemars(
        description = "Default execute runs actions. wait/cancel uses task_id to retrieve or cancel a task; unrelated execution options are ignored, but new actions are rejected."
    )]
    pub op: Option<ActAction>,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "Required for wait/cancel: task_id returned by a browser or screenshot tool."
    )]
    pub task_id: Option<String>,
    #[serde(default)]
    #[schemars(range(min = 0, max = 30))]
    #[schemars(
        description = "Task wait limit in seconds; default 30, zero polls immediately. Ignored for execute. Does not extend the task deadline."
    )]
    pub wait_secs: Option<u64>,
    #[serde(default, deserialize_with = "optional_steps")]
    #[schemars(length(min = 1, max = 20))]
    #[schemars(
        description = "Ordered steps for execute. Element interactions auto-wait. Validated before execution; a runtime failure stops without undoing completed steps. Requires AI control."
    )]
    pub actions: Option<Vec<ActStep>>,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "Execute only: initial tab for page actions, from browser_snapshot(view='tabs'). Default: the focused tab, or an available tab. Selecting it does not redirect desktop keyboard input."
    )]
    pub page_id: Option<String>,
    #[serde(default, deserialize_with = "publish_params::optional_choice")]
    #[schemars(
        description = "End-of-batch feedback: diff (default), snapshot, or none. Diff compares the previous returned observation; without a usable baseline, returns a snapshot. Failures may include a diagnostic observation."
    )]
    pub then: Option<ActThen>,
    #[serde(default)]
    #[schemars(range(min = 1, max = 30))]
    #[schemars(
        description = "Element interaction timeout in seconds, 1-30; default 5. Navigation, page-condition waits and downloads have separate limits."
    )]
    pub action_timeout_secs: Option<u64>,
    #[serde(default)]
    #[schemars(range(min = 1, max = 600))]
    #[schemars(
        description = "Task deadline in seconds, including preparation and all steps; default 120."
    )]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadExtract {
    Table,
    Links,
    Fields,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserReadParams {
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "Page ID from browser_snapshot(view='tabs'). Default: focused tab, or an available tab."
    )]
    pub page_id: Option<String>,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "Element ref from an observation. Read its content or extract within its subtree. Default: main page content. Also scopes query results."
    )]
    pub r#ref: Option<String>,
    #[serde(default, deserialize_with = "publish_params::optional_string")]
    #[schemars(
        description = "Find observed elements by a case-insensitive substring of their name or text, including iframes. Optional ref scopes the search. Returns up to 20 matches. Excludes extract; ignores max_chars."
    )]
    pub query: Option<String>,
    #[serde(default, deserialize_with = "publish_params::optional_choice")]
    #[schemars(
        description = "Extract tables, links or fields within ref, or across the page and iframes. Default: plain text. Excludes query; ignores max_chars. Results indicate truncation."
    )]
    pub extract: Option<ReadExtract>,
    #[serde(default)]
    #[schemars(range(min = 1, max = 20000))]
    #[schemars(
        description = "Plain-text character limit; default 4000. Ignored for query or extract."
    )]
    pub max_chars: Option<u64>,
}

pub fn is_ref(s: &str) -> bool {
    s.len() == 11
        && s.starts_with('e')
        && s[1..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn valid_url(s: &str) -> bool {
    url::Url::parse(s)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "file" | "about" | "data"))
}
/// Normalize known but inactive fields before their types or ranges are checked.
fn optional_steps<'de, D>(d: D) -> Result<Option<Vec<ActStep>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let raw = Option::<Vec<serde_json::Value>>::deserialize(d)?;
    raw.map(|items| {
        items
            .into_iter()
            .map(|mut v| {
                if let Some(m) = v.as_object_mut() {
                    let action = m
                        .get("do")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let has_ref = m.get("ref").is_some_and(|v| !v.is_null());
                    let desktop = m.get("target").and_then(|v| v.as_str()) == Some("desktop");
                    let remove: &[&str] = match action.as_str() {
                        "click" if has_ref => &["image_id", "x", "y"],
                        "drag" if has_ref => &["image_id", "x", "y", "to_x", "to_y"],
                        "press" if desktop => &["ref"],
                        "scroll" if desktop => &["ref", "until_text", "dy", "exact", "max"],
                        "scroll" if has_ref => &[
                            "image_id",
                            "x",
                            "y",
                            "delta",
                            "until_text",
                            "dy",
                            "exact",
                            "max",
                        ],
                        "scroll" => &["image_id", "x", "y", "delta"],
                        "wait" if m.get("secs").is_some_and(|v| !v.is_null()) => &["timeout_secs"],
                        "tab" if m.get("action").and_then(|v| v.as_str()) == Some("new") => {
                            &["page_id"]
                        }
                        "tab" => &["url"],
                        "navigate"
                            if m.get("mode")
                                .and_then(|v| v.as_str())
                                .is_some_and(|v| v != "open") =>
                        {
                            &["url"]
                        }
                        _ => &[],
                    };
                    for key in remove {
                        m.remove(*key);
                    }
                    if action == "scroll"
                        && !desktop
                        && !has_ref
                        && m.get("until_text").is_none_or(|v| v.is_null())
                    {
                        m.remove("exact");
                        m.remove("max");
                    }
                }
                serde_json::from_value(v).map_err(D::Error::custom)
            })
            .collect()
    })
    .transpose()
}
pub fn check_steps(steps: &[ActStep]) -> Result<(), String> {
    if steps.is_empty() || steps.len() > 20 {
        return Err("actions must contain 1-20 steps".into());
    }
    let coords = |id: &Option<String>, xs: &[Option<f64>]| {
        id.as_ref().is_some_and(|s| !s.is_empty())
            && xs
                .iter()
                .all(|n| n.is_some_and(|n| n.is_finite() && n >= 0.0))
    };
    for (i, s) in steps.iter().enumerate() {
        let valid = match s {
            ActStep::Click {
                r#ref,
                image_id,
                x,
                y,
                ..
            } => r#ref
                .as_deref()
                .map_or_else(|| coords(image_id, &[*x, *y]), is_ref),
            ActStep::Fill { r#ref, .. }
            | ActStep::Select { r#ref, .. }
            | ActStep::Check { r#ref, .. }
            | ActStep::Hover { r#ref } => is_ref(r#ref),
            ActStep::Drag {
                r#ref,
                to_ref,
                image_id,
                x,
                y,
                to_x,
                to_y,
            } => r#ref.as_deref().map_or_else(
                || to_ref.is_none() && coords(image_id, &[*x, *y, *to_x, *to_y]),
                |r| is_ref(r) && to_ref.as_deref().is_some_and(is_ref),
            ),
            ActStep::Press { key, r#ref, target } => {
                !key.is_empty()
                    && if target.unwrap_or_default() == ActionTarget::Desktop {
                        key.len() <= 80
                            && key
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_+ -".contains(&b))
                    } else {
                        r#ref.as_deref().is_none_or(is_ref)
                    }
            }
            ActStep::Upload { r#ref, paths } => {
                is_ref(r#ref)
                    && (1..=20).contains(&paths.len())
                    && paths
                        .iter()
                        .all(|p| p.starts_with('/') && !p.contains('\0'))
            }
            ActStep::Download {
                r#ref,
                timeout_secs,
            } => is_ref(r#ref) && timeout_secs.is_none_or(|n| (1..=60).contains(&n)),
            ActStep::Scroll {
                target,
                image_id,
                x,
                y,
                delta,
                r#ref,
                until_text,
                dy,
                max,
                ..
            } => {
                if target.unwrap_or_default() == ActionTarget::Desktop {
                    coords(image_id, &[*x, *y]) && delta.is_some_and(|n| (-30..=30).contains(&n))
                } else {
                    r#ref.as_deref().is_none_or(is_ref)
                        && until_text.as_ref().is_none_or(|s| !s.is_empty())
                        && dy.is_none_or(|n| (-20000..=20000).contains(&n))
                        && max.is_none_or(|n| (1..=30).contains(&n))
                }
            }
            ActStep::Wait {
                text,
                gone,
                secs,
                timeout_secs,
            } => {
                usize::from(text.is_some())
                    + usize::from(gone.is_some())
                    + usize::from(secs.is_some())
                    == 1
                    && text.as_ref().is_none_or(|s| !s.is_empty())
                    && gone.as_ref().is_none_or(|s| !s.is_empty())
                    && secs.is_none_or(|n| n <= 60)
                    && timeout_secs.is_none_or(|n| (1..=60).contains(&n))
            }
            ActStep::Navigate { mode, url } => {
                !matches!(mode, None | Some(NavigateMode::Open))
                    || url.as_deref().is_some_and(valid_url)
            }
            ActStep::Tab {
                action,
                page_id,
                url,
            } => match action {
                TabAction::New => url.as_deref().is_none_or(valid_url),
                _ => page_id.as_ref().is_none_or(|s| !s.trim().is_empty()),
            },
            ActStep::DesktopType { text } => text.len() <= 65536,
            ActStep::Window { .. } => true,
            ActStep::Dialog { .. } => steps[i + 1..].iter().any(|step| {
                !matches!(
                    step,
                    ActStep::Dialog { .. }
                        | ActStep::Tab { .. }
                        | ActStep::Window { .. }
                        | ActStep::DesktopType { .. }
                        | ActStep::Press {
                            target: Some(ActionTarget::Desktop),
                            ..
                        }
                        | ActStep::Scroll {
                            target: Some(ActionTarget::Desktop),
                            ..
                        }
                        | ActStep::Wait { secs: Some(_), .. }
                )
            }),
        };
        if !valid {
            return Err(format!(
                "invalid parameters in step {i}; consult the action schema; nothing was executed"
            ));
        }
    }
    Ok(())
}
impl ActParams {
    /// Discard fields that cannot affect the selected operation before validation.
    /// Conflicting intentions (new actions and an existing task) remain errors.
    pub fn normalize(mut self) -> Self {
        match self.op.unwrap_or(ActAction::Execute) {
            ActAction::Execute => {
                self.wait_secs = None;
            }
            ActAction::Wait | ActAction::Cancel => {
                self.page_id = None;
                self.then = None;
                self.action_timeout_secs = None;
                self.timeout_secs = None;
                if self.actions.as_ref().is_some_and(Vec::is_empty) {
                    self.actions = None;
                }
            }
        }
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        match self.op.unwrap_or(ActAction::Execute) {
            ActAction::Execute => {
                if self.task_id.is_some() {
                    return Err("execute requires new actions, not task_id; use op=wait/cancel for an existing task".into());
                }
                if self.timeout_secs.is_some_and(|n| !(1..=600).contains(&n))
                    || self
                        .action_timeout_secs
                        .is_some_and(|n| !(1..=30).contains(&n))
                {
                    return Err("timeout_secs must be 1-600 and action_timeout_secs 1-30".into());
                }
                check_steps(self.actions.as_deref().unwrap_or_default())
            }
            ActAction::Wait | ActAction::Cancel => {
                if self.task_id.is_none() || self.wait_secs.is_some_and(|n| n > 30) {
                    return Err("wait/cancel require task_id and wait_secs 0-30".into());
                }
                if self.actions.is_some() {
                    return Err("wait/cancel cannot execute new actions; submit them separately with op=execute".into());
                }
                Ok(())
            }
        }
    }
}
impl BrowserReadParams {
    pub fn normalize(mut self) -> Self {
        if self.query.is_some() || self.extract.is_some() {
            self.max_chars = None;
        }
        self
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.r#ref.as_deref().is_some_and(|r| !is_ref(r)) {
            return Err("ref must come from browser_snapshot or browser_read");
        }
        if self.max_chars.is_some_and(|n| !(1..=20000).contains(&n)) {
            return Err("max_chars must be 1-20000");
        }
        if self.query.is_some() && self.extract.is_some() {
            return Err("choose query to find elements or extract to read structured data");
        }
        if self.extract.is_some() && self.max_chars.is_some() {
            return Err("max_chars only applies to plain text");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_batches_before_execution_and_separates_task_control() {
        for v in [
            json!({"actions":[{"do":"click","ref":"old"}]}),
            json!({"op":"wait","task_id":"id","actions":[{"do":"navigate","mode":"reload"}]}),
            json!({"task_id":"id","actions":[{"do":"navigate","mode":"reload"}]}),
            json!({"op":"cancel"}),
            json!({"op":"wait","task_id":"id","wait_secs":31}),
            json!({"op":"cancel","task_id":"id","wait_secs":31}),
            json!({"actions":[{"do":"wait","secs":1,"text":"x"}]}),
        ] {
            assert!(serde_json::from_value::<ActParams>(v)
                .unwrap()
                .normalize()
                .validate()
                .is_err());
        }
        for v in [
            json!({"actions":[{"do":"wait","secs":1}]}),
            json!({"op":"wait","task_id":"id","wait_secs":0}),
            json!({"op":"cancel","task_id":"id"}),
            json!({"op":"wait","task_id":"id","wait_secs":30}),
            json!({"op":"cancel","task_id":"id","wait_secs":30}),
            json!({"op":"execute","actions":[{"do":"navigate","mode":"reload"}],"wait_secs":999}),
            json!({"op":"wait","task_id":"id","actions":[],"page_id":"old","then":"snapshot","timeout_secs":0,"action_timeout_secs":0}),
        ] {
            serde_json::from_value::<ActParams>(v)
                .unwrap()
                .normalize()
                .validate()
                .unwrap();
        }
        assert!(serde_json::from_value::<ActParams>(json!({"code":"return 1"})).is_err());
        assert!(serde_json::from_value::<ActStep>(
            json!({"do":"click","ref":"eabcdefghij","selector":"button"})
        )
        .is_err());
    }
    #[test]
    fn unified_actions_ignore_inactive_fields_before_type_checks() {
        for v in [
            json!({"do":"click","ref":"eabcdefghij","image_id":{},"x":"unused","y":-1}),
            json!({"do":"drag","ref":"eabcdefghij","to_ref":"e0123456789","image_id":{},"to_x":false}),
            json!({"do":"press","target":"desktop","key":"Control+a","ref":false}),
            json!({"do":"scroll","ref":"eabcdefghij","dy":999999,"max":false,"until_text":{},"exact":"unused"}),
            json!({"do":"scroll","target":"desktop","image_id":"image","x":0,"y":0,"delta":0,"ref":false,"dy":{}}),
            json!({"do":"tab","action":"new","page_id":false}),
            json!({"do":"tab","action":"close","url":false}),
            json!({"do":"navigate","mode":"reload","url":false}),
            json!({"do":"wait","secs":0,"timeout_secs":false}),
        ] {
            let params: ActParams = serde_json::from_value(json!({"actions":[v]})).unwrap();
            params.normalize().validate().unwrap();
        }
        for v in [
            json!({"actions":[{"do":"click","x":0,"y":0}]}),
            json!({"actions":[{"do":"drag","ref":"eabcdefghij","image_id":"image","x":0,"y":0,"to_x":1,"to_y":1}]}),
            json!({"actions":[{"do":"scroll","target":"desktop","image_id":"image","x":0,"y":0}]}),
            json!({"actions":[{"do":"navigate"}]}),
        ] {
            assert!(serde_json::from_value::<ActParams>(v)
                .unwrap()
                .validate()
                .is_err());
        }
        for v in [
            json!({"action":"wait","task_id":"task"}),
            json!({"actions":[{"do":"reload"}]}),
            json!({"actions":[{"do":"click_at","image_id":"i","x":0,"y":0}]}),
            json!({"actions":[{"do":"click","ref":"eabcdefghij","to_x":0}]}),
        ] {
            assert!(serde_json::from_value::<ActParams>(v).is_err());
        }
    }
    #[test]
    fn seconds_limits_and_irrelevant_wait_deadlines() {
        for value in [
            json!({"actions":[{"do":"wait","secs":0}]}),
            json!({"actions":[{"do":"wait","secs":60,"timeout_secs":0}],"action_timeout_secs":30}),
            json!({"actions":[{"do":"wait","text":"ready","timeout_secs":60}]}),
            json!({"actions":[{"do":"wait","gone":"busy"}]}),
            json!({"actions":[{"do":"download","ref":"eabcdefghij","timeout_secs":60}]}),
        ] {
            let p = serde_json::from_value::<ActParams>(value)
                .unwrap()
                .normalize();
            p.validate().unwrap();
        }
        for value in [
            json!({"actions":[{"do":"wait","secs":61}]}),
            json!({"actions":[{"do":"wait","text":"ready","timeout_secs":0}]}),
            json!({"actions":[{"do":"wait","gone":"busy","timeout_secs":61}]}),
            json!({"actions":[{"do":"wait","secs":0}],"action_timeout_secs":0}),
            json!({"actions":[{"do":"wait","secs":0}],"action_timeout_secs":31}),
            json!({"actions":[{"do":"download","ref":"eabcdefghij","timeout_secs":61}]}),
        ] {
            assert!(serde_json::from_value::<ActParams>(value)
                .unwrap()
                .normalize()
                .validate()
                .is_err());
        }
        let schema = serde_json::to_string(&schemars::schema_for!(ActParams)).unwrap();
        assert!(!schema.contains("timeout_ms"));
        assert!(!schema.contains("\"ms\""));
    }
    #[test]
    fn optional_null_and_read_modes_are_explicit() {
        let p: SnapshotParams =
            serde_json::from_value(json!({"view":" ","image":null,"diff":null,"text":null}))
                .unwrap();
        assert!(p.view.is_none());
        assert!(p.image.is_none());
        for value in [
            json!({"query":"foo","max_chars":0}),
            json!({"query":"foo","ref":"eabcdefghij","max_chars":999999}),
            json!({"extract":"fields","max_chars":0}),
            json!({"query":" ","ref":"","extract":null,"max_chars":4000}),
        ] {
            let p = serde_json::from_value::<BrowserReadParams>(value)
                .unwrap()
                .normalize();
            p.validate().unwrap();
            if p.query.is_some() || p.extract.is_some() {
                assert!(p.max_chars.is_none());
            }
        }
        for value in [
            json!({"query":"foo","extract":"fields"}),
            json!({"max_chars":0}),
            json!({"query":"foo","ref":"old"}),
        ] {
            assert!(serde_json::from_value::<BrowserReadParams>(value)
                .unwrap()
                .normalize()
                .validate()
                .is_err());
        }
        let p: BrowserReadParams =
            serde_json::from_value(json!({"ref":"eabcdefghij","extract":"fields"})).unwrap();
        p.validate().unwrap();
        let p =
            serde_json::from_value::<ActStep>(json!({"do":"fill","ref":"eabcdefghij","text":""}))
                .unwrap();
        assert!(matches!(p, ActStep::Fill { text, .. } if text.is_empty()));
    }
}
