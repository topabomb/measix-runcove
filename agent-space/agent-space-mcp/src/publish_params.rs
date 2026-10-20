use agent_engine::publication::PublishTarget;
use rmcp::schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WebPublishParams {
    Publish {
        name: String,
        #[serde(default, deserialize_with = "optional_string")]
        file: Option<String>,
        #[serde(default, deserialize_with = "optional_string")]
        directory: Option<String>,
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        keep_alive_secs: Option<u32>,
    },
    Get {
        name: String,
    },
    List {},
    Unpublish {
        name: String,
    },
}
/// Empty optional selectors are placeholders, not targets. Never use for payload text.
pub fn optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.filter(|value| !value.trim().is_empty()))
}
/// Optional enum selectors accept blank placeholders and surrounding whitespace.
/// Nonempty choices still go through the enum's normal validation.
pub fn optional_choice<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    optional_string(deserializer)?
        .map(|value| {
            T::deserialize(serde::de::value::StringDeserializer::<D::Error>::new(
                value.trim().to_owned(),
            ))
        })
        .transpose()
}
impl JsonSchema for WebPublishParams {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "WebPublishParams".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        // A flat catalogue works with clients that populate all optional fields.
        // The action-specific enum validates only the fields that action consumes.
        let value: Value = json!({
            "type":"object",
            "properties":{
                "action":{"type":"string","enum":["publish","get","list","unpublish"],"description":"publish creates or updates: provide name and exactly one of file/directory/port. get returns one publication with credentials; list omits credentials. unpublish removes access while retaining files and the app. get/list do not renew keepalive."},
                "name":{"type":["string","null"],"description":"Required for publish/get/unpublish: 1-64 ASCII letters, digits, underscores or hyphens."},
                "file":{"type":["string","null"],"description":"Publish only: absolute path to an existing file. Serves that file alone."},
                "directory":{"type":["string","null"],"description":"Publish only: absolute path to an existing directory. Serves index.html when present, otherwise a directory listing."},
                "port":{"type":["integer","null"],"description":"Publish only: port of an already running HTTP app, 1024-65535; 8080 is reserved. The app must support the returned URL path prefix for assets, APIs and redirects."},
                "keep_alive_secs":{"type":["integer","null"],"description":"Publish only: idle-shutdown protection in seconds. Omit or null uses the space default. Re-publish to extend protection. Expiry ends idle protection; publication lifetime follows the space."}
            },
            "required":["action"],"additionalProperties":true
        });
        value.try_into().expect("object schema")
    }
}
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("name must be 1-64 ASCII letters, digits, underscores or hyphens");
    }
    Ok(())
}
pub fn target(
    file: Option<String>,
    directory: Option<String>,
    port: Option<u16>,
) -> Result<PublishTarget, &'static str> {
    let result = match (file, directory, port) {
        (Some(path), None, None) => PublishTarget::File(path),
        (None, Some(path), None) => PublishTarget::Directory(path),
        (None, None, Some(port)) => {
            if port < 1024 {
                return Err("port must be between 1024 and 65535");
            }
            if matches!(port, 2222 | 5353 | 8080) {
                return Err("port is reserved (2222, 5353, 8080). Choose the port of your running HTTP service.");
            }
            PublishTarget::Port(port)
        }
        _ => return Err("provide exactly one publication target: file, directory, or port"),
    };
    if let PublishTarget::File(path) | PublishTarget::Directory(path) = &result {
        if !path.starts_with('/') || path.contains('\0') {
            return Err("path must be an absolute guest path without NUL");
        }
    }
    Ok(result)
}
