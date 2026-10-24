use agent_engine::{
    config::Config,
    engine::{mock::MockEngine, WorkspaceOpts},
    session::SessionManager,
    state::StateStore,
};
use agent_space_mcp::{server::AgentSpaceMcpServer, transport};
use rmcp::{
    model::CallToolRequestParams,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig,
        streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService},
        StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::Duration};

struct Harness {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    sessions: Arc<SessionManager>,
    sandbox: String,
    entry: String,
    server: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Harness {
    async fn new() -> Self {
        Self::with_browser(false).await
    }
    async fn with_browser(browser: bool) -> Self {
        Self::configured(browser, 10).await
    }
    async fn configured(browser: bool, keep_alive_minutes: u32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        StateStore::init(dir.path()).unwrap();
        let state = Arc::new(StateStore::open(dir.path()).unwrap());
        let (user, token) = if browser {
            state.create_user_with_image(
                "alice",
                "publish-tests",
                &[],
                agent_engine::image::SpaceImage::Desktop {
                    preset: "runcove-desktop".into(),
                    reference: format!("test@sha256:{}", "a".repeat(64)),
                    worker_contract: 1,
                },
            )
        } else {
            state.create_user("alice", "publish-tests", &[])
        }
        .unwrap();
        let mut config = toml::from_str::<Config>(include_str!("../../config.toml")).unwrap();
        config.web_publish.default_keep_alive_minutes = keep_alive_minutes;
        let config = Arc::new(config);
        let opts = WorkspaceOpts {
            image: "mock".into(),
            cpus: 1,
            memory_mib: 256,
            shell: "/bin/bash".into(),
            volumes: vec![],
            network_access: Default::default(),
            outbound_proxy: None,
            max_tcp_connections: None,
            max_udp_connections: None,
        };
        let sessions = Arc::new(SessionManager::new(Arc::new(MockEngine::new()), 1, opts));
        let mgr = sessions.clone();
        let cfg = config.clone();
        let mcp = StreamableHttpService::new(
            move || {
                Ok(AgentSpaceMcpServer::with_state(
                    cfg.clone(),
                    mgr.clone(),
                    Some(state.clone()),
                ))
            },
            transport::session_manager(&config.server).into(),
            StreamableHttpServerConfig::default(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().nest_service("/u/{username}/mcp", mcp),
            )
            .await
            .unwrap();
        });
        let headers = HashMap::from([(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {}", token.token)).unwrap(),
        )]);
        let client = ()
            .serve(StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/u/alice/mcp"))
                    .custom_headers(headers),
            ))
            .await
            .unwrap();
        Self {
            client,
            sessions,
            sandbox: user.sandbox_name,
            entry: format!("http://{addr}"),
            server,
            _dir: dir,
        }
    }
    async fn call(&self, args: Value) -> Value {
        let response = self
            .client
            .call_tool(
                CallToolRequestParams::new("web_publish")
                    .with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(response.is_error, Some(true), "{response:?}");
        let json = serde_json::to_value(response).unwrap();
        serde_json::from_str(json["content"][0]["text"].as_str().unwrap()).unwrap()
    }
    async fn rejects(&self, args: Value) {
        let response = self
            .client
            .call_tool(
                CallToolRequestParams::new("web_publish")
                    .with_arguments(args.as_object().unwrap().clone()),
            )
            .await;
        assert!(
            response.is_err() || response.unwrap().is_error == Some(true),
            "accepted invalid args: {args}"
        );
    }
}

