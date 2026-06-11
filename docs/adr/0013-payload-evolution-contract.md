---
status: accepted
date: 2026-06-11
deciders: [VOOM core]
---

# 0013 — Durable JSON payloads evolve under a deny-unknown-fields contract

## Context

VOOM stores ~40 JSON-encoded columns across ~20 SQLite tables (`tickets.payload`,
`commit_intents.target`, `artifact_commit_records.report`, `workflow_summaries.*`,
and many more — migrations `0002`–`0015`). Each column is written and read by the
**single** control-plane/CLI binary that owns the database; workers never touch
SQLite (no `voom-store` dependency — they communicate only over
`voom-worker-protocol`).

The Fable audit's M4 finding (#220) names a silent-failure class: rolling a binary
that changed a payload struct risks a **silent field drop** when reading old rows.
serde's default behavior ignores unknown fields and supplies `Default`/`None` for
missing ones, so a renamed or removed field deserializes to a silent wrong answer
instead of a loud failure. The audit asked the project to "decide the
mixed-version-read contract deliberately."

Two facts shape the decision:

- **Deployment is single-binary-per-database.** One control-plane binary owns a
  given DB at a time, swapped atomically, so two binary versions never operate
  concurrently. Two *sequential* cross-version reads still occur: (1) the **forward**
  read — after every upgrade the new binary reads rows the previous binary wrote
  (M4's headline "reading old rows"); a rename or removal there is made loud by
  `deny_unknown_fields`, while a new optional field is handled by additive-only
  defaults. (2) the **backward** read — a *rollback*, where a newer binary wrote rows
  and the downgraded older binary reads them; the unknown newer field is rejected
  loudly. The contract covers both directions.
- **The codebase already leans on `#[serde(deny_unknown_fields)]`** on plain content
  structs — the event payload content structs (`voom-events::payload::artifact`) and
  the worker-protocol operation request/response structs. No persisted type uses
  `#[serde(flatten)]` or `#[serde(untagged)]` (both mutually exclusive with
  `deny_unknown_fields`), so nothing *blocks* applying it. Effectiveness, though,
  depends on placement: it works on a plain or newtype-wrapped content struct but is a
  silent no-op on an internally tagged enum's *inline* struct-variant (Decision §1).
  Some existing tagged enums therefore use inline variants where the attribute would
  not take effect (e.g. the NDJSON `ProgressFrame`), which is why the contract acts on
  the content struct via a newtype variant rather than the enum.

Worker version skew is a **separate** axis handled at the wire boundary
(`x-voom-protocol-version` + `/v1/handshake` negotiation + `enforce_version`); the
one open inconsistency there is tracked in #231, not here.

Design doc:
[`docs/superpowers/specs/2026-06-11-issue-220-payload-schema-contract-design.md`](../superpowers/specs/2026-06-11-issue-220-payload-schema-contract-design.md).

## Decision

Adopt a structural compatibility contract for durable payloads, in the same
convention-plus-scoped-check shape as ADR 0012, instead of adding per-row
`schema_version` columns.

1. **Contract (convention).** The risk exists only where a column is deserialized
   into a `Deserialize`-deriving type; a column read back as untyped
   `serde_json::Value` has no struct to drop fields from. Such a type carries
   strictness at the unit serde enforces it: `#[serde(deny_unknown_fields)]` on a
   plain struct (including a struct used as a tagged enum's variant *content*). A
   **tagged enum** (`tag` / `tag`+`content`) is **not** annotated — serde ignores the
   attribute there — and gets its strictness from `deny_unknown_fields` on each
   variant's content struct reached via a **newtype** variant, plus serde's tag
   discriminator, which already errors on an unknown variant. Placement is verified
   empirically: the attribute is honored on a newtype/standalone content struct but a
   silent no-op on an internally tagged enum's *inline* struct-variant, so the
   contract's success condition is effective rejection (a behavioral test), not
   attribute presence. Payloads evolve **additive-only** by default: new fields are
   optional or `#[serde(default)]` so old rows still read. A **breaking** change
   (rename, remove, or retype a field) is a deliberate, coordinated operation — never
   a silent serde default — and requires binary-before-DB upgrade ordering (the
   writer of the new shape rolls out, and is not rolled back, while old rows of the
   breaking shape may still exist). The rule is recorded in `AGENTS.md` and the
   upgrade-ordering requirement in `docs/release-process.md`.

2. **Scoped check.** `scripts/check-payload-deny-unknown.sh`, wired into `just ci`
   with a self-test, scans the modules that define durable typed payloads and fails
   when a `Deserialize`-deriving struct lacks `#[serde(deny_unknown_fields)]` and
   carries no inline justified exemption (`// payload-contract: exempt — <reason>`),
   or when a `Deserialize` tagged enum has an inline struct-variant whose content is
   not a separately-annotated struct. It uses `ast-grep` so it matches real
   syntax-tree items, not comments or string literals — the same tooling choice as
   `check-test-layout.sh` and `check-paused-time-db.sh`. The scanned scope is
   **derived from a column→type→module inventory** (one row per durable column,
   produced as the first implementation task), not a guessed module list; it covers
   every crate that defines a deserialized payload type, which includes
   `voom-control-plane` (e.g. `WorkflowTicketPayload`, the type for the audit's
   headline `tickets.payload` column) — a crate a module-guess would have missed.

`deny_unknown_fields` closes the *unknown/stale-field* half of M4 — after a rename
or removal the stale field in an old row is *unknown* to the new struct, so
deserialization errors (surfaced as `VoomError::Database`) instead of defaulting,
and on rollback an old binary rejects a newer row's added field rather than silently
dropping it. It does **not** make the *missing-optional-field* half loud: a newly
added `#[serde(default)]` field reading an old row that legitimately lacks it still
defaults — the additive path, safe by construction because the field never existed
in those rows. The additive-only convention plus binary-before-DB ordering for
breaking changes governs that half.

**Untyped passthrough columns** (e.g. `tickets.payload`, stored and returned as
`serde_json::Value` at the store boundary) are not themselves a deserialization
boundary. Where a higher layer types them (`tickets.payload` → `WorkflowTicketPayload`
in `voom-control-plane`) the contract attaches to that typed parse — which the
inventory locates and the guard scope includes. Columns never typed-read at any
layer (e.g. the `report` audit blobs, whose in-memory `*Report` structs derive
neither `Serialize` nor `Deserialize`) carry no M4 risk and get no guard; the
inventory records them as such so the classification is explicit.

## Consequences

- A payload struct changed in a field-dropping way fails loudly at read time
  (`VoomError::Database`) instead of returning a silent wrong answer. The audited
  M4 risk is closed for typed payloads.
- A new durable payload type that forgets `deny_unknown_fields` fails `just ci`
  (locally via pre-commit and in CI) with a pointer to this ADR, before it can ship.
  `just ci` gains one fast `ast-grep` shell step plus its self-test, comparable in
  cost to the existing `check-*` guards.
- **Rollback across a payload change is not transparent.** Because
  `deny_unknown_fields` also rejects *additive* drift, downgrading the binary after
  it has written rows with a new field makes the old binary fail to read those rows.
  This is intentional — loud over silent — and is the operating cost of not carrying
  a per-row version. `docs/release-process.md` states the binary-before-DB ordering
  and that a rollback across a payload change requires restoring the pre-upgrade DB.
- The check is scoped by the **inventory-derived module set**, not a structural
  property visible at each type. The residual is a durable column missing from the
  inventory: if a future column is typed into a struct in a crate the inventory never
  listed, its type slips past the check. The written `AGENTS.md` convention is the
  backstop, the inventory is reviewed when columns are added, and the scope is a
  one-line edit to extend — the same residual ADR 0012 accepts for its signal set.
- Non-durable `Deserialize` types that happen to live in a scanned module (e.g. a
  helper deserialized only from a fixture, never from a column) take an explicit
  one-line `// payload-contract: exempt — <reason>`, keeping the exception visible
  and justified rather than silently widening the scope.
- **The initial sweep is broad even though the steady-state guard is cheap.** It
  touches every durable typed payload and extracts inline tagged-variants to newtype
  content structs, editing the encode/decode match arms of their codecs (the
  commit-intent wire types in `voom-store` most of all). The extraction is
  serialization-shape preserving and is covered by per-type round-trip and
  unknown-field tests, but the one-time regression risk is real and concentrated in
  those codecs — the cost of buying the structural guarantee without a migration.

## Considered & rejected

- **Per-table `schema_version` columns** (issue #220 Direction 1). Rejected as the
  primary mechanism: an up-only migration adding a column to ~20 tables plus
  write/read changes in ~10 repository modules is a large footprint for a
  single-binary-per-DB deployment, the per-table granularity is coarse for columns
  that evolve independently, and the version bump is unenforced discipline — it does
  not by itself prevent a silent default unless every read also branches on the
  version. `deny_unknown_fields` delivers the same "reject unknown/newer" property
  structurally and CI-enforced, without the migration.
- **Convention only (no check).** Rejected for the ADR 0012 reason: a written rule
  is silently ignorable, and M4 is precisely a silent-failure class. Enforcement is
  the point.
- **Check only (no written convention).** Rejected: a flagged author needs to know
  *why* and what to do instead (additive-only, or a coordinated breaking change).
- **Forbid `#[serde(default)]` on persisted types** (force every field present so a
  missing field errors). Rejected: it breaks legitimate additive evolution — a new
  field needs a default to read old rows — and would make every additive change a
  breaking one. `deny_unknown_fields` targets the actual M4 case (stale/unknown
  fields) without this cost.
- **Behavioral test per type instead of an `ast-grep` guard** (round-trip a payload
  with an injected unknown field, assert `Err`). Kept as encouraged per-type
  coverage but rejected as the *enforcement* mechanism: it needs a valid base
  fixture per type and, more importantly, does not catch a *new* durable type whose
  author never wrote the test. The default-deny `ast-grep` scan catches the
  forgotten attribute automatically; the ADR 0012 precedent favors the syntax-tree
  guard.
- **A `DurablePayload` marker trait with `T: DurablePayload` bounds retrofitted onto
  every store read/write helper.** Rejected as over-broad for a pre-release surgical
  change: it touches ~10 repository modules to gain compile-time registry
  completeness that the scoped scan approximates at a fraction of the churn. Revisit
  only if the enumerated-scope residual proves insufficient.
- **Global `PAYLOAD_FORMAT_VERSION` constant checked centrally.** Rejected: there is
  no single central deserialization chokepoint to check it at (each repository owns
  its own serde calls), and the DB-level "too new" guard already exists via the
  migrator and `DB_SCHEMA_TOO_NEW`. A payload-format constant would be a second,
  redundant version axis with nothing structural enforcing that it is bumped.
