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

## Step 1: Canonicalize active policy sources without narrowing behavior

Files:

- every active valid or invalid `.voom` fixture under
  `crates/voom-policy/fixtures/`;
- current policy strings in `voom-policy`, `voom-plan`, `voom-cli`, and
  `voom-control-plane` tests;
- current operator fixtures under
  `crates/voom-control-plane/tests/fixtures/policies/`;
- `crates/voom-policy/src/fixtures/policy_fixtures_test.rs`;
- `crates/voom-control-plane/tests/published_grammar_corpus.rs`;
- `docs/runbooks/operator-real-media-execution.md`;
- active compiled goldens whose source hashes change;
- retained pre-#350 compiled fixtures used as historical comparison evidence;
  and
- `crates/voom-policy/fixtures/diagnostics/production-normalize-reduced.json`
  when canonical source movement changes only its span offsets.

Tests:

- rewritten source fixtures have the hash of their canonical source;
- each retained historical compiled fixture and its canonical replacement are
  completely equal after normalizing only `source_hash`;
- `production-normalize-reduced.voom` continues to emit exactly its one intended
  filtered-`exists` diagnostic with the same code, stage, message, and
  suggestion; only source-derived span/location values may change;
- all active valid fixtures and integration policies remain green under the old
  permissive compiler; and
- the published corpus guard still accepts every canonical corpus file.

Implementation:

- capture the pre-#350 compiled fixture JSON before changing source;
- replace aliases with `language`;
- quote every language and codec token value;
- canonicalize active invalid fixtures as well as valid ones so grammar
  enforcement adds no unrelated diagnostic;
- preserve parser-only tests and historical docs that are not compiled current
  source;
- update active compiled-golden hashes and source-derived invalid-fixture
  locations only; and
- add normalized historical/current compatibility assertions.

Expected state before the commit:

- canonical rewrites compile with the current permissive compiler;
- active golden hashes and later diagnostic locations fail until updated;
- normalized historical/current values already match.

Verification:

```text
cargo test -p voom-policy policy_fixtures
cargo test -p voom-policy
cargo test -p voom-plan fixtures
cargo test -p voom-cli --test multi_phase_preview_envelope
cargo test -p voom-control-plane --test published_grammar_corpus
cargo test -p voom-control-plane --test remux_flow
cargo test -p voom-control-plane --test audio_transcode_flow
cargo test -p voom-control-plane --test audio_extract_flow
prek run
```

Commit:

`test(policy): use canonical track filter sources`

## Step 2: Pin the exact source boundary

Files:

- `crates/voom-policy/src/compile/validate/conditions.rs`
- `crates/voom-policy/src/compile/validate/operations.rs`
- `crates/voom-policy/src/compile/validate_test.rs`
- `crates/voom-policy/src/compile/pipeline_test.rs`
- `crates/voom-policy/src/compile/track_filter.rs`
- `crates/voom-policy/src/compile/mod.rs`
- `crates/voom-policy/src/compile/lower/conditions.rs`
- `crates/voom-policy/src/compile/lower/operations.rs`
- `crates/voom-policy/src/compile/compiled_test.rs`
- the dedicated historical compiled track-filter fixture.

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
- title strings retain the exact historical `strip_quotes` result, including
  escaped quote, escaped backslash, terminal escaped quote, and other
  backslash pairs under the identical source hash;
- 64 recursive filter levels compile while deeper repeated `not` and grouped
  filters fail validation without panicking;
- more than 64 flat `and` and `or` siblings compile because each child receives
  an independent copy of the path-local remaining-depth budget;
- a Boolean split plus 63 nested `not` descents on one child succeeds, while the
  same split plus 64 nested `not` descents fails validation without affecting
  the sibling or panicking;
- three-or-more top-level `and` and `or` children retain n-ary compiled arrays
  in source order, with mixed precedence and grouping unchanged;
- a forced second-pass failure in an optional consumer returns a compile
  diagnostic and cannot lower to `None`;
- a separate forced second-pass failure in required `synthesize audio from`
  returns a compile diagnostic and cannot lower to `None`;
- unterminated strings retain their general-parser diagnostic while complete
  malformed clauses use the track-filter validation diagnostic; and
- historical compiled `title_matches` JSON still deserializes.

Expected failure before implementation:

- aliases, bare values, `title matches`, shared comparison spellings, and some
  malformed lists compile successfully.

Implementation:

- replace separate validation/lowering recognizers with one recursive parser
  that returns `Result<TrackFilter, ParseError>`;
- add shared optional/required clause extractors that preserve absence but
  distinguish every present-clause failure;
- add a quote-aware stable-token list reader and quoted-string scanner that
  applies the unchanged historical `strip_quotes` value transformation;
- enforce a 64-level remaining-depth budget across `not`, groups, and Boolean
  child descent without decrementing one shared budget across siblings;
- preserve n-ary same-operator lowering and existing mixed precedence;
- reuse the six-comparator and ASCII `u64` checks;
- route operation validation and diagnostics-bearing lowering through the same
  extractor and parser;
- change `lower_operation` and `lower_synthesize` call chains to propagate
  optional and required filter parse errors as `PolicyDiagnostic` values;
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