#[tokio::test]
async fn actual_mcp_contract_three_targets_stable_credentials_and_explicit_renewal() {
    let h = Harness::new().await;
    let tools = h.client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names.len(), 5);
    assert!(names.contains(&"web_publish"));
    assert!(!names.contains(&"http_proxy"));
    let schema = &tools
        .iter()
        .find(|t| t.name == "web_publish")
        .unwrap()
        .input_schema;
    assert_eq!(
        schema["properties"]["action"]["enum"],
        json!(["publish", "get", "list", "unpublish"])
    );
    assert!(schema.get("oneOf").is_none());
    assert_eq!(schema["additionalProperties"], true);
    // The default comes from server configuration, not a fixed client-side value.
    assert_eq!(schema["properties"]["keep_alive_secs"]["default"], 600);
    std::fs::create_dir_all("target/web-publish-verification").unwrap();
    std::fs::write(
        "target/web-publish-verification/input-schema.json",
        serde_json::to_vec_pretty(schema).unwrap(),
    )
    .unwrap();
    assert_eq!(h.call(json!({"action":"list"})).await, json!([]));
    assert_eq!(h.sessions.active_space_count(), 0);
    h.rejects(json!({"action":"publish","name":"api","port":3000}))
        .await;
    let session = h.sessions.get_session(&h.sandbox).await.unwrap();
    session.exec("true", Duration::from_secs(1)).await.unwrap();
    let mut results = Vec::new();
    for args in [
        json!({"action":"publish","name":"report","file":"/workspace/report.pdf"}),
        json!({"action":"publish","name":"site","directory":"/workspace/site"}),
        json!({"action":"publish","name":"api","port":3000}),
    ] {
        let item = h.call(args).await;
        let username = item["username"].as_str().unwrap();
        assert!(
            username.starts_with("web")
                && username.len() == 5
                && username[3..].bytes().all(|b| b.is_ascii_digit())
        );
        assert!(results
            .iter()
            .all(|v: &serde_json::Value| v["username"] != item["username"]));
        let password = item["password"].as_str().unwrap();
        assert_eq!(password.len(), 6);
        assert!(password.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(item.as_object().unwrap().len(), 5);
        results.push(item);
    }
    assert!(results[0]["url"].as_str().unwrap().starts_with(&h.entry));
    assert_ne!(results[0]["url"], results[1]["url"]);
    assert_ne!(results[1]["url"], results[2]["url"]);
    let before = session.publication("api").unwrap();
    let remaining = before
        .keep_alive_until
        .duration_since(std::time::SystemTime::now())
        .unwrap();
    assert!(remaining <= Duration::from_secs(600) && remaining > Duration::from_secs(590));
    let get = h.call(json!({"action":"get","name":"api"})).await;
    assert_eq!(get, results[2]);
    let list = h.call(json!({"action":"list"})).await;
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v.as_object().unwrap().len() == 3
            && v.get("password").is_none()
            && v.get("username").is_none()));
    assert_eq!(
        session.publication("api").unwrap().keep_alive_until,
        before.keep_alive_until
    );
    let renewed = h
        .call(json!({"action":"publish","name":"api","port":3000,"keep_alive_secs":7200}))
        .await;
    assert_eq!(renewed["url"], get["url"]);
    assert_eq!(renewed["password"], get["password"]);
    let after = session.publication("api").unwrap();
    assert!(
        after
            .keep_alive_until
            .duration_since(before.keep_alive_until)
            .unwrap()
            >= Duration::from_secs(6599)
    );
    let shorter = h
        .call(json!({"action":"publish","name":"api","port":3000,"keep_alive_secs":60}))
        .await;
    assert_eq!(shorter, renewed);
    assert_eq!(
        session.publication("api").unwrap().keep_alive_until,
        after.keep_alive_until
    );
    let replaced = h
        .call(json!({"action":"publish","name":"api","directory":"/other"}))
        .await;
    assert_eq!(replaced["url"], get["url"]);
    assert_eq!(replaced["username"], get["username"]);
    assert_eq!(replaced["password"], get["password"]);
    assert_eq!(replaced["type"], "directory");
    let current = session.publication("api").unwrap();
    assert_eq!(current.id, before.id);
    assert!(before.epoch.is_cancelled());
    assert!(!current.epoch.is_cancelled());
    assert!(session
        .replace_publication(before.clone(), &before)
        .is_err());
    assert_eq!(session.publication("api").unwrap().target, current.target);
    assert!(!current.epoch.is_cancelled());
    assert_eq!(session.publications().len(), 3);
    let helper_port = session.publication("site").unwrap().port;
    h.rejects(json!({"action":"publish","name":"helper","port":helper_port}))
        .await;
    let report_id = session.publication("report").map(|item| item.id);
    h.call(json!({"action":"unpublish","name":"report"})).await;
    h.call(json!({"action":"unpublish","name":"report"})).await;
    let recreated = h
        .call(json!({"action":"publish","name":"report","file":"/workspace/report.pdf"}))
        .await;
    assert_eq!(recreated["url"], results[0]["url"]);
    assert_ne!(Some(session.publication("report").unwrap().id), report_id);
    assert_eq!(session.publications().len(), 3);
    h.sessions.stop_workspace(&h.sandbox).await.unwrap();
    assert_eq!(h.call(json!({"action":"list"})).await, json!([]));
}

