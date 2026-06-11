# Durable Payload Schema-Evolution Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close audit finding M4 (#220) by making a field-dropping change to any durable JSON payload fail loudly at read time, enforced in CI, with no schema migration.

**Architecture:** Every durable JSON column that is deserialized into a concrete `Deserialize` type (Class T / T-upstream) carries `#[serde(deny_unknown_fields)]` on the real serde unit (a plain or newtype-wrapped content struct; tagged enums get strictness from their variants' content structs plus serde's tag discriminator). A column read back as untyped `serde_json::Value` (Class P) carries no risk and gets no guard. A scoped `ast-grep` guard, wired into `just ci` with a self-test, fails when a `Deserialize` struct in the scanned scope lacks the attribute or a tagged enum uses an inline struct-variant. The scanned scope is derived from a committed column→type→module inventory, not guessed.

**Tech Stack:** Rust (serde, serde_json), `ast-grep` (syntax-tree guard, same as `check-test-layout.sh` / `check-paused-time-db.sh`), `bash` (guard + self-test), `just` (CI runner), `pre-commit`.

**Source of truth:** [ADR 0013](../../adr/0013-payload-evolution-contract.md) (decision + rejected alternatives) and the spec [`2026-06-11-issue-220-payload-schema-contract-design.md`](../specs/2026-06-11-issue-220-payload-schema-contract-design.md). When this plan and the spec disagree, the spec wins — stop and reconcile.

---

## Repo conventions every task must honor

- **Never** weaken or un-gate an existing test; the sweep only tightens deserialization.
- **Unknown-field tests are two assertions, in order:** (1) the *unmodified* base payload deserializes `Ok` — proving the base is valid so the test can't pass for the wrong reason (a malformed base would also be `Err`); then (2) the base **plus** an injected unknown field is `Err`. Build the base by serializing a constructed instance (`serde_json::to_value(value)`) or the type's existing valid fixture — **never** hand-write the JSON (a format mismatch, e.g. an `iso8601` field, would make assertion 2 pass vacuously).
- **No change to any serialized shape.** Adding `#[serde(deny_unknown_fields)]` and extracting an inline tagged-enum struct-variant to a newtype content struct are both serialization-shape preserving. Existing rows, fixtures, and `insta` snapshots stay valid.
- Sibling-test layout (`docs/adr/0004`): unit tests live in `<source>_test.rs` with `#[cfg(test)] #[path = "<source>_test.rs"] mod tests;` in the parent. `check-test-layout.sh` enforces it.
- Absolute imports only (no `..` paths). ≤100 lines/function, ≤8 cyclomatic complexity, 100-char lines. Google-style docstrings on non-trivial public APIs.
- Bash scripts start with `set -euo pipefail`; lint with `shellcheck` + `shfmt -d`. Support bash 3.2 (macOS CI) — use `while read` loops, not `mapfile`, matching the sibling guards.
- Guardrail per task: `just fmt-check && just lint`, plus the **scoped** tests for the crate edited **and its direct consumer crate** (for speed — e.g. the voom-events sweep must also run `cargo test -p voom-store`, since voom-store is the only crate that deserializes `Event`). Task 6's `just ci` is the full integration gate (`just test` builds and tests the whole workspace). A sweep task is not "done" on a green single-crate run if a downstream crate consumes the edited type.
- Commit one logical change at a time, imperative subject ≤72 chars, ending with the trailer:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- Conventional Commits. Do not squash code commits (per `CLAUDE.md`); each task is its own commit (or a small series).

## Where each piece lives (file map)

- `docs/payload-contract-inventory.md` — **create** (Task 1). The completeness artifact: one row per durable JSON column + transitively-reachable typed sub-fields.
- `scripts/payload-contract-scope.txt` — **create** (Task 1). Newline-separated list of in-scope source files (non-test). Read by the guard. The guard's scope is exactly this file.
- `scripts/check-payload-deny-unknown.sh` — **create** (Task 2). The guard.
- `scripts/check-payload-deny-unknown-selftest.sh` — **create** (Task 2). Fixture-driven self-test.
- `crates/voom-store/src/repo/media/commit_safety_gate/codecs.rs` + `codecs_test.rs` — **modify** (Task 3). Extract `CommitTargetWire` inline variants; add attribute to `*Wire` structs.
- `crates/voom-store/src/repo/media/commit_safety_gate.rs` + sibling test — **modify** (Task 3). `ForcePathToken`.
- `crates/voom-control-plane/src/workflow/plan/ticket_payload.rs` + `ticket_payload_test.rs` — **modify** (Task 4). `WorkflowTicketPayload`.
- `crates/voom-control-plane/src/workflow/execution/timing.rs` + sibling test — **modify** (Task 4). `EffectiveTiming`.
- `crates/voom-events/src/payload/{artifact,commit,execution,media_identity,policy,system,use_leases,workers}.rs` + their `_test.rs` siblings — **modify** (Task 5). Add attribute + unknown-field tests to every Event content struct (`artifact.rs` is only ~half-covered today).
- `justfile`, `.pre-commit-config.yaml` — **modify** (Task 6). Wire the guard + self-test.
- `AGENTS.md`, `docs/release-process.md` — **modify** (Task 7). Convention + upgrade ordering.

---

## Task 1: Build the durable-column inventory and guard scope

The inventory is the unit of completeness for the whole change: every durable JSON column is either covered (its type carries the contract) or explicitly Class-P (no typed read). The guard's scope is *derived from* this inventory, never guessed.

**Files:**
- Create: `docs/payload-contract-inventory.md`
- Create: `scripts/payload-contract-scope.txt`

**Where this fits:** First task. Tasks 2–5 act only on what this inventory enumerates; Task 6's guard scans exactly `scripts/payload-contract-scope.txt`.

- [ ] **Step 1: Enumerate every durable JSON column.** For each SQLite migration under `crates/voom-store/migrations/`, list every JSON/TEXT-holding-JSON column. For each, find the store read site (grep the column name near `from_value`, `from_str`, `serde_json::from`, `JsonValue`) and classify:
  - **Class T** — read via `from_value::<Type>` / `from_str::<Type>` into a concrete `Deserialize` type.
  - **Class P** — read as `serde_json::Value` / `JsonValue`, never into a struct.
  - **Class T-upstream** — `JsonValue` at the store, but deserialized into a typed struct in a higher layer (e.g. `tickets.payload` → `WorkflowTicketPayload` in voom-control-plane).

  This is a discovery step; the known starting set (verified while writing this plan) is in Step 3's table. Confirm it and add anything missed.

- [ ] **Step 2: Compute the transitive typed closure for each Class-T / T-upstream root.** A field-dropping change inside a *nested* `Deserialize` struct reached from a durable read is also M4 risk. For each root type, follow its fields into every named-field `Deserialize` struct it owns, recording each. Example: `WorkflowTicketPayload` → `EffectiveTiming` (named-field struct → in scope) and `OperationKind` (unit-variant enum → no field-drop surface, record as such). `rendered_payload: Value` / `source_file: Option<Value>` are untyped passthrough — not in scope.

  Tuple/newtype structs with no named fields (e.g. `TargetRowEpochTriple(TargetMemberKind, u64, u64)`) and unit-variant enums (`OperationKind`, `BypassKind`) carry **no field-drop surface**: serde already rejects an arity mismatch or unknown variant name. Record them as "no named fields → attribute inapplicable, safe by construction." They are **not** in the guard scope.

