# ADR 0097 — pre-promotion address containment

Goal: land one accepted Architecture Decision Record answering issue #615, with
its single index row, on a green `just ci`.

Architecture: two Markdown files. `docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md`
is the record; `docs/adr/README.md` gains one row for it. The record decides that
a pre-promotion `.committed` address is contained by its own staging root, that
ADR 0074's shared-owner-node rule is the whole staging↔output relationship, what
#616 enforces at configuration time, and that an unconfigured output root fails
closed. No Rust, no migration, no `justfile` change; nothing here ships behavior.

Tech stack: Markdown records under `docs/adr/`, gated by
`scripts/check-adr-index.sh` (invoked as `just check-adr-index`, which `just ci`
runs).

Expected implementation size: 200–210 changed lines (M) — derived from the file map below: one 202-line record plus one index row.

## Global Constraints

- **ADR number is 0097, assigned.** Do not take "next free". The index ends at
  0096 and no `docs/adr/0097-*` exists.
- **Record format follows `docs/adr/0096-run-scheduled-resource-cells-on-isolated-runners.md`**,
  the most recent record: H1 `# NNNN — Title`, then `## Status` (bare `Accepted`
  on its own line, no date — this repo installs no `check-records.sh`, only
  `scripts/check-adr-index.sh`), `## Context`, `## Decision`, `## Consequences`,
  `## Considered & rejected`.
- **Every `Considered & rejected` bullet opens its ground with `verified:` or
  `judgment:`.**
- **Index row format is exactly** `| [NNNN](NNNN-slug.md) | Title |`.
  `scripts/check-adr-index.sh:21` matches the literal prefix `| [NNNN](filename) |`
  at column one and requires exactly one such line. The table has two columns,
  `| ADR | Title |` — **there is no Status column**, so nothing couples the row to
  the record's `## Status` value.
- **Touch one row only.** Do not reflow, re-sort, or re-align the table; adjacent
  edits serialize parallel ADR PRs on merge conflicts.
- **File scope is `docs/adr/0097-*.md`, `docs/adr/README.md`, and these two design
  artifacts.** Do not edit Rust sources, migrations, or the `justfile`.
  `crates/voom-control-plane/src/operation_source.rs` and
  `operation_source_test.rs` are owned by parallel issue #617; report a nit there
  rather than fixing it.
- **The record must not authorize extending** the control plane's transitional
  filesystem-promotion path (ADR 0050, pending #416–#425).
- **Guardrail:** `just ci`, run bare — no pipe, no redirect, no `|| true`.

## File map

| Path | Created / changed | Answerable for |
|---|---|---|
| `docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md` | created | The decision, its context, consequences, and rejected alternatives |
| `docs/adr/README.md` | changed (one row appended) | The index entry `check-adr-index` requires |

## Task 1 — write the record

Creates `docs/adr/0097-pre-promotion-addresses-contained-by-their-own-root.md`.
Modifies nothing. This is the whole substance of the change; Task 2 is the gate's
bookkeeping.

**Interfaces.** Consumes nothing from an earlier task. Task 2 relies on this
task's exact filename, `0097-pre-promotion-addresses-contained-by-their-own-root.md`,
and its exact H1 title text, `Pre-promotion artifact addresses are contained by
their own storage root`, to build the index row.

**Verification.**

- Contract: the record's decision content. `Mode: task-test-not-applicable` —
  the contract is a decision expressed in prose, and no executable consumer reads
  it. `scripts/check-adr-index.sh` validates only the file-to-row correspondence,
  which is Task 2's contract, and nothing else in `just ci` reads a record's body.
  A test asserting prose wording would assert the words, not the decision.

**Steps.**

1. Write the record with the five sections named in Global Constraints, citing
   ADR 0055, ADR 0074's owner-node requirement, ADR 0075, and ADR 0050's
   byte-ownership and transitional-path constraints.
2. State the rule #616 must enforce concretely enough to implement without a
   second decision, and decide the unconfigured-output-root fallback.
3. In `## Consequences`, say what #616 and #618 are each unblocked to do, and
   reference #623 with the layout the decision implies.
4. Verify every `file:line` reference cited in the record resolves at the branch's
   base commit. Command:
   `for spec in <each cited path:line>; do sed -n "${spec##*:}p" "${spec%:*}"; done`
   Expect each to print the line the record's surrounding sentence describes.
5. Confirm every `Considered & rejected` bullet carries `verified:` or `judgment:`.
   Command: `grep -c 'verified:\|judgment:' docs/adr/0097-*.md` — expect a count
   equal to the number of bullets in that section.

**Acceptance criteria.**

- The file exists at the exact path above with all five sections non-empty.
- The H1 number `0097` matches the filename.
- Every cited `file:line` resolves.
- Every rejected alternative carries an evidence tag.
- The record authorizes no extension of the transitional promotion path.

## Task 2 — add the index row

Modifies `docs/adr/README.md`. Creates nothing.

**Interfaces.** Consumes Task 1's exact filename and H1 title. Nothing later
relies on this task.

**Verification.**

- Contract: `docs/adr/README.md` carries exactly one row for
  `0097-pre-promotion-addresses-contained-by-their-own-root.md`, as
  `scripts/check-adr-index.sh` requires.
  `Mode: focused-test` — the observable contract is that gate's exit status.
  Expected red, before the row is added:
  `just check-adr-index` exits 1 and prints
  `check-adr-index: 0097-pre-promotion-addresses-contained-by-their-own-root.md is missing from docs/adr/README.md`
  followed by `check-adr-index: 1 violation(s).`
  Green command: `just check-adr-index` — expect exit 0 and the sole line
  `check-adr-index: OK`.

**Steps.**

1. Run `just check-adr-index` and confirm the red output above. This proves the
   gate bites on the new record before the row exists.
2. Append one line after the `0096` row, at the end of the table:
   `| [0097](0097-pre-promotion-addresses-contained-by-their-own-root.md) | Pre-promotion artifact addresses are contained by their own storage root |`
3. Run `just check-adr-index` and confirm exit 0 and `check-adr-index: OK`.
4. Run `git diff --stat docs/adr/README.md` and confirm `1 insertion(+)` and no
   deletions — proof no other row was touched or reflowed.

**Acceptance criteria.**

- `just check-adr-index` exits 0.
- The README diff is exactly one added line.
- The row's link resolves to the file Task 1 created.

## Guardrails

Run `just ci` bare from the worktree root after both tasks. Expect exit 0. It runs
`fmt-check`, `lint`, `check-test-layout`, `check-adr-index`, `test`, `doc`, `deny`,
and `audit`; this change touches no Rust, so `check-adr-index` is the arm that
decides it.

Note the landmine recorded for this repo: the `prek` pre-push hook re-runs the
suite in an isolated worktree and can outlast a two-minute tool timeout. That is
slowness, not a hang — do not re-invoke it.

## Rollback

`git revert` of the single commit range. The change adds two documents and one
table row and carries no schema, no behavior, and no deployment ordering.

## Deferrals carried into the build

None yet. Any deferral a `$trial-loop` run on this design disposes of as
`deferred-tracked` is recorded here, with its owning record path or tracker issue,
before the build starts.
