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
        keep_alive_minutes: Option<u32>,
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
                "action":{"type":"string","enum":["publish","get","list","unpublish"],"description":"\"publish\" creates or updates a publication; provide name and exactly one of file, directory, or port. \"get\" returns one publication and its credentials; provide name. \"list\" returns publications without credentials. \"unpublish\" removes the named publication without deleting files or stopping the application; provide name. get/list do not renew keepalive."},
                "name":{"type":["string","null"],"description":"Publication name, required for publish, get, and unpublish: 1-64 ASCII letters, digits, underscores, or hyphens."},
                "file":{"type":["string","null"],"description":"For publish: absolute path of an existing regular file, e.g. /workspace/report.html. The returned URL serves that file only, without exposing its parent directory."},
                "directory":{"type":["string","null"],"description":"For publish: absolute path of an existing directory, e.g. /workspace/site. Publishes its contents, serving index.html if present or a directory listing otherwise."},
                "port":{"type":["integer","null"],"description":"For publish: port of an HTTP application you have already started. Range 1024-65535; 2222, 5353, 8080, and ports used by Runcove services are reserved. Assets, API URLs, and redirects must work under the returned URL's path prefix; hard-coded root paths are not rewritten."},
                "keep_alive_minutes":{"type":["integer","null"],"description":"For publish: minutes of protection from automatic idle shutdown, not a link lifetime. Omit or pass JSON null for the configured default; accepts 1-1440 within configured limits. Re-publishing can extend protection without accumulating durations. Expiry removes protection, not the publication."}
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
