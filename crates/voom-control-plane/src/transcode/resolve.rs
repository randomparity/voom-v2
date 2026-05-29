//! Resolution of policy video profile references into fully-typed worker
//! profiles. `Named` references are looked up in the durable registry; `Inline`
//! settings are assigned a deterministic `inline-<hash>` identity. Resolution is
//! the single point where a policy's `VideoProfileRef` becomes a concrete
//! `TranscodeVideoProfile` plus an output container, consumed by the planner.

use voom_core::VoomError;
use voom_plan::inline_profile_id;
use voom_policy::{MediaSnapshotInput, VideoProfileRef, VideoProfileSettings};
use voom_store::repo::video_profiles::{SqliteVideoProfileRepo, VideoProfileRepo};
use voom_worker_protocol::TranscodeVideoProfile;

#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub profile: TranscodeVideoProfile,
    pub output_container: String,
}

/// Resolves a policy profile reference into a fully-typed worker profile.
/// `Named` references are looked up in the registry (unknown -> `CONFIG_INVALID`);
/// `Inline` settings are assigned a deterministic `inline-<hash>` identity.
///
/// # Errors
/// Returns `CONFIG_INVALID` when a named profile does not exist or inline
/// settings fail descriptor validation.
pub async fn resolve_video_profile_ref(
    repo: &SqliteVideoProfileRepo,
    reference: &VideoProfileRef,
) -> Result<ResolvedProfile, VoomError> {
    match reference {
        VideoProfileRef::Named(name) => {
            let row = repo
                .get_by_name(name)
                .await?
                .ok_or_else(|| VoomError::Config(format!("unknown video profile `{name}`")))?;
            Ok(ResolvedProfile {
                output_container: row.output_container.clone(),
                profile: row.to_worker_profile(),
            })
        }
        VideoProfileRef::Inline(settings) => {
            let profile = inline_to_worker_profile(settings)?;
            // Belt-and-braces: validate even though the compiler already did.
            voom_worker_protocol::validate_profile_against_descriptor(&profile)
                .map_err(VoomError::Config)?;
            Ok(ResolvedProfile {
                output_container: settings
                    .output_container
                    .clone()
                    .unwrap_or_else(|| "mkv".to_owned()),
                profile,
            })
        }
    }
}

fn inline_to_worker_profile(s: &VideoProfileSettings) -> Result<TranscodeVideoProfile, VoomError> {
    let descriptor = voom_worker_protocol::encoder_descriptor(&s.encoder)
        .ok_or_else(|| VoomError::Config(format!("unknown encoder `{}`", s.encoder)))?;
    Ok(TranscodeVideoProfile {
        name: inline_profile_id(s),
        target_codec: descriptor.target_codec.to_owned(),
        encoder: s.encoder.clone(),
        crf: s.crf,
        preset: s.preset.clone(),
        tune: s.tune.clone(),
        codec_profile: s.codec_profile.clone(),
        codec_level: s.codec_level.clone(),
        pixel_format: s.pixel_format.clone(),
        max_width: s.max_width,
        max_height: s.max_height,
        copy_compatible: s.copy_compatible.unwrap_or(false),
    })
}

