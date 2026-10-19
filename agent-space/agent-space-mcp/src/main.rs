use agent_space_mcp::{mcp_auth, server, web_publish};

use std::sync::Arc;

use agent_engine::config::Config;
use agent_engine::engine::msb::MsbEngine;
use agent_engine::engine::{AgentEngine, VolumeSpec, WorkspaceOpts};
use agent_engine::session::SessionManager;
use agent_space_mcp::logging::{init_logging, start_log_cleanup};
use clap::Parser;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use server::AgentSpaceMcpServer;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value = "config.toml")]
    config: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Preview or write canonical Server configuration; never mutates runtime data.
    ConvertConfig {
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
    },
    /// Migrate schema2 image ownership offline; never starts any VM.
    MigrateState {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
    },
    /// Create an empty state snapshot. Refuses to replace an existing file.
    InitState,
    /// Import the static user roster without copying old tokens.
    ImportUsers {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(Command::ConvertConfig {
        output,
        dry_run,
        apply,
    }) = &args.command
    {
        anyhow::ensure!(*dry_run || *apply, "choose --dry-run or --apply");
        println!(
            "{}",
            Config::convert_file(
                std::path::Path::new(&args.config),
                output.as_deref(),
                *apply
            )?
        );
        return Ok(());
    }
    let config = Arc::new(if args.command.is_some() {
        Config::load_for_state_command(&args.config)?
    } else {
        Config::load(&args.config)?
    });
    if let Some(command) = &args.command {
        return run_state_command(&config, command).await;
    }

    let _guard = init_logging(&config.logging);
    start_log_cleanup(config.logging.dir.as_deref(), config.logging.max_days).await;

    let engine = Arc::new(MsbEngine::new()) as Arc<dyn AgentEngine>;

    let default_opts = WorkspaceOpts {
        image: config.workspace.image.resolve()?.reference().to_owned(),
        cpus: config.workspace.cpus,
        memory_mib: config.workspace.memory_mib,
        shell: config.workspace.shell.clone(),
        volumes: vec![
            VolumeSpec::disk("/workspace", "ws", config.workspace.workspace_size_mib),
            VolumeSpec::tmpfs("/tmp", config.workspace.tmpfs_size_mib),
        ],
        network_access: config.workspace.network_access().unwrap_or_default(),
        outbound_proxy: config.workspace.outbound_proxy.clone(),
        max_tcp_connections: config.workspace.max_tcp_connections,
        max_udp_connections: config.workspace.max_udp_connections,
    };

    let session_manager = Arc::new(
        SessionManager::new(engine, config.server.max_sessions, default_opts)
            .with_browser(config.browser.clone())
            .with_limits(
                config.workspace.max_output_lines,
                config.workspace.max_output_bytes,
            ),
    );

    let dynamic_state = if let Some(dir) = config.state_dir() {
        let state = Arc::new(agent_engine::state::StateStore::open(dir)?);
        for user in state.list() {
            session_manager
                .register_image(&user.sandbox_name, user.image.as_ref().unwrap().reference())?;
        }
        let service = Arc::new(
            agent_engine::workspace_service::WorkspaceService::new(
                state.clone(),
                session_manager.clone(),
                &config.resource_prefix,
                config.server.public_base_url.clone().unwrap_or_default(),
            )
            .with_dav_base_url(
                config
                    .webdav
                    .enabled
                    .then_some(config.dav_base_url().unwrap_or("")),
            )
            .with_dav_public_port(config.dav_public_port())
            .with_default_image(config.workspace.image.resolve()?),
        );
        service.retry_pending_stops().await?;
        service.retry_deletions().await?;
        Some((state, service))
    } else {
        None
    };
    let configured_workspaces = if let Some((state, _)) = &dynamic_state {
        state.registered_resources().0
    } else {
        config
            .users
            .values()
            .map(|u| u.workspace_name.clone())
            .collect()
    };
    session_manager
        .adopt_running(&configured_workspaces)
        .await?;

    // Verify sandbox image has all required dependencies before accepting requests
    if let Err(e) = verify_sandbox_image(
        &session_manager,
        config.workspace.image.resolve()?.reference(),
        &config.resource_prefix,
    )
    .await
    {
        tracing::error!(error = %e, "sandbox image verification failed, exiting");
        std::process::exit(1);
    }

    // Unknown engine resources are reported only. Deletion requires a
    // persisted deleting record and is not inferred from config absence.
    let (registered, registered_volumes) = if let Some((state, _)) = &dynamic_state {
        state.registered_resources()
    } else {
        let registered: std::collections::HashSet<String> = config
            .users
            .values()
            .map(|u| u.workspace_name.clone())
            .collect();
        let registered_volumes = registered.iter().map(|name| format!("{name}-ws")).collect();
        (registered, registered_volumes)
    };
    match session_manager
        .report_unregistered_resources(&registered, &registered_volumes)
        .await
    {
        Ok(unknown) if !unknown.is_empty() => {
            tracing::warn!(resources = ?unknown, "unregistered resources were not deleted");
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "unregistered resource report failed"),
    }

    if config.lifecycle.check_interval_secs > 0 {
        let lifecycle_config = config.lifecycle.clone();
        let ws_names = configured_workspaces.clone();
        session_manager.start_periodic_check(lifecycle_config, ws_names);
        tracing::info!(
            interval_secs = config.lifecycle.check_interval_secs,
            "lifecycle check started"
        );
    }

    if let Some((_, service)) = &dynamic_state {
        let retry_service = service.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if let Err(error) = retry_service.retry_pending_stops().await {
                    tracing::error!(error = %error, "pending stop retry failed");
                }
                if let Err(error) = retry_service.retry_deletions().await {
                    tracing::error!(error = %error, "deletion retry failed");
                }
            }
        });
    }

    // Periodic report only. A static roster must not destroy disks.
    let sm_periodic_report = session_manager.clone();
    let periodic_registered = registered.clone();
    let periodic_volumes = registered_volumes.clone();
    let report_state = dynamic_state.as_ref().map(|(state, _)| state.clone());
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let (current_sandboxes, current_volumes) = match &report_state {
                Some(state) => state.registered_resources(),
                None => (periodic_registered.clone(), periodic_volumes.clone()),
            };
            match sm_periodic_report
                .report_unregistered_resources(&current_sandboxes, &current_volumes)
                .await
            {
                Ok(unknown) if !unknown.is_empty() => {
                    tracing::warn!(resources = ?unknown, "unregistered resources were not deleted");
                }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "unregistered resource report failed"),
            }
        }
    });

    let bind_addr = config.server.bind_addr();

    let ct = tokio_util::sync::CancellationToken::new();

    let config_clone = config.clone();
    let sm_clone = session_manager.clone();
    let state_clone = dynamic_state.as_ref().map(|(state, _)| state.clone());
    let sm_shutdown = session_manager.clone();
    let mut http_config =
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token());
    if config.server.allowed_hosts.is_empty() {
        http_config = http_config.disable_allowed_hosts();
    } else {
        http_config = http_config.with_allowed_hosts(&config.server.allowed_hosts);
    }
    let mcp_auth = mcp_auth::McpAuthState::new(
        config.clone(),
        dynamic_state.as_ref().map(|(state, _)| state.clone()),
    );
    let service = StreamableHttpService::new(
        move || {
            Ok(AgentSpaceMcpServer::with_state(
                config_clone.clone(),
                sm_clone.clone(),
                state_clone.clone(),
            ))
        },
        mcp_auth.session_manager().into(),
        http_config,
    );

    let health_manager = session_manager.clone();
    let health_service = dynamic_state.as_ref().map(|(_, service)| service.clone());
    let mcp_router = axum::Router::new()
        .nest_service("/u/{username}/mcp", service)
        .layer(axum::middleware::from_fn_with_state(
            mcp_auth,
            mcp_auth::middleware,
        ));
    let health_router = axum::Router::new().route(
        "/healthz",
        axum::routing::get(move || {
            let manager = health_manager.clone();
            let service = health_service.clone();
            async move {
                let ready = if let Some(service) = service {
                    service
                        .readiness()
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                } else {
                    manager
                        .check_backend()
                        .await
                        .map_err(|error| error.to_string())
                };
                match ready {
                    Ok(()) => axum::http::StatusCode::OK,
                    Err(error) => {
                        tracing::error!(error = %error, "backend health check failed");
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    }
                }
            }
        }),
    );
    let router = health_router.clone().merge(mcp_router).merge(
        dynamic_state
            .as_ref()
            .map(|(_, service)| {
                agent_space_mcp::admin_api::router(agent_space_mcp::admin_api::AdminState {
                    service: service.clone(),
                    admin_token: config.admin_token().unwrap_or_default(),
                })
            })
            .unwrap_or_else(axum::Router::new),
    );

    let router = if let Some((state, _)) = &dynamic_state {
        router.merge(agent_space_mcp::desktop::router(
            agent_space_mcp::desktop::DesktopState::new(
                session_manager.clone(),
                state.clone(),
                std::env::var("AGENT_SPACE_DESKTOP_UI")
                    .unwrap_or_else(|_| "desktop-ui/dist".into()),
            ),
        ))
    } else {
        router
    };

    // Separate sockets own route isolation, independent of Host or public links.
    let dav_router = if let (true, Some((state, _))) = (config.webdav.enabled, &dynamic_state) {
        Some(
            agent_space_mcp::webdav::router(agent_space_mcp::webdav::DavState {
                sessions: session_manager.clone(),
                state: state.clone(),
                origin: url::Url::parse(config.dav_base_url().unwrap_or("http://127.0.0.1"))?,
                helper_binary: std::env::var("AGENT_SPACE_WEBDAV")
                    .unwrap_or_else(|_| "/usr/local/bin/agent-space-webdav".into()),
                startup_timeout: std::time::Duration::from_secs(config.webdav.startup_timeout_secs),
                io_idle_timeout: std::time::Duration::from_secs(config.webdav.io_idle_timeout_secs),
            })
            .merge(health_router)
            .layer(axum::middleware::from_fn_with_state(
                agent_space_mcp::origins::OriginPolicy::new(config.dav_base_url())?,
                agent_space_mcp::origins::origin_boundary,
            )),
        )
    } else {
        None
    };
    let router = router.layer(axum::middleware::from_fn_with_state(
        agent_space_mcp::origins::OriginPolicy::from_config(&config)?,
        agent_space_mcp::origins::origin_boundary,
    ));
    let router = router.layer(axum::middleware::from_fn_with_state(
        web_publish::PublishState {
            sessions: session_manager.clone(),
            state: dynamic_state.as_ref().map(|(state, _)| state.clone()),
            origin: url::Url::parse(&config.public_base_url())?,
            enabled: config.web_publish.enabled,
            startup_timeout: std::time::Duration::from_secs(
                config.web_publish.startup_timeout_secs,
            ),
            io_idle_timeout: std::time::Duration::from_secs(
                config.web_publish.io_idle_timeout_secs,
            ),
        },
        web_publish::dispatch,
    ));
    let tcp_listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("agent-space-mcp listening on {}", bind_addr);
    let dav_listener = if dav_router.is_some() {
        Some(tokio::net::TcpListener::bind(&config.webdav.listen).await?)
    } else {
        None
    };
    let dav_shutdown = ct.child_token();
    let dav_task = if let (Some(listener), Some(router)) = (dav_listener, dav_router) {
        tracing::info!("agent-space-webdav listening on {}", config.webdav.listen);
        Some(tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(dav_shutdown.cancelled_owned())
                .await
        }))
    } else {
        None
    };

    axum::serve(tcp_listener, router)
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
                let mut sigint =
                    signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
                tokio::select! {
                    _ = sigterm.recv() => { tracing::info!("received SIGTERM"); }
                    _ = sigint.recv() => { tracing::info!("received SIGINT"); }
                }
            }
            #[cfg(not(unix))]
            {
                tokio::signal::ctrl_c().await.ok();
            }
            tracing::info!("shutting down");
            ct.cancel();
            sm_shutdown.stop_all().await;
        })
        .await?;
    if let Some(task) = dav_task {
        task.await??;
    }

    Ok(())
}

