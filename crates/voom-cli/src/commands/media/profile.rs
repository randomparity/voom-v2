use std::io;

use serde::Serialize;
use voom_core::ErrorCode;
use voom_store::repo::video_profiles::VideoProfile;

use crate::cli::ProfileCommand;
use crate::commands::common::{emit_voom_error, open_control_plane};
use crate::envelope::{Local, emit_err, emit_ok};

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
}

pub async fn run(database_url: &str, local: Local, command: ProfileCommand) -> io::Result<i32> {
    match command {
        ProfileCommand::List => list(database_url, local).await,
        ProfileCommand::Show { name } => show(database_url, local, &name).await,
    }
}

async fn list(database_url: &str, local: Local) -> io::Result<i32> {
    let cp = match open_control_plane("profile", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.list_video_profiles().await {
        Ok(profiles) => emit_ok(
            "profile",
            ListData {
                profiles: profiles.into_iter().map(ProfileData::from).collect(),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Err(err) => emit_voom_error("profile", &err, local),
    }
}

async fn show(database_url: &str, local: Local, name: &str) -> io::Result<i32> {
    let cp = match open_control_plane("profile", database_url, &local).await? {
        Ok(cp) => cp,
        Err(code) => return Ok(code),
    };
    match cp.get_video_profile(name).await {
        Ok(Some(profile)) => emit_ok(
            "profile",
            ProfileEnvelopeData {
                profile: ProfileData::from(profile),
            },
            Some(local),
            Vec::new(),
        )
        .map(|()| 0),
        Ok(None) => {
            emit_err(
                "profile",
                ErrorCode::NotFound.as_str(),
                format!("profile show: name={name} not found"),
                None,
                Some(local),
            )?;
            Ok(2)
        }
        Err(err) => emit_voom_error("profile", &err, local),
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
        }
    }
}

#[cfg(test)]
#[path = "profile_test.rs"]
mod tests;
