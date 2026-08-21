# Release Process

VOOM follows the bump → tag → bump cadence so `main` always carries a `-dev`
SemVer suffix between releases. The release process is run from `main`.

## Steps

1. **Bump to the release version.** On `main`, edit the workspace
   `Cargo.toml`'s `[workspace.package] version` from `0.X.Y-dev` to `0.X.Y`.
   Run `cargo build` to refresh `Cargo.lock`, then commit:

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "Release: 0.X.Y"
   ```

2. **Tag the release commit.**

   ```bash
   git tag -a v0.X.Y -m "voom 0.X.Y"
   git push origin v0.X.Y
   ```

   The `release.yml` workflow builds linux-x64, linux-arm64, and macos-arm64
   binaries on tag push and uploads them to a draft GitHub Release.

3. **Bump to the next dev version.** Immediately on `main`, bump
   `[workspace.package] version` from `0.X.Y` → `0.X.(Y+1)-dev` (patch) or
   `0.(X+1).0-dev` (minor). Run `cargo build`, then commit:

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "Begin 0.X.(Y+1)-dev"
   ```

4. **Publish the GitHub Release.** Edit the draft, paste a changelog (or
   `git log v0.X.(Y-1)..v0.X.Y --oneline`), and publish. The release artifacts
   self-report version as `0.X.Y+<tag-sha>`.

   Build-script provenance smoke check (run once per release candidate): build
   the binary, commit an empty change (`git commit --allow-empty`), build
   again, run `voom version`, and confirm the reported SHA advanced.

   Dirty-flag smoke check (run once per release candidate against a debug
   build): from a clean tree, `cargo build -p voom-cli` and confirm
   `voom version` reports `dirty: false`. Edit any tracked file
   (`touch -m crates/voom-cli/src/main.rs` doesn't qualify — the file must
   actually change), rebuild, and confirm `voom version` now reports
   `dirty: true`. Debug builds re-probe via `git status --porcelain` at
   runtime so they reflect tree state regardless of build-script caching.
   Release binaries trust the compile-time flag captured by `build.rs` in CI.

## Payload compatibility (audit M4, ADR 0013)

Durable JSON payloads deny unknown fields, so cross-version reads fail loudly
rather than silently dropping a field:

- **Upgrade (binary before DB):** a new binary reading old rows tolerates absent
  optional fields (additive evolution) and rejects nothing it added.
- **Breaking change (rename/remove/retype a field):** roll the new binary out and
  do not roll it back while old-shape rows may still exist.
- **Rollback across a payload change is not transparent:** the older binary will
  intentionally reject rows the newer binary wrote. A rollback across such a
  change requires restoring the pre-upgrade database snapshot
  (see `docs/runbooks/migration-rollback.md`).

`tickets.payload` carries a breaking change under this contract (ADR 0069): a
byte-touching workflow ticket now requires a `declared_artifact_access` declaration, and a
row written before that binary no longer decodes. No backfill is possible — the
declaration names a storage root and location the old row never recorded.

**Upgrade.** Quiesce workflow ticket creation, then fail or delete every unfinished
workflow ticket whose kind names a byte-touching operation, then roll the new binary
out. No shipped command does either half yet — `voom ticket` offers only `list` and
`show`, and `voom job cancel` takes non-byte-touching tickets with the job — so both
steps currently require direct SQL against `tickets`. Issue #480 tracks shipping the
control. Quiescing first is load-bearing: the binary performing the drain is the one still
rendering old-shape tickets, so draining against a live writer leaves everything
rendered between the drain and the swap undecodable. A ticket rendered against
node-owned roots (ADR 0055) references a live rooted location and dispatches normally
today, so skipping the step loses completable work rather than delaying it.

**Which kinds.** Twelve operations are byte-touching (`OperationKind::is_byte_touching`).
The drain predicate is every one of them under the workflow namespace:

```sql
WHERE state NOT IN ('succeeded', 'failed', 'cancelled')
  AND kind IN (
    'synthetic.workflow.operation.scan_library',
    'synthetic.workflow.operation.probe_file',
    'synthetic.workflow.operation.hash_file',
    'synthetic.workflow.operation.back_up_file',
    'synthetic.workflow.operation.remux',
    'synthetic.workflow.operation.transcode_video',
    'synthetic.workflow.operation.transcode_audio',
    'synthetic.workflow.operation.edit_tracks',
    'synthetic.workflow.operation.extract_audio',
    'synthetic.workflow.operation.verify_artifact',
    'synthetic.workflow.operation.commit_artifact',
    'synthetic.workflow.operation.delete_artifact'
  )
```

