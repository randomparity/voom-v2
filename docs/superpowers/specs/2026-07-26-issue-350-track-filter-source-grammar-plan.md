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

Checked migration inventory:

| Class | Paths | Step 1 action |
|---|---|---|
| Active `voom-policy` valid source | `fixtures/policies/audio-transcode-eac3.voom`, `audio-transcode-extract.voom`, `filter-addressed-tracks.voom` | Rewrite aliases/bare values; update active hashes |
| Active `voom-policy` invalid source | `fixtures/policies/production-normalize-reduced.voom` | Rewrite its unrelated filter; preserve the single intended filtered-`exists` diagnostic |
| Historical compiled compatibility | `fixtures/compiled/production-normalize-reduced.json` | Leave unchanged and readable |
| Current compiler test source | `src/compile/compiled_test.rs`, `pipeline_test.rs`, `validate_test.rs` | Rewrite positive and unrelated-negative source; leave only cases explicitly reassigned to Step 2 rejection |
| `voom-plan` test source | `crates/voom-plan/src/fixtures_test.rs` | Rewrite |
| CLI test source | `crates/voom-cli/tests/multi_phase_preview_envelope.rs` | Rewrite |
| CLI generated snapshot | `crates/voom-cli/tests/snapshots/multi_phase_preview_envelope__compliance_report_previews_combined_multi_phase_policy.snap` | Review and accept changed source/plan/report hashes plus their derived IDs; all descriptive facts remain unchanged |
| Control-plane integration source | `audio_extract_flow.rs`, `audio_transcode_flow.rs`, `remux_flow.rs`, `phase_barrier_flow.rs`, `phase_barrier_combined_flow.rs` | Rewrite policy text and matching live test comments |
| Operator policy source | `tests/fixtures/policies/language-cleanup.voom`, `reference-user.voom` | Rewrite |
| Operator policy tests | `tests/sample_policies_plan.rs` | Run against rewritten fixture files |
| Current operator docs | `docs/runbooks/operator-real-media-execution.md` | Rewrite examples |
| Parser-only source | `crates/voom-policy/src/syntax/parser_test.rs` | Leave unchanged; parser permissiveness is intentional |
| Compiler implementation spellings | `compile/validate/conditions.rs`, `compile/validate/operations.rs`, `compile/lower/conditions.rs` | Leave for Step 2 |
| Corpus exclusions and grammar placeholders | `published_grammar_corpus.rs`, `published-grammar-coverage.md` | Leave existing guard strings/placeholders; extend rejection guard in Step 2 |

Historical/current golden pairs:

| Historical copy | Active canonical golden |
|---|---|
| `fixtures/compiled/historical-track-filter-source/audio-transcode-eac3.json` | `fixtures/compiled/audio-transcode-eac3.json` |
| `fixtures/compiled/historical-track-filter-source/audio-transcode-extract.json` | `fixtures/compiled/audio-transcode-extract.json` |
| `fixtures/compiled/historical-track-filter-source/filter-addressed-tracks.json` | `fixtures/compiled/filter-addressed-tracks.json` |

Before changing parser/lowering code, also add this exact historical source at
`crates/voom-policy/fixtures/historical/escaped-title-filters.voom` and capture
its current compiled JSON at
`crates/voom-policy/fixtures/compiled/historical-track-filter-source/escaped-title-filters.json`:

```text
policy "escaped-title-filters" {
  phase escaped_quote {
    keep subtitle where title contains "Director \"Cut\""
  }
  phase escaped_backslash {
    keep subtitle where title contains "Path\\Name"
  }
  phase interior_quotes {
    keep subtitle where title contains "\"Quoted\" middle"
  }
  phase other_pair {
    keep subtitle where title contains "Other\qPair"
  }
}
```

Step 2 recompiles these identical bytes and requires complete JSON equality,
including the unchanged `source_hash`, for all four historical values.

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

Before committing, run this exact inventory:

