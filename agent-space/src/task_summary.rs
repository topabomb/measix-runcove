//! Compact request summaries, never execution claims or copies of entered content.
use serde_json::Value;

pub(super) fn tool(body: &Value) -> &'static str {
    match body["operation"].as_str() {
        Some("act") => "browser_act",
        Some("snapshot") => "browser_snapshot",
        Some("read") => "browser_read",
        _ => "screen_observe",
    }
}
fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
fn short(s: &str, limit: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= limit {
        s
    } else {
        s.chars().take(limit - 1).chain(['…']).collect()
    }
}
fn quote(v: &Value) -> String {
    format!("“{}”", short(text(v), 40))
}
fn url(v: &Value) -> String {
    let Ok(mut u) = url::Url::parse(text(v)) else {
        return "URL".into();
    };
    if !matches!(u.scheme(), "http" | "https" | "file" | "about") {
        return format!("{} URL", u.scheme());
    }
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.set_query(None);
    u.set_fragment(None);
    short(u.as_str(), 60)
}
fn target(a: &Value) -> String {
    if let Some(r) = a["ref"].as_str() {
        return r.into();
    }
    if a["image_id"].is_string() {
        return format!("({}, {}) on image {}", a["x"], a["y"], text(&a["image_id"]));
    }
    if a["target"] == "desktop" {
        "desktop".into()
    } else {
        "page".into()
    }
}
fn step(a: &Value) -> String {
    let t = target(a);
    match text(&a["do"]) {
        "click" => format!(
            "{} {t}",
            if a["double"] == true {
                "Double-click"
            } else {
                "Click"
            }
        ),
        "fill" => format!("Fill {t}"),
        "select" => format!("Select option in {t}"),
        "check" => format!(
            "{} {t}",
            if a["checked"] == false {
                "Uncheck"
            } else {
                "Check"
            }
        ),
        "press" => format!("Press {} on {t}", short(text(&a["key"]), 30)),
        "hover" => format!("Hover {t}"),
        "drag" => {
            if a["ref"].is_string() {
                format!("Drag {t} to {}", text(&a["to_ref"]))
            } else {
                format!(
                    "Drag ({}, {}) to ({}, {}) on image {}",
                    a["x"],
                    a["y"],
                    a["to_x"],
                    a["to_y"],
                    text(&a["image_id"])
                )
            }
        }
        "upload" => format!("Upload {} files", a["paths"].as_array().map_or(0, Vec::len)),
        "download" => format!("Download via {t}"),
        "scroll" => {
            if a["ref"].is_string() {
                format!("Scroll to {t}")
            } else if a["until_text"].is_string() {
                format!("Scroll to find {}", quote(&a["until_text"]))
            } else {
                let delta = if a["target"] == "desktop" {
                    a["delta"].as_i64()
                } else {
                    a["dy"].as_i64()
                };
                format!(
                    "Scroll {} {}",
                    if a["target"] == "desktop" {
                        "desktop"
                    } else {
                        "page"
                    },
                    if delta.unwrap_or(800) < 0 {
                        "up"
                    } else {
                        "down"
                    }
                )
            }
        }
        "wait" => {
            if a["secs"].is_number() {
                format!("Wait {}s", a["secs"])
            } else if a["gone"].is_string() {
                format!("Wait for {} to disappear", quote(&a["gone"]))
            } else {
                format!("Wait for {}", quote(&a["text"]))
            }
        }
        "navigate" => match text(&a["mode"]) {
            "back" => "Go back".into(),
            "forward" => "Go forward".into(),
            "reload" => "Reload page".into(),
            _ => format!("Open {}", url(&a["url"])),
        },
        "tab" => match text(&a["action"]) {
            "new" => {
                if a["url"].is_string() {
                    format!("Open tab {}", url(&a["url"]))
                } else {
                    "Open tab".into()
                }
            }
            action => format!(
                "{} tab {}",
                if action == "close" { "Close" } else { "Focus" },
                a["page_id"].as_str().unwrap_or("selected")
            ),
        },
        "dialog" => format!(
            "{} subsequent JS dialogs",
            if a["accept"] == true {
                "Accept"
            } else {
                "Dismiss"
            }
        ),
        "desktop_type" => "Type into focused desktop control".into(),
        "window" => format!(
            "{} browser window",
            match text(&a["mode"]) {
                "maximized" => "Maximize",
                "fullscreen" => "Fullscreen",
                _ => "Restore",
            }
        ),
        _ => "Browser action".into(),
    }
}
pub(super) fn describe(body: &Value) -> String {
    let a = &body["args"];
    let scope = a["ref"]
        .as_str()
        .map(|r| format!("subtree {r}"))
        .unwrap_or_else(|| {
            body["page_id"]
                .as_str()
                .map(|p| format!("page {p}"))
                .unwrap_or("page".into())
        });
    let result = match text(&body["operation"]) {
        "act" => {
            let steps = a["actions"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut parts: Vec<_> = steps.iter().take(2).map(step).collect();
            if steps.len() > 2 {
                parts.push(format!("{} more steps", steps.len() - 2));
            }
            // Share the full budget; retain both steps and the remaining count.
            let suffix = if parts.len() > 2 {
                format!("; {}", parts.pop().unwrap())
            } else {
                String::new()
            };
            let budget = 120 - suffix.chars().count();
            if parts.len() == 2 {
                let lengths: Vec<_> = parts.iter().map(|s| s.chars().count()).collect();
                let first = lengths[0].min((budget - 2) - lengths[1].min((budget - 2) / 2));
                parts[0] = short(&parts[0], first);
                parts[1] = short(&parts[1], budget - 2 - first);
            }
            format!("{}{}", parts.join("; "), suffix)
        }
        "snapshot" => {
            if a["view"] == "tabs" {
                "List tabs".into()
            } else {
                format!(
                    "Observe {scope}{}",
                    if a["image"] == true {
                        " with screenshot"
                    } else {
                        ""
                    }
                )
            }
        }
        "read" => {
            if a["query"].is_string() {
                format!("Find {} in {scope}", quote(&a["query"]))
            } else if let Some(extract) = a["extract"].as_str() {
                format!(
                    "Extract {} from {scope}",
                    if extract == "table" {
                        "tables"
                    } else {
                        extract
                    }
                )
            } else {
                format!("Read text from {scope}")
            }
        }
        _ => format!(
            "Capture {}{}",
            text(&body["view"]),
            if body["count"].as_u64().unwrap_or(1) > 1 {
                format!(", {} images", body["count"])
            } else {
                String::new()
            }
        ),
    };
    short(&result, 120)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn coordinates_survive_real_image_ids_and_multiple_steps() {
        let id = "12345678-1234-1234-1234-123456789abc";
        let click = json!({"do":"click","image_id":id,"x":100,"y":200});
        let drag = json!({"do":"drag","image_id":id,"x":100,"y":200,"to_x":300,"to_y":400});
        for action in [&click, &drag] {
            let s = describe(&json!({"operation":"act","args":{"actions":[action]}}));
            assert!(s.contains(id) && s.contains("(100, 200)"), "{s}");
        }
        let s = describe(
            &json!({"operation":"act","args":{"actions":[drag,click,{"do":"wait","secs":1}]}}),
        );
        assert!(s.contains("(100, 200) to (300, 400)"), "{s}");
        assert!(s.contains("Click (100, 200)"), "{s}");
        assert!(s.ends_with("1 more steps"));
        assert!(s.chars().count() <= 120);
    }
    #[test]
    fn all_actions_and_sources_have_bounded_request_summaries() {
        for action in [
            "click",
            "fill",
            "select",
            "check",
            "press",
            "hover",
            "drag",
            "upload",
            "download",
            "scroll",
            "wait",
            "navigate",
            "tab",
            "dialog",
            "desktop_type",
            "window",
        ] {
            let b = json!({"operation":"act","args":{"actions":[{"do":action,"ref":"eabcdefghij","text":"SECRET","value":"SECRET","paths":["/secret"],"url":"https://user:pass@example.com/a?token=SECRET#SECRET","key":"Enter","action":"new","secs":4}]}});
            let s = describe(&b);
            assert!(!s.contains("SECRET") && !s.contains("pass@"), "{s}");
            assert!(!s.contains("Browser action"));
            assert!(s.chars().count() <= 120);
        }
        assert_eq!(
            describe(&json!({"operation":"snapshot","args":{"view":"tabs"}})),
            "List tabs"
        );
        assert_eq!(
            describe(&json!({"operation":"read","args":{"extract":"fields"}})),
            "Extract fields from page"
        );
        assert_eq!(
            describe(&json!({"view":"desktop","count":3})),
            "Capture desktop, 3 images"
        );
        let s = describe(
            &json!({"operation":"act","args":{"actions":[{"do":"wait","text":"🙂".repeat(200)},{"do":"wait","gone":"中".repeat(200)},{"do":"fill"}]}}),
        );
        assert!(s.ends_with("1 more steps"));
        assert!(s.chars().count() <= 120);
        assert_eq!(url(&json!("data:text/html,SECRET")), "data URL");
    }
}
