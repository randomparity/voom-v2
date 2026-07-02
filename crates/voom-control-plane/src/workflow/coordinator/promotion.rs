//! Terminal-artifact placement: promote scoped chain-tip artifacts out of their
//! working dirs into the operator's `--output-dir`, add-only, repointing each
//! artifact's durable location at the promoted path.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use tokio::io::AsyncReadExt;
use voom_core::{FileAssetId, FileLocationId, FileVersionId, JobId, VoomError};
use voom_store::repo::identity::{FileLocationKind, IdentityRepo};
use voom_store::repo::workflow_summaries::FilePhaseSummary;

use crate::ControlPlane;
use crate::cases::policy::compliance::PromotionPlan;
use crate::cases::{begin_tx, commit_tx};
use crate::workflow::coordinator::finalize::WorkingDirArtifact;

/// Canonicalized `(working dir, output dir)` pairs for a run. A working dir is
/// absent when its operation produced nothing this run, so it is dropped.
struct ResolvedPromotionDirs {
    working_to_output: Vec<(PathBuf, PathBuf)>,
}

impl ResolvedPromotionDirs {
    fn is_empty(&self) -> bool {
        self.working_to_output.is_empty()
    }

    /// The output dir for an artifact path, by longest working-dir prefix match.
    fn output_for(&self, path: &Path) -> Option<&Path> {
        self.working_to_output
            .iter()
            .filter(|(working, _)| path.starts_with(working))
            .max_by_key(|(working, _)| working.as_os_str().len())
            .map(|(_, output)| output.as_path())
    }
}

/// Canonicalize the promotion plan's working dirs that exist on disk.
async fn resolve_promotion_dirs(plan: &PromotionPlan) -> ResolvedPromotionDirs {
    let mut working_to_output = Vec::new();
    for pair in &plan.pairs {
        if let Ok(canonical) = tokio::fs::canonicalize(&pair.working_dir).await {
            working_to_output.push((canonical, pair.output_dir.clone()));
        }
    }
    ResolvedPromotionDirs { working_to_output }
}

/// The longest directory path shared by every input, compared component-wise
/// (purely lexical — no filesystem access). Empty when the inputs share no
/// leading component or the slice is empty.
fn longest_common_dir(dirs: &[PathBuf]) -> PathBuf {
    let mut iter = dirs.iter();
    let Some(first) = iter.next() else {
        return PathBuf::new();
    };
    let mut common: Vec<Component> = first.components().collect();
    for dir in iter {
        let shared = common
            .iter()
            .zip(dir.components())
            .take_while(|(a, b)| *a == b)
            .count();
        common.truncate(shared);
    }
    common.iter().collect()
}

pub(super) fn ensure_unique_active_branch_ids(
    branch_ids: &[(FileVersionId, String)],
) -> Result<(), VoomError> {
    let mut seen = HashMap::with_capacity(branch_ids.len());
    for &(file_version_id, ref branch_id) in branch_ids {
        if let Some(previous) = seen.insert(branch_id.as_str(), file_version_id) {
            if previous == file_version_id {
                return Err(VoomError::Config(format!(
                    "active file {file_version_id} appears more than once with branch id \
                     `{branch_id}`; phase-barrier summaries require one row per active file"
                )));
            }
            return Err(VoomError::Config(format!(
                "active files {previous} and {file_version_id} both derive branch id \
                 `{branch_id}`; phase-barrier summaries require a unique branch id per file"
            )));
        }
    }
    Ok(())
}

/// Create and canonicalize an output directory ahead of a promotion move.
async fn ensure_output_dir(output_dir: &Path) -> Result<PathBuf, VoomError> {
    tokio::fs::create_dir_all(output_dir).await.map_err(|err| {
        VoomError::Config(format!("create output dir {}: {err}", output_dir.display()))
    })?;
    tokio::fs::canonicalize(output_dir).await.map_err(|err| {
        VoomError::Config(format!(
            "canonicalize output dir {}: {err}",
            output_dir.display()
        ))
    })
}

