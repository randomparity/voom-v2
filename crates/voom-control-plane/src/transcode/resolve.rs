//! Resolution of policy video profile references into fully-typed worker
//! profiles. `Named` references are looked up in the durable registry; `Inline`
//! settings are assigned a deterministic `inline-<hash>` identity. Resolution is
//! the single point where a policy's `VideoProfileRef` becomes a concrete
//! `TranscodeVideoProfile` plus an output container, consumed by the planner.

use voom_core::{
    TranscodeVideoProfile, VoomError, encoder_descriptor, validate_profile_against_descriptor,
};
use voom_plan::planner::transcode_video::inline_profile_id;
use voom_policy::{VideoProfileRef, VideoProfileSettings};
use voom_store::repo::policy::video_profiles::SqliteVideoProfileRepo;

#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub profile: TranscodeVideoProfile,
    pub output_container: String,
}

/// Resolves a policy profile reference into a fully-typed worker profile.
/// `Named` references are looked up in the registry (unknown -> `CONFIG_INVALID`);
/// `Inline` settings are assigned a deterministic `inline-<hash>` identity.
/// Both kinds are validated against the encoder descriptor here, so the
/// resolver is the single guard that rejects a profile the descriptor refuses.
///
/// # Errors
/// Returns `CONFIG_INVALID` when a named profile does not exist, or when either
/// a named or inline profile fails descriptor validation.
pub async fn resolve_video_profile_ref(
    repo: &SqliteVideoProfileRepo,
    reference: &VideoProfileRef,
) -> Result<ResolvedProfile, VoomError> {
    let resolved = match reference {
        VideoProfileRef::Named(name) => {
            let row = repo
                .get_by_name(name)
                .await?
                .ok_or_else(|| VoomError::Config(format!("unknown video profile `{name}`")))?;
            ResolvedProfile {
                output_container: row.output_container.clone(),
                profile: row.to_worker_profile(),
            }
        }
        VideoProfileRef::Inline(settings) => ResolvedProfile {
            output_container: settings
                .output_container
                .clone()
                .unwrap_or_else(|| "mkv".to_owned()),
            profile: inline_to_worker_profile(settings)?,
        },
    };
    // The resolver is the single guard: both reference kinds converge here, so a
    // malformed seed row, a future writer that passes the migration's coarse SQL
    // CHECKs, or a future reference arm cannot resolve a profile the encoder
    // descriptor refuses.
    validate_profile_against_descriptor(&resolved.profile).map_err(VoomError::Config)?;
    Ok(resolved)
}

fn inline_to_worker_profile(s: &VideoProfileSettings) -> Result<TranscodeVideoProfile, VoomError> {
    let descriptor = encoder_descriptor(&s.encoder)
        .ok_or_else(|| VoomError::Config(format!("unknown encoder `{}`", s.encoder)))?;
    Ok(TranscodeVideoProfile {
        name: inline_profile_id(s),
        target_codec: descriptor.target_codec.to_owned(),
        encoder: s.encoder.clone(),
        crf: s.crf,
        cq: s.cq,
        qp: s.qp,
        bitrate_kbps: s.bitrate_kbps,
        preset: s.preset.clone(),
        tune: s.tune.clone(),
        codec_profile: s.codec_profile.clone(),
        codec_level: s.codec_level.clone(),
        pixel_format: s.pixel_format.clone(),
        max_width: s.max_width,
        max_height: s.max_height,
        copy_compatible: s.copy_compatible.unwrap_or(false),
        decode: s.decode,
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
        let mut pending = phase.operations.iter_mut().rev().collect::<Vec<_>>();
        while let Some(operation) = pending.pop() {
            match operation {
                voom_policy::CompiledOperation::Conditional(conditional) => {
                    pending.extend(conditional.operations.iter_mut().rev());
                }
                voom_policy::CompiledOperation::Rules(rules) => {
                    for rule in rules.rules.iter_mut().rev() {
                        pending.extend(rule.operations.iter_mut().rev());
                    }
                }
                voom_policy::CompiledOperation::TranscodeVideo(
                    voom_policy::compiled::CompiledTranscodeVideoOperation {
                        profile,
                        target_codec,
                        container,
                        resolved_profile,
                    },
                ) => match profile {
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
                },
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "resolve_test.rs"]
mod tests;
