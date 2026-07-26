use serde::{Deserialize, Serialize};

pub const REMUX_CONTAINER_MKV: &str = "mkv";

const FONT_ATTACHMENT_MIME_TYPES: &[&str] = &[
    "font/sfnt",
    "font/ttf",
    "font/otf",
    "font/collection",
    "font/woff",
    "font/woff2",
    "application/x-truetype-font",
    "application/x-font-ttf",
    "application/vnd.ms-opentype",
    "application/font-sfnt",
    "application/font-woff",
];

#[must_use]
pub fn is_supported_remux_container(container: &str) -> bool {
    container.eq_ignore_ascii_case(REMUX_CONTAINER_MKV)
}

#[must_use]
pub fn is_font_attachment_mime_type(mime_type: &str) -> bool {
    FONT_ATTACHMENT_MIME_TYPES.contains(&mime_type)
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
