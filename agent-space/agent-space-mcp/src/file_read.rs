//! File-tool media adaptation; queue and process ownership remain in Session.
use crate::server::{compact_error, file_command, file_error, internal_error, media_error};
use agent_engine::session::Session;
use rmcp::{
    model::{CallToolResult, Content},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn media_report(result: &agent_engine::engine::ExecResult) -> Result<bool, CallToolResult> {
    if result.exit_code != 0 {
        return Ok(false);
    }
    match (result.stderr.trim(), result.stdout.trim()) {
        ("RUNCOVE_READ_MEDIA", text) if !result.truncated && !text.is_empty() => Ok(true),
        ("RUNCOVE_READ_MEDIA", _) => Err(media_error(
            "INVALID_MEDIA_REPORT",
            "Image report was truncated or incomplete",
        )),
        ("RUNCOVE_READ_TEXT", text) if !text.is_empty() => Ok(false),
        (_, "EMPTY" | "NO_CONTENT") => Ok(false),
        _ => Err(media_error(
            "INVALID_READ_REPORT",
            "File report is missing its type or contents",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_or_truncated_media_reports_never_become_text() {
        let mut result = agent_engine::engine::ExecResult {
            stdout: "{\"data\":\"AA==\"}".into(),
            stderr: "RUNCOVE_READ_MEDIA\n".into(),
            exit_code: 0,
            truncated: false,
            output_path: None,
        };
        assert!(media_report(&result).unwrap());
        result.truncated = true;
        assert!(media_report(&result).is_err());
        result.truncated = false;
        result.stderr.clear();
        assert!(media_report(&result).is_err());
        result.stdout.clear();
        assert!(media_report(&result).is_err());
        result.stderr = "RUNCOVE_READ_TEXT\n".into();
        result.stdout = "     1\thello\n".into();
        assert!(!media_report(&result).unwrap());
    }
}
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub(crate) struct ReadParams {
    #[schemars(
        description = "File path in the connected Runcove space. Prefer an absolute path; /workspace stores persistent files."
    )]
    pub(crate) path: String,
    #[schemars(description = "First text line, starting at 1; default 1.")]
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub(crate) offset: Option<u64>,
    #[schemars(
        description = "Maximum text lines; default 500. Output size limits may shorten the result."
    )]
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub(crate) limit: Option<u64>,
    #[serde(default)]
    #[schemars(
        description = "View an image; default false, including null. Text line options are ignored for images."
    )]
    pub(crate) multimodal: Option<bool>,
}

