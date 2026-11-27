# Implementation plan: option-like worker paths (#260)

Authority: issue #260 and WORK:SCOPE `q260-9f97589b`.
Spec: `docs/workflow/specs/2026-11-27-option-like-worker-paths-design.md`.
Lane: full-spec. Complexity M. Fixed denominator: 250 changed lines.
Expected implementation size: 140–240 changed lines (M) — two local predicates, error mapping, boundary calls, and benign rejection regressions.

Architecture: keep path validation and typed errors in each adapter.
Stack: Rust 1.95.0, tokio processes, existing worker protocol.

## Global Constraints

Mirror ffmpeg's first-encoded-byte predicate. Preserve existing error mappings.
No shared protocol export, dependency, migration, or schema change.
Sibling unit tests follow AGENTS.md. No merge or cleanup authority.

## Task 1: ffprobe defensive boundary

- Add benign sibling tests showing first-byte-dash rejection before spawn and
  acceptance of ordinary/prefixed paths by the predicate.
- Add adapter-local validation and ConfigInvalid mapping without changing
  existing failure mappings.
- focused-test: `cargo test -p voom-ffprobe-worker option_like`.
- Files: `crates/voom-ffprobe-worker/src/ffprobe.rs` and `ffprobe_test.rs`.
- Interface: `run_ffprobe_json(&Path, &FfprobeConfig) -> Result<Value, FfprobeError>`
  stays unchanged. Local `reject_option_like_path(&Path) -> Result<(), FfprobeError>`.
- Expected red: first-byte-dash produces a spawn error instead of ConfigInvalid.
- Contract: rejected path has `FailureClass::MalformedWorkerResult`,
  `ErrorCode::ConfigInvalid`, and payload stage `input_path`.

## Task 2: mkvmerge defensive boundaries

- Add benign sibling tests for identify and remux input/output rejection before
  spawn; preserve accepted normal paths and internal dash components.
- Validate raw input/output in `handle_remux` before file access or identify.
- Validate before argument construction and identify subprocess construction,
  using the existing ConfigInvalid error.
- focused-test: `cargo test -p voom-mkvtoolnix-worker option_like`.
- Files: `crates/voom-mkvtoolnix-worker/src/{mkvmerge,handler}.rs` and sibling tests.
- Interface: crate-local `reject_option_like_path(&str, &Path) -> Result<(), MkvtoolnixError>`; public signatures unchanged.
- Expected red: identify/remux reports a spawn error; handler reports a filesystem
  error before the malformed output is rejected.
- Contract: identify/remux reject first-byte-dash paths; handler error class
  is MalformedWorkerResult with ConfigInvalid code, preserving existing mapping.

## Verification and review

Run focused tests red before implementation and green afterward. Run the two
worker package suites. Independent design review and scope audit precede build;
independent branch review and security review follow it. Simplify only inside
approved scope. Run `just ci`, installed commit hooks, and exact-head remote CI.
No migration, persistence, schema, dependency, or AI surface changes apply, so
no corresponding task tests are applicable. Leave a reviewed merge-ready PR;
no merge or cleanup is authorized.
