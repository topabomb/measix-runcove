//! Shared immutable wire declarations; no runtime/credential state.
pub const ALLOW: &str = "OPTIONS, GET, HEAD";
pub const CAPABILITY: &str = r#"{"schemaVersion":1,"supported":true,"sizes":[128,256,512],"contentType":"image/webp","mediaKinds":["IMAGE","VIDEO"]}"#;