pub(crate) async fn read_file(
    session: Session,
    params: ReadParams,
    timeout: Duration,
    cancel: CancellationToken,
) -> Result<CallToolResult, McpError> {
    let multimodal = params.multimodal.unwrap_or(false);
    let offset = params.offset.unwrap_or(1);
    let limit = params.limit.unwrap_or(500);
    if !multimodal && (offset == 0 || limit == 0) {
        return Err(McpError::invalid_params(
            "offset and limit must be positive integers",
            None,
        ));
    }
    // The owner retains activity and codec capacity until actual completion,
    // even if the MCP waiter disappears or reaches its deadline.
    let lease = session.activity_lease().map_err(internal_error)?;
    let deadline = tokio::time::Instant::now() + timeout;
    let waiter_cancel = cancel.clone();
    let owner = tokio::spawn(async move {
        let permit = if multimodal {
            Some(tokio::select! {
                permit = agent_engine::media::acquire() => permit.map_err(internal_error)?,
                _ = cancel.cancelled() => return Ok(media_error("READ_CANCELLED", "File read cancelled before reading")),
                _ = tokio::time::sleep_until(deadline) => return Ok(media_error("READ_TIMEOUT", "Waiting for image capacity exceeded the deadline")),
            })
        } else {
            None
        };
        let cmd = file_command(
            serde_json::json!({"operation":"read", "path":params.path, "offset":offset, "limit":limit,
                "multimodal":multimodal}),
        );
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let result = session
            .exec_file_report(
                &cmd,
                remaining,
                // Type markers and image errors must survive even an 8-byte
                // text limit. Successful text is bounded again below.
                Some(if multimodal {
                    agent_engine::media::REPORT_LIMIT
                } else {
                    4096
                }),
                cancel.clone(),
            )
            .await;
        if let Ok(result) = &result {
            let media = match media_report(result) {
                Ok(media) => media,
                Err(error) => return Ok(error),
            };
            if media {
                let report: serde_json::Value = match serde_json::from_str(&result.stdout) {
                    Ok(value) => value,
                    Err(_) => {
                        return Ok(media_error(
                            "INVALID_MEDIA_REPORT",
                            "Image report could not be parsed",
                        ))
                    }
                };
                if let Some(error) = report.get("error") {
                    if error["code"] == "INVALID_ARGUMENT" {
                        return Err(McpError::invalid_params(
                            error["message"].as_str().unwrap_or_default().to_owned(),
                            None,
                        ));
                    }
                    return Ok(CallToolResult::error(vec![Content::text(
                        json!({"error":error}).to_string(),
                    )]));
                }
                use base64::Engine;
                let bytes = match report["data"]
                    .as_str()
                    .and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
                {
                    Some(bytes) => bytes,
                    None => {
                        return Ok(media_error(
                            "INVALID_MEDIA_REPORT",
                            "Image data is missing or invalid",
                        ))
                    }
                };
                let processed = tokio::task::spawn_blocking(move || {
                    let (_permit, _lease) = (permit, lease);
                    if tokio::time::Instant::now() >= deadline || cancel.is_cancelled() {
                        anyhow::bail!("IMAGE_TIMEOUT: image deadline exceeded");
                    }
                    let picture = agent_engine::media::process(bytes, None)?;
                    anyhow::ensure!(
                        tokio::time::Instant::now() < deadline && !cancel.is_cancelled(),
                        "IMAGE_TIMEOUT: image deadline exceeded"
                    );
                    Ok(picture)
                })
                .await
                .unwrap_or_else(|error| {
                    Err(anyhow::anyhow!(
                        "IMAGE_PROCESSING_FAILED: image worker failed: {error}"
                    ))
                });
                return Ok(match processed {
                    Ok(picture) => {
                        let mut metadata = picture.summary();
                        metadata["path"] = report["path"].clone();
                        CallToolResult::success(vec![
                            Content::text(metadata.to_string()),
                            Content::image(
                                base64::engine::general_purpose::STANDARD.encode(&picture.bytes),
                                picture.mime_type,
                            ),
                        ])
                    }
                    Err(error) => media_error(
                        "IMAGE_PROCESSING_FAILED",
                        &compact_error(&error.to_string()),
                    ),
                });
            }
        }
        match result {
            Ok(result) => match result.stdout.trim() {
                "FILE_NOT_FOUND" => Ok(CallToolResult::error(vec![Content::text(compact_error(&format!(
                    "File not found: {}",
                    params.path
                )))])),
                "NOT_A_FILE" => Ok(CallToolResult::error(vec![Content::text(
                    "The path refers to a directory or another non-regular file. Provide a regular file path.",
                )])),
                "EMPTY" => Ok(CallToolResult::success(vec![Content::text("(empty)")])),
                "NO_CONTENT" => Ok(CallToolResult::success(vec![Content::text(
                    "(no content in range)",
                )])),
                _ if result.exit_code != 0 => Ok(CallToolResult::error(vec![Content::text(
                    compact_error(&format!("read failed: {}", result.stderr.trim_start_matches("RUNCOVE_READ_TEXT\n").trim_start_matches("RUNCOVE_READ_MEDIA\n"))),
                )])),
                _ => {
                    let result = session.truncate_text(result);
                    let output = if result.truncated {
                        format!("{}\n[output truncated] Request a smaller line range using offset and limit.", result.stdout)
                    } else { result.stdout };
                    Ok(CallToolResult::success(vec![Content::text(output)]))
                },
            },
            Err(error) => Ok(CallToolResult::error(vec![Content::text(file_error("read", &error))])),
        }
    });
    tokio::select! {
        result = owner => result.map_err(internal_error)?,
        _ = waiter_cancel.cancelled() => Ok(media_error("READ_CANCELLED", "File read cancelled")),
        _ = tokio::time::sleep_until(deadline) => Ok(media_error("READ_TIMEOUT", "File read deadline exceeded")),
    }
}
