# ADR 0097 — pre-promotion address containment

Goal: land one accepted Architecture Decision Record answering issue #615, with
its single index row, on a green `just ci`.

Architecture: two Markdown files plus two design artifacts.
`docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md` is the
record; `docs/adr/README.md` gains one row for it. The record decides that a
pre-promotion `.committed` address is contained by the registered storage root
resolved for `DestinationRole::Staging`, that ADR 0074's shared-owner-node rule is
the whole staging↔output relationship, what #616 enforces at configuration time,
and that an unconfigured output root fails closed. No Rust, no migration, no
`justfile` change; nothing here ships behavior.

Tech stack: Markdown records under `docs/adr/`, gated by
`scripts/check-adr-index.sh` (invoked as `just check-adr-index`, which `just ci`
runs and which the `prek` pre-commit hook also runs on staged `docs/adr/` changes).

Expected implementation size: 330–350 changed lines (M) — derived from the file map below: one 336-line record, two ~16-line appended back-reference sections, and one index row.

That line has been revised on every commit of this branch (200–210 → 255–270 →
295–315 → this one), tracking a record that grew 202 → 260 → 302 → 315 under three
rounds of review. Recorded here rather than smoothed over, because an estimate
re-derived from the artifact's current length can never fail and so bounds
nothing. The growth is accounted for: rounds two and three cut four passages the
review named as defending rather than deciding, and added more than that back in
corrections the review's blocking findings required — the bidirectional
owner-assignment rule, the amendment-visibility residual, and the `CONFIG_INVALID`
correction. The four decisions themselves have not grown since the first draft.

## Global Constraints

- **ADR number is 0097, assigned.** Do not take "next free". The index ends at
  0096 and no `docs/adr/0097-*` exists.
- **Record format follows `docs/adr/0096-run-scheduled-resource-cells-on-isolated-runners.md`**,
  the most recent record: H1 `# NNNN — Title`, then `## Status` (bare `Accepted`
  on its own line, no date — this repo installs no `.github/scripts/check-records.sh`,
  only `scripts/check-adr-index.sh`), `## Context`, `## Decision`,
  `## Consequences`, `## Considered & rejected`.
- **Every `Considered & rejected` bullet opens its ground with `verified:` or
  `judgment:`.**
- **Index row format is exactly** `| [NNNN](NNNN-slug.md) | Title |`.
  `scripts/check-adr-index.sh:21` matches the literal prefix `| [NNNN](filename) |`
  at column one and requires exactly one such line. The table has two columns,
  `| ADR | Title |` — **there is no Status column**, so nothing couples the row to
  the record's `## Status` value.
- **Touch one row only.** Do not reflow, re-sort, or re-align the table; adjacent
  edits serialize parallel ADR PRs on merge conflicts.
- **File scope is `docs/adr/0097-*.md`, `docs/adr/README.md`, and the two design
  artifacts below.** Do not edit Rust sources, migrations, or the `justfile`.
  `crates/voom-control-plane/src/operation_source.rs` and
  `operation_source_test.rs` are owned by parallel issue #617; report a nit there
  rather than fixing it.
