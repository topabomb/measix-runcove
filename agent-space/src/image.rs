//! A space's immutable image selection is also its browser capability authority.
use serde::{Deserialize, Serialize};

pub const DEFAULT_IMAGE: &str = "nikolaik/python-nodejs:python3.12-nodejs22-slim";
pub const DESKTOP_PRESET: &str = "runcove-desktop";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ImageSelection {
    Custom(String),
    Preset(PresetSelection),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PresetSelection {
    pub preset: String,
}

impl From<&str> for ImageSelection {
    fn from(value: &str) -> Self {
        Self::Custom(value.into())
    }
}
impl From<String> for ImageSelection {
    fn from(value: String) -> Self {
        Self::Custom(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpaceImage {
    Custom {
        reference: String,
    },
    Desktop {
        preset: String,
        reference: String,
        #[serde(rename = "workerContract")]
        worker_contract: u32,
    },
}

impl Default for SpaceImage {
    fn default() -> Self {
        Self::Custom {
            reference: DEFAULT_IMAGE.into(),
        }
    }
}
impl SpaceImage {
    pub fn reference(&self) -> &str {
        match self {
            Self::Custom { reference } | Self::Desktop { reference, .. } => reference,
        }
    }
    pub fn browser(&self) -> bool {
        matches!(self, Self::Desktop { .. })
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_reference(self.reference())?;
        if let Self::Desktop {
            preset,
            reference,
            worker_contract,
        } = self
        {
            anyhow::ensure!(
                preset == DESKTOP_PRESET && *worker_contract == 1,
                "unsupported desktop contract"
            );
            let (_, digest) = reference
                .rsplit_once("@sha256:")
                .ok_or_else(|| anyhow::anyhow!("desktop image requires an OCI digest"))?;
            anyhow::ensure!(
                digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid desktop image digest"
            );
        }
        Ok(())
    }
}

fn validate_reference(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 2048
            && !value.chars().any(char::is_whitespace)
            && !value.contains('\0'),
        "invalid image reference"
    );
    Ok(())
}

impl ImageSelection {
    pub fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Custom(reference) => validate_reference(reference),
            Self::Preset(value) => {
                anyhow::ensure!(
                    value.preset == DESKTOP_PRESET,
                    "unknown image preset: {}",
                    value.preset
                );
                Ok(())
            }
        }
    }
    pub fn resolve(&self) -> anyhow::Result<SpaceImage> {
        self.validate()?;
        let image = match self {
            Self::Custom(reference) => SpaceImage::Custom {
                reference: reference.clone(),
            },
            Self::Preset(_) => {
                serde_json::from_str(include_str!("../images/desktop-image.lock.json"))?
            }
        };
        image.validate()?;
        Ok(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_is_explicit_and_strict() {
        let custom: ImageSelection = serde_json::from_str("\"my-browser:latest\"").unwrap();
        assert!(!custom.resolve().unwrap().browser());
        let preset: ImageSelection =
            serde_json::from_str(r#"{"preset":"runcove-desktop"}"#).unwrap();
        preset.validate().unwrap();
        assert!(serde_json::from_str::<ImageSelection>(
            r#"{"preset":"runcove-desktop","reference":"anything"}"#
        )
        .is_err());
        assert!(ImageSelection::from("").validate().is_err());
        assert!(
            serde_json::from_str::<ImageSelection>(r#"{"preset":"unknown"}"#)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    #[test]
    fn desktop_requires_immutable_reference() {
        let mut image = SpaceImage::Desktop {
            preset: DESKTOP_PRESET.into(),
            reference: "repo:latest".into(),
            worker_contract: 1,
        };
        assert!(image.validate().is_err());
        if let SpaceImage::Desktop { reference, .. } = &mut image {
            *reference = format!("repo@sha256:{}", "a".repeat(64));
        }
        image.validate().unwrap();
    }
}