/// Move a terminal artifact into its promoted destination, add-only.
///
/// A live foreign destination collision fails the run (mirrors the commit's
/// no-replace contract). A destination that already holds this artifact's bytes
/// is a resume of an interrupted promotion — recognised and repointed rather than
/// failed: either the source is already gone (an earlier run promoted and crashed
/// before repointing) or the source is still present and byte-equal to the
/// destination (a cross-filesystem copy whose source removal or DB repoint did not
/// complete). Cross-filesystem placement goes through a temp sibling so the
/// destination is never observed partial.
async fn move_terminal_artifact(current: &Path, dest: &Path) -> Result<PathBuf, VoomError> {
    match tokio::fs::symlink_metadata(dest).await {
        Ok(dest_meta) => return resolve_existing_destination(current, dest, &dest_meta).await,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(VoomError::Config(format!(
                "stat promotion destination {}: {err}",
                dest.display()
            )));
        }
    }
    if tokio::fs::rename(current, dest).await.is_ok() {
        return Ok(dest.to_path_buf());
    }
    // A failed rename (typically a cross-filesystem EXDEV) falls back to an atomic
    // copy-into-place: stream into a temp sibling, then rename it over dest.
    copy_into_place(current, dest).await?;
    Ok(dest.to_path_buf())
}

/// Classify a pre-existing promotion destination: a resumed/interrupted promotion
/// of this artifact (repoint) versus a genuine foreign collision (fail).
async fn resolve_existing_destination(
    current: &Path,
    dest: &Path,
    dest_meta: &std::fs::Metadata,
) -> Result<PathBuf, VoomError> {
    if tokio::fs::symlink_metadata(current).await.is_err() {
        // Source gone: an earlier run promoted the bytes and crashed before the
        // repoint. Resume completes the repoint.
        return Ok(dest.to_path_buf());
    }
    if dest_meta.file_type().is_file() && files_have_equal_contents(current, dest).await? {
        tracing::info!(
            source = %current.display(),
            destination = %dest.display(),
            "recovered an interrupted cross-filesystem promotion; the source is \
             already copied to the destination"
        );
        remove_promoted_source(current).await;
        return Ok(dest.to_path_buf());
    }
    Err(VoomError::Config(format!(
        "promotion destination already exists: {}",
        dest.display()
    )))
}

/// Whether two files hold identical bytes. Size-first (a cheap reject), then a
/// chunked streaming compare so a multi-GB media artifact is never loaded whole.
async fn files_have_equal_contents(a: &Path, b: &Path) -> Result<bool, VoomError> {
    let len_a = tokio::fs::metadata(a)
        .await
        .map_err(|err| VoomError::Config(format!("stat {} to compare: {err}", a.display())))?
        .len();
    let len_b = tokio::fs::metadata(b)
        .await
        .map_err(|err| VoomError::Config(format!("stat {} to compare: {err}", b.display())))?
        .len();
    if len_a != len_b {
        return Ok(false);
    }
    let mut file_a = tokio::fs::File::open(a)
        .await
        .map_err(|err| VoomError::Config(format!("open {} to compare: {err}", a.display())))?;
    let mut file_b = tokio::fs::File::open(b)
        .await
        .map_err(|err| VoomError::Config(format!("open {} to compare: {err}", b.display())))?;
    let mut buf_a = vec![0u8; 64 * 1024];
    let mut buf_b = vec![0u8; 64 * 1024];
    let mut remaining = len_a;
    while remaining > 0 {
        let chunk_len = remaining.min(buf_a.len() as u64);
        let chunk = usize::try_from(chunk_len)
            .map_err(|_| VoomError::Internal(format!("compare chunk {chunk_len} exceeds usize")))?;
        file_a
            .read_exact(&mut buf_a[..chunk])
            .await
            .map_err(|err| VoomError::Config(format!("read {} to compare: {err}", a.display())))?;
        file_b
            .read_exact(&mut buf_b[..chunk])
            .await
            .map_err(|err| VoomError::Config(format!("read {} to compare: {err}", b.display())))?;
        if buf_a[..chunk] != buf_b[..chunk] {
            return Ok(false);
        }
        remaining -= chunk_len;
    }
    Ok(true)
}

