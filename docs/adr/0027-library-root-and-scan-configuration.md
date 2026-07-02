---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0027 — Library and library-root configuration, root-scoped scan, and fail-closed disabled roots

## Context

Nothing existed for library configuration: no `libraries`/`library_roots`
tables, no repository, no CLI. `voom scan` accepted an explicit `--path` only
(`crates/voom-cli/src/commands/media/scan.rs`), and the whole-library policy
input builder (`ControlPlane::create_policy_input_set_from_whole_scan`,
`cases/policy/policy_inputs.rs`) selected **every** live video file-version in
the database because there was no root to scope by — the "DB-per-library"
workaround.

Issue #280 (Sprint 17, T11) asks for durable `Library`/`LibraryRoot` records
with CLI CRUD, `voom scan --root <id>`, root-scoped input building, and
fail-closed behavior for disabled roots, per the design doc §Library
Configuration Model. This is the highest daemon-readiness stake: "a daemon may
not watch a path that lacks a live `LibraryRoot`; if a root is disabled or its
safety/scheduling profile is missing or stale, the daemon records a blocked
issue and does not synthesize defaults."

Two facts shape the design:

- **Sibling subsystems this config references do not exist yet.** The spec lists
  a library's default scheduling policy / safety policy (T12/#281), default
  quality-scoring profile (T16/#285), and external-system links / path-mapping
  references (T15/#284) as fields. None of those tables exist on this branch.
- **`voom scan` already canonicalizes and rejects symlinks per file**
  (`scan/discovery.rs`), and the design doc mandates canonical-path storage so a
  watcher "cannot escape the configured root through path aliases."

## Decision

**1. Two durable tables (migration `0019_libraries.sql`), embedded in the
hand-rolled `MIGRATOR`.** `sqlx::migrate!` is deliberately not used (ADR 0025
context); the new SQL is added as `MIGRATION_0019_SQL` with a `Migration::new(19,
"libraries", …)` entry, the single source of truth for `init()` and
`probe_schema()`.

- `libraries`: `id` (ROWID PK), `slug` (UNIQUE), `display_name`, `media_kind`
  (`CHECK IN ('movie','episode','personal','unknown')`, mirroring the existing
  `media_works.kind` vocabulary), `description` (NULL), `enabled` (`INTEGER 0/1`),
  `created_at`/`updated_at` (TEXT ISO-8601, app-supplied).
- `library_roots`: `id`, `library_id` (`REFERENCES libraries(id) ON DELETE
  CASCADE`), `root_kind` (`CHECK IN ('local_path','shared_mount')`),
  `canonical_path` (UNIQUE across all roots — two roots may not watch one
  physical path), `display_path`, `include_globs`/`exclude_globs`/
  `extension_allowlist` (`TEXT DEFAULT '[]' CHECK(json_valid(...))`), `scan_mode`
  (`CHECK IN ('explicit_only','manual_recursive','watch_enabled')`),
  `symlink_policy` (`CHECK IN ('reject','follow')`), `hidden_file_policy`
  (`CHECK IN ('ignore','include')`), `max_depth` (NULL = unlimited),
  `stability_seconds`/`debounce_seconds` (`INTEGER >= 0`),
  `default_output_root`/`default_staging_root`/`default_backup_root` (NULL),
  `enabled`, `created_at`/`updated_at`. All `STRICT`.

FK enforcement is on (`pool.rs` sets `.foreign_keys(true)`), so deleting a
library cascades its roots.

**2. `SqliteLibraryRepo` in `crates/voom-store/src/repo/library/`** (module +
`libraries.rs` + `library_roots.rs`), following the `backups.rs` conventions
exactly: manual `sqlx::query` + `try_get`, `serialize_json`/`serde_json::from_str`
for the three list columns, `LibraryId`/`LibraryRootId` ROWID newtypes in
`voom-core`, app-supplied ISO-8601 timestamps. Domain structs (`Library`,
`NewLibrary`, `LibraryUpdate`, `LibraryMediaKind`, `LibraryRoot`,
`NewLibraryRoot`, `LibraryRootUpdate`, `LibraryRootKind`, `LibraryScanMode`,
`SymlinkPolicy`, `HiddenFilePolicy`) live in the repo files and are re-exported
from `repo/mod.rs`. CRUD: create / get / get-by-slug / list / update /
set-enabled / delete for libraries, and create / get / list(-by-library) /
update / set-enabled / delete for roots. `update`/`set_enabled`/`delete` return
`NotFound` for a missing id.

**3. `voom library …` and `voom library root …` CLI CRUD.** `Library(LibraryCommand)`
with a nested `Root(LibraryRootCommand)` (the two-level nesting precedent is
`PolicyCommand`). Verbs: `add`, `list`, `show`, `update`, `enable`, `disable`,
`remove` at both levels. Read-side commands open via `ControlPlane::open`
(never migrates, ADR 0003); each emits the standard JSON envelope with a
`#[derive(Serialize)]` DTO (never the store domain type). `library root add`
canonicalizes `--path` with `fs::canonicalize`, stores the result as
`canonical_path` and the operator input as `display_path`, and **rejects a
symlinked ancestor** by comparing `std::path::absolute(input)` to the
canonicalized path (they differ iff a symlink was resolved) — the conservative
alias-safety property the spec calls for.

**4. `voom scan --root <id>` is fail-closed on a disabled root or library.**
`Scan { --path | --root }` are mutually exclusive (clap). The control plane adds
`scan_library_root(root_id) -> Result<RootScanOutcome, ScanCommandError>`:
- root missing → `NotFound` (envelope `NOT_FOUND`, exit 2);
- root disabled **or** parent library disabled → `Ok(RootScanOutcome::Blocked{
  library_id, library_root_id, reason, canonical_path })` — **no discovery, no
  worker launch, no persistence.** The CLI renders it as an error envelope with a
  new `ErrorCode::Blocked` (`"BLOCKED"`) and a structured `data` payload, exit 2.
  This is the fail-closed contract: a disabled root produces a block, not a scan;
- enabled → discovery runs against the root's `canonical_path`, honoring the
  root's `extension_allowlist` (empty ⇒ the built-in `SUPPORTED_EXTENSIONS`),
  then the existing scan pipeline persists as usual.

`ScanPathInput` gains `extension_allowlist: Vec<String>` (explicit-path scan
passes empty = unchanged behavior); `discovery::discover_path` takes the
allowlist and the primary-media extension test consults it.

**5. `ErrorCode::Blocked` (`"BLOCKED"`)** is added to `voom-core` (enum + `ALL`
+ `as_str` + the exhaustive `error_test` list). It is a standalone wire code with
no `VoomError` variant: the blocked outcome is an `Ok` domain result carrying
structured fields, not an error propagated up the stack. The daemon (Sprint 18)
reuses this code.

**6. Root-scoped policy input building** adds `ControlPlane::
create_policy_input_set_from_root(RootScopedScanInput { slug, library_root_id })`
and a `voom policy input create-from-scan --root <id>` mode (mutually exclusive
with `--all` and the single-file args). It reuses the whole-scan selection but
keeps only file-versions whose live location `canonical_path` is a descendant of
the root's `canonical_path`. This directly replaces the "select every video"
workaround and subsumes the deferred `--under <path>` refinement (a root **is** a
canonical path prefix).

## Consequences

- The daemon-readiness invariant "no watch of a path lacking a live LibraryRoot"
  has its durable substrate and a tested fail-closed refusal for disabled roots.
- **Config the watcher will consume but this CLI does not yet apply:**
  `include_globs`/`exclude_globs`, `stability_seconds`/`debounce_seconds`,
  `symlink_policy` beyond the existing per-file symlink rejection,
  `hidden_file_policy`, and `max_depth` are stored durable config, read by the
  Sprint 18 watcher. Storing-ahead is deliberate (the field set is the T11
  deliverable); applying them in discovery is Sprint 18 scope. `extension_allowlist`
  **is** applied by `scan --root`.
- **Cross-issue linkages deferred to their owning issues** rather than added as
  orphan nullable columns now (no speculative schema): default scheduling/safety
  policy (#281), default quality-scoring profile (#285), external-system links and
  path-mapping references (#284), and `last_scan_session_id` (no `scan_sessions`
  table until Sprint 18). Each is an additive column in its owning migration.
- Migration `0019` and ADR `0027` are cross-agent-assigned (concurrent #281 owns
  `0020`/`0026`). The hand-rolled `MIGRATOR` tolerates the numbering.
- No new events and no durable `Issue` row for the block (see rejected list).

## Considered & rejected

- **Persist a durable `library_root_blocked` `Issue` for the disabled-root
  block.** Rejected for this issue. The `issues.kind` CHECK is a closed vocabulary
  (migration 0004); adding a value requires a full table rebuild, and migrations
  run inside a transaction (`MIGRATOR { no_tx: false }`) where `PRAGMA
  foreign_keys` cannot be toggled — so rebuilding `issues` while `issue_links`
  holds an `ON DELETE CASCADE` FK to it is unsafe (the drop would cascade-delete
  the links). There is no daemon consumer of the issue yet. The fail-closed
  **safety** property (refuse to scan, exit non-zero, structured `BLOCKED`) is
  fully delivered and tested; the durable blocked-`Issue` is a Sprint 18
  follow-up where the `issues.kind` evolution can be done deliberately.
- **Tag scanned rows with `library_root_id` and scope input building by FK.**
  Rejected: it mutates the hot `file_locations`/`file_versions` identity tables
  for every scan and couples identity to config. Canonical-path prefix scoping
  needs no schema change to identity and matches the spec's "stores canonical
  paths so a watcher cannot escape the root" framing.
- **Reuse an existing `ErrorCode` (`CONFLICT`, `CONFIG_INVALID`) for the block.**
  Rejected: `CONFLICT` is documented as an optimistic-lock "re-read and retry"
  signal (retry never clears a disabled root); `CONFIG_INVALID` means malformed
  config, but a disabled root is valid config in an intentional state. A dedicated
  `BLOCKED` code is honest and reusable by the daemon.
- **Add a glob engine (`globset`) and apply include/exclude globs in `scan
  --root` now.** Rejected: a new dependency (attack surface) for behavior whose
  real consumer is the Sprint 18 watcher. Globs are stored config now; canonical
  path + extension allowlist already scope the scan.
- **Make `media_kind`/`root_kind`/… free TEXT.** Rejected: the codebase
  convention is `TEXT` + `CHECK` enums (backups, nodes, video_profiles); CHECK
  constraints catch typos at write time.
