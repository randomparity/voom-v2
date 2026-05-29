//! Resolution of policy video profile references into fully-typed worker
//! profiles. `Named` references are looked up in the durable registry; `Inline`
//! settings are assigned a deterministic `inline-<hash>` identity. Resolution is
//! the single point where a policy's `VideoProfileRef` becomes a concrete
//! `TranscodeVideoProfile` plus an output container, consumed by the planner.

use voom_core::VoomError;
use voom_plan::inline_profile_id;
use voom_policy::{VideoProfileRef, VideoProfileSettings};
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

#[cfg(test)]
#[path = "resolve_test.rs"]
mod tests;
