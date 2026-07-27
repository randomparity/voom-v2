# Issue 358 implementation plan: Define quoted track-filter escape semantics

Base branch: `main`

Base commit: `4fc6bf74e00dba51b253d084c7cb7654cf3df47c`

Design:
`docs/superpowers/specs/2026-07-27-issue-358-track-filter-escape-semantics-design.md`

ADR: `docs/adr/0044-preserve-v2-track-filter-escape-semantics.md`

Guardrails:

- `cargo test -p voom-policy track_filter`
- `cargo test -p voom-policy policy_fixtures`
- `cargo test -p voom-store policy`
- `just fmt-check`
- `just lint`
- `prek run`
- `just ci`

Mandatory merge gate: rebase this branch after campaign issues #344 and #346
merge, rerun every focused command and `just ci`, and wait for green GitHub CI.

## Step 1: Capture the pre-change boundary oracle

Files:

- add
  `crates/voom-policy/fixtures/historical/escaped-title-filter-boundaries.voom`;
- add
  `crates/voom-policy/fixtures/compiled/historical-track-filter-source/escaped-title-filter-boundaries.json`;
- update
  `crates/voom-policy/src/fixtures/policy_fixtures_test.rs`.

Before changing any compiler implementation, add a two-phase historical source
fixture:

```text
policy "escaped-title-filter-boundaries" {
  phase terminal_escaped_backslash {
    keep subtitle where title contains "Path\\"
  }
  phase escaped_unicode_scalar {
    keep subtitle where title contains "Caf\é"
  }
}
```

Capture the complete deterministic JSON from the unmodified compiler. Review
the exact `title_contains.value` strings, schema version, provenance, and
literal 64-character source hash. Add a fixture test that:

- hashes the exact included source bytes and compares it to the pinned literal;
- compiles the source with the still-unmodified compiler;
- compares the complete deterministic JSON to the new golden; and
- deserializes the golden as `CompiledPolicy`, asserts both exact semantic
  values, and reserializes it to the same JSON value.

Extend the existing escaped-title fixture test to pin its literal hash and
assert exact terminal-escaped-quote, escaped-backslash, interior-escaped-quote,
and unknown-pair values after deserialization. Do not regenerate its existing
golden.

Expected pre-golden failure: the focused fixture test reports the missing
boundary golden and prints the unmodified compiler output. Adding and reviewing
that exact output makes the test green before production code changes.

Verification:

```text
cargo test -p voom-policy policy_fixtures
```

Logical commit:

```text
test(policy): capture title escape compatibility oracles
```

## Step 2: Make the V2 transformation explicit and publish it

Files:

- update `crates/voom-policy/src/compile/track_filter.rs`;
- update `crates/voom-policy/src/compile/track_filter_test.rs`;
- update `docs/specs/voom-control-plane-design.md`.

Add behavior cases that compile or parse:

- an escaped quote away from the closing boundary;
- an escaped backslash;
- the historical odd terminal-escaped-quote case;
- the new even terminal-backslash case;
- the escaped Unicode scalar case;
- an unknown backslash pair; and
- the existing unterminated quote and terminal-backslash failures.

Assert complete `TrackFilter::TitleContains` values, not only acceptance.
Retain full-leaf consumption coverage.

Replace the incidental `value.trim_matches('"')` expression with one
narrowly named schema-V2 transformation. After the scanner has proved a
complete valid lexeme, remove the opening ASCII quote and then remove the
maximal trailing quote run. Keep the function private to the track-filter
compiler boundary and add no generic string utility.

This is a behavior-preserving contract hardening, so the characterization tests
are expected to pass against the pre-change implementation. Verify that they
can catch the dangerous regression by temporarily changing the V2
transformation to remove exactly one closing delimiter: the terminal
escaped-quote assertion and complete historical golden must fail. Restore the
V2 implementation and rerun green; do not commit the mutation.

Update the authoritative track-filter section in
`docs/specs/voom-control-plane-design.md` with:

