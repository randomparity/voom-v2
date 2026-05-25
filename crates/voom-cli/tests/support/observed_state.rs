#![allow(
    dead_code,
    reason = "E2E support helpers are shared across ignored cases"
)]

use std::io;
use std::path::{Component, Path};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;

pub async fn export_observed_state(
    database_url: &str,
    run_dir: &Path,
    output_path: &Path,
    consumer_version: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let pool = voom_store::connect(database_url).await?;
    let library_root = run_dir.join("library").canonicalize()?;
    let run_id = fixture_run_id(run_dir)?;
    let rows = sqlx::query_as::<_, (i64, i64, String, i64, String, Option<String>)>(
        "SELECT fa.id AS file_asset_id, fv.id AS file_version_id, fv.content_hash, \
                fv.size_bytes, fl.value AS location_value, ms.payload AS snapshot_payload \
         FROM file_assets fa \
         JOIN file_versions fv ON fv.file_asset_id = fa.id AND fv.retired_at IS NULL \
         JOIN file_locations fl ON fl.file_version_id = fv.id \
              AND fl.retired_at IS NULL AND fl.kind = 'local_path' \
         LEFT JOIN media_snapshots ms ON ms.id = ( \
             SELECT max(ms2.id) FROM media_snapshots ms2 WHERE ms2.file_version_id = fv.id \
         ) \
         WHERE fa.retired_at IS NULL \
         ORDER BY fa.id ASC, fv.id ASC, fl.id ASC",
    )
    .fetch_all(&pool)
    .await?;

    let mut assets = Vec::with_capacity(rows.len());
    for (
        file_asset_id,
        _file_version_id,
        content_hash,
        size_bytes,
        location_value,
        snapshot_payload,
    ) in rows
    {
        let current_path = library_relative_path(&library_root, Path::new(&location_value))?;
        let mut asset = serde_json::Map::new();
        asset.insert(
            "observed_ref".to_owned(),
            Value::String(format!("file_asset_{file_asset_id}")),
        );
        asset.insert("current_path".to_owned(), Value::String(current_path));
        if let Some(hash) = maybe_sha256_to_observed_hash(&content_hash)? {
            asset.insert("content_hash".to_owned(), Value::String(hash));
        }
        if let Some(probed) = probed_media(snapshot_payload.as_ref(), size_bytes)? {
            asset.insert("probed".to_owned(), probed);
        }
        let sidecars = observed_sidecars(&library_root, Path::new(&location_value))?;
        if !sidecars.is_empty() {
            asset.insert("sidecars".to_owned(), Value::Array(sidecars));
        }
        assets.push(Value::Object(asset));
    }

    let observed_at = time::OffsetDateTime::now_utc().format(&Rfc3339)?;
    let observed = json!({
        "schema_version": 1,
        "consumer": {
            "name": "voom",
            "version": consumer_version,
        },
        "run_id": run_id,
        "observed_at": observed_at,
        "assets": assets,
    });
    std::fs::write(output_path, serde_json::to_vec_pretty(&observed)?)?;
    Ok(observed)
}

pub fn library_relative_path(
    library_root: &Path,
    absolute_path: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let canonical = absolute_path.canonicalize()?;
    let relative = canonical.strip_prefix(library_root).map_err(|_| {
        io::Error::other(format!(
            "path {} is outside library root {}",
            canonical.display(),
            library_root.display()
        ))
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let text = part.to_str().ok_or_else(|| {
                    io::Error::other("library-relative path contains non-UTF-8 segment")
                })?;
                if text.is_empty() || text == "." || text == ".." || text.contains('\\') {
                    return Err(io::Error::other(format!(
                        "invalid observed-state path segment: {text:?}"
                    ))
                    .into());
                }
                parts.push(text.to_owned());
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(io::Error::other(format!(
                    "invalid observed-state path: {}",
                    relative.display()
                ))
                .into());
            }
        }
    }
    if parts.is_empty() {
        return Err(io::Error::other("observed-state path must not be empty").into());
    }
    Ok(parts.join("/"))
}

pub fn sha256_to_observed_hash(hash: &str) -> Result<String, Box<dyn std::error::Error>> {
    maybe_sha256_to_observed_hash(hash)?.ok_or_else(|| {
        io::Error::other(format!(
            "observed-state export requires sha256 hash, got {hash}"
        ))
        .into()
    })
}