/// Hidden temp sibling for the copy fallback. A dotfile prefixed/suffixed out of
/// the plain-filename destination namespace, so it can never equal another
/// artifact's promoted destination in a shared output dir.
fn promotion_temp_path(dest: &Path) -> Result<PathBuf, VoomError> {
    let file_name = dest.file_name().ok_or_else(|| {
        VoomError::Internal(format!(
            "promotion destination has no file name: {}",
            dest.display()
        ))
    })?;
    let mut temp_name = OsString::from(".voom-promote.");
    temp_name.push(file_name);
    temp_name.push(".partial");
    Ok(dest.with_file_name(temp_name))
}

/// Remove a promoted artifact's source once its bytes are safe at the
/// destination. Best-effort: the promotion's commit is the durable location
/// repoint, so a failed cleanup is logged, not fatal, and cannot wedge a resume.
async fn remove_promoted_source(current: &Path) {
    if let Err(err) = tokio::fs::remove_file(current).await {
        tracing::warn!(
            source = %current.display(),
            error = %err,
            "promoted terminal artifact is placed at its destination but removing \
             the source failed; leaving an orphaned source in the working dir"
        );
    }
}

/// Place a terminal artifact at `dest` across filesystems without ever leaving a
/// partial `dest`: stream into a hidden temp sibling on `dest`'s filesystem, then
/// atomically `rename` it into place (an intra-filesystem rename is atomic). The
/// source is removed best-effort afterward. Used when a direct `rename` fails
/// (typically a cross-filesystem `EXDEV`).
async fn copy_into_place(current: &Path, dest: &Path) -> Result<(), VoomError> {
    let temp = promotion_temp_path(dest)?;
    if let Err(err) = tokio::fs::copy(current, &temp).await {
        // A partial copy may already exist at temp; remove it so no stray
        // `.partial` dotfile is left in the operator's output dir.
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(VoomError::Config(format!(
            "copy terminal artifact {} -> {}: {err}",
            current.display(),
            temp.display()
        )));
    }
    if let Err(err) = tokio::fs::rename(&temp, dest).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(VoomError::Config(format!(
            "place terminal artifact {} -> {}: {err}",
            temp.display(),
            dest.display()
        )));
    }
    remove_promoted_source(current).await;
    Ok(())
}

