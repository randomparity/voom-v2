use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// Typed inline encode settings. `encoder`, `crf`, `preset` are mandatory in an
/// inline body; the remaining fields are optional. Validation against the
/// per-encoder capability descriptors happens in the compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoProfileSettings {
    pub encoder: String,
    pub crf: u8,
    pub preset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tune: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_container: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_compatible: Option<bool>,
}

/// A policy reference to a video encode profile: a registry name or inline
/// settings. Serializes tagged (`{"named": ...}` / `{"inline": ...}`); also
/// deserializes a legacy bare JSON string as `Named` for backward compatibility
/// with compiled policies persisted before Sprint 15.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoProfileRef {
    Named(String),
    Inline(VideoProfileSettings),
}

impl<'de> Deserialize<'de> for VideoProfileRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RefVisitor;

        impl<'de> Visitor<'de> for RefVisitor {
            type Value = VideoProfileRef;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a profile name string or a tagged {named|inline} object")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(VideoProfileRef::Named(value.to_owned()))
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let key: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::custom("empty profile ref object"))?;
                let value = match key.as_str() {
                    "named" => VideoProfileRef::Named(map.next_value()?),
                    "inline" => VideoProfileRef::Inline(map.next_value()?),
                    other => return Err(de::Error::unknown_variant(other, &["named", "inline"])),
                };
                if map.next_key::<String>()?.is_some() {
                    return Err(de::Error::custom("unexpected trailing key in profile ref"));
                }
                Ok(value)
            }
        }

        deserializer.deserialize_any(RefVisitor)
    }
}

#[cfg(test)]
#[path = "video_profile_test.rs"]
mod tests;
