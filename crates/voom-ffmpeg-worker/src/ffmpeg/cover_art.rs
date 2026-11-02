use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::process::Command;

use super::{
    FfmpegConfig, FfmpegError, disposition_bool, probe_json, run_ffmpeg_command, stream_tag,
};
use crate::observe::observe_file_facts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverArt {
    pub index: u32,
    pub codec: String,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub title: Option<String>,
}

fn invalid(message: impl std::fmt::Display) -> FfmpegError {
    FfmpegError::OutputFactsMismatch(format!("cover art: {message}"))
}

pub(super) fn from_probe(probe: &Value) -> Result<Vec<CoverArt>, FfmpegError> {
    let streams = probe
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("missing stream inventory"))?;
    let mut covers = Vec::new();
    let mut indices = std::collections::BTreeSet::new();
    for stream in streams
        .iter()
        .filter(|stream| disposition_bool(stream, "attached_pic") == Some(true))
    {
        let index = stream
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| u32::try_from(index).ok())
            .ok_or_else(|| invalid("invalid attached-picture stream index"))?;
        if !indices.insert(index) {
            return Err(invalid("duplicate attached-picture stream index"));
        }
        let codec = stream
            .get("codec_name")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing attached-picture codec"))?
            .to_owned();
        covers.push(CoverArt {
            index,
            codec,
            filename: stream_tag(stream, "filename"),
            mime_type: stream_tag(stream, "mimetype"),
            title: stream_tag(stream, "title"),
        });
    }
    Ok(covers)
}

pub(crate) fn validate(
    covers: &[CoverArt],
    container: &str,
    attachments: usize,
) -> Result<(), FfmpegError> {
    if container == "mp4" && attachments != 0 {
        return Err(FfmpegError::UnsupportedInput(
            "mp4 cannot preserve generic attachments".to_owned(),
        ));
    }
    for cover in covers {
        let supported = match container {
            "mkv" => matches!(cover.codec.as_str(), "mjpeg" | "png" | "gif" | "tiff"),
            "mp4" => matches!(cover.codec.as_str(), "mjpeg" | "png"),
            _ => false,
        };
        if !supported {
            return Err(FfmpegError::UnsupportedInput(format!(
                "{container} cannot preserve cover art codec `{}`",
                cover.codec
            )));
        }
    }
    Ok(())
}

fn metadata(cover: &CoverArt) -> (String, String) {
    let (extension, mime) = match cover.codec.as_str() {
        "mjpeg" => ("jpg", "image/jpeg"),
        "png" => ("png", "image/png"),
        "gif" => ("gif", "image/gif"),
        _ => ("tiff", "image/tiff"),
    };
    (
        cover
            .filename
            .clone()
            .unwrap_or_else(|| format!("cover-{}.{}", cover.index, extension)),
        cover.mime_type.clone().unwrap_or_else(|| mime.to_owned()),
    )
}

pub(super) struct PreparedCovers {
    directory: tempfile::TempDir,
    paths: Vec<PathBuf>,
}

impl PreparedCovers {
    pub(super) async fn prepare(
        config: &FfmpegConfig,
        input: &Path,
        staging: &Path,
        covers: &[CoverArt],
    ) -> Result<Self, FfmpegError> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("voom-cover-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let directory = builder.tempdir_in(staging).map_err(invalid)?;
        let mut prepared = Self {
            directory,
            paths: Vec::with_capacity(covers.len()),
        };
        for (ordinal, cover) in covers.iter().enumerate() {
            let path = prepared
                .directory
                .path()
                .join(format!("source-{ordinal}.bin"));
            extract(config, input, cover.index, &path).await?;
            prepared.paths.push(path);
        }
        Ok(prepared)
    }

    pub(super) fn append(
        &self,
        command: &mut Command,
        covers: &[CoverArt],
        container: &str,
        attachments: usize,
    ) {
        for (ordinal, (cover, path)) in covers.iter().zip(&self.paths).enumerate() {
            if container == "mp4" {
                command
                    .args(["-map", &format!("0:{}", cover.index)])
                    .arg(format!("-c:v:{}", ordinal + 1))
                    .arg("copy")
                    .arg(format!("-disposition:v:{}", ordinal + 1))
                    .arg("attached_pic");
            } else {
                let (filename, mime) = metadata(cover);
                let attachment = attachments + ordinal;
                command
                    .arg("-attach")
                    .arg(path)
                    .arg(format!("-metadata:s:t:{attachment}"))
                    .arg(format!("filename={filename}"))
                    .arg(format!("-metadata:s:t:{attachment}"))
                    .arg(format!("mimetype={mime}"));
                if let Some(title) = &cover.title {
                    command
                        .arg(format!("-metadata:s:t:{attachment}"))
                        .arg(format!("title={title}"));
                }
            }
        }
    }

    pub(super) async fn verify(
        &self,
        config: &FfmpegConfig,
        output: &Path,
        covers: &[CoverArt],
        container: &str,
    ) -> Result<(), FfmpegError> {
        let actual = from_probe(&probe_json(config, output).await?)?;
        if actual.len() != covers.len() {
            return Err(invalid("output attached-picture count differs from source"));
        }
        for (ordinal, ((source, actual), path)) in
            covers.iter().zip(&actual).zip(&self.paths).enumerate()
        {
            if source.codec != actual.codec
                || (container == "mkv"
                    && (metadata(source) != metadata(actual) || source.title != actual.title))
            {
                return Err(invalid(
                    "output cover codec or metadata differs from source",
                ));
            }
            let extracted = self.directory.path().join(format!("output-{ordinal}.bin"));
            extract(config, output, actual.index, &extracted).await?;
            let before = observe_file_facts(path).await.map_err(invalid)?;
            let after = observe_file_facts(&extracted).await.map_err(invalid)?;
            if before.size_bytes != after.size_bytes || before.content_hash != after.content_hash {
                return Err(invalid("output cover bytes differ from source"));
            }
        }
        Ok(())
    }

    pub(super) fn close(self) -> Result<(), FfmpegError> {
        self.directory.close().map_err(invalid)
    }
}

async fn extract(
    config: &FfmpegConfig,
    input: &Path,
    index: u32,
    output: &Path,
) -> Result<(), FfmpegError> {
    let mut command = Command::new(&config.ffmpeg_path);
    // The image2 muxer rewrites GIF delays even under stream-copy; data preserves packets.
    command
        .args(["-hide_banner", "-nostdin", "-n", "-i"])
        .arg(input)
        .args([
            "-map",
            &format!("0:{index}"),
            "-c",
            "copy",
            "-frames:v",
            "1",
            "-f",
            "data",
        ])
        .arg(output)
        .kill_on_drop(true);
    run_ffmpeg_command(config, command).await
}

#[cfg(test)]
#[path = "cover_art_test.rs"]
mod tests;
