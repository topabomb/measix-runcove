use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Owned by one MCP connection, never shared through the page or workspace hub.
pub(crate) struct Feedback {
    next_order: u64,
    scope: String,
    pages: BTreeMap<String, Value>,
}

impl Default for Feedback {
    fn default() -> Self {
        Self {
            next_order: 0,
            scope: uuid::Uuid::new_v4().to_string(),
            pages: BTreeMap::new(),
        }
    }
}

impl Feedback {
    pub fn prepare(&mut self, args: &mut Value, page_id: Option<&str>) {
        self.next_order += 1;
        args["_observation_scope"] = json!(self.scope);
        args["_observation_order"] = json!(self.next_order);
        let mut entries: Vec<_> = self.pages.iter().collect();
        entries.sort_by_key(|(_, state)| std::cmp::Reverse(state["order"].as_u64()));
        let mut baselines = serde_json::Map::new();
        for (id, state) in entries {
            if page_id.is_some_and(|selected| selected != id) {
                continue;
            }
            baselines.insert(id.clone(), state.clone());
            args["_baselines"] = Value::Object(baselines.clone());
            // Preserve the helper's request bound and space for task metadata.
            if args.to_string().len() > 60 * 1024 {
                baselines.remove(id);
                args["_baselines"] = Value::Object(baselines.clone());
            }
        }
    }

    pub fn accept(&mut self, value: &mut Value) {
        let state = value
            .pointer_mut("/value")
            .and_then(Value::as_object_mut)
            .and_then(|v| v.remove("_observation"));
        let Some(state) = state else { return };
        let Some(id) = state["page_id"].as_str() else {
            return;
        };
        let order = state["order"].as_u64().unwrap_or(0);
        let current_order = self
            .pages
            .get(id)
            .and_then(|v| v["order"].as_u64())
            .unwrap_or(0);
        let own_scope = state["scope"] == self.scope;
        // A concurrently completed observation may have replaced the baseline
        // after this task started. Render the saved current tree in that case.
        // Foreign task queries also cannot assume this connection saw their base.
        if (!own_scope
            || (order > current_order
                && state["base_order"].as_u64().unwrap_or(0) != current_order))
            && value["value"].get("observation_diff").is_some()
        {
            if let Some(rows) = state["rows"].as_array() {
                let text = rows
                    .iter()
                    .filter_map(|r| r["line"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let payload = value["value"].as_object_mut().unwrap();
                payload.remove("observation_diff");
                payload.insert("observation".into(), json!(text));
            }
        }
        // Querying another connection's task cannot import its baseline. Orders
        // alone are not an ownership boundary; each request also carries a scope.
        if !own_scope || state["rows"].as_array().is_none() {
            return;
        }
        if self
            .pages
            .get(id)
            .is_some_and(|old| old["order"].as_u64().unwrap_or(0) >= order)
        {
            return;
        }
        self.pages.insert(id.to_owned(), state);
        while self.pages.len() > 8 {
            let oldest = self
                .pages
                .iter()
                .min_by_key(|(_, v)| v["order"].as_u64())
                .unwrap()
                .0
                .clone();
            self.pages.remove(&oldest);
        }
    }
}

/// Only the public MCP boundary changes; worker checkpoints remain owned by the
/// existing task supervisor. Never recurse into arbitrary website/file content.
pub(crate) fn project(value: &mut Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    if let Some(Value::Object(payload)) = root.remove("value") {
        for (key, item) in payload {
            if key != "_observation" {
                root.entry(key).or_insert(item);
            }
        }
    }
    for (old, new) in [
        ("completed", "completed_steps"),
        ("of", "total_steps"),
        ("observe_error", "observation_error"),
    ] {
        if let Some(item) = root.remove(old) {
            root.insert(new.into(), item);
        }
    }
    let mut page = serde_json::Map::new();
    for key in ["page_id", "url", "title"] {
        if let Some(item) = root.remove(key) {
            if !item.is_null() {
                page.insert(key.into(), item);
            }
        }
    }
    if !page.is_empty() {
        root.insert("page".into(), Value::Object(page));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(args: &Value, page: &str, text: &str) -> Value {
        json!({"state":"succeeded", "value": {"observation":text,
            "_observation":{"page_id":page,"scope":args["_observation_scope"],
            "order":args["_observation_order"],"document":"doc","url":"url",
            "mode":"compact","rows":[{"key":"e1","line":text,"parent":null,"depth":0}],"truncated":false}}})
    }

    #[test]
    fn sessions_and_repeated_old_task_queries_do_not_change_each_others_baselines() {
        let (mut a, mut b) = (Feedback::default(), Feedback::default());
        let mut first = json!({});
        a.prepare(&mut first, None);
        let original = response(&first, "page", "first");
        a.accept(&mut original.clone());
        b.accept(&mut original.clone());
        assert!(b.pages.is_empty());
        let mut second = json!({});
        a.prepare(&mut second, Some("page"));
        assert_eq!(second["_baselines"]["page"]["rows"][0]["line"], "first");
        a.accept(&mut response(&second, "page", "second"));
        a.accept(&mut original.clone());
        assert_eq!(a.pages["page"]["rows"][0]["line"], "second");
    }

    #[test]
    fn truncated_response_invalidates_previous_complete_baseline_and_is_private() {
        let mut state = Feedback::default();
        let mut args = json!({});
        state.prepare(&mut args, None);
        let mut result = response(&args, "page", "partial");
        result["value"]["_observation"]["truncated"] = json!(true);
        state.accept(&mut result);
        assert!(result["value"].get("_observation").is_none());
        assert_eq!(state.pages["page"]["truncated"], true);
    }

    #[test]
    fn concurrent_or_foreign_diff_without_the_published_base_returns_saved_snapshot() {
        let (mut a, mut b) = (Feedback::default(), Feedback::default());
        let mut first = json!({});
        let mut second = json!({});
        a.prepare(&mut first, None);
        a.prepare(&mut second, None);
        a.accept(&mut response(&first, "page", "first"));
        let mut stale = response(&second, "page", "second");
        stale["value"]
            .as_object_mut()
            .unwrap()
            .remove("observation");
        stale["value"]["observation_diff"] = json!("+ second");
        let mut foreign = stale.clone();
        a.accept(&mut stale);
        b.accept(&mut foreign);
        for result in [stale, foreign] {
            assert_eq!(result["value"]["observation"], "second");
            assert!(result["value"].get("observation_diff").is_none());
        }
        assert!(b.pages.is_empty());
    }

    #[test]
    fn projection_preserves_execution_errors_files_and_website_field_values() {
        let mut result = json!({"state":"failed","error":{"code":"STALE_REF"},
          "failed_step":{"step_number":2},"files":[{"path":"/workspace/file"}],
          "value":{"completed":1,"of":2,"page_id":"page","url":"url",
            "fields":[{"value":"literal","of":5}],"_observation":{},
            "observation":"e1 button \"Retry\""}});
        project(&mut result);
        assert!(result.get("value").is_none());
        assert!(result.get("_observation").is_none());
        assert_eq!(result["completed_steps"], 1);
        assert_eq!(result["total_steps"], 2);
        assert_eq!(result["page"]["page_id"], "page");
        assert_eq!(result["fields"][0]["value"], "literal");
        assert_eq!(result["failed_step"]["step_number"], 2);
        assert_eq!(result["files"][0]["path"], "/workspace/file");
    }
}
