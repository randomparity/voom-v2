# Spec: Cross-filesystem promotion resume recovery (issue #257)

Status: draft
Date: 2026-07-01
Issue: #257
Base ref audited: `78ecdc1`

## Context

`move_terminal_artifact`
(`crates/voom-control-plane/src/workflow/coordinator/promotion.rs:109`) places a
terminal (chain-tip) artifact into its promoted destination, add-only. Its caller
`promote_artifact` (`promotion.rs:260`) then repoints the artifact's durable
`file_location` value at the promoted path inside a DB transaction. The **DB
repoint is the promotion's commit point**: `working_dir_artifacts` only returns a
location whose value still lives under a working dir, so once the value is
repointed a resume skips the artifact.

On a same-filesystem promotion `rename` is atomic and there is no window. On a
**cross-filesystem** promotion `rename` fails with `EXDEV` and the code falls back
to `copy` then `remove_file(current)` (`promotion.rs:132`, `:139`). These are two
non-atomic filesystem steps that both complete **before** the caller repoints the
DB.

### Failure scenario (finding M-A)

Model the cross-FS fallback as a state machine. `L` is the durable location value.

| State | `current` on disk | `dest` on disk | `L` points at | Reached by |
|-------|-------------------|----------------|---------------|------------|
| S0 | yes | no  | `current` | start |
| S1 | yes | yes | `current` | after `copy` |
| S2 | no  | yes | `current` | after `remove_file` |
| S3 | no  | yes | `dest`    | after DB repoint (**done**) |

A run/resume calls `move_terminal_artifact` whenever `L` still points under a
working dir — i.e. in S0, S1, and S2. Today:

- **S0** → `dest` absent → `rename`, or `copy` + `remove` → returns `dest`, caller
  repoints. Correct.
- **S2** → `dest` present, `current` gone → the already-moved branch
  (`promotion.rs:112`) returns `dest`, caller repoints. Correct (an earlier run
  promoted the bytes and crashed before repointing).
- **S1** → `dest` present **and** `current` present → the already-moved branch
  requires `current` to be *gone*, so it is skipped and the function returns the
  hard collision error `"promotion destination already exists"`
  (`promotion.rs:115`). **The DB is never repointed, so `L` stays at `current` and
  every subsequent resume re-enters S1 and re-errors.** The workflow is wedged
  permanently until a human deletes `dest` or `current` by hand.

S1 is reached whenever `copy` succeeds but `remove_file(current)` fails — source
turned read-only, transient `EIO`, permissions — or whenever the process is killed
after the copy and before the DB repoint commits. No bytes are lost (the artifact
is intact at `dest`); the run cannot make progress.

## Decision

Make the cross-FS promotion **idempotently resumable** by (a) recognising a
byte-identical resumed copy in the collision branch and (b) treating source
removal as best-effort cleanup rather than part of the promotion commit. Both
changes are contained to `move_terminal_artifact` and two small private helpers in
`promotion.rs`; the caller's DB-repoint transaction is unchanged.

### D1 — Content-verified already-moved recovery (fixes S1)

When `dest` already exists and `current` **also** exists, distinguish a resumed
copy (S1) from a genuine foreign collision:

- If `dest` is a regular file **and** its bytes are byte-for-byte equal to
  `current` → this is a resumed cross-FS copy. Remove `current` best-effort (D2)
  and return `dest` so the caller repoints. Forward progress guaranteed.
- Otherwise (`dest` is a directory/symlink/other, or its bytes differ) → a
  genuine collision with a foreign file. Return the existing
  `"promotion destination already exists"` error unchanged.

Byte-equality is the check, not existence or size, because the add-only contract
must never repoint a location at, and remove the source of, a *different* file —
that would be silent data loss. `WorkingDirArtifact` carries no durable
size/digest fact (`finalize.rs:64` — only `location_id`, `asset_id`, `value`,
`epoch`), so the ground truth is a direct comparison of `current` against `dest`;
`dest` was copied *from* `current`, so in S1 they are identical by construction.

The comparison is size-first (cheap reject), then a chunked streaming byte compare
that bounds memory (media artifacts can be multi-GB — never load either file
whole). A read/stat failure during the comparison propagates as a descriptive
`VoomError` (fail loud, AGENTS.md Rule 12) rather than guessing a verdict.

The S2 branch (`dest` present, `current` gone) is unchanged.

### D2 — Best-effort source removal after a successful copy

After a successful cross-FS `copy`, the bytes are durably at `dest`; removing
`current` is cleanup, not part of the commit. Make `remove_file(current)`
best-effort: on failure, log a `tracing::warn!` naming the orphaned source and
proceed to return `dest` so the caller repoints. This lets the **first** run
complete in S1 instead of erroring, and it is applied identically in the D1
recovery path so that a source that can never be removed still cannot wedge the
workflow (the DB repoints to `dest`; the orphaned `current` is left in the
ephemeral working dir and no longer resolved).