- [ ] **Step 3: Write `docs/payload-contract-inventory.md`.** One row per column. Use exactly these columns: `table.column | class | read site (file:line) | typed root (if any) | reachable typed sub-structs | defining file(s) | currently effective? | action`. Seed it with this verified content (confirm each `file:line` before committing — line numbers drift):

```markdown
# Durable Payload Contract Inventory (audit M4, #220)

Completeness artifact for the deny-unknown-fields contract (ADR 0013). Every
durable JSON column is listed exactly once. A Class-T / T-upstream row is "done"
only when its typed root (and reachable named-field sub-structs) carry an
**effective** `#[serde(deny_unknown_fields)]` (per the spec §1 placement rule) and
a behavioral unknown-field-rejection test exists. Class-P rows carry no M4 risk.

The guard (`scripts/check-payload-deny-unknown.sh`) scans exactly the files listed
in `scripts/payload-contract-scope.txt`, which is the set of "defining file(s)"
for every Class-T / T-upstream row below.

## Class T / T-upstream (contract applies)

| table.column | class | read site | typed root | reachable typed sub-structs | defining file(s) | action |
|---|---|---|---|---|---|---|
| events.payload | T | repo/audit/events.rs:280 `from_value::<Event>` | `Event` (adjacently tagged, newtype variants — enum itself effective) | all variant content structs in voom-events/src/payload/{artifact,commit,execution,media_identity,policy,system,use_leases,workers}.rs | those 8 files | artifact.rs is only ~half-covered (16/32) — sweep all 8 files incl. artifact.rs (Task 5) |
| commit_intents.target | T | repo/media/commit_safety_gate/codecs.rs (`decode_target`) | `CommitTargetWire` (internally tagged, **inline** struct-variants) | `FileLocationProposalWire` | codecs.rs | extract variants to newtype content structs; add attr (Task 3) |
| commit_intents.closure_initial / closure_authorized | T | codecs.rs / authorize.rs | `AffectedScopeClosureWire` | `ClosureWarningWire` | codecs.rs | add attr (Task 3) |
| commit_intents.override_token | T | codecs.rs (`from_str::<ForcePathToken>`) | `ForcePathToken` | — (`BypassKind` unit enum) | commit_safety_gate.rs | add attr (Task 3) |
| commit_intents.target_row_epochs | T | finalize.rs `from_str::<Vec<TargetRowEpochTriple>>` | `TargetRowEpochTriple` (tuple newtype, no named fields) | — | codecs.rs | safe by construction; NOT in scope |
| commit_intents.accepted_evidence_ids | T | authorize.rs `Vec<EvidenceId>` | `EvidenceId` (id newtype) | — | n/a | safe by construction; NOT in scope |
| worker_capabilities.{codecs,hardware,artifact_access}; worker_grants.{can_execute,can_access_read,can_access_write,denies}; workflow_file_phase_summaries.ticket_ids | T | workers.rs / workflow_summaries.rs | `Vec<String>` / `Vec<u64>` | — | n/a | scalar element types, no named-field surface; NOT in scope |
| tickets.payload | T-upstream | store: execution/tickets.rs (`JsonValue`); typed: control-plane | `WorkflowTicketPayload` | `EffectiveTiming` (named struct); `OperationKind` (unit enum — no surface) | ticket_payload.rs, timing.rs | add attr+tests to `WorkflowTicketPayload` and `EffectiveTiming` (Task 4) |

## Class P (passthrough JsonValue — no typed read, no risk)

tickets.result; worker_capabilities.extra; worker_grants.max_parallel;
artifact_handles.{allowed_access_modes,source_lineage};
artifact_commit_records.report; artifact_verifications.report
(in-memory `*Report` derive neither Serialize nor Deserialize);
external_systems.{connection_profile,rate_limit_config};
quality_scoring_profiles.definition; quality_scores.{dimension_scores,provenance};
remote_idempotency_keys.response_json;
artifact_access_plans.{input_handles,output_handles,evidence};
policy_versions.compiled_json; workflow_summaries.per_operation;
workflow_phase_summaries.report; scheduler_decisions.explanation_json;
nodes.metadata; identity_evidence.provenance; media_snapshots.payload.

If a future change starts typing any Class-P column, add it to the Class-T table
above and to `scripts/payload-contract-scope.txt`.
```

- [ ] **Step 4: Write `scripts/payload-contract-scope.txt`** — the "defining file(s)" union from the Class-T table, one path per line, comments allowed with `#`:

```
# In-scope source files for check-payload-deny-unknown.sh.
# Derived from docs/payload-contract-inventory.md (Class-T / T-upstream rows).
# Add a line here when a new durable column is typed into a struct.
crates/voom-events/src/payload/artifact.rs
crates/voom-events/src/payload/commit.rs
crates/voom-events/src/payload/execution.rs
crates/voom-events/src/payload/media_identity.rs
crates/voom-events/src/payload/policy.rs
crates/voom-events/src/payload/system.rs
crates/voom-events/src/payload/use_leases.rs
crates/voom-events/src/payload/workers.rs
crates/voom-events/src/payload/mod.rs
crates/voom-store/src/repo/media/commit_safety_gate/codecs.rs
crates/voom-store/src/repo/media/commit_safety_gate.rs
crates/voom-control-plane/src/workflow/plan/ticket_payload.rs
crates/voom-control-plane/src/workflow/execution/timing.rs
```

- [ ] **Step 5: Verify every path resolves.**

Run: `while IFS= read -r f; do case "$f" in ''|\#*) continue;; esac; test -f "$f" || echo "MISSING: $f"; done < scripts/payload-contract-scope.txt`
Expected: no `MISSING:` output.

- [ ] **Step 5b: Reconcile discovery against the sweep tasks (feedback loop).** This is the load-bearing check that the rest of the plan is complete. For **every** Class-T / T-upstream defining file the inventory lists, confirm it is owned by a sweep task:
  - `crates/voom-events/src/payload/*.rs` → Task 5
  - `crates/voom-store/src/repo/media/commit_safety_gate/codecs.rs`, `…/commit_safety_gate.rs` → Task 3
  - `crates/voom-control-plane/src/workflow/plan/ticket_payload.rs`, `…/execution/timing.rs` → Task 4

  If Step 1–2 discovery surfaced a Class-T / T-upstream root in **any other** file (e.g. in `voom-policy`, `voom-scheduler`, or elsewhere), the plan is incomplete: **stop and add a sweep task** for that file (modeled on Task 3 for an inline tagged enum, or Task 4 for a plain struct + nested types), and add the file to `scripts/payload-contract-scope.txt`, **before** continuing. Record any such addition here:

  ```
  Reconciliation result: [ ] all discovered roots map to Tasks 3–5 (no new task needed)
                         [ ] new sweep task(s) added: ____________________
  ```

  Do not proceed to Task 6 with any scope-file entry that no sweep task covers — Task 6's CI wiring would then fail with no owner.

- [ ] **Step 6: Commit.**

```bash
git add docs/payload-contract-inventory.md scripts/payload-contract-scope.txt
git commit -m "docs: inventory durable payload columns for M4 contract (#220)"
```

**Acceptance:** Every durable JSON column appears exactly once across the two inventory sections. Every Class-T / T-upstream row names a defining file that exists and appears in `scripts/payload-contract-scope.txt`. No Class-P column appears in the scope file.

---

## Task 2: Build the guard script and its self-test (not yet wired into CI)

Built early so it is the sweep's mechanical checklist (run manually per file), but **not wired into `just ci` until Task 6** — the spec's ship-order requires the CI guard to pass only against the completed sweep. TDD: the self-test fixtures are the guard's failing test first.

