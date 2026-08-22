//! Pure scan classification logic, relocated from the control plane's
//! transitional discovery module (`voom-control-plane/src/scan/discovery.rs`)
//! per ADR 0077 / spec C3: extension tables, sidecar kinds and roles,
//! allowlist filtering, and longest-stem sidecar-to-primary matching.
//!
//! Filesystem walking lives in [`crate::walk`]; everything here is pure so
//! tests exercise classification without touching the filesystem.

use std::path::Path;

/// Media extensions scanned by default. An empty request allowlist means
/// "use these defaults" rather than "scan nothing".
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "avi", "m2ts", "m4v", "mkv", "mov", "mp4", "mpeg", "mpg", "ts", "webm",
];

/// Image extensions classified as poster sidecars.
pub const SUPPORTED_IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "tbn"];

/// Kind of external sidecar asset a discovered file maps to. Maps to a
/// `voom_store` `BundleMemberRole` in `scan::persist`. See ADR 0022.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarKind {
    Subtitle,
    Nfo,
    Poster,
    Trailer,
}

impl SidecarKind {
    /// Wire `kind` string carried on a sidecar `ScanCandidateFile`.
    #[must_use]
    pub const fn role(self) -> &'static str {
        match self {
            Self::Subtitle => "external_subtitle",
            Self::Nfo => "nfo",
            Self::Poster => "poster",
            Self::Trailer => "trailer",
        }
    }
}

#[must_use]
pub fn is_supported_media_path(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| ext.eq_ignore_ascii_case(supported))
        })
}

/// True when `path` is primary media under the given extension allowlist. An
/// **empty** allowlist means "use the built-in `SUPPORTED_EXTENSIONS`" — so a
/// root that configures no allowlist scans the default media set. A non-empty
/// allowlist restricts primary-media discovery to those extensions
/// (case-insensitive). Sidecar classification is unaffected.
#[must_use]
pub fn matches_media_extension(path: &Path, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return is_supported_media_path(path);
    }
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| {
            allowlist
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
}

/// Classify a file as an external sidecar asset by extension and, for
/// trailers, filename convention. Returns `None` for primary media and for
/// anything outside the V1 sidecar set. See ADR 0022.
#[must_use]
pub fn classify_sidecar(path: &Path) -> Option<SidecarKind> {
    let ext = path.extension().and_then(std::ffi::OsStr::to_str)?;
    if ext.eq_ignore_ascii_case("srt") {
        return Some(SidecarKind::Subtitle);
    }
    if ext.eq_ignore_ascii_case("nfo") {
        return Some(SidecarKind::Nfo);
    }
    if SUPPORTED_IMAGE_EXTENSIONS
        .iter()
        .any(|supported| ext.eq_ignore_ascii_case(supported))
    {
        return Some(SidecarKind::Poster);
    }
    if is_supported_media_path(path) && has_trailer_suffix(path) {
        return Some(SidecarKind::Trailer);
    }
    None
}

fn has_trailer_suffix(path: &Path) -> bool {
    path.file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|stem| {
            let stem = stem.to_ascii_lowercase();
            stem.ends_with("-trailer") || stem.ends_with(".trailer")
        })
}

/// Index of the primary candidate whose stem best matches `sidecar`, using
/// the control plane's longest-stem rule: an exact stem match wins over a
/// prefix match; among matches of equal stem length the lexicographically
/// smallest candidate locator wins, keeping grouping deterministic.
///
/// `candidates` are primary locators in discovery order; returned indexes
/// align with the caller's candidate list.
///
/// A sidecar whose stem prefixes no accepted primary (for example
/// `movie2.srt` beside only `movie.mkv`) yields `None` and degrades to a
/// counted skip upstream.
#[must_use]
pub fn best_sidecar_candidate(candidates: &[String], sidecar: &str) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            sidecar_matches_media(Path::new(candidate), Path::new(sidecar))
                .map(|stem_len| (index, stem_len, candidate))
        })
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.2.cmp(left.2)))
        .map(|(index, _, _)| index)
}

fn sidecar_matches_media(media: &Path, sidecar: &Path) -> Option<usize> {
    let media_stem = media.file_stem()?.to_str()?;
    let sidecar_stem = sidecar.file_stem()?.to_str()?;
    if sidecar_stem == media_stem {
        return Some(media_stem.len());
    }
    sidecar_stem
        .strip_prefix(media_stem)
        .filter(|suffix| suffix.starts_with('.') || suffix.starts_with('-'))
        .map(|_| media_stem.len())
}

#[cfg(test)]
#[path = "discover_test.rs"]
mod tests;
