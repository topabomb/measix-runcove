use clap::Parser;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value = "/etc/runcove/config.toml")]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = agent_space_mcp::standalone::StandaloneConfig::load(&args.config)?;
    let _guard = agent_space_mcp::logging::init_logging(&config.logging);
    agent_space_mcp::logging::start_log_cleanup(
        config.logging.dir.as_deref(),
        config.logging.max_days,
    )
    .await;
    agent_space_mcp::standalone::serve(config).await
}