/// Resolves only `Inline` profiles (no registry needed). A `Named` reference
/// returns `CONFIG_INVALID` directing the operator to a store-backed plan, rather
/// than crashing the planner on a `None` `resolved_profile`.
///
/// # Errors
/// Returns `CONFIG_INVALID` for any `Named` reference or invalid inline settings.
pub fn resolve_inline_profiles_in_policy(
    policy: &mut voom_policy::CompiledPolicy,
) -> Result<(), VoomError> {
    for phase in &mut policy.phases {
        for operation in &mut phase.operations {
            if let voom_policy::CompiledOperation::TranscodeVideo {
                profile,
                target_codec,
                container,
                resolved_profile,
            } = operation
            {
                match profile {
                    VideoProfileRef::Inline(settings) => {
                        let typed = inline_to_worker_profile(settings)?;
                        target_codec.clone_from(&typed.target_codec);
                        *container = settings
                            .output_container
                            .clone()
                            .unwrap_or_else(|| "mkv".to_owned());
                        *resolved_profile = Some(typed);
                    }
                    VideoProfileRef::Named(name) => {
                        return Err(VoomError::Config(format!(
                            "named video profile `{name}` cannot be resolved offline; \
                             use `voom plan show` against an initialized store"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Decides whether the worker can stream-copy the video track rather than
/// re-encoding it.
///
/// Returns `true` only when ALL of the following hold:
/// - `profile.copy_compatible` is set (the profile explicitly opts in),
/// - the source video codec already matches the target (`canonical_video_codec`
///   alias-aware comparison, mirroring `planner.rs::transcode_video_needs_change`),
/// - dimension caps are satisfied (source is within `max_width`/`max_height`),
/// - if a target `pixel_format` is constrained, the source matches,
/// - if a target `codec_profile`/`codec_level` is constrained, the source
///   matches (using `normalize_codec_token`, as in `planner.rs`).
///
/// Any constrained observable that is unknown in the snapshot returns `false`
/// (refuse to copy when we can't verify compliance).
#[must_use]
pub fn decide_copy_video(profile: &TranscodeVideoProfile, snapshot: &MediaSnapshotInput) -> bool {
    if !profile.copy_compatible {
        return false;
    }

    // Codec must already be correct (alias-aware).
    let Some(observed_codec) = snapshot.video_codec.as_deref() else {
        return false;
    };
    let codec_matches = voom_worker_protocol::canonical_video_codec(observed_codec)
        .is_some_and(|canonical| canonical.eq_ignore_ascii_case(&profile.target_codec));
    if !codec_matches {
        return false;
    }

    // Dimensions must be within caps.
    if let Some(cap_w) = profile.max_width {
        let Some(width) = snapshot.width else {
            return false;
        };
        if width > cap_w {
            return false;
        }
    }
    if let Some(cap_h) = profile.max_height {
        let Some(height) = snapshot.height else {
            return false;
        };
        if height > cap_h {
            return false;
        }
    }

    // Pixel format must match if constrained.
    if let Some(target_pf) = profile.pixel_format.as_deref() {
        let Some(observed_pf) = video_stream_field(snapshot, "pixel_format") else {
            return false;
        };
        if !observed_pf.eq_ignore_ascii_case(target_pf) {
            return false;
        }
    }

    // Codec profile must match if constrained (normalize whitespace/case like the planner).
    // Cross-reference: planner.rs::codec_profile_needs_change uses the same normalization.
    if let Some(target_cp) = profile.codec_profile.as_deref() {
        let Some(observed_cp) = video_stream_field(snapshot, "profile") else {
            return false;
        };
        if normalize_codec_token(observed_cp) != normalize_codec_token(target_cp) {
            return false;
        }
    }

    // Codec level must match if constrained.
    if let Some(target_cl) = profile.codec_level.as_deref() {
        let Some(observed_cl) = video_stream_field(snapshot, "level") else {
            return false;
        };
        if normalize_codec_token(observed_cl) != normalize_codec_token(target_cl) {
            return false;
        }
    }

    true
}

/// Returns the value of a field from the first video stream in the snapshot's
/// `stream_summary.streams` array.
fn video_stream_field<'a>(snapshot: &'a MediaSnapshotInput, key: &str) -> Option<&'a str> {
    snapshot
        .stream_summary
        .get("streams")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .find(|s| s.get("kind").and_then(serde_json::Value::as_str) == Some("video"))
        .and_then(|s| s.get(key))
        .and_then(serde_json::Value::as_str)
}

/// Normalizes codec profile/level tokens for comparison. ffprobe may report
/// `"Main 10"` while a profile uses `"main10"`. Matches the normalization in
/// `voom_plan::planner::normalize_codec_token`.
fn normalize_codec_token(token: &str) -> String {
    token
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
#[path = "resolve_test.rs"]
mod tests;