impl ControlPlane {
    /// Promote scoped terminal (chain-tip) artifacts out of their working dirs
    /// into the operator's `--output-dir`, repointing each artifact's durable
    /// location at the promoted path so the chain tip resolves there.
    ///
    /// `location_ids` is the run/resume scope: file-phase produced locations plus
    /// succeeded ticket result locations for sidecar outputs. Only a version that
    /// is its asset's chain tip is promoted; intermediate artifacts stay in the
    /// working dir. Idempotent: once promoted, a location no longer lives under a
    /// working dir, so a re-run or resume skips it. Mirrors the commit's add-only
    /// contract — a destination collision fails the run.
    pub(super) async fn promote_terminal_artifacts(
        &self,
        plan: &PromotionPlan,
        location_ids: &[FileLocationId],
    ) -> Result<(), VoomError> {
        let dirs = resolve_promotion_dirs(plan).await;
        if dirs.is_empty() || location_ids.is_empty() {
            return Ok(());
        }
        // Pass 1: collect the terminal artifacts that will promote, each with the
        // directory of its asset's original scanned source. The longest common
        // ancestor of those source dirs anchors a subtree-mirroring layout under
        // the output dir, so two sources sharing a basename across different
        // subdirectories (issue #197) promote to distinct destinations instead of
        // colliding after their transcodes already ran.
        let mut candidates = Vec::new();
        let mut source_dirs = Vec::new();
        for artifact in self.working_dir_artifacts(location_ids).await? {
            // `resolve_promotion_dirs` canonicalizes each working dir, so the
            // candidate must be canonicalized too or a symlinked path component
            // (e.g. macOS `/tmp` -> `/private/tmp`) breaks the prefix match and
            // the terminal artifact is silently left in the working dir. The
            // artifact exists at promotion time; fall back to the raw value if it
            // does not so a vanished-but-still-live location still fails loudly in
            // the move rather than being silently skipped.
            let raw = PathBuf::from(&artifact.value);
            let current = tokio::fs::canonicalize(&raw).await.unwrap_or(raw);
            let Some(output_dir) = dirs.output_for(&current) else {
                continue;
            };
            let source_dir = self
                .asset_source_path(artifact.asset_id)
                .await?
                .and_then(|path| path.parent().map(Path::to_path_buf));
            if let Some(dir) = &source_dir {
                source_dirs.push(dir.clone());
            }
            candidates.push((artifact, current, output_dir.to_path_buf(), source_dir));
        }
        let common_root = longest_common_dir(&source_dirs);
        // Pass 2: move each terminal artifact under its mirrored subtree. A
        // source dir under the common root contributes the relative subtree; an
        // unknown source (no local-path location) falls back to a flat promotion.
        for (artifact, current, output_dir, source_dir) in candidates {
            let relative = source_dir
                .as_deref()
                .and_then(|dir| dir.strip_prefix(&common_root).ok())
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let dest_dir = output_dir.join(&relative);
            self.promote_artifact(&artifact, &current, &dest_dir)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn promotion_location_ids(
        &self,
        job_ids: &[JobId],
        file_phases: &[FilePhaseSummary],
    ) -> Result<Vec<FileLocationId>, VoomError> {
        let mut seen = HashSet::new();
        let mut location_ids = Vec::new();
        for row in file_phases {
            let Some(location_id) = row.produced_file_location_id else {
                continue;
            };
            if seen.insert(location_id) {
                location_ids.push(location_id);
            }
        }
        for &job_id in job_ids {
            for location_id in self.ticket_result_location_ids(job_id).await? {
                if seen.insert(location_id) {
                    location_ids.push(location_id);
                }
            }
        }
        Ok(location_ids)
    }

    /// The directory of an asset's original scanned source: the earliest
    /// `file_version`'s first local-path location. `None` when the asset has no
    /// such location (it then promotes flat). Add-only commits keep the earliest
    /// version pointing at the scanned source even after later versions chain on.
    async fn asset_source_path(&self, asset_id: FileAssetId) -> Result<Option<PathBuf>, VoomError> {
        let versions = self.identity.list_file_versions_by_asset(asset_id).await?;
        let Some(first) = versions.first() else {
            return Ok(None);
        };
        let locations = self
            .identity
            .list_file_locations_by_version(first.id)
            .await?;
        Ok(locations
            .into_iter()
            .find(|location| location.kind == FileLocationKind::LocalPath)
            .map(|location| PathBuf::from(location.value)))
    }

    /// Move a terminal artifact into `dest_dir` and repoint its location.
    async fn promote_artifact(
        &self,
        artifact: &WorkingDirArtifact,
        current: &Path,
        dest_dir: &Path,
    ) -> Result<(), VoomError> {
        let file_name = current.file_name().ok_or_else(|| {
            VoomError::Internal(format!(
                "terminal artifact path has no file name: {}",
                current.display()
            ))
        })?;
        let dest_dir = ensure_output_dir(dest_dir).await?;
        let dest = dest_dir.join(file_name);
        let dest = move_terminal_artifact(current, &dest).await?;
        let mut tx = begin_tx(&self.pool).await?;
        self.identity
            .update_file_location_value_in_tx(
                &mut tx,
                artifact.location_id,
                artifact.epoch,
                dest.display().to_string(),
                self.clock().now(),
            )
            .await?;
        commit_tx(tx).await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "promotion_test.rs"]
mod tests;
