#![allow(
    dead_code,
    reason = "E2E support helpers are shared across ignored cases"
)]

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path};

use serde_json::{Value, json};
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
    let rows = sqlx::query_as::<_, (i64, i64, String, i64, String, Option<String>, Option<i64>)>(
        "SELECT fa.id AS file_asset_id, fv.id AS file_version_id, fv.content_hash, \
                fv.size_bytes, fl.value AS location_value, ms.payload AS snapshot_payload, \
                bm.bundle_id AS bundle_id \
         FROM file_assets fa \
         JOIN file_versions fv ON fv.file_asset_id = fa.id AND fv.retired_at IS NULL \
         JOIN file_locations fl ON fl.file_version_id = fv.id \
              AND fl.retired_at IS NULL AND fl.kind = 'local_path' \
         LEFT JOIN asset_bundle_members bm ON bm.file_asset_id = fa.id \
         LEFT JOIN media_snapshots ms ON ms.id = ( \
             SELECT max(ms2.id) FROM media_snapshots ms2 WHERE ms2.file_version_id = fv.id \
         ) \
         WHERE fa.retired_at IS NULL \
           AND (bm.role IS NULL OR bm.role <> 'external_subtitle') \
         ORDER BY fa.id ASC, fv.id ASC, fl.id ASC",
    )
    .fetch_all(&pool)
    .await?;
    let sidecars_by_bundle = durable_sidecars_by_bundle(&pool, &library_root).await?;

    let mut assets = Vec::with_capacity(rows.len());
    for (
        file_asset_id,
        _file_version_id,
        content_hash,
        size_bytes,
        location_value,
        snapshot_payload,
        bundle_id,
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
        if let Some(sidecars) = bundle_id.and_then(|id| sidecars_by_bundle.get(&id)) {
            asset.insert("sidecars".to_owned(), Value::Array(sidecars.clone()));
        }
        assets.push(Value::Object(asset));
    }

    let observed_at = time::OffsetDateTime::now_utc().format(&Rfc3339)?;
    let observed = json!({
        "schema_version": 4,
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
                .filter_map(probed_stream)
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

fn probed_stream(stream: &Value) -> Option<Value> {
    let kind = stream.get("kind").and_then(Value::as_str)?;
    let codec = stream.get("codec_name").and_then(Value::as_str)?;
    if !matches!(kind, "video" | "audio" | "subtitle") {
        return None;
    }
    let mut out = serde_json::Map::new();
    out.insert("kind".to_owned(), Value::String(kind.to_owned()));
    out.insert("codec".to_owned(), Value::String(codec.to_owned()));
    for (source, target) in [("language", "language"), ("title", "title")] {
        if let Some(value) = stream.get(source).and_then(Value::as_str) {
            out.insert(target.to_owned(), Value::String(value.to_owned()));
        }
    }
    // The oracle records channel_layout and role for audio streams only; a
    // ROLE tag on a video or subtitle stream must not be exported, or it
    // diverges against an expected null.
    if kind == "audio" {
        for (source, target) in [("channel_layout", "channel_layout"), ("role", "role")] {
            if let Some(value) = stream.get(source).and_then(Value::as_str) {
                out.insert(target.to_owned(), Value::String(value.to_owned()));
            }
        }
    }
    // Chaos Librarian's oracle reads MP4 audio titles from the hdlr box
    // (ffprobe handler_name); mirror that fallback so titled MP4 audio matches.
    if kind == "audio"
        && !out.contains_key("title")
        && let Some(handler_name) = stream.get("handler_name").and_then(Value::as_str)
    {
        out.insert("title".to_owned(), Value::String(handler_name.to_owned()));
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
    // The oracle records default/forced dispositions only for subtitle streams;
    // exporting them for audio/video would diverge against an expected null.
    if kind == "subtitle" {
        for flag in ["default", "forced"] {
            if let Some(value) = stream
                .pointer(&format!("/disposition/{flag}"))
                .and_then(Value::as_bool)
            {
                out.insert(flag.to_owned(), Value::Bool(value));
            }
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

#[cfg(test)]
pub fn probed_stream_for_test(stream: &Value) -> Option<Value> {
    probed_stream(stream)
}

async fn durable_sidecars_by_bundle(
    pool: &sqlx::SqlitePool,
    library_root: &Path,
) -> Result<BTreeMap<i64, Vec<Value>>, Box<dyn std::error::Error>> {
    let rows = sqlx::query_as::<_, (i64, i64, i64, String, i64, String)>(
        "SELECT bm.bundle_id, fa.id, fv.id, fv.content_hash, fv.size_bytes, fl.value \
         FROM asset_bundle_members bm \
         JOIN file_assets fa ON fa.id = bm.file_asset_id AND fa.retired_at IS NULL \
         JOIN file_versions fv ON fv.file_asset_id = fa.id AND fv.retired_at IS NULL \
         JOIN file_locations fl ON fl.file_version_id = fv.id \
              AND fl.retired_at IS NULL AND fl.kind = 'local_path' \
         WHERE bm.role = 'external_subtitle' \
         ORDER BY bm.bundle_id ASC, fl.value ASC",
    )
    .fetch_all(pool)
    .await?;

    let mut by_bundle = BTreeMap::<i64, Vec<Value>>::new();
    for (bundle_id, file_asset_id, _file_version_id, content_hash, _size_bytes, location_value) in
        rows
    {
        let relative_path = library_relative_path(library_root, Path::new(&location_value))?;
        by_bundle.entry(bundle_id).or_default().push(json!({
            "observed_ref": format!("file_asset_{file_asset_id}"),
            "kind": "subtitle",
            "path": relative_path,
            "content_hash": sha256_to_observed_hash(&content_hash)?,
        }));
    }
    Ok(by_bundle)
}

fn parse_ratio(text: &str) -> Option<f64> {
    let (num, den) = text.split_once('/')?;
    let num = num.parse::<f64>().ok()?;
    let den = den.parse::<f64>().ok()?;
    if den == 0.0 { None } else { Some(num / den) }
}
