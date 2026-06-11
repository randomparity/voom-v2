# Issue 220 — Durable payload schema-evolution contract (audit M4)

## Context

Audit finding **M4** (#220): the ~40 JSON-encoded columns across ~20 SQLite tables
carry no schema version, so rolling a binary that changed a payload struct risks a
**silent field drop** (serde defaults the missing/renamed field) when reading old
rows — a silent wrong answer rather than a loud failure. The audit asked the
project to "decide the mixed-version-read contract deliberately." It was
deliberately deferred from #219 as deserving its own focused design.

The chosen direction and its rejected alternatives are recorded in
[ADR 0013](../../adr/0013-payload-evolution-contract.md). This spec is the
buildable design.

### The risk exists only at a typed-read boundary

The M4 silent-drop happens when JSON is deserialized **into a Rust struct/enum that
derives `Deserialize`** and a changed field defaults silently. A column that is read
back as an untyped `serde_json::Value` has no struct to drop fields from and carries
**no** M4 risk. So the organizing question for every column is: *is it deserialized
into a `Deserialize`-deriving type, and if so, which type and in which crate?*

Verified against source, durable columns fall into three classes:

- **Class T — typed read.** The column is deserialized into a concrete
  `Deserialize` type. These carry M4 risk and the contract applies to the type.
  Examples: `events.payload` → `Event` (adjacently tagged enum, deserialized at
  `repo/audit/events.rs:280`); `commit_intents.target` → `CommitTargetWire`
  (internally tagged enum, `repo/media/commit_safety_gate/codecs.rs`);
  `tickets.payload` → `WorkflowTicketPayload`
  (`voom-control-plane/src/workflow/plan/ticket_payload.rs:8`, parsed at `:83`);
  the worker-protocol operation payloads; policy/video-profile data types.
- **Class P — passthrough, untyped at the boundary.** The column is stored and read
  as `serde_json::Value` and never deserialized into a struct. No struct, no
  silent-drop. Example: `artifact_commit_records.report` /
  `artifact_verifications.report` (`repo/media/artifacts.rs:176,195` store and read
  `report: JsonValue`; the in-memory `CommitArtifactReport`/`VerifyArtifactReport`
  derive neither `Serialize` nor `Deserialize`, so they never touch the column).
- **Class T-upstream.** The store returns `JsonValue` (Class P at the store) but a
  higher layer deserializes it into a `Deserialize` type — that typed parse is the
  boundary. `tickets.payload` is the canonical case (Class P at
  `repo/execution/tickets.rs`, Class T in control-plane). The contract attaches to
  the upstream type.

### Established facts (verified against source)

- **Single-binary-per-DB.** One control-plane/CLI binary owns a given database,
  swapped atomically, so two versions never operate concurrently. Two *sequential*
  cross-version reads still occur: the **forward** read — after every upgrade the new
  binary reads rows the previous binary wrote (M4's headline "reading old rows") — and
  the **backward** read — a rollback, where the downgraded binary reads rows a newer
  binary wrote. The contract covers both. Workers never touch SQLite (no `voom-store`
  dependency); they talk over `voom-worker-protocol`.
- **`deny_unknown_fields` is already the house pattern** on plain content structs —
  the event payload content structs (`voom-events::payload::artifact`, 12+ structs)
  and the `voom-worker-protocol::operations::*` request/response structs (~12 files
  use it). Nothing *blocks* applying it more widely.
- **Effectiveness depends on placement (see Design §1).** The attribute is honored on
  a plain or newtype-wrapped content struct but is a silent no-op when placed on a
  tagged enum or on an internally tagged enum's *inline* struct-variant. `Event`
  (`#[serde(tag,content)]`) gets its strictness from its variants' content structs;
  `CommitTargetWire` (`#[serde(tag="kind")]`) currently uses inline variants and is
  therefore unguarded until they are extracted to newtype content structs. So the
  contract acts on the content struct via a newtype variant, not the enum.
- **No persisted type uses `#[serde(flatten)]` or `#[serde(untagged)]`** (both
  mutually exclusive with `deny_unknown_fields`).

## Goals

- Close the M4 silent-drop risk for every Class-T / T-upstream durable payload by
  making a field-dropping change fail **loudly** at read time.
- Enforce the contract in CI so a new typed payload cannot silently opt out.
- Document the operator-facing upgrade-ordering consequence.
- Stay surgical: extend the existing `deny_unknown_fields` convention and the
  existing `check-*` guard idiom; **no schema migration, no per-table columns.**

## Non-Goals

- No per-row/per-table `schema_version` columns (ADR 0013 rejected alternative).
- No worker-protocol version work — separate axis tracked in #231.
- No change to the *serialized shape* of any payload — only deserialization
  strictness (the `deny_unknown_fields` attribute) and, where a tagged enum uses
  inline struct-variants, a shape-preserving refactor to named content structs so
  the attribute applies.
- No struct guard on Class-P columns that are never typed-read (they carry no risk).
- No retrofit of a `DurablePayload` marker trait onto repository helpers.
- No relaxation or un-gating of any existing test.

## Design

### 0. Inventory first (the completeness artifact)

The plan's first task produces a table with **one row per durable JSON column**:
`table.column → class (T / P / T-upstream) → deserialized type (if any) → crate &
module → already-effectively-guarded?` — where "effectively guarded" means the
attribute is present *and* on an effective unit (a plain or newtype content struct,
not an inline tagged-variant), per §2, not mere attribute presence. This table is
the unit of completeness:
every row is either covered (its type carries the contract) or explicitly classified
Class-P-no-typed-read. The guard's scanned scope is *derived from this table* (the
set of modules that define Class-T / T-upstream types), not guessed. Expected to
include `voom-control-plane` (e.g. `WorkflowTicketPayload`) — which a module-guess
would have missed.

### 1. The contract (convention), acting on the real deserialization unit

A type read from a durable column carries strictness at the unit serde enforces it:

- **Plain struct** (incl. a struct used as a tagged enum's variant *content*):
  `#[serde(deny_unknown_fields)]` on the struct.
- **Tagged enum** (`tag` or `tag`+`content`): the enum is **not** annotated (serde
  ignores it there). Strictness comes from (a) `deny_unknown_fields` on each
  variant's content struct, reached via a **newtype** variant (`Foo(FooContent)`),
  and (b) serde's tag discriminator, which already errors on an **unknown variant
  name** — the desired loud failure for a typed column read (this is *not* the M15
  list-level skip, which is specific to scanning the whole `events` table). Where a
  payload enum currently uses **inline** struct-variants (e.g.
  `CommitTargetWire::Replace { .. }`), extract a named content struct per variant as
  a newtype variant; for an internally/adjacently tagged enum this is
  **serialization-shape preserving** — the on-disk object is unchanged (flat for an
  internally tagged enum, content nested under the content key for an adjacently
  tagged one) — and it gives the variant a struct on which the attribute is honored.

  **Placement is load-bearing — verified empirically on this repo's serde.** The
  attribute is honored on a standalone/newtype content struct (unknown field →
  rejected loudly) but is a **silent no-op on an inline struct-variant** of an
  internally tagged enum (unknown field → accepted, silently dropped). So the
  newtype extraction is required, not cosmetic: it is the difference between an
  effective and an ineffective guard. Adjacently tagged content (e.g. `Event`'s
  content structs) and standalone structs are already effective.

Durable payloads evolve **additive-only** by default — new fields are
`Option`/`#[serde(default)]` so old rows still read.

**Which half of M4 this closes (stated explicitly):** `deny_unknown_fields` makes
the *unknown/stale-field* direction loud — a renamed or removed field leaves an
unknown field in old rows → a loud deserialize error (surfaced through the read
site's own wrapping; see Error handling); and on rollback, an old binary reading a
row a newer binary wrote rejects the unknown new field instead of dropping it. It does **not** make the *missing-optional-field*
direction loud: a newly added `#[serde(default)]` field reading an old row that
legitimately lacks it still defaults — that is the additive path and is safe by
construction (the field never existed in those rows). A **breaking** change (rename,
remove, retype) is therefore a deliberate, coordinated operation requiring
binary-before-DB upgrade ordering, never a silent default.

Recorded in `AGENTS.md` and `docs/release-process.md` (upgrade ordering).

### 2. The sweep

**The sweep's success condition is effective unknown-field rejection, not attribute
presence** (per the §1 placement result). For every Class-T / T-upstream type in the
inventory: add `#[serde(deny_unknown_fields)]` where missing (on the struct, or on
each tagged enum's newtype variant content per §1); and **audit existing
placements** for effectiveness — an attribute already present on an internally
tagged enum's inline struct-variant is ineffective and that variant must be
extracted to a newtype content struct. Each type's covering behavioral test (an
injected unknown field → `Err`) is what confirms effectiveness; a type that "has"
the attribute but still accepts unknown fields fails that test. Extraction is
serialization-shape preserving, so existing rows, fixtures, and `insta` snapshots
remain valid. The guard's inline-struct-variant rule (§3) is the structural
enforcement of this, so its scope must cover every internally tagged durable enum's
module.

**Dual-use check.** A type deserialized from a durable column *and* from a
non-durable source (CLI/config/worker request) has its tolerance tightened
everywhere by the attribute. The inventory flags such types; the plan decides per
case whether tightening is wanted, rather than letting the sweep change non-durable
behavior silently.

### 3. The guard

`scripts/check-payload-deny-unknown.sh`, wired into `just ci` and pre-commit, with a
companion `check-payload-deny-unknown-selftest.sh` (matching the `check-paused-time-db`
pair). Behavior:

- Scan the modules the inventory identifies as defining Class-T / T-upstream types.
- For each `Deserialize`-deriving **struct** in scope, require either
  `#[serde(deny_unknown_fields)]` on it or an inline
  `// payload-contract: exempt — <reason>` immediately preceding it.
- For each `Deserialize`-deriving **tagged enum** in scope, require that it has no
  inline struct-variant carrying fields directly (so every variant's content is a
  struct covered by the struct rule) — i.e. flag inline struct-variants as needing
  extraction. Unit and newtype-of-covered-struct variants pass.
- Fail with the offending `file:line`, type name, and a pointer to ADR 0013.
- Implement with `ast-grep` (syntax-tree items, not text), like the sibling guards.
- The self-test runs the guard against temporary pass/fail fixtures (a struct missing
  the attribute; a struct with it; a justified exemption; a tagged enum with an inline
  struct-variant) so the matching logic cannot rot, and runs in CI.

Non-durable `Deserialize` types that live in a scanned module take the exemption
marker, keeping the exception visible and justified.

### 4. Class-P columns (untyped at every layer)

Columns read only as `serde_json::Value` and never deserialized into a struct (e.g.
the `report` audit blobs) carry no M4 risk and get no guard. The inventory records
them as Class-P so their classification is explicit rather than an unverified
omission. If a future change starts typing such a column, the new type enters the
inventory and the guard scope.

## Error handling

- A field-dropping read fails **loudly at its existing read site**, surfaced through
  that site's own error wrapping — `VoomError::Database` at the `voom-store` codecs
  (e.g. `decode_target` → `"decode commit_target: {e}"`; the tickets store read →
  `"parse payload: {e}"`), and the owning layer's typed error at the T-upstream
  control-plane parses (e.g. `WorkflowTicketPayload::parse_ticket` →
  `WorkflowTicketPayloadError`, mapped onward to binding/config errors). No new error
  code or variant is introduced: the contract changes *when* these errors fire (a
  changed struct now rejects a stale field) — not their type. The loud failure is
  reached instead of bypassed.
- The guard fails closed: a missing `ast-grep`, or a scanned path that does not
  resolve, exits non-zero with a clear message (matching the sibling guards' `exit 2`
  on missing `ast-grep`).

## Testing

- **Completeness = the inventory.** Every inventory row is either (a) a Class-T /
  T-upstream type with the contract applied and a behavioral unknown-field-rejection
  test, or (b) Class-P with no typed read. A row in neither state fails review.
- **Per-type behavioral tests:** for each swept type, round-trip a valid payload with
  an injected unknown field and assert `Err` (in the type's sibling `*_test.rs`),
  added where absent.
- **Tagged-enum regression:** for at least one tagged-enum column whose variant
  content carries the attribute (e.g. `commit_intents.target` → `CommitTargetWire`
  after extraction), assert that (i) an unknown field inside a variant fails loudly
  and (ii) an unknown variant *name* fails loudly. This pins serde's tagged-enum
  behavior the design relies on.
- **Plain-struct regression:** a Class-T plain-struct column row with an extra
  unknown field fails the read loudly rather than defaulting — the concrete M4
  scenario on a type where `deny_unknown_fields` is directly effective.
- **Guard self-test** as in §3.
- `just ci` green: `fmt-check`, `lint`, `check-test-layout`, `check-paused-time-db`,
  the new `check-payload-deny-unknown` + self-test, `test`, `doc`, `deny`, `audit`.

## Rollout / rollback

- **Ship order within the branch:** inventory → sweep + tests → guard (added last so
  it passes against the completed sweep) → docs.
- **Operator upgrade ordering:** documented in `docs/release-process.md` —
  binary-before-DB; a rollback across a payload change requires restoring the
  pre-upgrade DB because the older binary will (intentionally) reject rows the newer
  binary wrote.
- **Reverting the change itself** is safe: it adds attributes, shape-preserving
  variant extractions, tests, a script, and docs; no schema or data changes, so a
  straight `git revert` of the branch returns to prior behavior with no DB impact.