This is a deliberate failure-contract change: **the promotion commit point is the
durable DB repoint; filesystem source removal is best-effort cleanup.** A `copy`
failure still errors (bytes are *not* safe at `dest`); only the post-copy source
removal is downgraded.

## Decisions (process)

- **No new ADR.** This refines the existing add-only / no-replace promotion
  contract (documented on `promote_terminal_artifacts`, mirroring the commit
  recovery contract) and applies the codebase's durability-first model (durable
  pointer is the commit point; FS is reconciled on resume — ADR 0001, ADR 0009).
  It introduces no new layer boundary, interface split, concurrency invariant, or
  migration. It is directly analogous to the recovery-classification fix in issue
  #261 Item 1 / commit `c05185f` ("distinguish absent vs unstattable recovery
  target"), which was scoped as a spec item with no ADR. The considered-and-
  rejected alternatives are captured below.
- **Direct implementation, not subagent fan-out.** The change is one function plus
  two helpers and its sibling unit tests — tightly coupled, single file. Direct
  TDD in one session is the right execution mode.

## Considered & rejected

- **Reorder: repoint the DB before removing the source (issue Option A).** Move
  the `remove_file` out of `move_terminal_artifact` and have `promote_artifact`
  do copy → repoint → best-effort remove. Rejected as the primary mechanism: it
  splits the DB transaction across the filesystem steps in the caller (larger
  blast radius on a churn-heavy coordinator path) and *still* does not cover a
  process kill between `copy` and the repoint commit — that lands in S1 and needs
  the same content-verified recovery anyway. D1+D2 subsume Option A's benefit
  (first-run completion) with a smaller, self-contained diff and cover the crash
  window Option A leaves open. The best-effort-remove *idea* from Option A is
  adopted as D2.
- **Size-only or existence-only "facts match".** Treat `dest` as already-moved
  when it merely exists, or when its size equals `current`. Rejected: a foreign
  file of equal (or any) size at `dest` would be accepted, the DB repointed at it,
  and the real artifact's source removed — silent data loss / corruption, exactly
  what the add-only no-replace contract exists to prevent. Byte-equality is the
  minimum safe check.
- **Write a durable "promoted" marker before removing the source.** Rejected: adds
  a new on-disk artifact with its own creation/cleanup/idempotency surface and a
  second thing to reconcile on resume, to encode a fact (`dest` holds the promoted
  bytes) that the bytes themselves already prove. Content comparison needs no new
  durable state.
- **Keep source removal a hard error in the recovery path.** Rejected: if the
  removal keeps failing, `move_terminal_artifact` keeps returning `Err`, the DB is
  never repointed, and the workflow re-wedges — the original bug. Forward progress
  requires the recovery path to repoint regardless of whether the orphan can be
  cleaned up.

## Edge cases

- **S1, identical bytes** (`dest` regular file == `current`) → best-effort remove
  `current`, return `dest`, caller repoints. Primary new-behavior path.
- **S1, differing bytes** (`dest` regular file != `current`) → genuine collision,
  `"promotion destination already exists"`. Preserves existing behavior and the
  two integration tests that assert it
  (`mod_test.rs:824`, `audio_transcode_flow.rs:172`), both of which seed a foreign
  `dest` (`b"existing"`) unequal to the real artifact.
- **`dest` is a directory or symlink, `current` present** → not a regular file, so
  not a resumed copy → genuine collision error (never read-through / clobber a
  non-regular path).
- **S2** (`dest` present, `current` gone) → already-moved, return `dest`
  (unchanged).
- **S0** (`dest` absent) → `rename`, else `copy` + best-effort remove (unchanged
  placement, D2 removal semantics).
- **Content-compare read/stat failure** (e.g. `dest` unreadable) → propagate a
  descriptive `VoomError`, do not silently pick a verdict.
- **Source-removal failure in first-run copy or recovery** → `warn!` and proceed;
  orphaned `current` left in the ephemeral working dir, DB repointed to `dest`.

## Acceptance criteria

Sibling unit tests on `move_terminal_artifact` (`promotion_test.rs` via `#[path]`,
per ADR 0004), all on a single tmpdir so no real cross-FS mount is required — the
S1 state is constructed directly by pre-creating `dest`:

- **Resumed copy recovers (the fix).** `current` and `dest` both present with
  identical bytes → returns `Ok(dest)`, `current` is removed, `dest` bytes
  unchanged. With today's code this returns the collision `Err` (guards the
  regression).
- **Genuine collision still fails.** `current` and `dest` present with *different*
  bytes → returns `Err` containing `"promotion destination already exists"`; both
  files left untouched.
- **Non-regular destination fails.** `dest` is a directory (or symlink to one),
  `current` present → returns the collision `Err`; nothing removed.
- **Already-moved (S2) unchanged.** `dest` present, `current` absent → returns
  `Ok(dest)`.
- **Normal move (S0) unchanged.** `current` present, `dest` absent → returns
  `Ok(dest)`, `current` gone, `dest` holds the bytes.
- Existing `just ci` suite stays green — in particular the two collision
  integration tests above.