#[tokio::test]
async fn mcp_rejects_ambiguous_old_and_malformed_arguments_without_starting_vm() {
    let h = Harness::new().await;
    for args in [
        json!({"action":"set","target":"{\"type\":\"port\",\"port\":8000}"}),
        json!({"action":"publish","name":"a"}),
        json!({"action":"publish","port":8000}),
        json!({"action":"publish","name":"a","file":"/a","port":8000}),
        json!({"action":"publish","name":"a","file":"relative"}),
        json!({"action":"publish","name":"a","file":null}),
        json!({"action":"publish","name":"","port":8000}),
        json!({"action":"publish","name":"../a","port":8000}),
        json!({"action":"publish","name":"a","port":80}),
        json!({"action":"publish","name":"a","port":2222}),
        json!({"action":"publish","name":"a","port":8080}),
        json!({"action":"publish","name":"a","port":65536}),
        json!({"action":"publish","name":"a","port":"8000"}),
        json!({"action":"publish","name":"a","port":8000,"keep_alive_secs":0}),
        json!({"action":"publish","name":"a","port":8000,"keep_alive_secs":86401}),
        json!({"action":"get"}),
    ] {
        h.rejects(args).await;
    }
    assert_eq!(h.sessions.active_space_count(), 0);
    assert_eq!(h.call(json!({"action":"list"})).await, json!([]));
}

#[tokio::test]
async fn optional_placeholders_and_irrelevant_fields_do_not_block_actions() {
    let h = Harness::new().await;
    assert_eq!(
        h.call(
            json!({"action":"list","name":null,"port":"unused","keep_alive_secs":10,"extra":{}})
        )
        .await,
        json!([])
    );
    let session = h.sessions.get_session(&h.sandbox).await.unwrap();
    session.exec("true", Duration::from_secs(1)).await.unwrap();
    let result=h.call(json!({"action":"publish","name":"api","port":3000,"file":null,"directory":"","keep_alive_secs":null,"extra":true})).await;
    assert!(result["url"].is_string());
    let got = h
        .call(json!({"action":"get","name":"api","file":false,"keep_alive_secs":"unused"}))
        .await;
    assert_eq!(result, got);
    assert_eq!(h.call(json!({"action":"unpublish","name":"api","file":null,"directory":null,"port":null,"keep_alive_secs":10})).await["status"],"unpublished");
}

