# Issue 358 design: Define quoted track-filter escape semantics

## Objective

Make the published `title contains <quoted-string>` escape behavior explicit,
deterministic, and guarded at the compiler and durable policy-version
boundaries. Identical exact source bytes must keep one compiled meaning across
historical, current, upgraded, and rolled-back binaries.

## Scope and ownership

In scope:

- quoted-title scanning and compiled-value transformation in `voom-policy`;
- the exact-byte `source_hash` and schema-V2 compiled projection;
- duplicate-source behavior in the `voom-store` policy registry;
- the track-filter grammar section in the authoritative published contract,
  `docs/specs/voom-control-plane-design.md`; and
- historical, current, upgrade, and rollback compatibility fixtures for
  escaped quotes, escaped backslashes, and terminal escaped quotes.

Explicitly excluded:

- new policy grammar, escape spellings, source-version markers, or aliases;
- changing the source-hash algorithm or its exact-byte preimage;
- changing a compiled-policy field, discriminator, schema version, or
  provenance format;
- broad durable-payload strictness owned by #344; and
- source-form acceptance owned and completed by #350.

An excluded concern returns to scope only if this design depends on it or makes
it worse.

## Governing invariants

`source_hash(source)` is the lowercase 64-character BLAKE3 digest of the exact
accepted UTF-8 source bytes. It is computed before parser normalization.
Migration `0007_policy_registry.sql` enforces
`UNIQUE(policy_document_id, source_hash)`, and
`SqlitePolicyRepo::add_version` returns the existing immutable row for duplicate
source before it invokes the current compiler.

Compiled policy schema version remains `2`. Historical V2 JSON remains readable
and current V2 JSON must remain readable after rollback. This change therefore
cannot intentionally assign a new compiled title value to source already
accepted by the historical compiler.

## V2 quoted-title semantics

The published form remains:

```text
title contains <quoted-string>
```

A quoted string:

- begins and ends with an ASCII double quote;
- is non-empty under the existing source grammar;
- uses a backslash to escape the next Unicode scalar value only while locating
  its closing delimiter;
- does not decode or normalize any backslash pair; and
- must consume the complete filter leaf.

After lexical validation, schema V2 computes the durable `value` by removing
the opening delimiter and the maximal trailing run of double quotes. That run
contains the closing delimiter and, for a terminal escaped quote, also contains
the escaped quote. This is the historical `trim_matches('"')` behavior stated
as an explicit compatibility algorithm.

Pinned examples use Rust debug notation for the compiled string:

| Source lexeme | Compiled `value` |
|---|---|
| `"Director \"Cut\""` | `"Director \\\"Cut\\"` |
| `"Path\\Name"` | `"Path\\\\Name"` |
| `"\"Quoted\" middle"` | `"\\\"Quoted\\\" middle"` |