- ASCII double-quote delimiters;
- scalar-aware backslash scanning;
- no escape decoding or normalization;
- complete-leaf consumption;
- rejection of missing closing delimiters and terminal backslashes; and
- the pinned V2 result for a terminal escaped quote.

The test cases compile every normative example to its documented value.

Verification:

```text
cargo test -p voom-policy track_filter
cargo test -p voom-policy policy_fixtures
just fmt-check
just lint
```

Logical commit:

```text
fix(policy): pin V2 title escape semantics
```

## Step 3: Prove durable upgrade and rollback identity

Files:

- update `crates/voom-store/src/repo/policy/policies_test.rs`.

Create a small table of the two source/golden oracle pairs. For each pair, run
both paths against a fresh initialized SQLite database.

Upgrade path:

1. Insert a policy document and its historical immutable version directly,
   using the exact fixture source, pinned hash, schema version `2`, and compact
   serialization of the pre-change golden.
2. Set the current-version pointer and epoch to the state produced by a first
   accepted version.
3. Snapshot the complete `PolicyDocument`, ordered version list, and
   deterministic compiled JSON text.
4. Call `SqlitePolicyRepo::add_version` with identical source and a different
   timestamp.
5. Assert the returned version equals the existing version; the complete
   document, epoch, pointer, ordered version list, source text, source hash,
   schema version, and compiled projection remain unchanged; and exactly one
   version row exists.

Add a permanent ordering sentinel beside the two escape-oracle cases. Seed an
immutable row whose exact source bytes are rejected by the current compiler but
whose stored source hash and compiled policy identity agree. Uploading those
identical bytes must still return the existing version. This distinguishes the
required outer duplicate lookup from the later in-transaction duplicate check:
any attempt to compile first returns a policy diagnostic and fails the test.
The sentinel guards repository ordering only; it is not a published source
fixture and does not add an accepted grammar form.

Rollback path:

1. Create the same policy through
   `SqlitePolicyRepo::create_document_with_version`.
2. Assert stored source and hash equal the exact fixture and pinned digest.
3. Assert schema version remains `2`.
4. Assert the complete stored compiled JSON and its deterministic compact
   serialization equal the pre-change golden.

These tests inspect durable state. They do not infer success from a returned ID
or process status alone.

Mutation evidence:

1. Temporarily bypass the outer
   `get_version_by_document_and_hash` lookup in `add_version`. The rejected-source
   ordering sentinel must fail at compilation before reaching the
   in-transaction lookup.
2. Restore the outer lookup and rerun the sentinel green.
3. Temporarily change the V2 lowering transformation to remove exactly one
   closing delimiter. Both fresh-database rollback cases must fail their
   complete historical-golden comparison, including the terminal escaped-quote
   case.
4. Restore V2 lowering and rerun the complete store and policy fixture tests
   green.

Do not commit either mutation. Record the expected focused failures in the
implementation checkpoint.

The durable tests are expected to pass once their test-local seeding helpers
and assertions compile because production ordering and V2 output already exist.
Their mutation failures provide the required proof that the tests detect both
load-bearing regressions. Production repository behavior remains unchanged.

Verification:

```text
cargo test -p voom-store policy
just fmt-check
just lint
```

Logical commit:

```text
test(store): prove title escape upgrade compatibility
```

## Step 4: Integrate and verify

Re-read the complete branch diff for:

- any new source grammar or escape decoder;
- any changed source-hash preimage, schema version, compiled field, enum
  discriminator, or provenance format;
- accidental fixture regeneration;
- assertions that inspect only success status; and
- unrelated files.

Run:

```text
cargo test -p voom-policy track_filter
cargo test -p voom-policy policy_fixtures
cargo test -p voom-store policy
prek run
just ci
```

Transition #358 to review, run the full branch adversarial review, a
security-focused hostile-input review, and the three-lens simplification
review. Apply only defensible in-scope findings, rerun focused checks after
each change, then rerun `just ci`.

Before merge, rebase after #344 and #346, rerun this complete verification, push
without force, and wait for all GitHub checks plus a clean mergeable state.
