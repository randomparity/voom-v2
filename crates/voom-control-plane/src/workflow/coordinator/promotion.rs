//! Terminal-artifact placement: promote scoped chain-tip artifacts out of their
//! working dirs into the operator's `--output-dir`, add-only, repointing each
//! artifact's durable location at the promoted path.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::File;
use std::path::{Component, Path, PathBuf};

use same_file::Handle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use voom_core::{FileAssetId, FileLocationId, FileVersionId, VoomError};
use voom_policy::{PolicyInputSetDraft, TargetRef};
use voom_store::repo::execution::workflow_summaries::FilePhaseSummary;
use voom_store::repo::media::identity::{FileLocationAddress, FileLocationRepo, FileVersionRepo};

use crate::ControlPlane;
use crate::cases::commit_tx;
use crate::cases::policy::compliance::PromotionPlan;
use crate::workflow::coordinator::finalize::WorkingDirArtifact;
use voom_store::tx::begin_write_first;

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
        self.pair_for(path).map(|(_, output)| output)
    }

    fn pair_for(&self, path: &Path) -> Option<(&Path, &Path)> {
        self.working_to_output
            .iter()
            .filter(|(working, _)| path.starts_with(working))
            .max_by_key(|(working, _)| working.as_os_str().len())
            .map(|(working, output)| (working.as_path(), output.as_path()))
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

fn promotion_relative_dir(
    source_dir: Option<&Path>,
    source_root: &Path,
    branch_source_dir: Option<&Path>,
    auxiliary_root: &Path,
    working_root: &Path,
) -> PathBuf {
    let branch_relative = branch_source_dir
        .and_then(|dir| dir.strip_prefix(source_root).ok())
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let Some(source_dir) = source_dir else {
        return branch_relative;
    };
    if source_dir.starts_with(working_root)
        && !auxiliary_root.as_os_str().is_empty()
        && let Ok(relative) = source_dir.strip_prefix(auxiliary_root)
    {
        return branch_relative.join(relative);
    }
    if !source_root.as_os_str().is_empty()
        && let Ok(relative) = source_dir.strip_prefix(source_root)
    {
        return relative.to_path_buf();
    }
    source_dir
        .strip_prefix(working_root)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

pub(super) fn ensure_unique_selected_branch_ids(
    branch_ids: &[(FileVersionId, String)],
) -> Result<(), VoomError> {
    let mut seen = HashMap::with_capacity(branch_ids.len());
    for &(file_version_id, ref branch_id) in branch_ids {
        if let Some(previous) = seen.insert(branch_id.as_str(), file_version_id) {
            if previous == file_version_id {
                return Err(VoomError::Config(format!(
                    "selected file version {file_version_id} appears more than once with branch id \
                     `{branch_id}`; phase-barrier summaries require one row per selected file"
                )));
            }
            return Err(VoomError::Config(format!(
                "selected file versions {previous} and {file_version_id} both derive branch id \
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
async fn move_terminal_artifact(
    current: &Path,
    dest: &Path,
    location_id: FileLocationId,
) -> Result<PathBuf, VoomError> {
    match tokio::fs::symlink_metadata(dest).await {
        Ok(dest_meta) => {
            let destination = resolve_existing_destination(current, dest, &dest_meta).await?;
            let temp_path = promotion_temp_path(dest, location_id)?;
            remove_existing_promotion_temp(&temp_path).await?;
            return Ok(destination);
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(VoomError::Config(format!(
                "stat promotion destination {}: {err}",
                dest.display()
            )));
        }
    }
    if let Ok(()) = tokio::fs::hard_link(current, dest).await {
        remove_promoted_source(current).await;
        let temp_path = promotion_temp_path(dest, location_id)?;
        remove_existing_promotion_temp(&temp_path).await?;
        return Ok(dest.to_path_buf());
    }
    // A failed hard link (typically cross-filesystem EXDEV) falls back to an
    // atomic copy-into-place.
    let temp_path = promotion_temp_path(dest, location_id)?;
    copy_terminal_artifact(current, dest, &temp_path).await
}

async fn copy_terminal_artifact(
    current: &Path,
    dest: &Path,
    temp_path: &Path,
) -> Result<PathBuf, VoomError> {
    let temp = PromotionTempOwnership::acquire(temp_path).await?;
    let result = match tokio::fs::symlink_metadata(dest).await {
        Ok(dest_meta) => resolve_existing_destination(current, dest, &dest_meta).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            copy_into_place(current, dest, &temp)
                .await
                .map(|()| dest.to_path_buf())
        }
        Err(error) => Err(VoomError::Config(format!(
            "stat promotion destination {}: {error}",
            dest.display()
        ))),
    };
    let cleanup = remove_promotion_temp(temp_path).await;
    match result {
        Ok(destination) => {
            cleanup?;
            Ok(destination)
        }
        Err(source) => {
            if let Err(cleanup_error) = cleanup {
                tracing::warn!(
                    temp = %temp_path.display(),
                    error = %cleanup_error,
                    "failed to remove a promotion temp after placement failed"
                );
            }
            Err(source)
        }
    }
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

/// Durable hidden temp sibling for the copy fallback. The location id makes the
/// name stable across retries and distinct across artifacts, so resume reclaims
/// an interrupted copy instead of allocating another full-size partial. The
/// file also carries the exclusive lock that prevents concurrent resumes from
/// unlinking or republishing each other's partial copy.
fn promotion_temp_path(dest: &Path, location_id: FileLocationId) -> Result<PathBuf, VoomError> {
    let file_name = dest.file_name().ok_or_else(|| {
        VoomError::Internal(format!(
            "promotion destination has no file name: {}",
            dest.display()
        ))
    })?;
    let mut temp_name = OsString::from(".voom-promote.");
    temp_name.push(file_name);
    temp_name.push(format!(".location-{}.partial", location_id.0));
    Ok(dest.with_file_name(temp_name))
}

struct PromotionTempOwnership {
    path: PathBuf,
    file: File,
}

impl PromotionTempOwnership {
    async fn acquire(path: &Path) -> Result<Self, VoomError> {
        loop {
            let file = open_promotion_temp(path)?;
            if let Some(ownership) = Self::lock_and_validate(path, file).await? {
                return Ok(ownership);
            }
        }
    }

    async fn lock_and_validate(path: &Path, file: File) -> Result<Option<Self>, VoomError> {
        let display_path = path.to_path_buf();
        let file = tokio::task::spawn_blocking(move || {
            file.lock()?;
            Ok::<File, std::io::Error>(file)
        })
        .await
        .map_err(|error| {
            VoomError::Internal(format!(
                "join promotion temp lock for {}: {error}",
                display_path.display()
            ))
        })?
        .map_err(|error| {
            VoomError::Config(format!(
                "lock promotion temp {}: {error}",
                display_path.display()
            ))
        })?;
        let path_metadata = match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(VoomError::Config(format!(
                    "stat locked promotion temp {}: {error}",
                    path.display()
                )));
            }
        };
        if !path_metadata.file_type().is_file() {
            return Err(VoomError::Config(format!(
                "promotion temp is not a regular file: {}",
                path.display()
            )));
        }
        let owned_file = file.try_clone().map_err(|error| {
            VoomError::Config(format!(
                "clone owned promotion temp {}: {error}",
                path.display()
            ))
        })?;
        let owned_handle = Handle::from_file(owned_file).map_err(|error| {
            VoomError::Config(format!(
                "identify owned promotion temp {}: {error}",
                path.display()
            ))
        })?;
        let path_handle = Handle::from_path(path).map_err(|error| {
            VoomError::Config(format!(
                "identify promotion temp path {}: {error}",
                path.display()
            ))
        })?;
        Ok((owned_handle == path_handle).then(|| Self {
            path: path.to_path_buf(),
            file,
        }))
    }

    fn restart_file(&self) -> std::io::Result<tokio::fs::File> {
        let file = self.file.try_clone()?;
        file.set_len(0)?;
        Ok(tokio::fs::File::from_std(file))
    }
}

fn open_promotion_temp(path: &Path) -> Result<File, VoomError> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            VoomError::Config(format!("open promotion temp {}: {error}", path.display()))
        })
}

async fn remove_promotion_temp(temp: &Path) -> Result<(), VoomError> {
    match tokio::fs::remove_file(temp).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VoomError::Config(format!(
            "remove owned promotion temp {}: {error}",
            temp.display()
        ))),
    }
}

async fn remove_existing_promotion_temp(temp: &Path) -> Result<(), VoomError> {
    match tokio::fs::symlink_metadata(temp).await {
        Ok(_) => {
            let _ownership = PromotionTempOwnership::acquire(temp).await?;
            remove_promotion_temp(temp).await
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VoomError::Config(format!(
            "stat promotion temp {}: {error}",
            temp.display()
        ))),
    }
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

/// Place a terminal artifact at `dest` across filesystems without exposing a
/// partial file or replacing a concurrent destination.
async fn copy_into_place(
    current: &Path,
    dest: &Path,
    temp: &PromotionTempOwnership,
) -> Result<(), VoomError> {
    let copy_result = async {
        let mut source = tokio::fs::File::open(current).await?;
        let mut target = temp.restart_file()?;
        tokio::io::copy(&mut source, &mut target).await?;
        target.flush().await?;
        target.sync_all().await
    }
    .await;
    if let Err(err) = copy_result {
        return Err(VoomError::Config(format!(
            "copy terminal artifact {} -> {}: {err}",
            current.display(),
            temp.path.display()
        )));
    }
    match tokio::fs::hard_link(&temp.path, dest).await {
        Ok(()) => {
            remove_promoted_source(current).await;
            Ok(())
        }
        Err(error) => match tokio::fs::symlink_metadata(dest).await {
            Ok(dest_meta) => resolve_existing_destination(current, dest, &dest_meta)
                .await
                .map(|_| ()),
            Err(_) => Err(VoomError::Config(format!(
                "place terminal artifact {} -> {} without replacement: {error}",
                temp.path.display(),
                dest.display()
            ))),
        },
    }
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
        source_root: &Path,
        branch_source_dir: Option<&Path>,
    ) -> Result<(), VoomError> {
        let dirs = resolve_promotion_dirs(plan).await;
        if dirs.is_empty() || location_ids.is_empty() {
            return Ok(());
        }
        // Pass 1: collect terminal artifacts and their original source dirs.
        // Primary outputs mirror the owning branch below the job-wide source
        // root. Sidecars prepend the same branch subtree, then retain any
        // operation-relative subtree needed to distinguish multiple outputs.
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
            let current = crate::operation_source::resolve_root_relative_existing_path(
                self,
                "workflow promotion",
                artifact.storage_root_id,
                &artifact.provider_relative_locator,
            )
            .await?;
            let Some((working_dir, output_dir)) = dirs.pair_for(&current) else {
                continue;
            };
            let source_dir = self
                .asset_source_path(artifact.asset_id)
                .await?
                .and_then(|path| path.parent().map(Path::to_path_buf));
            if let Some(dir) = &source_dir {
                source_dirs.push(dir.clone());
            }
            candidates.push((
                artifact,
                current,
                working_dir.to_path_buf(),
                output_dir.to_path_buf(),
                source_dir,
            ));
        }
        let common_root = if source_root.as_os_str().is_empty() {
            longest_common_dir(&source_dirs)
        } else {
            source_root.to_path_buf()
        };
        let auxiliary_dirs = candidates
            .iter()
            .filter_map(|(_, _, working_dir, _, source_dir)| {
                source_dir
                    .as_ref()
                    .filter(|source_dir| source_dir.starts_with(working_dir))
                    .cloned()
            })
            .collect::<Vec<_>>();
        let auxiliary_root = longest_common_dir(&auxiliary_dirs);
        // Pass 2: move each terminal artifact under its branch-scoped mirrored
        // subtree. An unknown artifact source still uses the branch subtree.
        for (artifact, current, working_dir, output_dir, source_dir) in candidates {
            let relative = promotion_relative_dir(
                source_dir.as_deref(),
                &common_root,
                branch_source_dir,
                &auxiliary_root,
                &working_dir,
            );
            let dest_dir = output_dir.join(&relative);
            self.promote_artifact(&artifact, &current, &dest_dir)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn promotion_source_root(
        &self,
        input: &PolicyInputSetDraft,
    ) -> Result<PathBuf, VoomError> {
        let mut source_dirs = Vec::new();
        for snapshot in &input.media_snapshots {
            let TargetRef::FileVersion { id } = snapshot.target else {
                continue;
            };
            let version = self.identity.get_file_version(id).await?.ok_or_else(|| {
                VoomError::NotFound(format!("promotion source file version {id}"))
            })?;
            let source_dir = self
                .asset_source_path(version.file_asset_id)
                .await?
                .and_then(|path| path.parent().map(Path::to_path_buf));
            if let Some(source_dir) = source_dir {
                source_dirs.push(source_dir);
            }
        }
        Ok(longest_common_dir(&source_dirs))
    }

    pub(super) async fn reclaim_superseded_intermediates(
        &self,
        plan: &PromotionPlan,
        file_phases: &[FilePhaseSummary],
    ) -> Result<(), VoomError> {
        let terminal_location = file_phases
            .iter()
            .rev()
            .find_map(|row| row.produced_file_location_id);
        let mut seen = HashSet::new();
        let candidates = self
            .validated_committed_location_ids_for_rows(file_phases)
            .await?
            .into_iter()
            .filter(|location_id| {
                Some(*location_id) != terminal_location && seen.insert(*location_id)
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(());
        }
        let dirs = resolve_promotion_dirs(plan).await;
        for location_id in candidates {
            let Some(location) = self.identity.get_file_location(location_id).await? else {
                continue;
            };
            if location.retired_at.is_some()
                || !matches!(location.address, FileLocationAddress::Rooted { .. })
            {
                continue;
            }
            let canonical = crate::operation_source::resolve_rooted_existing_path(
                self,
                "workflow reclaim",
                &location,
            )
            .await?;
            if dirs.output_for(&canonical).is_none() {
                continue;
            }
            self.reclaim_intermediate_location(&location, &canonical)
                .await?;
        }
        Ok(())
    }

    async fn reclaim_intermediate_location(
        &self,
        location: &voom_store::repo::media::identity::FileLocation,
        path: &Path,
    ) -> Result<(), VoomError> {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(VoomError::Config(format!(
                    "reclaim superseded intermediate {}: {error}",
                    path.display()
                )));
            }
        }
        let mut tx =
            begin_write_first(&self.pool, "promotion: reclaim_intermediate_location").await?;
        self.identity
            .retire_file_location_in_tx(&mut tx, location.id, self.clock().now(), location.epoch)
            .await?;
        commit_tx(tx).await
    }

    pub(super) async fn promotion_location_ids_for_branches(
        &self,
        file_phases: &[FilePhaseSummary],
        branches: &[String],
    ) -> Result<Vec<FileLocationId>, VoomError> {
        let branches = branches.iter().map(String::as_str).collect::<HashSet<_>>();
        let selected_rows = file_phases
            .iter()
            .filter(|row| branches.contains(row.branch_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        let mut location_ids = Vec::new();
        for location_id in self
            .validated_committed_location_ids_for_rows(&selected_rows)
            .await?
        {
            if seen.insert(location_id) {
                location_ids.push(location_id);
            }
        }
        Ok(location_ids)
    }

    /// The directory of an asset's original scanned source: the earliest
    /// `file_version`'s first local-path location. `None` when the asset has no
    /// such location (it then promotes flat). Add-only commits keep the earliest
    /// version pointing at the scanned source even after later versions chain on.
    pub(super) async fn asset_source_path(
        &self,
        asset_id: FileAssetId,
    ) -> Result<Option<PathBuf>, VoomError> {
        let versions = self.identity.list_file_versions_by_asset(asset_id).await?;
        let Some(first) = versions.first() else {
            return Ok(None);
        };
        let locations = self
            .identity
            .list_file_locations_by_version(first.id)
            .await?;
        for location in locations {
            if matches!(location.address, FileLocationAddress::Rooted { .. }) {
                return crate::operation_source::resolve_rooted_existing_path(
                    self,
                    "workflow source",
                    &location,
                )
                .await
                .map(Some);
            }
        }
        Ok(None)
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
        let (target_storage_root_id, target_relative_locator, dest) =
            crate::operation_source::resolve_artifact_target(
                self,
                "workflow promotion",
                artifact.storage_root_id,
                &dest,
            )
            .await?;
        move_terminal_artifact(current, &dest, artifact.location_id).await?;
        let mut tx = begin_write_first(&self.pool, "promotion: promote_artifact").await?;
        self.identity
            .update_file_location_address_in_tx(
                &mut tx,
                artifact.location_id,
                artifact.epoch,
                target_storage_root_id,
                target_relative_locator,
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