Backslash is not a general escape decoder. For example, `\q` remains the two
source characters `\` and `q`. Unterminated input and a terminal backslash with
no closing delimiter remain invalid.

The implementation introduces one narrowly named schema-V2 transformation at
the shared track-filter parser boundary. It removes the opening delimiter and
then removes trailing quote characters. It does not reuse a generic
`strip_quotes` helper, because doing so hides the compatibility-sensitive
terminal behavior.

## Durable compatibility matrix

The source fixture
`crates/voom-policy/fixtures/historical/escaped-title-filters.voom` and its
pre-#358 compiled golden are the historical compiler oracle. They already
contain:

- a terminal escaped quote;
- an escaped backslash;
- escaped quotes away from the terminal boundary; and
- an unknown backslash pair proving no decoding occurs.

Before changing compiler code, add
`crates/voom-policy/fixtures/historical/escaped-title-filter-boundaries.voom`
and capture its complete unmodified-compiler output at
`crates/voom-policy/fixtures/compiled/historical-track-filter-source/escaped-title-filter-boundaries.json`.
It contains these exact title lexemes:

- `"Path\\"`: an even two-backslash run immediately before the closing
  delimiter, proving the first backslash escapes the second and the following
  quote still closes the string; and
- `"Caf\é"`: a backslash followed by a multi-byte Unicode scalar and then the
  closing quote, proving scanning advances by one scalar rather than one UTF-8
  byte.

The boundary fixture's exact bytes, BLAKE3 hash, compiled values, schema
version, and complete deterministic JSON become immutable pre-change oracles.
They join the existing fixture in every matrix row; neither fixture is first
generated or accepted after the lowering implementation changes.

The matrix is:

| Path | Setup | Required evidence |
|---|---|---|
| Historical | Deserialize the pre-#358 compiled golden | Exact semantic values and schema V2 are readable |
| Current | Compile the exact historical source | Exact hash and complete deterministic JSON equal the historical golden |
| Upgrade | Insert the historical row, then upload identical source through the current repository | Existing version ID/number/current pointer/epoch and compiled JSON remain unchanged; only one version row exists |
| Rollback | Create the source through the current repository | Stored hash and complete compiled JSON equal the pre-#358 golden, so the old reader receives its own V2 shape and meanings |

The store tests compare parsed JSON plus deterministic serialization, not only
an exit status. They inspect the document pointer, epoch, version count,
version identity, source text, source hash, schema version, and compiled
projection.

## Failure behavior

This issue adds no new accepted or rejected source form. Existing malformed
quoted strings retain their parse/validation diagnostics. No compiler fallback
or compatibility branch chooses semantics by compiler version.

A duplicate upload on an upgraded database returns the stored row without
rewriting, recompiling, advancing the document epoch, or inserting a partial
version. A fresh current write produces the same schema-V2 projection that an
older binary produced.

## Security and observability

Policy text is untrusted. The bounded quote scanner continues to consume Unicode
by scalar boundaries, treats content as data, and rejects a missing closing
delimiter. No escape is executed or interpreted by a provider.

There is no runtime event for compilation. Compatibility is observable through
the stored `source_text`, `source_hash`, `schema_version`, and `compiled_json`
already exposed by policy-version inspection.

## Testing strategy

Compiler tests:

- assert exact values for escaped quotes, escaped backslashes, terminal escaped
  quotes, non-decoded unknown backslash pairs, an even backslash run at the
  closing delimiter, and a backslash-escaped non-ASCII scalar;
- assert each source hash equals BLAKE3 of the exact fixture bytes and a pinned
  literal digest;
- compare the complete current deterministic JSON to both historical goldens;
- deserialize and reserialize both historical goldens without semantic change;
- keep unterminated and terminal-backslash inputs rejected.

Store tests:

- for each historical fixture, seed a document/version row and prove a current
  duplicate upload returns it without mutation; and
- for each fixture, create the same source in a fresh database and prove its
  stored identity and projection equal the historical oracle that a rollback
  binary understands.

Published-contract evidence:

- update the track-filter language section of
  `docs/specs/voom-control-plane-design.md` to state the ASCII delimiter,
  scalar-aware backslash scanning, no decoding or normalization, complete-leaf
  consumption, rejection of missing delimiters and terminal backslashes, and
  the pinned schema-V2 terminal escaped-quote result; and
- re-read the rendered source examples and require every example to compile to
  the value stated by the contract.

Focused verification:

```text
cargo test -p voom-policy track_filter
cargo test -p voom-policy policy_fixtures
cargo test -p voom-store policy
just fmt-check
just lint
```

The full repository gate is `just ci`.

## Rollout and rollback

The source grammar, source hash, compiled schema, and durable projection do not
change, so rollout requires no migration or data rewrite. An upgraded binary
reads old rows and returns them for duplicate source. A rolled-back binary reads
new rows because they are byte-equivalent schema-V2 projections for this
surface.

Before merge, rebase after campaign issues #344 and #346, then rerun the
focused checks and `just ci`.