fn maybe_sha256_to_observed_hash(hash: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let Some(hex) = hash.strip_prefix("sha256:") else {
        return Ok(None);
    };
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(Some(format!("sha256:{}", hex.to_ascii_lowercase())))
    } else {
        Err(io::Error::other(format!(
            "invalid sha256 hash for observed-state export: {hash}"
        ))
        .into())
    }
}

fn fixture_run_id(run_dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let replay_path = run_dir.join("replay.json");
    let replay: Value = serde_json::from_slice(&std::fs::read(&replay_path)?)?;
    replay
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            io::Error::other(format!("{} does not contain run_id", replay_path.display())).into()
        })
}

fn probed_media(
    snapshot_payload: Option<&String>,
    size_bytes: i64,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let Some(payload) = snapshot_payload else {
        return Ok(None);
    };
    let snapshot: Value = serde_json::from_str(payload)?;
    let Some(container) = snapshot
        .pointer("/container/format_name")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let Some(duration_seconds) = snapshot
        .pointer("/container/duration_seconds")
        .and_then(Value::as_f64)
    else {
        return Ok(None);
    };
    let streams = snapshot
        .get("streams")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|stream| probed_stream(stream, container))
                .collect::<Vec<Value>>()
        })
        .unwrap_or_default();
    Ok(Some(json!({
        "container": container,
        "duration_seconds": duration_seconds,
        "size_bytes": size_bytes,
        "streams": streams,
    })))
}

fn probed_stream(stream: &Value, container: &str) -> Option<Value> {
    let kind = stream.get("kind").and_then(Value::as_str)?;
    let codec = stream.get("codec_name").and_then(Value::as_str)?;
    if !matches!(kind, "video" | "audio" | "subtitle") {
        return None;
    }
    let mut out = serde_json::Map::new();
    out.insert("kind".to_owned(), Value::String(kind.to_owned()));
    out.insert("codec".to_owned(), Value::String(codec.to_owned()));
    if let Some(language) = stream
        .get("language")
        .and_then(Value::as_str)
        .or_else(|| mp4_default_language(container))
    {
        out.insert("language".to_owned(), Value::String(language.to_owned()));
    }
    for (source, target) in [
        ("width", "width"),
        ("height", "height"),
        ("channels", "channels"),
        ("sample_rate", "sample_rate"),
    ] {
        if let Some(value) = stream.get(source).and_then(Value::as_u64) {
            out.insert(target.to_owned(), Value::Number(value.into()));
        }
    }
    if let Some(fps) = stream
        .get("avg_frame_rate")
        .and_then(Value::as_str)
        .and_then(parse_ratio)
    {
        out.insert("fps".to_owned(), serde_json::Number::from_f64(fps)?.into());
    }
    Some(Value::Object(out))
}

fn mp4_default_language(container: &str) -> Option<&'static str> {
    if container
        .split(',')
        .any(|part| part == "mp4" || part == "mov")
    {
        Some("und")
    } else {
        None
    }
}

fn observed_sidecars(
    library_root: &Path,
    asset_path: &Path,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let canonical = asset_path.canonicalize()?;
    let Some(stem) = canonical.file_stem().and_then(|value| value.to_str()) else {
        return Ok(Vec::new());
    };
    let mut candidates = Vec::new();
    collect_sidecar_candidates(
        library_root,
        stem,
        canonical.parent().unwrap_or(library_root),
        &mut candidates,
    )?;
    if canonical.parent() != Some(library_root) {
        collect_sidecar_candidates(library_root, stem, library_root, &mut candidates)?;
    }
    candidates.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["path"].as_str().unwrap_or_default())
    });
    Ok(candidates)
}

fn collect_sidecar_candidates(
    library_root: &Path,
    asset_stem: &str,
    dir: &Path,
    candidates: &mut Vec<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("srt") {
            continue;
        }
        let Some(sidecar_stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if !sidecar_stem
            .strip_prefix(asset_stem)
            .is_some_and(|suffix| suffix.starts_with('.'))
        {
            continue;
        }
        let relative_path = library_relative_path(library_root, &path)?;
        candidates.push(json!({
            "observed_ref": format!("sidecar:{relative_path}"),
            "kind": "subtitle",
            "path": relative_path,
            "content_hash": sha256_file(&path)?,
        }));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(path)?);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn parse_ratio(text: &str) -> Option<f64> {
    let (num, den) = text.split_once('/')?;
    let num = num.parse::<f64>().ok()?;
    let den = den.parse::<f64>().ok()?;
    if den == 0.0 { None } else { Some(num / den) }
}
