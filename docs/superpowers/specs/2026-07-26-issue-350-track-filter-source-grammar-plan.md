# Issue 350 implementation plan: Enforce the published track-filter source grammar

Base branch: `main`

Base commit: `87df7af019351971b422cb1162a6994c041f2aac`

Guardrails:

- `cargo test -p voom-policy track_filter`
- `cargo test -p voom-policy policy_fixtures`
- `cargo test -p voom-plan fixtures`
- `cargo test -p voom-cli --test multi_phase_preview_envelope`
- `cargo test -p voom-control-plane --test published_grammar_corpus`
- `cargo test -p voom-control-plane --test remux_flow`
- `cargo test -p voom-control-plane --test audio_transcode_flow`
- `cargo test -p voom-control-plane --test audio_extract_flow`
- `prek run`
- `just ci`

## Step 1: Pin the exact source boundary

Files:

- `crates/voom-policy/src/compile/validate/conditions.rs`
- `crates/voom-policy/src/compile/validate/operations.rs`
- `crates/voom-policy/src/compile/validate_test.rs`
- `crates/voom-policy/src/compile/pipeline_test.rs`
- `crates/voom-policy/src/compile/track_filter.rs`
- `crates/voom-policy/src/compile/mod.rs`
- `crates/voom-policy/src/compile/lower/conditions.rs`
- `crates/voom-policy/src/compile/compiled_test.rs`
- a dedicated historical compiled track-filter fixture

Red tests:

- every published leaf compiles and lowers to its existing `TrackFilter`;
- all six channel comparators compile;
- nested Boolean grouping keeps `not`/`and`/`or` precedence;
- `keep`, `remove`, `transcode audio`, `extract audio`, filter-addressed
  `defaults`, filter-addressed `order tracks`, and `synthesize audio from` all
  accept a canonical filter;
- each consumer rejects `lang`, bare equality/list values, `title matches`,
  unpublished comparators, malformed lists, missing Boolean operands,
  unbalanced/empty groups, numeric overflow, non-ASCII digits, and trailing
  input;
- quoted tokens reject whitespace, commas, quotes, backslashes, and escapes;
- title strings decode `\"` and `\\` into their exact compiled value and reject
  every other malformed escape;
- every validator-accepted filter lowers to `Some(TrackFilter)`; and
- historical compiled `title_matches` JSON still deserializes.

Expected failure before implementation:

- aliases, bare values, `title matches`, shared comparison spellings, and some
  malformed lists compile successfully.

Implementation:

- replace separate validation/lowering recognizers with one recursive parser
  that returns the compiled `TrackFilter`;
- add a quote-aware stable-token list reader and quoted-string decoder;
- reuse the six-comparator and ASCII `u64` checks;
- route operation validation and lowering through the one parser;
- recursively validate parsed language values with the existing diagnostic;
- omit unpublished `lang` and `title matches` source branches; and
- leave the compiled enum and serde behavior unchanged.

Verification:

```text
cargo test -p voom-policy track_filter
cargo test -p voom-policy compiled
```

Commit:

`fix(policy): enforce published track filter grammar`

## Step 2: Canonicalize active policy sources

Files:

- current compilable `.voom` fixtures under `crates/voom-policy/fixtures/`;
- current policy strings in `voom-policy`, `voom-plan`, `voom-cli`, and
  `voom-control-plane` tests;
- current operator fixtures under
  `crates/voom-control-plane/tests/fixtures/policies/`;
- `crates/voom-control-plane/tests/published_grammar_corpus.rs`; and
- `docs/runbooks/operator-real-media-execution.md`;
- active compiled goldens whose source hashes change; and
- retained pre-#350 compiled fixtures used as historical comparison evidence.

Red tests:

- the narrowed compiler identifies every active fixture or integration policy
  still using an alias or bare token;
- rewritten source fixtures have the hash of their canonical source;
- historical and canonical compiled fixtures are completely equal after
  normalizing only `source_hash`; and
- the published corpus rejects representative unpublished bare-value forms.

Expected failure before implementation:

- active tests and fixtures depend on `lang` and unquoted list values.

Implementation:

- replace aliases with `language`;
- quote every language and codec token value;
- preserve parser-only and historical documentation examples that are not
  current compilable policy;
- extend the corpus source guard without claiming execution behavior; and
- update active compiled-golden hashes while retaining the pre-#350 values as
  compatibility fixtures.

Verification:

```text
cargo test -p voom-policy
cargo test -p voom-plan fixtures
cargo test -p voom-cli --test multi_phase_preview_envelope
cargo test -p voom-control-plane --test published_grammar_corpus
cargo test -p voom-control-plane --test remux_flow
cargo test -p voom-control-plane --test audio_transcode_flow
cargo test -p voom-control-plane --test audio_extract_flow
```

Commit:

`test(policy): use canonical track filter sources`

## Step 3: Review and ship

Review target:

- complete branch diff against `main`;
- issue #350 design and implementation plan.

Review focus:

- exact source acceptance and complete consumption;
- no accidental narrowing of general condition grammar;
- every filter consumer reaches the same validator;
- compiled-policy readability, especially `title_matches`;
- no semantic changes in rewritten fixtures; and
- no execution work from #331, #332, or #336.

Verification:

```text
prek run
just ci
```

Commit boundary:

- review fixes are separate conventional commits when behavior changes;
- formatting-only cleanup may accompany the nearest logical change.