/// Verify that the sandbox image contains all commands required by builtin tools.
/// Creates a temporary workspace, runs a dependency check script, then cleans up.
/// Exits the process if verification fails.
async fn verify_sandbox_image(
    session_manager: &SessionManager,
    image: &str,
    resource_prefix: &str,
) -> anyhow::Result<()> {
    use std::time::Duration;

    tracing::info!(image = %image, "verifying sandbox image dependencies...");

    // Startup validation must never wake, stop, or execute inside an adopted
    // user workspace. When all resident capacity is already occupied, defer
    // the check to the normal first-tool activation path.
    if session_manager.available_capacity() == 0 {
        tracing::warn!(
            image = %image,
            "skipping image dependency probe because all workspace capacity is occupied"
        );
        return Ok(());
    }

    let check_workspace = format!(
        "{}-imgcheck-{}",
        resource_prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );

    let session = session_manager.get_or_create(&check_workspace).await;

    let check_cmd = r#"
        missing=""
        for cmd in bash test nl sed printf wc mkdir dirname grep tr python3; do
            command -v "$cmd" >/dev/null 2>&1 || missing="$missing $cmd"
        done
        if echo "test" | grep -P "test" >/dev/null 2>&1; then
            :
        else
            missing="$missing grep-P"
        fi
        if [ -n "$missing" ]; then
            echo "MISSING:$missing"
            exit 1
        fi
        echo "OK"
    "#;

    // Creation can install the probe's volume before image activation fails.
    // Keep cleanup on that failure path as well as on command failure.
    let result = match session {
        Ok(session) => session.exec(check_cmd, Duration::from_secs(30)).await,
        Err(error) => Err(error),
    };

    // The probe owns this exact workspace. A failed cleanup must block
    // startup and expose its name for operator recovery.
    session_manager
        .remove(&check_workspace)
        .await
        .map_err(|error| {
            anyhow::anyhow!("verification workspace '{check_workspace}' cleanup failed: {error}")
        })?;
    let result = result.map_err(|e| anyhow::anyhow!("dependency check exec failed: {}", e))?;

    let output = result.stdout.trim();
    if result.exit_code != 0 || output.starts_with("MISSING:") {
        let missing = output.strip_prefix("MISSING:").unwrap_or(output);
        anyhow::bail!(
            "sandbox image '{}' missing required commands:{}\n\
             required: bash, test, nl, sed, printf, wc, mkdir, dirname, grep (with -P), tr, python3\n\
             fix: use nikolaik/python-nodejs:python3.12-nodejs22-slim or compatible image",
            image, missing
        );
    }

    tracing::info!(image = %image, "sandbox image dependencies verified");
    Ok(())
}

