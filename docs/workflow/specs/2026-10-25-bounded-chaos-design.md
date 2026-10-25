# Bounded chaos execution (#747)

## Problem and scope
The local runner stops one producer PID; bootstrap children run in separate sessions.
Heavy tests use temporary directories that disappear on failure. Existing weekly/manual
CI cannot upload a reproducible failure. #747 and parent #740 require bounded execution,
cleanup and diagnostics, with existing entrypoints and scheduling preserved.

## Design
Use one Python standard-library supervisor around local and heavy entrypoints. It starts
one process group, polls aggregate workdir logical bytes and group process count every
200ms, and enforces a monotonic deadline. On normal exit, failure, timeout or interrupt,
it sends TERM then KILL to the group, with a bounded grace. Bootstrap children inherit
that group only under the supervisor's explicit environment; ordinary bootstrap behavior
remains unchanged. No production execution ownership changes.

Limits are positive bounded integers: elapsed seconds, aggregate MiB, process count,
and retained bundle MiB. Defaults: local 900 seconds, heavy 2700 seconds, 4096 MiB live,
128 processes, 64 MiB published diagnostics; soak gets 7500 seconds for its existing 2h
wall-clock duration. These are abort thresholds, not filesystem quotas: concurrent writes
can overshoot between samples. Setup/build time counts. Cargo build artifacts and uv's
shared cache are outside the run-data limit. Output logs are inside it. Run one lane per
invocation; soak explicitly defaults seed 248, fuzz-regression/adversarial-media. Ordinary
local scenario and upstream lane selection remain unchanged. Generation requires both
explicit seed and lane; generated scenario and generator receipt are retained.

Only a new, empty owned workdir is accepted. Supervised bootstrap credentials stay inside
that private tree; abrupt cleanup cannot strand them in an unowned sibling. Child outputs live there. Heavy tests use
an opt-in retained tempdir helper under its evidence subdirectory, including corpus_matrix and shared DB fixtures; ordinary tests retain
their existing cleanup. On success or failure, collect diagnostics before deleting heavy
raw evidence. Local raw evidence remains private and subject to the live cap; publication
uses the bundle only. Timeouts/interrupts may have incomplete replay data: record missing
artifacts honestly, never fabricate replayability before materialization finishes.

The bundle includes runner seed/lane/profile, exact pin, selected tool version strings,
limits, duration/exit/reason, scenario YAML, replay.json, manifest.json, journal.jsonl,
validation.json, per-asset reports, observed-state/comparison JSON and owned process logs.
Use exact basename/pattern allowlisting, reject symlinks and special files, cap copied bytes,
and record omitted files. Never upload databases, node config, credentials, media or the
whole workdir. No environment dump. An offline upstream visualizer is attempted from a
retained run using uv run --offline python scripts/visualize_run.py RUN --output PATH, with its own short deadline and process cleanup. Its failure
is diagnostic only; retain the original command status. Generated HTML shares the bundle
budget. CI uploads only that directory on failure, with short retention and a pinned action.

## Failure model
Actors are maintainers and weekly/manual Linux CI executing repository-owned commands
against synthetic fixtures. Files may disappear during abort; record missing evidence.
Preserve the original nonzero status, including 124 timeout, 130 interrupt, 143 TERM;
disk/process cap exits are 125. Success requires the command's success and clean shutdown.
TERM-ignoring descendants inside the inherited group must be killed. Host crash, SIGKILL
of the supervisor, deliberately escaping sessions and malicious repository code are outside
this opt-in diagnostic runner's process boundary; the CI job timeout remains an outer guard.
A replay bundle cannot exist before upstream writes it; timeout bundles identify partial
runs. Exact encoded bytes across tools are not promised (#746 owns semantic replay).
Private raw bootstrap directories may contain configuration and stay excluded from uploads.

## Alternatives
A shell watchdog cannot reliably own descendants across the existing bootstrap sessions.
A new service or scheduler is unnecessary; reuse just and the workflow. A full workdir
upload would expose config/DB/media and defeat bounded retention. Filesystem quotas are
not portable across the supported Linux/macOS developer hosts; sampled abort thresholds
are explicit and tested. No dependency, schema or ADR decision is introduced.

## Success and validation
Focused supervisor tests construct nested TERM-ignoring children, forced failure, timeout,
interrupt, disk/process overflow and symlink/secret publication attempts. Assert status,
descendant death, bounded artifacts and replay evidence retention. Existing shell fakes
verify integration and seed/lane selection. An equipped heavy run measures actual cost;
run final just ci with RUST_TEST_THREADS=4. Weekly/manual triggers, default ci separation,
notification deduplication and artifact-only-on-failure behavior are structurally checked.