```text
rg -n --pcre2 \
  '\blang\b|language\s*==\s*[a-z]|language\s+in\s+\[(?!\s*")|codec\s+in\s+\[(?!\s*")' \
  crates docs/runbooks -g '*.rs' -g '*.voom' -g '*.md'
```

Every result must appear in the checked table above or be one of these explicit
Step 2/unchanged classes:

- Step 2 implementation:
  `compile/validate/conditions.rs`, `compile/validate/operations.rs`,
  `compile/lower/conditions.rs`;
- Step 2 rejection/conformance tests:
  `compile/pipeline_test.rs`, `compile/validate_test.rs`,
  `compile/compiled_test.rs`, `published_grammar_corpus.rs`;
- parser-only: `syntax/parser_test.rs`;
- grammar placeholder:
  `published-grammar-coverage.md`.

An unclassified result blocks the commit.

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
cargo insta review
cargo test -p voom-control-plane --test published_grammar_corpus
cargo test -p voom-control-plane --test remux_flow
cargo test -p voom-control-plane --test audio_transcode_flow
cargo test -p voom-control-plane --test audio_extract_flow
cargo test -p voom-control-plane --test phase_barrier_flow
cargo test -p voom-control-plane --test phase_barrier_combined_flow
cargo test -p voom-control-plane --test sample_policies_plan
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
- `crates/voom-control-plane/tests/published_grammar_corpus.rs`;
- `crates/voom-policy/fixtures/historical/escaped-title-filters.voom`; and
- `crates/voom-policy/fixtures/compiled/historical-track-filter-source/escaped-title-filters.json`.

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
- a property-style direct parser test iterates every ASCII byte for both
  `language == "<token>"` and `codec in ["<token>"]`: bytes in `a-z`, `0-9`,
  `_`, and `-` are accepted inside a non-empty token, and every other ASCII byte
  is rejected;
- empty tokens and representative non-ASCII scalars are rejected separately;
- codec-list cases prove the byte-domain result independently of semantic
  language-code validation;
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
- a direct `compile_ast` test bypasses validation with the optional-filter
  fixture below and expects exactly the listed compile diagnostic and no
  operation result;
- a separate direct `compile_ast` test uses the required-filter fixture below
  with its listed compile diagnostic and no operation result;
- the unterminated and complete-malformed public fixtures below assert their
  complete listed diagnostic objects; and
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
  optional and required filter parse errors as one compile-stage
  `PolicyDiagnostic` with the statement span and an exact actionable message;
- recursively validate parsed language values with the existing diagnostic;
- omit unpublished `lang` and `title matches` source branches; and
- leave the compiled enum and serde behavior unchanged.

Exact diagnostic fixtures and oracles:

```text
optional lowering:
policy "p" {
  phase a {
    keep audio where lang in ["eng"]
  }
}
```

Direct `compile_ast`: one diagnostic with code
`unknown_phase_statement_or_operation`, severity `error`, stage `compile`,
message `validated track filter could not be lowered`, span `29..61`, location
line 3 column 5, `suggestion: null`, and empty `related`.

```text
required lowering:
policy "p" {
  phase a {
    synthesize audio from lang in ["eng"] { codec aac channels 2 }
  }
}
```

Direct `compile_ast`: the same code, severity, stage, message, suggestion, and
related values; span `29..91`, location line 3 column 5.

```text
unterminated:
policy "p" {
  phase a {
    keep subtitle where title contains "broken
  }
}
```

Public `compile_policy`: outer code `POLICY_PARSE_ERROR`; one diagnostic with
code `unexpected_token`, severity `error`, stage `parse`, message
`unterminated string`, span `64..65`, location line 3 column 40,
`suggestion: null`, and empty `related`.

```text
complete malformed:
policy "p" {
  phase a {
    keep audio where language in ["eng",]
  }
}
```

Public `compile_policy`: outer code `POLICY_VALIDATION_ERROR`; one diagnostic
with code `unknown_phase_statement_or_operation`, severity `error`, stage
`validate`, message `unknown track filter predicate`, span `29..66`, location
line 3 column 5, `suggestion: null`, and empty `related`.

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
