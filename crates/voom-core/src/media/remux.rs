use serde::{Deserialize, Serialize};

pub const REMUX_CONTAINER_MKV: &str = "mkv";

#[must_use]
pub fn is_supported_remux_container(container: &str) -> bool {
    container.eq_ignore_ascii_case(REMUX_CONTAINER_MKV)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemuxTrackGroup {
    Video,
    Audio,
    Subtitle,
    Attachment,
}

#[cfg(test)]
#[path = "remux_test.rs"]
mod tests;