async fn run_state_command(config: &Config, command: &Command) -> anyhow::Result<()> {
    let dir = config
        .state
        .dir
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("state_dir is required"))?;
    match command {
        Command::ConvertConfig { .. } => {
            anyhow::bail!("configuration conversion must run before runtime initialization")
        }
        Command::MigrateState { dry_run, apply } => {
            anyhow::ensure!(
                *dry_run != *apply,
                "choose exactly one of --dry-run or --apply"
            );
            let fallback = match &config.workspace.image {
                agent_engine::image::ImageSelection::Custom(reference) => reference.clone(),
                _ => anyhow::bail!(
                    "migrate with the previous custom image before selecting the desktop preset"
                ),
            };
            let apply_migration = *apply;
            let directory = dir.to_owned();
            let runtime = tokio::runtime::Handle::current();
            // Resolve read-only SDK metadata while the exact same state lock
            // remains held through validation, backup and atomic replacement.
            let users = tokio::task::spawn_blocking(move || {
                agent_engine::state::StateStore::migrate_v2(&directory, apply_migration, |name| {
                    match runtime.block_on(MsbEngine::new().sandbox_image(name)) {
                        Ok(reference) => Ok(reference),
                        Err(agent_engine::engine::EngineError::NotFound(_)) => Ok(fallback.clone()),
                        Err(error) => Err(agent_engine::state::StateError::Corrupt(format!(
                            "image for {name}: {error}"
                        ))),
                    }
                })
            })
            .await??;
            println!(
                "{} {} users to schema3",
                if *apply { "migrated" } else { "preview" },
                users.len()
            );
        }
        Command::InitState => {
            agent_engine::state::StateStore::init(dir)?;
            println!("initialized {dir}");
        }
        Command::ImportUsers { dry_run, apply } => {
            if *dry_run == *apply {
                anyhow::bail!("choose exactly one of --dry-run or --apply");
            }
            let mut imports = config
                .users
                .iter()
                .map(|(username, user)| agent_engine::state::ImportUser {
                    image: Default::default(),
                    username: username.clone(),
                    sandbox_name: user.workspace_name.clone(),
                    owned_volume_names: vec![format!("{}-ws", user.workspace_name)],
                })
                .collect::<Vec<_>>();
            let fallback = config.workspace.image.resolve()?;
            anyhow::ensure!(
                !fallback.browser(),
                "import legacy users with their previous custom image"
            );
            for user in &mut imports {
                user.image = match MsbEngine::new().sandbox_image(&user.sandbox_name).await {
                    Ok(reference) => agent_engine::image::SpaceImage::Custom { reference },
                    Err(agent_engine::engine::EngineError::NotFound(_)) => fallback.clone(),
                    Err(error) => return Err(error.into()),
                };
            }
            let state = agent_engine::state::StateStore::open(dir)?;
            if *dry_run {
                state.preview_import_users(&imports)?;
                for user in &imports {
                    println!(
                        "import {} sandbox={} volume={}",
                        user.username,
                        user.sandbox_name,
                        user.owned_volume_names.join(",")
                    );
                }
                return Ok(());
            }
            let created = state.import_users(&imports)?;
            for user in created {
                println!("imported {} space={}", user.username, user.agent_space_id);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_engine::engine::mock::MockEngine;

    #[tokio::test]
    async fn image_probe_does_not_touch_an_adopted_workspace_at_capacity() {
        let engine = Arc::new(MockEngine::new());
        let manager = SessionManager::new(
            engine.clone(),
            1,
            WorkspaceOpts {
                image: "test".into(),
                cpus: 1,
                memory_mib: 256,
                shell: "/bin/bash".into(),
                volumes: vec![],
                network_access: Default::default(),
                outbound_proxy: None,
                max_tcp_connections: None,
                max_udp_connections: None,
            },
        );
        manager.get_or_create("smcp-user").await.unwrap();

        verify_sandbox_image(&manager, "test", "smcp")
            .await
            .unwrap();

        assert_eq!(
            engine
                .create_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(manager.session_count().await, 1);
    }

    #[tokio::test]
    async fn import_command_previews_then_commits_without_tokens() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("state");
        let config_file = root.path().join("config.toml");
        let content = include_str!("../../config.toml")
            .replace(
                "image = { preset = \"runcove-desktop\" }",
                "image = \"nikolaik/python-nodejs:python3.12-nodejs22-slim\"",
            )
            .replace(
                "dir = \"/var/lib/microsandbox/agent-space-state\"",
                &format!("dir = \"{}\"", dir.display()),
            )
            .replace("[webdav]\nenabled = true", "[webdav]\nenabled = false")
            + "\n[users.alice]\ntoken = \"tok-alice\"\nworkspace_name = \"smcp-alice\"\n";
        std::fs::write(&config_file, content).unwrap();
        let path = config_file.to_str().unwrap();
        assert!(Config::load(path).is_err());
        let config = Config::load_for_state_command(path).unwrap();
        run_state_command(&config, &Command::InitState)
            .await
            .unwrap();
        run_state_command(
            &config,
            &Command::ImportUsers {
                dry_run: true,
                apply: false,
            },
        )
        .await
        .unwrap();
        assert!(std::fs::read_to_string(dir.join("state.json"))
            .unwrap()
            .contains("\"users\": []"));
        run_state_command(
            &config,
            &Command::ImportUsers {
                dry_run: false,
                apply: true,
            },
        )
        .await
        .unwrap();
        let stored = std::fs::read_to_string(dir.join("state.json")).unwrap();
        assert!(stored.contains("smcp-alice"));
        assert!(!stored.contains("tok-alice"));
        assert!(run_state_command(
            &config,
            &Command::ImportUsers {
                dry_run: true,
                apply: false
            }
        )
        .await
        .is_err());
        assert!(run_state_command(
            &config,
            &Command::ImportUsers {
                dry_run: false,
                apply: true
            }
        )
        .await
        .is_err());
    }
}
