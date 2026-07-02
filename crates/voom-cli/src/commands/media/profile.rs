use std::io;

use serde::Serialize;
use voom_control_plane::ControlPlane;
use voom_core::ErrorCode;
use voom_store::repo::video_profiles::{NewVideoProfile, VideoProfile};

use crate::cli::{ProfileCommand, VideoProfileFields};
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

const COMMAND: &str = "profile";

#[derive(Debug, Serialize)]
struct ListData {
    profiles: Vec<ProfileData>,
}

#[derive(Debug, Serialize)]
struct ProfileEnvelopeData {
    profile: ProfileData,
}

#[derive(Debug, Serialize)]
struct ProfileData {
    id: String,
    name: String,
    target_codec: String,
    encoder: String,
    crf: u8,
    preset: String,
    tune: Option<String>,
    codec_profile: Option<String>,
    codec_level: Option<String>,
    pixel_format: Option<String>,
    max_width: Option<u32>,
    max_height: Option<u32>,
    output_container: String,
    copy_compatible: bool,
    retired_at: Option<String>,
}

pub async fn run(database_url: &str, local: Local, command: ProfileCommand) -> io::Result<i32> {
    let cp = match open_control_plane(COMMAND, database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match command {
        ProfileCommand::List => list(&cp, local).await,
        ProfileCommand::Show { name } => show(&cp, local, &name).await,
        ProfileCommand::Create(fields) => {
            emit_one(cp.create_video_profile(fields.into()).await, local)
        }
        ProfileCommand::Update(fields) => {
            let name = fields.name.clone();
            emit_optional(cp.update_video_profile(fields.into()).await, &name, local)
        }
        ProfileCommand::Retire { name } => {
            emit_optional(cp.retire_video_profile(&name).await, &name, local)
        }
    }
}

async fn list(cp: &ControlPlane, local: Local) -> io::Result<i32> {
    match cp.list_video_profiles().await {
        Ok(profiles) => emit_ok(
            COMMAND,
            ListData {
                profiles: profiles.into_iter().map(ProfileData::from).collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

async fn show(cp: &ControlPlane, local: Local, name: &str) -> io::Result<i32> {
    match cp.get_video_profile(name).await {
        Ok(Some(profile)) => emit_profile(profile, local),
        Ok(None) => not_found(name, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_one(result: Result<VideoProfile, voom_core::VoomError>, local: Local) -> io::Result<i32> {
    match result {
        Ok(profile) => emit_profile(profile, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_optional(
    result: Result<Option<VideoProfile>, voom_core::VoomError>,
    name: &str,
    local: Local,
) -> io::Result<i32> {
    match result {
        Ok(Some(profile)) => emit_profile(profile, local),
        Ok(None) => not_found(name, local),
        Err(err) => emit_voom_error(COMMAND, &err, local),
    }
}

fn emit_profile(profile: VideoProfile, local: Local) -> io::Result<i32> {
    emit_ok(
        COMMAND,
        ProfileEnvelopeData {
            profile: ProfileData::from(profile),
        },
        Some(local),
        Vec::new(),
    )
    .map(|()| 0)
}

fn not_found(name: &str, local: Local) -> io::Result<i32> {
    emit_err(
        COMMAND,
        ErrorCode::NotFound.as_str(),
        format!("profile {name:?} not found"),
        None,
        Some(local),
    )?;
    Ok(2)
}

impl From<VideoProfileFields> for NewVideoProfile {
    fn from(fields: VideoProfileFields) -> Self {
        Self {
            name: fields.name,
            encoder: fields.encoder,
            crf: fields.crf,
            preset: fields.preset,
            tune: fields.tune,
            codec_profile: fields.codec_profile,
            codec_level: fields.codec_level,
            pixel_format: fields.pixel_format,
            max_width: fields.max_width,
            max_height: fields.max_height,
            output_container: fields.output_container,
            copy_compatible: fields.copy_compatible,
        }
    }
}

impl From<VideoProfile> for ProfileData {
    fn from(profile: VideoProfile) -> Self {
        Self {
            id: profile.id,
            name: profile.name,
            target_codec: profile.target_codec,
            encoder: profile.encoder,
            crf: profile.crf,
            preset: profile.preset,
            tune: profile.tune,
            codec_profile: profile.codec_profile,
            codec_level: profile.codec_level,
            pixel_format: profile.pixel_format,
            max_width: profile.max_width,
            max_height: profile.max_height,
            output_container: profile.output_container,
            copy_compatible: profile.copy_compatible,
            retired_at: profile.retired_at.map(voom_core::format_iso8601),
        }
    }
}

#[cfg(test)]
#[path = "profile_test.rs"]
mod tests;
