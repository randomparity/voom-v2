# Bounded chaos implementation plan

Branch feat/bounded-chaos-soak-747; BASE_BRANCH main. Frozen scope q747-7668b66b.
Expected implementation size: 750–1050 changed lines (M). The original 450–650 estimate
under-counted the regular-file/parent-link/content publication checks, bounded diagnostics,
resource/interrupt failure tests and private DB/credential ownership adapters. The current
file inventory measures about 800 implementation changes; each maps to the approved criteria.
Review-stage finalization and linked-renderer regressions add roughly 90 lines after the
882-line build-complete snapshot; these address existing lifecycle/publication criteria.
The frozen denominator stays 250; this corrects planning evidence, not scope or complexity.
Execution is Cast: tasks share the supervisor lifecycle, environment and evidence contract.

## Task 1 — supervisor and executable contract evidence
Create scripts/chaos-e2e-bounded.py and scripts/test-chaos-e2e-bounded.py; add a just
chaos-e2e-bounded-selftest recipe. Interface: python3 chaos-e2e-bounded.py MODE -- COMMAND,
MODE local|heavy; environment CHAOS_WORKDIR, CHAOS_MAX_SECONDS, CHAOS_MAX_MIB,
CHAOS_MAX_PROCESSES, CHAOS_BUNDLE_MIB. Child environment adds CHAOS_SUPERVISED=1,
CHAOS_EVIDENCE_ROOT and VOOM_ACCEPTANCE_PROCESS_GROUP=inherit. Bundle: WORKDIR/bundle.
Verification Mode: focused-test; lifecycle, statuses, diagnostic retention, allowlist and
caps; new selftest subprocess cases; expected red missing supervisor, then green
just chaos-e2e-bounded-selftest (exit 0). Use subprocess.Popen(start_new_session=True),
monotonic polling, os.killpg, file-backed logs and bounded finalization; copy only regular
allowlisted files under the owned workdir. Never follow symlinks. Record metadata and omissions.
Grace TERM 2s then KILL and reap. Visualizer reads only the validated bundle copy,
with an offline 10s deadline and separate group;
its status cannot replace original result. JSON status metadata is written even without replay.

## Task 2 — reuse entrypoints and retain test evidence
Modify scripts/chaos-e2e-local.sh to exec supervisor before setup, keeping the supervised
body intact. Generate a scenario only when CHAOS_SEED and CHAOS_LANE are both supplied;
use upstream generate --profile fuzz-regression --seed SEED --lane LANE --out PATH --json.
Capture producer stderr. Modify acceptance-bootstrap.py launch so start_new_session is
false only for the supervisor's inherit environment. Under that environment, place temporary
credentials inside the private workdir so forced cleanup leaves nothing outside its ownership. Extend shell fake tests accordingly.
Modify chaos_librarian.rs with retained_tempdir(): io::Result<TempDir>; when
CHAOS_EVIDENCE_ROOT exists, create there and disable cleanup, otherwise ordinary TempDir.
Migrate chaos_librarian_e2e.rs and support/corpus_matrix.rs tempdir callers and materialize helper.
Use TempDatabase::new_in in support/voom_cli.rs under the same evidence root, so private DB
bytes count toward the live cap but are never bundled. Record helper command output and
comparison.json before returning or propagating errors for heavy diagnostics. Supervisor cleans
heavy raw evidence after bounded bundle collection; no raw deletion in Rust Drop.
Interfaces consumed: supervisor environment and owned evidence root from task 1.
Verification Mode: focused-test; seed/lane, integration, retention and conditional bootstrap
grouping; shell and Python tests; expected red absent seed generation/retention, green
just chaos-e2e-local-script-test, just chaos-e2e-bounded-selftest and
just acceptance-bootstrap-selftest. Meaningful Rust retention test asserts files survive drop
only for the opt-in root; standard tempdirs still disappear.

## Task 3 — just, workflow and operations
Wrap existing chaos-e2e-ci build/test commands with the heavy supervisor; use a private inner
recipe for the existing commands without recursion. Soak sets explicit seed 248/lane/profile
and 7500s default cap, preserving duration/speed overrides. Workflow sets a fresh RUNNER_TEMP
workdir and uploads only its bundle on failure using actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9
(v7.0.2, verified official tag via GitHub API), 7-day retention. Keep triggers and notification deduplication unchanged.
Verification Mode: focused-test; workflow artifact path/failure condition and preserved triggers,
just entrypoint separation; structural selftest plus just check-workflows; expected red missing
bundle upload/wrapper, then green checks. No required PR chaos gate.
Document caps (sampling overshoot), synthetic-only artifact policy, incomplete timeout bundles,
repeat seeds and upstream replay command, offline visualization, measured equipped cost.
Verification Mode: task-test-not-applicable for prose; operator instructions require human
review plus exercised commands, not tests matching wording.

## Integration and rollback
Run just fmt; focused recipes; equipped just chaos-e2e-ci; final RUST_TEST_THREADS=4 just ci
before push. Own worktree target only. Mandatory hooks run unmodified. Revert this PR to roll
back; no persisted product change. Preserve private raw evidence until bundle finalized, then
remove heavy evidence; never delete caller directories or upload raw evidence. Open findings,
owned deferrals and follow-up candidates: none at planning time.