**Files:**
- Create: `scripts/check-payload-deny-unknown.sh`
- Create: `scripts/check-payload-deny-unknown-selftest.sh`

**Where this fits:** Provides the completeness oracle for Tasks 3–5.

- [ ] **Step 1: Write the failing self-test first.** It lays out fixture files in a throwaway tree, points the guard at a scope file listing them, and asserts exit codes. Mirrors `scripts/check-paused-time-db-selftest.sh`.

```bash
#!/usr/bin/env bash
# Self-test for check-payload-deny-unknown.sh. Lays out fixture source files in a
# throwaway tree with a scope file pointing at them, runs the real guard, and
# asserts its exit code per case. Wired into `just ci` so the guard's ast-grep
# rules cannot silently rot.

set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
check="$script_dir/check-payload-deny-unknown.sh"

failures=0

# expect_exit <expected-code> <fixture-body>
# Writes the body to a single source file, a scope file naming it, runs the guard.
expect_exit() {
	local want="$1" body="$2"
	local work
	work=$(mktemp -d)
	printf '%s\n' "$body" >"$work/fixture.rs"
	printf '%s\n' "$work/fixture.rs" >"$work/scope.txt"
	local got=0
	(PAYLOAD_CONTRACT_SCOPE="$work/scope.txt" "$check" >/dev/null 2>&1) || got=$?
	rm -rf "$work"
	if [[ "$got" -ne "$want" ]]; then
		echo "FAIL: expected exit $want, got $got for body:" >&2
		printf '%s\n' "$body" >&2
		failures=$((failures + 1))
	fi
}

# --- Violation (exit 1): Deserialize struct missing deny_unknown_fields ---
expect_exit 1 '#[derive(Deserialize)]
struct Bad { a: u32 }'

# --- Violation (exit 1): internally tagged enum with an inline struct-variant ---
expect_exit 1 '#[derive(Deserialize)]
#[serde(tag = "kind")]
enum BadEnum { Replace { retired: u64 } }'

# --- Clean (exit 0): struct with the attribute ---
expect_exit 0 '#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Good { a: u32 }'

# --- Clean (exit 0): justified exemption immediately preceding ---
expect_exit 0 '// payload-contract: exempt — fixture-only, never read from a column
#[derive(Deserialize)]
struct Exempted { a: u32 }'

# --- Clean (exit 0): tagged enum with newtype variants over covered structs ---
expect_exit 0 '#[derive(Deserialize)]
#[serde(tag = "kind")]
enum GoodEnum { Replace(ReplaceContent) }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceContent { retired: u64 }'

# --- Clean (exit 0): exempted inline tagged enum (escape hatch, parity w/ struct) ---
expect_exit 0 '// payload-contract: exempt — non-durable fixture enum
#[derive(Deserialize)]
#[serde(tag = "kind")]
enum ExemptEnum { Replace { retired: u64 } }'

# --- Clean (exit 0): tuple/newtype struct (no named fields) is ignored ---
expect_exit 0 '#[derive(Deserialize)]
struct Triple(u64, u64, u64);'

# --- Clean (exit 0): non-Deserialize struct is ignored ---
expect_exit 0 '#[derive(Debug, Clone)]
struct NotSerde { a: u32 }'

# --- REAL-SHAPE fixtures: the guard must work on production struct shapes ---

# Violation: doc comment between the derive and the struct, multi-derive line,
# pub, missing deny_unknown_fields (the dominant Event-payload shape).
expect_exit 1 '#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// A payload the audit log writes.
pub struct RealMissing {
    pub job_id: String,
    #[serde(default)]
    pub note: Option<String>,
}'

# Violation: rename_all present but deny_unknown_fields absent.
expect_exit 1 '#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RenamedMissing { retired_at: String }'

# Clean: multi-derive line, doc comment, pub, deny present below rename_all.
expect_exit 0 '/// Doc above the derive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RealPresent {
    pub job_id: String,
}'

# Clean: exemption marker sits ABOVE the derive, with a doc comment between
# derive and struct (exercises the contiguous upward scan, not line-1 only).
expect_exit 0 '// payload-contract: exempt — deserialized only from a test fixture
#[derive(Debug, Deserialize)]
/// Helper, never read from a column.
struct ExemptedWithDoc {
    a: u32,
}'

# CROSS-ITEM SHADOWING (the iter-2 regression): a missing-deny struct positioned
# AFTER a has-deny struct in the same file MUST still be flagged. A guard that
# binds attributes via `follows: stopBy: end` would wrongly count the second as
# covered. This is the normal mid-sweep state of the 14–17-struct payload files.
expect_exit 1 '#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct First { a: u32 }
#[derive(Deserialize)]
struct SecondMissing { b: u32 }'

# Clean: a PLAIN (untagged) enum with a struct variant, positioned after a tagged
# enum, must NOT be flagged — the `serde(tag` test must bind to the enum itself,
# not a preceding item.
expect_exit 0 '#[derive(Deserialize)]
#[serde(tag = "kind")]
enum Tagged { A(AContent) }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AContent { x: u32 }
#[derive(Deserialize)]
enum PlainAfterTagged { Variant { y: u32 } }'

if [[ "$failures" -gt 0 ]]; then
	echo "check-payload-deny-unknown-selftest: $failures failure(s)." >&2
	exit 1
fi

echo "check-payload-deny-unknown-selftest: OK"
```

- [ ] **Step 2: Run the self-test to verify it fails** (the guard does not exist yet).

Run: `./scripts/check-payload-deny-unknown-selftest.sh`
Expected: FAIL — `check: ...: No such file or directory` / non-zero exit (guard script missing).

- [ ] **Step 3: Write the guard.** Reads its scope from `$PAYLOAD_CONTRACT_SCOPE` (default `scripts/payload-contract-scope.txt`). Uses `ast-grep` inline rules + `--json=stream` and bash set-difference, exactly like `check-paused-time-db.sh`. Fails closed when `ast-grep` is missing or a scoped path does not resolve.

```bash
#!/usr/bin/env bash
# Guard the durable-payload schema-evolution contract (audit M4, ADR 0013).
#
# For every source file listed in the scope file (default
# scripts/payload-contract-scope.txt), fail when:
#   1. A `Deserialize`-deriving named-field struct lacks
#      `#[serde(deny_unknown_fields)]` and is not preceded by an inline
#      `// payload-contract: exempt — <reason>` marker.
#   2. A `Deserialize`-deriving tagged enum (`#[serde(tag = ...)]`) has an inline
#      struct-variant carrying fields directly — the attribute is a silent no-op
#      there, so each variant's content must be a separate struct (newtype
#      variant) covered by rule 1.
#
# Tuple/newtype structs (no named fields) and unit-variant enums carry no
# field-drop surface and are ignored. Uses ast-grep (syntax-tree items, not
# text), like check-test-layout.sh and check-paused-time-db.sh.
#
# See docs/adr/0013-payload-evolution-contract.md and
# docs/payload-contract-inventory.md.

set -euo pipefail

if ! command -v ast-grep >/dev/null; then
	echo "check-payload-deny-unknown: ast-grep is required. Run 'just setup' to install." >&2
	exit 2
fi

scope_file="${PAYLOAD_CONTRACT_SCOPE:-scripts/payload-contract-scope.txt}"
if [[ ! -f "$scope_file" ]]; then
	echo "check-payload-deny-unknown: scope file not found: $scope_file" >&2
	exit 2
fi

# Read scope into an array (bash 3.2: read loop, not mapfile). Skip blanks/#.
scope=()
while IFS= read -r line; do
	case "$line" in '' | \#*) continue ;; esac
	if [[ ! -f "$line" ]]; then
		echo "check-payload-deny-unknown: scoped path does not resolve: $line" >&2
		exit 2
	fi
	scope+=("$line")
done <"$scope_file"

if [[ "${#scope[@]}" -eq 0 ]]; then
	echo "check-payload-deny-unknown: OK (empty scope)"
	exit 0
fi

# Position-only rules: locate items by SHAPE, never by binding an attribute via
# `follows`. (`follows: { stopBy: end }` traverses ALL preceding nodes, so it can
# match an *earlier* item's attribute — e.g. a missing-deny struct placed after a
# has-deny struct would be wrongly counted as covered. That cross-item shadowing
# is the normal mid-sweep state in the 14–17-struct payload files, so attributes
# are bound per-item by scanning each item's OWN text region below.)

# Every named-field struct (brace body excludes tuple/newtype structs).
rule_named_struct='
id: named-struct
language: rust
severity: error
rule:
  kind: struct_item
  has: { field: body, kind: field_declaration_list }
'

# Every enum that has at least one inline struct-variant (carries fields directly).
rule_enum_inline_struct_variant='
id: enum-inline-struct-variant
language: rust
severity: error
rule:
  kind: enum_item
  has:
    kind: enum_variant
    stopBy: end
    has: { kind: field_declaration_list }
'

# Emit "file:line" for every node a rule matches across the scope.
# `scan` exits non-zero when an error-severity rule matches; tolerate under set -e.
matches() {
	local rule="$1"
	ast-grep scan --inline-rules "$rule" --json=stream "${scope[@]}" 2>/dev/null |
		grep -oE '"file":"[^"]*","range":\{"byteOffset":\{[^}]*\},"start":\{"line":[0-9]+' |
		sed -E 's/.*"file":"([^"]*)".*"line":([0-9]+)/\1:\2/' | sort -u || true
}

# The text region in which THIS item's attributes live, bound to the item alone
# (no cross-item shadowing): the contiguous attribute (`#[...]`) / line-comment
# (`//`) / blank block immediately ABOVE `line`, plus the item header from `line`
# down to the line that opens its body `{`. Covering both sides makes the scan
# robust to whether ast-grep anchors the match at the first attribute or at the
# struct/enum keyword. Assumes single-line `#[derive(...)]` (enforced below).
item_region() {
	local file="$1" line="$2" n text
	# Upward: contiguous attribute/comment/blank block.
	n=$((line - 1))
	while [[ "$n" -ge 1 ]]; do
		text=$(sed -n "${n}p" "$file")
		printf '%s\n' "$text" | grep -qE '^[[:space:]]*(#\[|//|$)' || break
		printf '%s\n' "$text"
		n=$((n - 1))
	done
	# Downward: item header through the line that opens the body.
	n="$line"
	while [[ "$n" -le "$((line + 40))" ]]; do
		text=$(sed -n "${n}p" "$file")
		[[ -z "$text" && "$n" -gt "$line" ]] && break
		printf '%s\n' "$text"
		printf '%s\n' "$text" | grep -q '{' && break
		n=$((n + 1))
	done
}

errors=0

# Fail closed on multi-line `#[derive(` (open paren ends the line): the per-item
# region scan assumes single-line derives, so reject the unsupported shape loudly
# instead of risking a silent miss. (None exist in scope today; rustfmt keeps
# these inline.)
for f in "${scope[@]}"; do
	while IFS= read -r ml; do
		[[ -z "$ml" ]] && continue
		echo "check-payload-deny-unknown: $f:${ml%%:*} — multi-line #[derive(...)] is unsupported; keep it single-line" >&2
		errors=$((errors + 1))
	done < <(grep -nE '#\[derive\($' "$f" 2>/dev/null || true)
done

# Rule 1: a named-field struct that derives Deserialize must carry
# deny_unknown_fields (or an exemption marker).
while IFS= read -r hit; do
	[[ -z "$hit" ]] && continue
	file="${hit%%:*}"
	line="${hit##*:}"
	region=$(item_region "$file" "$line")
	printf '%s' "$region" | grep -q 'payload-contract: exempt' && continue
	printf '%s' "$region" | grep -q 'Deserialize' || continue
	printf '%s' "$region" | grep -q 'deny_unknown_fields' && continue
	echo "check-payload-deny-unknown: $file:$line — Deserialize struct missing #[serde(deny_unknown_fields)]" >&2
	echo "  Add the attribute, or mark '// payload-contract: exempt — <reason>'. See docs/adr/0013." >&2
	errors=$((errors + 1))
done < <(matches "$rule_named_struct")

# Rule 2: a tagged enum (serde tag) must not use inline struct-variants
# (deny_unknown_fields is a no-op there) — extract each to a newtype struct. Plain
# (untagged) enums with struct variants are normal Rust and are NOT flagged, so
# the `serde(tag` test is bound to the enum's own region, not a preceding item.
while IFS= read -r hit; do
	[[ -z "$hit" ]] && continue
	file="${hit%%:*}"
	line="${hit##*:}"
	region=$(item_region "$file" "$line")
	printf '%s' "$region" | grep -q 'payload-contract: exempt' && continue
	printf '%s' "$region" | grep -qE 'serde\(tag' || continue
	echo "check-payload-deny-unknown: $file:$line — tagged enum has an inline struct-variant" >&2
	echo "  Extract each variant's content to a named struct (newtype variant) — deny_unknown_fields is a no-op on inline variants — or mark '// payload-contract: exempt — <reason>'. See docs/adr/0013." >&2
	errors=$((errors + 1))
done < <(matches "$rule_enum_inline_struct_variant")

if [[ "$errors" -gt 0 ]]; then
	echo "check-payload-deny-unknown: $errors violation(s)." >&2
	exit 1
fi

echo "check-payload-deny-unknown: OK"
```

- [ ] **Step 4: Iterate the guard against the self-test until green.** The `matches()` JSON parse depends on the installed `ast-grep` output shape. Run the self-test; if a known-violation fixture is not detected or a clean fixture is flagged, adjust the `matches()` extraction (e.g. use `ast-grep scan ... --json` file-grouped form and parse with the same `grep -oE`/`sed` discipline as `check-paused-time-db.sh`) until every case passes — including the real-shape fixtures (doc comment between derive and struct, `rename_all` + missing deny, multi-derive `pub` struct, exemption above the derive) and the two iter-2 regression fixtures (a missing-deny struct **after** a has-deny struct must still flag — the cross-item-shadowing guard; a plain untagged enum after a tagged enum must **not** flag). The `item_region` scan binds attributes per item, so a `matches()` line anchored at either the first attribute or the struct keyword resolves correctly. Do **not** change the fixtures to fit a broken guard.

Run: `./scripts/check-payload-deny-unknown-selftest.sh`
Expected: `check-payload-deny-unknown-selftest: OK`

- [ ] **Step 5: Lint the scripts.**

Run: `shellcheck scripts/check-payload-deny-unknown.sh scripts/check-payload-deny-unknown-selftest.sh && shfmt -d scripts/check-payload-deny-unknown.sh scripts/check-payload-deny-unknown-selftest.sh`
Expected: no output (clean). Add `# shellcheck disable=SC2016` with a reason on lines where `$` is an ast-grep meta-variable, matching the sibling scripts.

- [ ] **Step 6: Make executable and commit.**

```bash
chmod +x scripts/check-payload-deny-unknown.sh scripts/check-payload-deny-unknown-selftest.sh
git add scripts/check-payload-deny-unknown.sh scripts/check-payload-deny-unknown-selftest.sh
git commit -m "feat: add payload deny-unknown-fields guard and self-test (#220)"
```

**Acceptance:** Every self-test case passes, including the real-shape fixtures, the exempted-inline-enum case, and the cross-item-shadowing regression (missing-deny struct after a has-deny struct still flags; plain enum after a tagged enum does not). The guard exits 2 (not 1) when `ast-grep` is absent or a scoped path is missing. `shellcheck`/`shfmt` clean. The guard is **not** referenced from `justfile` or `.pre-commit-config.yaml` yet.

---

## Task 3: Sweep voom-store commit-safety-gate codecs

The trickiest sweep: `CommitTargetWire` is an internally tagged enum with **inline** struct-variants, where `deny_unknown_fields` is a silent no-op. Extract each variant's fields to a named content struct as a newtype variant (serialization-shape preserving for an internally tagged enum — the on-disk object stays flat), then the attribute on each content struct is effective. Add the attribute to the plain `*Wire` structs and `ForcePathToken`.

**Files:**
- Modify: `crates/voom-store/src/repo/media/commit_safety_gate/codecs.rs`
- Test: `crates/voom-store/src/repo/media/commit_safety_gate/codecs_test.rs`
- Modify: `crates/voom-store/src/repo/media/commit_safety_gate.rs`
- Test: `crates/voom-store/src/repo/media/commit_safety_gate_test.rs`

**Where this fits:** Covers `commit_intents.target`, `closure_initial`, `closure_authorized`, `override_token`.

- [ ] **Step 1: Write the failing tagged-enum regression test.** In `codecs_test.rs`, assert (i) the unmodified base parses `Ok`, then the base + an unknown field inside a variant fails, and (ii) an unknown variant name fails. Build the base by serializing a **constructed `CommitTarget`** through the existing `commit_target_to_wire` encoder (`use super::*` exposes it) — this is valid before *and* after the Step-3 extraction and references no new type, so the test compiles at the failing stage and avoids any hand-written `iso8601`/format trap. Reuse the valid `CommitTarget` the file's existing codec round-trip tests already construct (mirror that helper here as `sample_replace_target()` if it is not already shared).

```rust
// In codecs_test.rs (uses super::* for the private wire types + encoder).

fn replace_base_value() -> serde_json::Value {
    // Encode a real CommitTarget through the production encoder so the base JSON
    // is always valid, independent of the wire enum's internal shape.
    serde_json::to_value(commit_target_to_wire(&sample_replace_target())).unwrap()
}

#[test]
fn replace_variant_rejects_unknown_field() {
    // (1) base is valid — guards against a wrong-reason pass.
    let base = replace_base_value();
    assert!(
        serde_json::from_value::<CommitTargetWire>(base.clone()).is_ok(),
        "base replace payload must deserialize Ok",
    );
    // (2) base + unknown field is rejected.
    let mut v = base;
    v.as_object_mut().unwrap().insert("surprise".into(), serde_json::json!(1));
    assert!(
        serde_json::from_value::<CommitTargetWire>(v).is_err(),
        "unknown field in variant must be rejected",
    );
}

#[test]
fn commit_target_rejects_unknown_variant_name() {
    let v = serde_json::json!({ "kind": "teleport_file_location", "retired": "floc_1" });
    assert!(
        serde_json::from_value::<CommitTargetWire>(v).is_err(),
        "unknown variant name must be rejected",
    );
}
```

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p voom-store commit_safety_gate::codecs 2>&1 | tail -20`
Expected: `replace_variant_rejects_unknown_field` — the `Ok` base assertion passes, the unknown-field assertion FAILS (inline variant silently drops `surprise`). The unknown-variant test already passes (serde rejects unknown tags) — keep it as a regression pin.

- [ ] **Step 3: Extract the inline variants to newtype content structs.** Replace the `CommitTargetWire` enum (currently `codecs.rs:21`) and add three content structs. The flat on-disk shape is preserved because internally tagged + newtype-of-struct serializes the struct's fields inline alongside the tag.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CommitTargetWire {
    #[serde(rename = "delete_file_location")]
    Delete(DeleteFileLocationWire),
    #[serde(rename = "replace_file_location")]
    Replace(ReplaceFileLocationWire),
    #[serde(rename = "move_file_location")]
    Move(MoveFileLocationWire),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteFileLocationWire {
    retired: FileLocationId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceFileLocationWire {
    retired: FileLocationId,
    new: FileLocationProposalWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveFileLocationWire {
    retired: FileLocationId,
    new: FileLocationProposalWire,
}
```

- [ ] **Step 4: Update the encode mapper to construct the newtype content.** In `commit_target_to_wire` (the encode direction; the decode mapper `commit_target_from_wire` is Step 4b), replace `CommitTargetWire::Replace { retired, new }` constructors with `CommitTargetWire::Replace(ReplaceFileLocationWire { retired, new })`, and likewise for `Delete`/`Move`. Match the existing mapper structure in the file.

```rust
fn commit_target_to_wire(t: &CommitTarget) -> CommitTargetWire {
    match t {
        CommitTarget::DeleteFileLocation(id) => {
            CommitTargetWire::Delete(DeleteFileLocationWire { retired: *id })
        }
        CommitTarget::ReplaceFileLocation { retired, new } => {
            CommitTargetWire::Replace(ReplaceFileLocationWire {
                retired: *retired,
                new: FileLocationProposalWire::from_proposal(new),
            })
        }
        CommitTarget::MoveFileLocation { retired, new } => {
            CommitTargetWire::Move(MoveFileLocationWire {
                retired: *retired,
                new: FileLocationProposalWire::from_proposal(new),
            })
        }
    }
}
```

- [ ] **Step 4b: Update the decode mapper.** The inverse mapper `commit_target_from_wire` lives in the **same file** (`codecs.rs`, ~line 193) — it is the only other site that destructures `CommitTargetWire` (verified: `commit_target_to_wire` and `commit_target_from_wire` are the sole match sites; `decode_target` at `:181` calls `from_str` then `commit_target_from_wire`). Rewrite its arms to bind through the new content struct:

```rust
fn commit_target_from_wire(w: CommitTargetWire) -> Result<CommitTarget, VoomError> {
    Ok(match w {
        CommitTargetWire::Delete(DeleteFileLocationWire { retired }) => {
            CommitTarget::DeleteFileLocation(retired)
        }
        CommitTargetWire::Replace(ReplaceFileLocationWire { retired, new }) => {
            CommitTarget::ReplaceFileLocation {
                retired,
                new: file_location_proposal_from_wire(new)?,
            }
        }
        CommitTargetWire::Move(MoveFileLocationWire { retired, new }) => {
            CommitTarget::MoveFileLocation {
                retired,
                new: file_location_proposal_from_wire(new)?,
            }
        }
    })
}
```

The downstream callers (`decode_target` at `:181`, and its users in `abort_list.rs:277`, `finalize.rs:377`, `authorize.rs:360`) consume `CommitTarget`, not the wire type, so they are unaffected by the extraction.

- [ ] **Step 5: Add `#[serde(deny_unknown_fields)]` to the remaining plain `*Wire` structs.** On `FileLocationProposalWire` (`codecs.rs:37`), `AffectedScopeClosureWire` (`codecs.rs:110`), and `ClosureWarningWire` (`codecs.rs:119`) — add the attribute line directly under each `#[derive(...)]`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLocationProposalWire {
    // ... unchanged fields ...
}
```

- [ ] **Step 6: Add `#[serde(deny_unknown_fields)]` to `ForcePathToken`.** In `commit_safety_gate.rs` (`ForcePathToken` at ~line 492):

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForcePathToken {
    pub actor: String,
    pub reason: String,
    pub bypass: BTreeSet<BypassKind>,
}
```

- [ ] **Step 7: Add a plain-struct unknown-field test** for `ForcePathToken` in `commit_safety_gate_test.rs` (build a valid instance, serialize to a `Value`, inject an unknown key, assert `from_value` is `Err`):

```rust
#[test]
fn force_path_token_rejects_unknown_field() {
    let token = ForcePathToken {
        actor: "a".into(),
        reason: "r".into(),
        bypass: std::collections::BTreeSet::new(),
    };
    let base = serde_json::to_value(&token).unwrap();
    assert!(serde_json::from_value::<ForcePathToken>(base.clone()).is_ok());
    let mut v = base;
    v.as_object_mut().unwrap().insert("extra".into(), serde_json::json!(true));
    assert!(serde_json::from_value::<ForcePathToken>(v).is_err());
}
```

- [ ] **Step 8: Run the targeted tests, then the guard against this file.**

Run: `cargo test -p voom-store commit_safety_gate 2>&1 | tail -20`
Expected: all pass, including `replace_variant_rejects_unknown_field`.

Run: `printf '%s\n' crates/voom-store/src/repo/media/commit_safety_gate/codecs.rs crates/voom-store/src/repo/media/commit_safety_gate.rs > /tmp/scope-$$.txt && PAYLOAD_CONTRACT_SCOPE=/tmp/scope-$$.txt ./scripts/check-payload-deny-unknown.sh`
Expected: `check-payload-deny-unknown: OK`.

- [ ] **Step 9: Guardrails + commit.**

Run: `just fmt-check && just lint && cargo test -p voom-store`
Expected: green.

```bash
git add crates/voom-store/src/repo/media/commit_safety_gate/
git commit -m "feat: enforce deny-unknown-fields on commit-intent wire payloads (#220)"
```

**Acceptance:** `commit_intents.target` round-trips unchanged (existing codec tests still pass — shape preserved), an unknown field in any variant and an unknown variant name both fail loudly, and the guard passes for both files. No `insta` snapshot changed.

---

## Task 4: Sweep voom-control-plane ticket payload

`tickets.payload` is Class-T-upstream: stored as `JsonValue`, deserialized into `WorkflowTicketPayload` at `parse_ticket` (errors mapped to `WorkflowTicketPayloadError`). Tighten the root and its one named-field sub-struct `EffectiveTiming`.

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/plan/ticket_payload.rs`
- Test: `crates/voom-control-plane/src/workflow/plan/ticket_payload_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/execution/timing.rs`
- Test: `crates/voom-control-plane/src/workflow/execution/timing_test.rs`

- [ ] **Step 1: Write the failing test** in `ticket_payload_test.rs`, asserting a parsed ticket payload with an injected unknown top-level field fails (and that the error is the owning typed error, confirming the loud path is reached):

```rust
#[test]
fn parse_ticket_rejects_unknown_field() {
    let payload = WorkflowTicketPayload::new_for_test(
        "wf", "plan", "node", "branch", OperationKind::ProbeFile, serde_json::json!({}),
    );
    let base = payload.to_ticket_payload().unwrap();
    // (1) base parses Ok — proves the rejection below is the only behavior change.
    assert!(
        WorkflowTicketPayload::parse_ticket("ticket.probe_file", base.clone()).is_ok(),
        "base ticket payload must parse Ok",
    );
    // (2) base + unknown field is rejected.
    let mut value = base;
    value.as_object_mut().unwrap().insert("rogue".into(), serde_json::json!(1));
    let parsed = WorkflowTicketPayload::parse_ticket("ticket.probe_file", value);
    assert!(parsed.is_err(), "unknown field must fail the typed parse");
}
```

(Confirm the exact `ticket_kind` string and `new_for_test` signature against the file before running; adjust the literals to match.)

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p voom-control-plane ticket_payload 2>&1 | tail -20`
Expected: `parse_ticket_rejects_unknown_field` FAILS (unknown field silently dropped today).

- [ ] **Step 3: Add the attribute to `WorkflowTicketPayload`** (`ticket_payload.rs:7`):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTicketPayload {
    // ... unchanged fields ...
}
```

- [ ] **Step 4: Add the attribute to `EffectiveTiming`** (`timing.rs:3`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveTiming {
    // ... unchanged fields ...
}
```

- [ ] **Step 5: Add an `EffectiveTiming` unknown-field test** in `timing_test.rs` (construct via its existing test constructor `EffectiveTiming::for_test(25, 10)`, serialize, inject an unknown key, assert `Err`):

```rust
#[test]
fn effective_timing_rejects_unknown_field() {
    let base = serde_json::to_value(EffectiveTiming::for_test(25, 10)).unwrap();
    assert!(serde_json::from_value::<EffectiveTiming>(base.clone()).is_ok());
    let mut v = base;
    v.as_object_mut().unwrap().insert("extra".into(), serde_json::json!(0));
    assert!(serde_json::from_value::<EffectiveTiming>(v).is_err());
}
```

- [ ] **Step 6: Run tests + guard for both files.**

Run: `cargo test -p voom-control-plane ticket_payload && cargo test -p voom-control-plane timing 2>&1 | tail -20`
Expected: all pass.

Run: `printf '%s\n' crates/voom-control-plane/src/workflow/plan/ticket_payload.rs crates/voom-control-plane/src/workflow/execution/timing.rs > /tmp/scope-$$.txt && PAYLOAD_CONTRACT_SCOPE=/tmp/scope-$$.txt ./scripts/check-payload-deny-unknown.sh`
Expected: `check-payload-deny-unknown: OK`.

- [ ] **Step 7: Guardrails + commit.**

Run: `just fmt-check && just lint && cargo test -p voom-control-plane`
Expected: green. (If `EffectiveTiming` is deserialized anywhere from a non-durable source — check with the dual-use note below — confirm the tightening is wanted before committing.)

```bash
git add crates/voom-control-plane/src/workflow/plan/ticket_payload.rs \
        crates/voom-control-plane/src/workflow/plan/ticket_payload_test.rs \
        crates/voom-control-plane/src/workflow/execution/timing.rs \
        crates/voom-control-plane/src/workflow/execution/timing_test.rs
git commit -m "feat: enforce deny-unknown-fields on workflow ticket payload (#220)"
```

**Dual-use check (do before Step 7 commit):** grep for other deserialization sites of `WorkflowTicketPayload` / `EffectiveTiming` (`rg -n "from_value::<WorkflowTicketPayload>|from_str::<WorkflowTicketPayload>|deserialize.*EffectiveTiming"`). If either is also parsed from CLI/config/worker input where extra fields are tolerated by design, note it in the commit body and confirm tightening is intended (it almost certainly is — these are internal types).

**Acceptance:** `parse_ticket` rejects an unknown field with `WorkflowTicketPayloadError` (no new error type introduced), `EffectiveTiming` rejects unknown fields, existing ticket round-trip tests still pass.

---

## Task 5: Sweep voom-events Event payload content structs

`events.payload` deserializes into `Event` (adjacently tagged, already newtype variants). Its content structs span eight files. **`artifact.rs` is only partially guarded** — it has ~32 `Deserialize`-deriving named-field structs but only ~16 `deny_unknown_fields` (the attribute first appears around line 208; the leading payload structs at lines 7–200 are unguarded). So `artifact.rs` is **not** done and must be swept like the others. Add the attribute to every `Deserialize` named-field struct in all eight files, with an unknown-field test per struct that already has a round-trip test. The guard for these files is the completeness oracle.

**Files (modify each + its `_test.rs` sibling):**
- `crates/voom-events/src/payload/artifact.rs` (partially done — finish the unguarded structs)
- `crates/voom-events/src/payload/commit.rs`
- `crates/voom-events/src/payload/execution.rs`
- `crates/voom-events/src/payload/media_identity.rs`
- `crates/voom-events/src/payload/policy.rs`
- `crates/voom-events/src/payload/system.rs`
- `crates/voom-events/src/payload/use_leases.rs`
- `crates/voom-events/src/payload/workers.rs`

**Where this fits:** Covers the largest durable column, `events.payload`. `mod.rs` holds the `Event` enum (newtype variants → guard-clean) but **verify** it in Step 0a before relying on it.

- [ ] **Step 0a: Enumerate the real gap (do not trust "already effective").** Run the guard over `artifact.rs` and `mod.rs` to list exactly which structs still lack the attribute, so the sweep covers them rather than assuming they are done:

Run: `printf '%s\n' crates/voom-events/src/payload/artifact.rs crates/voom-events/src/payload/mod.rs > /tmp/scope-$$.txt && PAYLOAD_CONTRACT_SCOPE=/tmp/scope-$$.txt ./scripts/check-payload-deny-unknown.sh; rm -f /tmp/scope-$$.txt`
Expected: a list of `artifact.rs` structs missing the attribute (≈16) and `OK`/violations for `mod.rs`. Fold every flagged struct into the per-file sweep below. If `mod.rs` flags anything beyond the `Event` enum, sweep it too.

- [ ] **Step 0: Dual-use check (once, before sweeping any file).** Tightening these structs tightens them at *every* deserialization site, not only the `events.payload` read. Confirm no Event payload type is also deserialized from a non-durable source where extra fields are tolerated by design:

Run: `rg -n "from_value::<.*Payload>|from_str::<.*Payload>" crates/ | rg -v "crates/voom-events/|repo/audit/events"`
Expected: no hit that deserializes a payload type from CLI/config/worker input. The widespread `use voom_events::payload::*` across control-plane is for **constructing/emitting** events (serialize) — not a dual-use deserialize, which is unaffected by `deny_unknown_fields`. If any hit *does* deserialize a payload type from a non-durable, extra-field-tolerant source, record it and confirm tightening is intended before sweeping that type.

Process the eight files **one at a time** (one commit each), repeating Steps 1–5 per file. Doing one file per commit keeps `git bisect` precise and each change reviewable. For `artifact.rs`, the per-file run only needs to add the attribute to the structs Step 0a flagged (and tests for them), not the 16 already covered.

- [ ] **Step 1: Add a unknown-field assertion to each struct's existing round-trip test.** Each `_test.rs` already constructs valid instances. For every `Deserialize` named-field struct in `<file>.rs` (public **or** private — the guard scopes by shape, not visibility, so Step 1, Step 3, and the guard must agree), add (reusing the file's existing valid-instance constructor):

```rust
#[test]
fn <struct_snake>_rejects_unknown_field() {
    let base = serde_json::to_value(<existing valid instance>).unwrap();
    // (1) base parses Ok — guards against a wrong-reason pass.
    assert!(serde_json::from_value::<<StructName>>(base.clone()).is_ok());
    // (2) base + unknown field is rejected.
    let mut v = base;
    v.as_object_mut().unwrap().insert("__unknown".into(), serde_json::json!(true));
    assert!(serde_json::from_value::<<StructName>>(v).is_err());
}
```

  If a struct has no existing valid instance in the test file, build the minimal valid one from its fields (it is a payload struct — all fields are owned data).

- [ ] **Step 2: Run to verify the new tests fail.**

Run: `cargo test -p voom-events payload::<module> 2>&1 | tail -30`
Expected: every new `*_rejects_unknown_field` FAILS (no attribute yet → unknown field accepted).

- [ ] **Step 3: Add `#[serde(deny_unknown_fields)]` under the `#[derive(...)]` of every `Deserialize` named-field struct in `<file>.rs`.** Do not touch tuple structs or unit enums. Match the existing attribute placement style in `artifact.rs`.

- [ ] **Step 4: Run the file's tests + the guard for that one file.**

Run: `cargo test -p voom-events payload::<module> 2>&1 | tail -30`
Expected: all pass.

Run: `printf '%s\n' crates/voom-events/src/payload/<file>.rs > /tmp/scope-$$.txt && PAYLOAD_CONTRACT_SCOPE=/tmp/scope-$$.txt ./scripts/check-payload-deny-unknown.sh; rm -f /tmp/scope-$$.txt`
Expected: `check-payload-deny-unknown: OK` (the guard claims no struct in the file was missed).

  **Ground-truth cross-check (guard-false-negative defense).** The guard's `OK` is only trustworthy if it actually *sees* every Deserialize struct. Independently count the Deserialize named-field structs in the file and confirm each now carries the attribute, so a guard parse bug cannot pass an unswept struct:

Run: `ast-grep run --lang rust --pattern '#[derive($$$D)] struct $N { $$$ }' --selector struct_item crates/voom-events/src/payload/<file>.rs | rg -c "struct "` (count of derive-bearing named-field structs)
Run: `rg -c "deny_unknown_fields" crates/voom-events/src/payload/<file>.rs` (count of attributes added)
Expected: the deny-count is **≥** the count of those structs that derive `Deserialize` (subtract any `Serialize`-only or exempted structs you noted). If the guard says `OK` but this count is short, the guard has a false negative — fix the guard (Task 2), do not proceed.

- [ ] **Step 5: Guardrails + commit (per file).**

Run: `just fmt-check && just lint`
Expected: green.

```bash
git add crates/voom-events/src/payload/<file>.rs crates/voom-events/src/payload/<file>_test.rs
git commit -m "feat: enforce deny-unknown-fields on <file> event payloads (#220)"
```

- [ ] **Step 6: After all eight files, run the guard against the full events scope + the producing AND consuming crates.**

Run: `printf '%s\n' crates/voom-events/src/payload/*.rs | grep -v _test > /tmp/scope-$$.txt && PAYLOAD_CONTRACT_SCOPE=/tmp/scope-$$.txt ./scripts/check-payload-deny-unknown.sh; rm -f /tmp/scope-$$.txt`
Expected: `OK` (all eight files now clean, including the finished `artifact.rs` and `mod.rs`).

Run: `cargo test -p voom-events -p voom-store`
Expected: green. **voom-store is the crate that deserializes `Event`** (`repo/audit/events.rs:280` + `events_test.rs` round-trips), so a `deny_unknown_fields` addition that breaks a voom-store fixture surfaces here, not three tasks later at Task 6. Also run `cargo test -p voom-control-plane -p voom-artifact` once (both depend on voom-events) before treating the events sweep as done.

**Acceptance:** Every `Deserialize` named-field struct in all eight `payload/*.rs` files carries an effective `deny_unknown_fields` (including the ~16 previously-unguarded `artifact.rs` structs); the guard passes for the full set; each Event content struct rejects an injected unknown field; no serialized shape changed (existing round-trip and any `insta` tests still pass).

---

## Task 6: Wire the guard into CI and pre-commit

The sweep is complete, so the guard now passes against the full scope. Wire it in — last, per the spec's ship-order.

**Files:**
- Modify: `justfile`
- Modify: `.pre-commit-config.yaml`

- [ ] **Step 1: Confirm full scope coverage before wiring.** Every file in `scripts/payload-contract-scope.txt` must have been swept by Task 3, 4, or 5 (verify against Task 1 Step 5b's reconciliation result — no scope entry may be unswept). Then run the full guard and self-test:

Run: `./scripts/check-payload-deny-unknown.sh && ./scripts/check-payload-deny-unknown-selftest.sh`
Expected: both `OK`. A failure here means a scope-file entry was not swept (or the guard regressed) — fix the owning sweep task, do not weaken the guard.

- [ ] **Step 2: Add the recipes and extend `ci` in `justfile`.** Add the two checks to the `ci` line (after the paused-time pair) and add their recipes alongside the existing ones:

```
ci: fmt-check lint check-test-layout check-paused-time-db check-paused-time-db-selftest check-payload-deny-unknown check-payload-deny-unknown-selftest test doc deny audit
    @echo "==> All CI checks passed"
```

```
# Guard: every durable typed payload denies unknown fields (audit M4, ADR 0013)
check-payload-deny-unknown:
    ./scripts/check-payload-deny-unknown.sh

# Self-test for the payload contract guard (keeps its ast-grep rules honest)
check-payload-deny-unknown-selftest:
    ./scripts/check-payload-deny-unknown-selftest.sh
```

- [ ] **Step 3: Add the two pre-commit hooks** in `.pre-commit-config.yaml`, in the `repo: local` block right after the `check-paused-time-db-selftest` hook, mirroring its format:

```yaml
      - id: check-payload-deny-unknown
        name: just check-payload-deny-unknown
        entry: just check-payload-deny-unknown
        language: system
        pass_filenames: false
        files: '\.rs$'

      - id: check-payload-deny-unknown-selftest
        name: just check-payload-deny-unknown-selftest
        entry: just check-payload-deny-unknown-selftest
        language: system
        pass_filenames: false
        files: '\.rs$'
```

- [ ] **Step 4: Lint the workflow/config and run the full gate.**

Run: `prek run check-payload-deny-unknown --all-files && just ci`
Expected: `just ci` ends with `==> All CI checks passed`.

- [ ] **Step 5: Commit.**

```bash
git add justfile .pre-commit-config.yaml
git commit -m "ci: enforce payload deny-unknown-fields guard in just ci and pre-commit (#220)"
```

**Acceptance:** `just ci` runs the guard and self-test and is green end-to-end. Pre-commit runs both hooks on `.rs` changes.

---

## Task 7: Document the contract and upgrade ordering

**Files:**
- Modify: `AGENTS.md`
- Modify: `docs/release-process.md`

- [ ] **Step 1: Add the convention to `AGENTS.md`.** Under the Architecture / load-bearing-invariants area (near the existing testing-layout and guard references), add:

```markdown
### Durable payload schema-evolution contract (audit M4, ADR 0013)

A JSON column deserialized into a `Deserialize` type carries
`#[serde(deny_unknown_fields)]` on the real serde unit — a plain or newtype-wrapped
content struct. A tagged enum is not annotated (serde ignores it there); its
variants are newtype variants over annotated content structs, and serde's tag
discriminator rejects unknown variant names. Inline tagged struct-variants are a
silent no-op and are forbidden for durable enums. Payloads evolve **additive-only**
(new fields `Option`/`#[serde(default)]`); a rename/remove/retype is a deliberate,
coordinated change requiring binary-before-DB upgrade ordering, never a silent
default. New durable typed columns are added to `docs/payload-contract-inventory.md`
and `scripts/payload-contract-scope.txt`. Enforced by
`scripts/check-payload-deny-unknown.sh` in `just ci`.
```

- [ ] **Step 2: Add upgrade ordering to `docs/release-process.md`.** Add a section after `## Steps`:

```markdown
## Payload compatibility (audit M4, ADR 0013)

Durable JSON payloads deny unknown fields, so cross-version reads fail loudly
rather than silently dropping a field:

- **Upgrade (binary before DB):** a new binary reading old rows tolerates absent
  optional fields (additive evolution) and rejects nothing it added.
- **Breaking change (rename/remove/retype a field):** roll the new binary out and
  do not roll it back while old-shape rows may still exist.
- **Rollback across a payload change is not transparent:** the older binary will
  intentionally reject rows the newer binary wrote. A rollback across such a change
  requires restoring the pre-upgrade database snapshot.
```

- [ ] **Step 3: Cross-link the inventory one-directionally.** ADR 0013 is accepted and the ADR index (`docs/adr/README.md`) declares ADRs append-only, so **do not edit 0013**. Instead, ensure `docs/payload-contract-inventory.md` (Task 1) and the spec link TO ADR 0013 — the inventory already references it; just confirm the link resolves. The ADR's existing generic "the inventory" wording is fine without a back-pointer.

- [ ] **Step 4: Doc guardrails + full gate.**

Run: `just doc && just ci`
Expected: green.

- [ ] **Step 5: Commit.**

```bash
git add AGENTS.md docs/release-process.md
git commit -m "docs: document payload contract and upgrade ordering (#220)"
```

**Acceptance:** `AGENTS.md` states the convention with the inventory/scope pointers; `docs/release-process.md` states binary-before-DB ordering and the rollback consequence; `just ci` green.

---

## Self-review (plan author checklist — completed)

**Spec coverage:** §0 Inventory → Task 1. §1 contract (plain struct + tagged enum newtype) → Tasks 3–5. §1 empirical placement / inline-variant extraction → Task 3 (CommitTargetWire). §2 sweep + audit-existing-placements + dual-use → Tasks 3–5 (dual-use checks in Task 4 Step 7 and Task 5 Step 0). §3 guard + self-test + fail-closed → Task 2; wiring → Task 6. §4 Class-P recorded, no guard → Task 1 inventory. Error handling (site-dependent, no new error type) → Tasks 3 (`VoomError::Database` codecs) & 4 (`WorkflowTicketPayloadError`). Testing (per-type behavioral, tagged-enum regression, plain-struct regression, guard self-test) → Tasks 2–5. Rollout ship-order (inventory → sweep+tests → guard wired last → docs) → task ordering; operator ordering → Task 7.

**Per-type test scale:** the spec's "per-type behavioral test" is satisfied via one unknown-field test per swept struct (reusing each struct's existing valid instance), with the guard providing the structural completeness guarantee so a *forgotten* type is caught even without a hand-written test. The tagged-enum and plain-struct regression tests (Tasks 3–4) pin the serde mechanisms the contract relies on.

**Type-name consistency:** content structs introduced in Task 3 (`DeleteFileLocationWire`, `ReplaceFileLocationWire`, `MoveFileLocationWire`) are referenced only within Task 3. Guard env var `PAYLOAD_CONTRACT_SCOPE` and scope file `scripts/payload-contract-scope.txt` are consistent across Tasks 1, 2, 3, 4, 5, 6. Marker comment `// payload-contract: exempt — <reason>` is identical in the guard (Task 2) and AGENTS.md (Task 7).

**Known residual flagged for plan review:** the guard's `ast-grep` JSON parsing (`matches()` in Task 2 Step 3) is version-sensitive; Task 2 Step 4 makes the self-test the authority and instructs iterating the parse until all fixtures pass, rather than asserting a fixed JSON shape works first try.