- **The record must not authorize extending** the control plane's transitional
  filesystem-promotion path (ADR 0050, pending #416–#425), and must not authorize
  the resolver change it mandates — it names that gap instead.
- **Shell: the loop variable in Task 1 step 4 must not be named `path`.** `path`
  is a zsh special tied to `$PATH`; assigning it inside the loop makes every
  subsequent command in that loop exit 127, and the check then "fails" for a
  reason that has nothing to do with citations.
- **Guardrail:** `just ci`, run bare — no pipe, no redirect, no `|| true`.

## File map

| Path | Created / changed | Answerable for |
|---|---|---|
| `docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md` | created | The decision, its context, consequences, and rejected alternatives |
| `docs/adr/README.md` | changed (one row appended) | The index entry `check-adr-index` requires |
| `docs/adr/0055-node-owned-roots-and-relative-file-locations.md` | changed (section appended) | `## Later decision:` back-reference to ADR 0097 |
| `docs/adr/0069-byte-work-tickets-declare-canonical-artifact-access.md` | changed (section appended) | `## Later decision:` back-reference to ADR 0097 |
| `docs/workflow/specs/2026-09-05-issue-615-staging-containment-design.md` | created | The design record: problem, decisions, scope, verification contracts |
| `docs/workflow/plans/2026-09-05-adr-0097-staging-containment.md` | created | This plan |

The two design artifacts are excluded from the implementation-size estimate above,
which measures the implementation the plan produces.

## Task 1 — write the record

Creates `docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md`.
Modifies nothing. This is the whole substance of the change; Task 2 is the gate's
bookkeeping.

**Interfaces.** Consumes nothing from an earlier task. Task 2 relies on this
task's exact filename, `0097-pre-promotion-addresses-contained-by-their-own-root.md`,
and its exact H1 title text, `Pre-promotion artifact addresses are contained by
their own storage root`, to build the index row.

**Verification.**

- Contract: every `file:line` the record cites resolves to a line containing a
  distinctive token from the sentence citing it, at the branch base.
  `Mode: focused-test` — step 4's command is the observable check. Expected red,
  demonstrated with a deliberately wrong pair
  (`crates/voom-cli/tests/support/voom_cli.rs|49|default_output_root_id`):
  exit 1 and
  `CITATION FAILED: crates/voom-cli/tests/support/voom_cli.rs:49 lacks default_output_root_id`.
  Green: exit 0 and `citations OK`. This is the check that catches the class of
  defect a record like this is most exposed to — the first draft misread that
  exact line and inverted a Consequences bullet on the strength of it.
- Contract: the record's decision content. `Mode: task-test-not-applicable` — the
  contract is a decision expressed in prose, and no executable consumer reads it.
  Nothing in `just ci` reads a record's body, and a test asserting prose wording
  would assert the words, not the decision.

**Steps.**

1. Write the record with the five sections named in Global Constraints, citing
   ADR 0055, ADR 0074's owner-node requirement, ADR 0075, and ADR 0050's
   byte-ownership and transitional-path constraints.
2. State the rule #616 must enforce concretely enough to implement without a
   second decision, and decide the unconfigured-output-root fallback. Say which
   reading of "the staging root" governs — the registered root, not the directory
   prefix — because the two readings give opposite verdicts on the transitional
   `--staging-root` path.
3. In `## Consequences`, say what #616 and #618 are each unblocked to do,
   reference #623 with the layout the decision implies, and name #625 as the
   owner of the resolver change.
4. Assert every citation. First confirm the cited sources are unchanged from the
   base, then check each line. Run from the worktree root:

   ```sh
   git diff --quiet main -- crates/ scripts/ justfile \
     || { echo "cited sources differ from base"; exit 1; }
   # ADR 0055 and 0069 are amended by this change, so they are not unchanged from
   # base. Assert instead that both diffs are append-only: cited line numbers sit
   # before the appended sections, so they still resolve.
   for adr in docs/adr/0055-*.md docs/adr/0069-*.md; do
     dels=$(git diff --numstat main -- "$adr" | cut -f2)
     [ "${dels:-0}" = "0" ] || { echo "not append-only: $adr deleted $dels line(s)"; exit 1; }
   done
   while IFS='|' read -r f line token; do
     [ -z "$f" ] && continue
     sed -n "${line}p" "$f" | grep -qF -- "$token" \
       || { printf 'CITATION FAILED: %s:%s lacks %s\n' "$f" "$line" "$token"; exit 1; }
   done <<'EOF'
   crates/voom-control-plane/src/operation_source.rs|158|resolve_artifact_target
   crates/voom-control-plane/src/operation_source.rs|187|default_output_root_id
   crates/voom-control-plane/src/operation_source.rs|196|library_id != source_library_id
   crates/voom-control-plane/src/operation_source.rs|275|owner != local
   crates/voom-control-plane/src/operation_source.rs|290|require_contained
   crates/voom-control-plane/src/workflow/plan/envelope.rs|207|destination_root
   crates/voom-control-plane/src/workflow/coordinator/promotion.rs|724|promote_artifact
   crates/voom-control-plane/src/workflow/coordinator/promotion.rs|742|artifact.storage_root_id
   crates/voom-control-plane/src/artifact/commit/prepare.rs|233|source.source_storage_root_id
   crates/voom-control-plane/src/cases/policy/compliance.rs|696|COMMITTED_SUBDIR
   crates/voom-control-plane/src/artifact/commit/mod_test.rs|1467|default_output_root_id
   crates/voom-control-plane/src/operation_source_test.rs|67|default_output_root_id
   crates/voom-control-plane/src/operation_source_test.rs|181|CONFIG_INVALID
   crates/voom-core/src/error.rs|183|CONFIG_INVALID
   crates/voom-core/src/error.rs|371|ErrorCode::ConfigInvalid
   crates/voom-cli/tests/support/owner_node.rs|492|root_path
   crates/voom-cli/tests/support/owner_node.rs|581|.committed
   crates/voom-cli/tests/support/voom_cli.rs|49|default_staging_root_id = id
   crates/voom-store/src/repo/library/library_roots.rs|309|require_default_ids_in_library
   crates/voom-store/src/repo/library/library_roots.rs|361|assign_library_root_owner_in_tx
   crates/voom-store/src/repo/library/library_roots.rs|379|owner_node_id = ?
   crates/voom-store/src/repo/library/library_roots.rs|602|require_default_ids_in_library
   crates/voom-store/src/repo/library/library_roots.rs|614|require_default_ids_in_library
   crates/voom-store/src/repo/library/library_roots_test.rs|471|default_output_root_id
   crates/voom-cli/tests/chaos_librarian_e2e.rs|247|staging flag mirrors
   scripts/chaos-e2e-local.sh|11|CHAOS_EXECUTE_POLICY:-0
   scripts/chaos-e2e-local.sh|49|library_dir="$run_dir/library"
   scripts/chaos-e2e-local.sh|113|--path "$library_dir"
   scripts/chaos-e2e-local.sh|160|execute_policy" = "execute"
   scripts/chaos-e2e-local.sh|166|--staging-root
   scripts/check-adr-index.sh|21|row_prefix
   docs/adr/0069-byte-work-tickets-declare-canonical-artifact-access.md|212|## Consequences
   crates/voom-store/src/repo/library/library_roots.rs|438|retire_library_root_in_tx
   justfile|249|chaos-e2e-ci:
   docs/adr/0055-node-owned-roots-and-relative-file-locations.md|103|falling back to the same
   docs/adr/0069-byte-work-tickets-declare-canonical-artifact-access.md|256|unwrap_or(source)
   docs/adr/0019-commit-gate-lineage-commit-check.md|99|## Later decision
   docs/adr/0025-backup-worker-and-backup-before-mutation-gate.md|154|## Later decision
   docs/adr/0027-library-root-and-scan-configuration.md|194|## Later decision
   docs/adr/0034-policy-tool-requirements-use-worker-capabilities.md|202|## Later decision
   EOF
   echo "citations OK"
   ```

   Expect `citations OK` and exit 0. Two properties of this check are earned
   rather than assumed, and both were established by it failing first:

   - **It bites.** Substitute the pair
     `crates/voom-cli/tests/support/voom_cli.rs|49|default_output_root_id` and
     confirm exit 1 with
     `CITATION FAILED: crates/voom-cli/tests/support/voom_cli.rs:49 lacks default_output_root_id`.
     That is the exact misreading the first draft of the record shipped.
   - **The token must come from the asserted fact, not merely from the cited
     line.** The first version of this table paired `chaos-e2e-local.sh:49` with
     the token `library_dir` and passed, while the sentence citing that line
     claimed the harness "passes `$workdir/staging-<checkpoint>`" — a claim living
     on line 166. A token that any nearby line would satisfy proves nothing. Pick
     the token the record's sentence actually asserts.

   Run it as a script (`bash /tmp/check-citations.sh`), not pasted into an
   interactive shell: the here-doc keeps `exit 1` in the current shell, so a
   failure would close that shell.
5. Confirm the `Considered & rejected` section carries one evidence tag per
   bullet. Command:

   ```sh
   awk '/^## Considered & rejected/{s=1; next}
        s && /^- \*\*/{b++}
        s && /verified:|judgment:/{t++}
        END{printf "bullets=%d tags=%d\n", b, t; exit (b>0 && b==t) ? 0 : 1}' \
     docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md
   ```

   Expect `bullets=8 tags=8` and exit 0. This proves count agreement within the
   section, not per-bullet placement; read the section once to confirm each tag
   sits on its own bullet.

**Acceptance criteria.**

- The file exists at the exact path above with all five sections non-empty.
- The H1 number `0097` matches the filename.
- Step 4 exits 0 with `citations OK`, and exits 1 on a deliberately wrong pair.
- Step 5 reports equal bullet and tag counts.
- The record authorizes no extension of the transitional promotion path and no
  resolver change.

## Task 2 — add the index row

Modifies `docs/adr/README.md`. Creates nothing. Consumes Task 1's exact filename
and H1 title.

**Verification.** Contract: `docs/adr/README.md` carries exactly one row for
`0097-pre-promotion-addresses-contained-by-their-own-root.md`.
`Mode: focused-test` — `just check-adr-index` is the observable check.
Expected red before the row exists: exit 1 with
`check-adr-index: 0097-pre-promotion-addresses-contained-by-their-own-root.md is missing from docs/adr/README.md`
and `check-adr-index: 1 violation(s).` Green: exit 0 and `check-adr-index: OK`.

**Steps.**

1. Run `just check-adr-index` and confirm the red output above.
2. Append this line after the `0096` row, at the end of the table:
   `| [0097](0097-pre-promotion-addresses-contained-by-their-own-root.md) | Pre-promotion artifact addresses are contained by their own storage root |`
3. Run `just check-adr-index` and confirm exit 0 and `check-adr-index: OK`.
4. Run `git diff --stat docs/adr/README.md` and confirm `1 insertion(+)` with no
   deletions, proving no other row was touched or reflowed.

## Guardrails

Run `just ci` bare from the worktree root after both tasks. Expect exit 0. It runs
the repository guardrail suite; `check-adr-index` is the arm this change
exercises, and the rest passes untouched because no Rust changes.

Note the landmine recorded for this repo: the `prek` pre-push hook re-runs the
suite in an isolated worktree and can outlast a two-minute tool timeout. That is
slowness, not a hang — do not re-invoke it. The pre-*commit* hook also runs
`check-adr-index`, so Task 1's record cannot be committed without Task 2's row;
the two land in one commit.

## Rollback

`git revert` of the single commit range. The change adds three documents and one
table row and carries no schema, no behavior, and no deployment ordering.

## Deferrals carried into the build

None. The design review ran three iterations under one charter and every finding
was dispositioned `accepted-fixed`; no `deferred-tracked` or
`rejected-with-evidence` disposition was recorded, and `docs/debt/` is outside this
run's permitted surface.
