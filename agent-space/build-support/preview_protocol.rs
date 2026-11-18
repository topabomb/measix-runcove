//! Shared immutable wire declarations; no runtime/credential state.
pub const ALLOW: &str = "OPTIONS, GET, HEAD";
// Private helper-to-Host diagnostic. The Host consumes it before responding.
pub const BUSY_REASON_HEADER: &str = "x-agent-space-preview-busy-reason";
pub const GENERATION_BUSY: &str = "generation_busy";
pub const GENERATION_CLEANUP: &str = "generation_cleanup";
pub const CAPABILITY: &str = r#"{"schemaVersion":1,"supported":true,"sizes":[128,256,512],"contentType":"image/webp","mediaKinds":["IMAGE","VIDEO"]}"#;