#[tokio::test]
async fn desktop_catalog_and_structured_arguments_reach_the_mcp_handlers() {
    let h = Harness::with_browser(true).await;
    let tools = h.client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 9);
    for tool in &tools {
        let description = tool.description.as_deref().unwrap();
        assert!(!description.trim().is_empty());
        assert!(!description.contains("README.md"));
        let disclosure = serde_json::to_string(tool).unwrap();
        for detail in [
            "1980",
            "2560",
            "CatmullRom",
            "Lanczos",
            "Q90",
            "alpha_quality",
            "resizing",
            "encoding",
        ] {
            assert!(
                !disclosure.contains(detail),
                "{} exposes {detail}",
                tool.name
            );
        }
        if ["browser_snapshot", "browser_act", "browser_read"].contains(&tool.name.as_ref()) {
            assert_eq!(
                tool.input_schema.get("additionalProperties"),
                Some(&json!(false))
            );
        }
    }
    let schema = |name: &str| &tools.iter().find(|t| t.name == name).unwrap().input_schema;
    assert_eq!(schema("bash")["required"], json!(["cmd"]));
    assert_eq!(schema("bash")["properties"]["cmd"]["type"], "string");
    assert!(schema("bash")["properties"].get("command").is_none());
    assert!(h
        .client
        .call_tool(
            CallToolRequestParams::new("bash").with_arguments(
                json!({"command":"printf must-not-run"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .is_err());
    for old in ["browser_observe", "browser_run", "browser_task"] {
        assert!(!tools.iter().any(|tool| tool.name == old));
    }
    assert_eq!(
        schema("browser_act")["$defs"]["ActAction"]["enum"],
        json!(["execute", "wait", "cancel"])
    );
    assert_eq!(
        schema("browser_act")["properties"]["timeout_secs"]["minimum"],
        1
    );
    assert_eq!(
        schema("browser_act")["properties"]["timeout_secs"]["maximum"],
        600
    );
    assert_eq!(
        schema("browser_snapshot")["properties"]["recent_tasks_limit"]["maximum"],
        10
    );
    assert_eq!(
        schema("browser_read")["properties"]["max_chars"]["maximum"],
        20000
    );
    // Advertised constraints must match the existing pre-execution validator.
    let act = schema("browser_act");
    assert_eq!(act["properties"]["actions"]["minItems"], 1);
    assert_eq!(act["properties"]["actions"]["maxItems"], 20);
    assert_eq!(act["properties"]["action_timeout_secs"]["maximum"], 30);
    assert!(!serde_json::to_string(&act).unwrap().contains("timeout_ms"));
    let steps = act["$defs"]["ActStep"]["oneOf"].as_array().unwrap();
    let step = |name: &str| {
        &steps
            .iter()
            .find(|s| s["properties"]["do"]["const"] == name)
            .unwrap()["properties"]
    };
    for (name, param, min, max) in [
        ("download", "timeout_secs", 1, 60),
        ("scroll", "dy", -20000, 20000),
        ("scroll", "max", 1, 30),
        ("wait", "secs", 0, 60),
        ("wait", "timeout_secs", 1, 60),
        ("scroll", "delta", -30, 30),
    ] {
        let p = &step(name)[param];
        assert_eq!(p["minimum"], min, "{name}.{param}");
        assert_eq!(p["maximum"], max, "{name}.{param}");
    }
    assert_eq!(step("upload")["paths"]["minItems"], 1);
    assert_eq!(step("upload")["paths"]["maxItems"], 20);
    assert_eq!(steps.len(), 16);
    assert!(act["properties"].get("action").is_none());
    assert_eq!(act["properties"]["op"]["default"], "execute");
    let catalog = serde_json::to_string(&tools).unwrap();
    for removed in [
        "click_at",
        "drag_at",
        "desktop_press",
        "desktop_scroll",
        "Params\"",
        "uint64",
        "uint32",
        "int32",
        "\"$schema\"",
    ] {
        assert!(
            !catalog.contains(removed),
            "catalog still exposes {removed}"
        );
    }
    std::fs::create_dir_all("target/browser-contract").unwrap();
    std::fs::write(
        "target/browser-contract/tools.json",
        serde_json::to_vec_pretty(&tools).unwrap(),
    )
    .unwrap();
    for s in steps {
        assert!(!s["description"].as_str().unwrap().is_empty());
        for (name, p) in s["properties"].as_object().unwrap() {
            if name != "do" {
                assert!(
                    !p["description"].as_str().unwrap().is_empty(),
                    "{}.{name}",
                    s["properties"]["do"]["const"]
                );
            }
            if name == "ref" || name == "to_ref" {
                assert!(p.get("pattern").is_none());
                assert!(p["description"].as_str().unwrap().contains("ref"));
            }
            if ["x", "y", "to_x", "to_y"].contains(&name.as_str()) {
                assert_eq!(p["minimum"], 0);
            }
        }
    }
    for name in ["browser_snapshot", "browser_read"] {
        assert!(schema(name)["properties"]["ref"].get("pattern").is_none());
    }
    assert_eq!(schema("read")["properties"]["multimodal"]["default"], false);
    assert_eq!(schema("screen_observe")["required"], json!(["view"]));
    for name in [
        "page_id",
        "count",
        "interval_secs",
        "save_original",
        "save_dir",
    ] {
        assert!(!schema("screen_observe")["properties"][name]["description"]
            .as_str()
            .unwrap()
            .is_empty());
    }
    assert_eq!(
        schema("screen_observe")["properties"]["save_original"]["default"],
        false
    );
    assert_eq!(
        schema("browser_act")["properties"]["wait_secs"]["maximum"],
        30
    );
    // The runtime default is conditional on wait/cancel, not execute.
    assert_eq!(
        schema("browser_act")["properties"]["wait_secs"]["default"],
        30
    );
    for action in ["wait", "cancel"] {
        for wait in [None, Some(30), Some(31)] {
            let mut args = json!({"op": action, "task_id": "missing"});
            if let Some(wait) = wait {
                args["wait_secs"] = json!(wait);
            }
            let result = h
                .client
                .call_tool(
                    CallToolRequestParams::new("browser_act")
                        .with_arguments(args.as_object().unwrap().clone()),
                )
                .await;
            if wait == Some(31) {
                assert!(result.is_err());
            } else {
                let result = result.unwrap();
                assert_eq!(result.is_error, Some(true));
                assert!(serde_json::to_string(&result)
                    .unwrap()
                    .contains("TASK_NOT_FOUND"));
            }
        }
    }
    // Valid optional selectors and arbitrary extra fields pass the real HTTP/SDK
    // decoding layer. Browser failures below are deliberate runtime validation,
    // rather than deserialization errors or real browser execution.
    for (name, args, runtime_error) in [
        (
            "bash",
            json!({"cmd":"printf ok", "workdir":"", "timeout_secs":null,"extra":{"x":[null,1]}}),
            None,
        ),
        (
            "read",
            json!({"path":"/workspace/a", "offset":null,"limit":null,"extra":false}),
            None,
        ),
        (
            "write",
            json!({"path":"/workspace/a", "content":"nonempty", "extra":[]}),
            None,
        ),
        (
            "edit",
            json!({"path":"/workspace/a", "old_text":" x ","new_text":"", "extra":null}),
            None,
        ),
        (
            "web_publish",
            json!({"action":"list", "name":null,"port":{"unused":true},"extra":1}),
            None,
        ),
        (
            "browser_act",
            json!({"task_id":"missing","op":"wait","wait_secs":0,"actions":[],"page_id":"old","then":"snapshot","action_timeout_secs":0,"timeout_secs":0}),
            Some("TASK_NOT_FOUND"),
        ),
    ] {
        let reply = h
            .client
            .call_tool(
                CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        if let Some(message) = runtime_error {
            assert_eq!(reply.is_error, Some(true));
            assert!(serde_json::to_string(&reply).unwrap().contains(message));
        }
    }
    for (name, args) in [
        (
            "bash",
            json!({"cmd":"printf must-not-run", "timeout_secs":u64::MAX}),
        ),
        (
            "bash",
            json!({"cmd":"printf must-not-run", "timeout_secs":0}),
        ),
        (
            "write",
            json!({"path":"/workspace/a", "content":null,"extra":true}),
        ),
        ("browser_snapshot", json!({"view":"unknown"})),
        ("browser_snapshot", json!({"recent_tasks_limit":11})),
        ("browser_snapshot", json!({"ref":"eabcdefghij","diff":true})),
        (
            "browser_act",
            json!({"code":"","page_id":null,"timeout_secs":null,"extra":{}}),
        ),
        ("screen_observe", json!({"view":"desktop","count":9})),
        (
            "screen_observe",
            json!({"view":"page","count":8,"interval_secs":100000}),
        ),
        ("screen_observe", json!({})),
        ("write", json!({"path":"/workspace/a","content":""})),
        ("browser_act", json!({"action":"wait","task_id":"missing"})),
        (
            "browser_act",
            json!({"actions":[{"do":"click_at","image_id":"x","x":1,"y":1}]}),
        ),
        ("browser_act", json!({"task_id":"missing","op":"unknown"})),
        ("web_publish", json!({"action":"unknown","extra":true})),
    ] {
        let result = h
            .client
            .call_tool(
                CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone()),
            )
            .await;
        assert!(
            result.is_err(),
            "invalid supported argument accepted: {name}"
        );
    }
    // The oversized timeout leaves the existing owner usable.
    let reply = h
        .client
        .call_tool(
            CallToolRequestParams::new("bash")
                .with_arguments(json!({"cmd":"printf alive"}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(reply.is_error, Some(true));
    let wire = serde_json::to_value(reply).unwrap();
    assert_eq!(wire["content"].as_array().unwrap().len(), 2);
    assert_eq!(wire["content"][1]["text"], "Exit code: 0");
    h.sessions.remove(&h.sandbox).await.unwrap();
}

#[tokio::test]
async fn omitted_and_null_keepalive_use_the_configured_server_default() {
    let h = Harness::configured(false, 17).await;
    let session = h.sessions.get_session(&h.sandbox).await.unwrap();
    session.exec("true", Duration::from_secs(1)).await.unwrap();
    for args in [
        json!({"action":"publish","name":"omitted","port":3000}),
        json!({"action":"publish","name":"null","port":3000,"keep_alive_secs":null}),
    ] {
        let name = args["name"].as_str().unwrap().to_owned();
        h.call(args).await;
        let remaining = session
            .publication(&name)
            .unwrap()
            .keep_alive_until
            .duration_since(std::time::SystemTime::now())
            .unwrap();
        assert!(remaining > Duration::from_secs(1010) && remaining <= Duration::from_secs(1020));
    }
    h.sessions.remove(&h.sandbox).await.unwrap();
}
