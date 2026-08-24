//! Byte-free output naming shared with envelope rendering (ADR 0075): the
//! `<stem>.<profile_id>.<codec>.<container>` layout a transcode ticket's
//! planned output is addressed by.
//!
//! The staging/target byte-path halves were removed in the T8 sweep —
//! transcode tickets execute through their storage owner's agent, which owns
//! its own destination layout.

use std::path::Path;

/// Borrowed inputs that determine the output file name for a transcode.
#[derive(Debug)]
pub struct OutputName<'a> {
    pub source_path: &'a str,
    pub profile_id: &'a str,
    pub codec: &'a str,
    pub container: &'a str,
}

/// Builds the output file name from the source stem, profile identity,
/// target codec, and container extension. The `profile_id` is sanitized
/// so any character outside `[A-Za-z0-9._-]` is replaced with `-`, keeping
/// file names safe across all filesystems.
///
/// Format: `<stem>.<profile_id>.<codec>.<container>`
pub fn output_file_name(output: &OutputName<'_>) -> String {
    let stem = Path::new(output.source_path)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("output");
    let sanitized_id: String = output
        .profile_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!(
        "{stem}.{sanitized_id}.{}.{}",
        output.codec, output.container
    )
}
