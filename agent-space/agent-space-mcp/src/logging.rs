use tracing_subscriber::fmt::writer::MakeWriterExt;

pub fn init_logging(
    config: &agent_engine::config::LoggingConfig,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            let base = &config.level;
            format!(
                "{base},microsandbox_agent_client=warn,microsandbox=info,rmcp=warn,sea_orm_migration=info"
            ).into()
        });

    let mut guard = None;

    match (&config.dir, config.format.as_str()) {
        (Some(dir), "json") => {
            std::fs::create_dir_all(dir).ok();
            let file_appender = tracing_appender::rolling::daily(dir, "agent-space.log");
            let (non_blocking, g) = tracing_appender::non_blocking(file_appender);
            guard = Some(g);
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .json()
                .with_writer(std::io::stdout.and(non_blocking))
                .init();
        }
        (Some(dir), _) => {
            std::fs::create_dir_all(dir).ok();
            let file_appender = tracing_appender::rolling::daily(dir, "agent-space.log");
            let (non_blocking, g) = tracing_appender::non_blocking(file_appender);
            guard = Some(g);
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_writer(std::io::stdout.and(non_blocking))
                .init();
        }
        (None, "json") => {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .json()
                .init();
        }
        (None, _) => {
            tracing_subscriber::fmt().with_env_filter(env_filter).init();
        }
    }

    guard
}

pub async fn start_log_cleanup(dir: Option<&str>, max_days: u32) {
    let Some(dir) = dir else { return };
    let dir = dir.to_string();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // skip first immediate tick
        loop {
            ticker.tick().await;
            cleanup_old_logs(&dir, max_days);
        }
    });
}

fn cleanup_old_logs(dir: &str, max_days: u32) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(max_days as u64 * 86400);

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("agent-space.log.") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if modified < cutoff && std::fs::remove_file(&path).is_ok() {
                    tracing::info!(path = %path.display(), "removed old log file");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};
    #[test]
    fn test_cleanup_old_logs_removes_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();

        // Create a fake old log file
        let old_file = dir_path.join("agent-space.log.2020-01-01");
        fs::write(&old_file, "old log").unwrap();

        // Set modification time to 10 days ago
        let old_time = SystemTime::now() - Duration::from_secs(10 * 86400);
        filetime::set_file_mtime(&old_file, filetime::FileTime::from_system_time(old_time))
            .unwrap();

        // Create a recent log file
        let recent_file = dir_path.join("agent-space.log.2026-06-24");
        fs::write(&recent_file, "recent log").unwrap();

        // Cleanup with 7 days max
        cleanup_old_logs(dir_path.to_str().unwrap(), 7);

        // Old file should be removed
        assert!(!old_file.exists(), "old log file should be removed");
        // Recent file should remain
        assert!(recent_file.exists(), "recent log file should remain");
    }

    #[test]
    fn test_cleanup_old_logs_ignores_non_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();

        // Create files that don't match the pattern
        let other_file = dir_path.join("other.log");
        fs::write(&other_file, "other").unwrap();

        let old_time = SystemTime::now() - Duration::from_secs(10 * 86400);
        filetime::set_file_mtime(&other_file, filetime::FileTime::from_system_time(old_time))
            .unwrap();

        cleanup_old_logs(dir_path.to_str().unwrap(), 7);

        // Non-matching file should remain
        assert!(other_file.exists(), "non-matching file should remain");
    }

    #[test]
    fn test_cleanup_old_logs_ignores_current_log() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path();

        // Create current log file (no date suffix)
        let current_file = dir_path.join("agent-space.log");
        fs::write(&current_file, "current").unwrap();

        cleanup_old_logs(dir_path.to_str().unwrap(), 7);

        // Current log should remain
        assert!(current_file.exists(), "current log file should remain");
    }

    #[test]
    fn test_cleanup_old_logs_nonexistent_dir() {
        // Should not panic on nonexistent directory
        cleanup_old_logs("/nonexistent/path/xyz", 7);
    }
}
