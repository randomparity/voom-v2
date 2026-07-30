//! Resolved-profile helpers for the planner: a deterministic `inline-<hash>`
//! identity for inline encode settings and a fixed encoder/speed → cpu-cost
//! lookup used to annotate `transcode_video` resource notes.

use voom_core::VideoDecodeMode;
use voom_policy::VideoProfileSettings;

/// Deterministic `inline-<hash>` identity for an inline profile, computed over a
/// canonical representation so it does not drift with serde layout changes.
#[must_use]
pub fn inline_profile_id(settings: &VideoProfileSettings) -> String {
    let canonical = canonical_form(settings);
    let digest = blake3::hash(canonical.as_bytes());
    format!("inline-{}", &digest.to_hex()[..12])
}

fn canonical_form(s: &VideoProfileSettings) -> String {
    let mut parts = Vec::new();
    parts.push(format!("encoder={}", s.encoder.to_ascii_lowercase()));
    if let Some(crf) = s.crf {
        parts.push(format!("crf={crf}"));
    }
    if let Some(cq) = s.cq {
        parts.push(format!("cq={cq}"));
    }
    if let Some(qp) = s.qp {
        parts.push(format!("qp={qp}"));
    }
    // Omitted when absent for the same reason `preset` is: every profile predating
    // the bitrate domain carries `None`, so their canonical form — and therefore
    // their `inline-<hash>` identity — stays byte-identical. Leaving it out
    // entirely made two profiles differing only in bitrate hash the same.
    if let Some(bitrate_kbps) = s.bitrate_kbps {
        parts.push(format!("bitrate_kbps={bitrate_kbps}"));
    }
    // An encoder with no speed knob contributes no `preset=` part at all. Every
    // pre-#409 profile carries one, so their canonical form — and therefore their
    // `inline-<hash>` identity — is byte-identical to before `preset` became optional.
    if let Some(preset) = &s.preset {
        parts.push(format!("preset={}", preset.trim()));
    }
    if let Some(v) = &s.tune {
        parts.push(format!("tune={}", v.to_ascii_lowercase()));
    }
    if let Some(v) = &s.codec_profile {
        parts.push(format!("codec_profile={}", v.to_ascii_lowercase()));
    }
    if let Some(v) = &s.codec_level {
        parts.push(format!("codec_level={}", v.trim().to_ascii_lowercase()));
    }
    if let Some(v) = &s.pixel_format {
        parts.push(format!("pixel_format={}", v.to_ascii_lowercase()));
    }
    if let Some(v) = s.max_width {
        parts.push(format!("max_width={v}"));
    }
    if let Some(v) = s.max_height {
        parts.push(format!("max_height={v}"));
    }
    // Always emit copy_compatible and output_container using the SAME resolved
    // defaults `resolve::inline_to_worker_profile` applies, so an inline profile
    // with these optionals omitted and one with them set to the defaults yield
    // the byte-identical resolved profile AND the same inline-<hash> id.
    let output_container = s
        .output_container
        .as_deref()
        .unwrap_or("mkv")
        .to_ascii_lowercase();
    parts.push(format!("output_container={output_container}"));
    parts.push(format!(
        "copy_compatible={}",
        s.copy_compatible.unwrap_or(false)
    ));
    // Explicit over the vocabulary: software decode is the omitted default and
    // contributes nothing, so a pre-#409 id is unchanged, while a new backend has to
    // decide rather than silently hash as software.
    match s.decode {
        VideoDecodeMode::Software(_) => {}
        VideoDecodeMode::Nvidia(_)
        | VideoDecodeMode::Vaapi(_)
        | VideoDecodeMode::VideoToolbox(_) => {
            parts.push(format!("decode={}", s.decode.as_str()));
        }
    }
    parts.join(";")
}

/// Fixed encoder+speed → cpu cost class lookup for resource notes.
#[must_use]
#[expect(
    clippy::match_same_arms,
    reason = "arms are grouped per encoder for readability; merging cross-encoder patterns would obscure the per-encoder cost model"
)]
pub fn cpu_cost(encoder: &str, speed: &str) -> &'static str {
    match (encoder, speed) {
        ("libx265", "placebo" | "veryslow") => "high",
        ("libx265", "slower" | "slow" | "medium") => "medium",
        ("libx265", _) => "low",
        ("libaom-av1", "0" | "1" | "2") => "high",
        ("libaom-av1", "3" | "4") => "medium",
        ("libaom-av1", _) => "low",
        ("libsvtav1", s) => match s.parse::<u8>() {
            Ok(0..=3) => "high",
            Ok(4..=7) => "medium",
            _ => "low",
        },
        _ => "medium",
    }
}

#[cfg(test)]
#[path = "profile_test.rs"]
mod tests;
