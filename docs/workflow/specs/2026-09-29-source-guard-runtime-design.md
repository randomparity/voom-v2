# Measured source-guard runtime reduction — #637

## Problem and authority

Issue #637 requires faster measured SQL-boundary and durable-payload guards without
weakening AST coverage, actionable diagnostics, selftests, or hook integration.
Scope token `q637-91e6c83a` binds the eight-field WORK:SCOPE charter.
The operator approved its exact exclusions on 2026-09-29, then approved payload
one-pass POSIX awk processing and the refined structural SQL file prefilter.
Complexity M gives a fixed 250-line design denominator; review is iterating.

## Design

Keep the existing shell entrypoints and existing Rust AST rules. No dependency,
hook recipe, Cargo, CI scheduling, or production Rust change is needed.

For SQL, an AST candidate rule finds `identifier` or `type_identifier` nodes exactly
`sqlx` or `r#sqlx`. Deduplicate their files, preserving deterministic ordering, and
run the existing rules and file-local alias/nested-use processing on those files.
Every forbidden namespace starts with such an identifier inside its own file;
strings and comments cannot introduce that namespace. Existing scope exclusions
for sibling/integration test files still apply before candidate discovery.
Validate scanner status and output at this new stage. Keep existing diagnostics
and alias parser unchanged. An empty candidate set yields the existing OK result.

For payloads, batch the two existing AST shape rules. Validate scanner status and
match coordinates before processing. Read each scoped source once into POSIX awk
arrays, then reproduce the old per-item region: contiguous attribute/comment/blank
lines above the anchor plus at most 41 header lines below, stopping at blank/end
or an opening brace. Classify only anchored derive/serde/exemption lines.
Preserve multiline-derive failures first, followed by named-struct failures and
inline-tagged-enum failures in the previous deterministic match ordering.
Preserve exit 0 for clean/empty scope, 1 for violations, and 2 for tool/input errors.
The former scanner-error swallowing must be removed because #637 explicitly
requires the new path to fail on tool errors. Processor errors must also fail.

## Alternatives and measurement

Unchanged AST scan batching alone is insufficient: eight scans spent about 0.17s
while the macOS SQL guard spent 14.790s. SQL inventory processing dominates.
A temporary scanner wrapper restricted the unmodified guard to the 33 candidate
files out of 101 production files; it completed in 8.492s. This is an experiment,
not the final implementation timing. Rewriting the SQL parser would add semantic
risk beyond the measured bottleneck. Payload subprocesses are replaced in place.

## Success and validation

Run the four existing just guard/selftest recipes before/after on macOS arm64/BSD
and Linux arm64/GNU. Use the same ast-grep 0.45.3 and just 1.58.0 on both snapshots.
Linux Docker measurements disclose container startup and bind-mounted storage;
they do not claim equivalence to hosted CI. Report final per-recipe timing and exit.

Selftests add scanner failure and malformed-output cases, payload processor errors,
exact diagnostics and aggregate counts, and SQL candidate raw-identifier,
macro-token, comment-only and no-candidate controls. Preserve existing fixtures.
New tests must be observed red before implementation, then green after.
Run focused just recipes after each implementation edit, configured prek hooks on
commits, final `just ci` once for the assembled reviewed candidate, then hosted CI.

## Failure model

Actors/deployments: local developers and repository CI on macOS/Linux, using the
installed AST scanner and ordinary scoped Rust paths. Assets: AST boundary coverage,
file-local association, deterministic diagnostic ordering, and nonzero tool failures.
Accepted classes: timing variation is bounded by reporting environment and samples;
no guaranteed speedup is claimed for every machine. Scanner/source concurrent mutation
is outside this performance change's snapshot model, as in existing guards.
Covered elsewhere: multiline-serde correctness #259; hooks #485; profiles/runner
#638/#639; scheduling/sharding #640/#641; unmeasured guards separately authorized work.

## Threat model

Existing boundaries: repository Rust text and scanner JSON become guard decisions.
No boundary is added or widened. Trust the installed scanner and shell/awk tools;
contributors control source text. AST kinds remain authoritative, match IDs and
coordinates are checked, and source/scanner/processor failures cannot emit clean OK.
No source text becomes executable shell code. Deliberately compromised installed
executables and broader repository paths policy are outside this local guard change.