Confirm the list against `is_byte_touching` before running it — nothing enforces the
agreement, and issue #512 tracks a guardrail that would. **The seven-operation list further
down, under the rights-table procedure, is the #484 output-producing set and is not the
drain set.** Draining only those seven leaves `scan_library`, `probe_file`, `hash_file`,
`verify_artifact`, and `delete_artifact` behind; `verify_artifact` has a production
producer today, so that is not a theoretical omission.

**What skipping the drain actually looks like.** The first undrained byte-touching ticket
the dispatch loop polls aborts the entire workflow run it belongs to
(`executor/expansion.rs::ready_workflow_tickets`), naming the ticket and the decode error.
Well-formed tickets in the same workflow that had not yet dispatched do not run, and the
expansion children they would have produced are never created.

The signal is therefore a failed run, not a per-ticket `terminal_failure` issue (ADR
0018): such a ticket never leases, so it never reaches the terminal transition that would
open one. Issue #486 tracks giving it a lease-free terminal failure path, which is what
would let the run skip it and carry on; until that lands, treat the drain as required
rather than advisable, and size the window on the assumption that a missed ticket costs
its whole workflow. Fold the drain into the ADR 0055 flag-day root-assignment and rescan
procedure, which such a deployment already owes.

The same drain covers a second break in the same release, and must be stated to,
because the two have different symptoms. Ticket-kind normalization became strict
(ADR 0069): a kind of the form `synthetic.workflow.operation.<suffix>` whose suffix
is not a known operation is now rejected at lease acquisition, so such a ticket is
permanently unleasable rather than merely undecodable. Drain those alongside the
byte-touching ones.

Two capacity effects follow for such a kind, and they are not the same one.

- **Audit `worker_grants` keys.** `max_parallel` is looked up by the kind as stored, so a
  grant entry keyed on the bare `<suffix>` no longer matches. That worker falls back to
  its `*` limit, or to 1 when it has none — which can raise *or* lower its effective
  limit depending on the grant. This holds on every deployment carrying such a grant.
- **A held lease usually still counts.** Capacity computed under the same namespaced kind
  binds that kind on both sides and counts the lease. It goes uncounted only where a
  separate custom-local ticket kind exactly equal to the suffix also exists, so that
  capacity is computed for the bare token; that worker can then be over-subscribed until
  the leases drain or expire. Check whether any such kind exists before watching for it.

All of these end once the drain completes, and none can recur afterwards, since no
renderer emits an unknown suffix.

**This drain is the standing procedure for any change to the rights table, not a
one-off for this release.** `validate_artifact_access` compares a stored declaration
against the whole entry list `declaration_for` computes from the operation→rights
mapping compiled into the binary reading it, and the payload carries no version
marker. So editing that mapping invalidates every in-flight persisted ticket for the
affected operations, with the same symptom as above: an undecodable row, no possible
backfill, and a drain required.

That edit is already scheduled. Closing #484 adds a second `storage_root` entry naming
the true destination root on the seven output-producing operations — `remux`,
`transcode_video`, `transcode_audio`, `extract_audio`, `edit_tracks`, `back_up_file`,
and `commit_artifact` — which invalidates every such ticket written by this release.
Adding an entry is cheap in the *vocabulary* and not cheap in the *stored rows*,
because the equality check binds the list rather than any single entry. Plan the same
quiesce-and-drain into that deployment.

The alternative — a declaration schema version plus a reader tolerant of older
mappings — is deliberately not taken. It is more machinery than the risk warrants and
would re-introduce the second accepted wire format that issue #475's sixth acceptance
criterion forbids. The cost is paid at deployment instead, which is why it is written
down here.

**Rollback.** Restoring the pre-upgrade snapshot, as the general rule above says, is
always safe and reverts everything. This change also permits a narrower option, because
its new shape is confined to `tickets.payload` for every production row: quiesce, then fail or delete the byte-touching
tickets the new binary wrote, before the older binary reads them. That preserves every
other row the new binary committed, at the cost of leaving those tickets' workflows
incomplete. Take the snapshot if you want a clean revert of all of it.

`policy_versions.compiled_json` follows this contract. Existing compiled policy
versions remain readable by a newer binary, including documented legacy wire
forms. Add a field only as optional/defaulted, deploy the reader before any
writer can persist it, and take the pre-upgrade snapshot before that write path
is enabled. If rollback is required after the newer field has been written,
restore that snapshot; an older binary must reject the newer row rather than
silently reinterpret the policy.

## Never

- Amend tags after creation.
- Force-push to `main`.
- Skip the post-release bump commit (otherwise the next `main` build reports
  the released version, breaking `--release` provenance).
