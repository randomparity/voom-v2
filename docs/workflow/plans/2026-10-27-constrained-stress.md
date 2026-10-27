# Implementation plan — #756

Expected implementation size: 180–260 changed lines (M).
File map: remote_runner.rs sending loop/envelope metadata (45–65), sibling HTTP
regressions and their small server fixture (135–195). Design denominator remains
250 from the live assessment in WORK:SCOPE q756-a8b62e91.
Branch `feat/constrained-stress-756`; base `main`.

## One coupled Cast task: completion-window replay

Files: `crates/voom-fakes/src/remote_runner.rs` and sibling
`crates/voom-fakes/src/remote_runner_test.rs`.
Interfaces: preserve `complete(&self, &ActiveWorker, LeaseId, JsonValue, String)`
and generic `post<T>(&self, &str, &str, JsonValue)` signatures. Add private
`post_request<T>(&self, &str, &str, JsonValue, bool)` returning
`Result<T, RemoteRunnerError>`; only complete passes true. Existing callers use
post, which delegates with false. No ownership migration or obsolete public path.

Verification inventory:
- Mode: focused-test. `completion_window_replays_frozen_request` serves identical
  timeout envelopes then success, captures body bytes/key, checks every request
  identical and returns success; red is current REQUEST_TIMEOUT API error.
- Mode: focused-test. `completion_window_rejects_inconsistent_envelopes` varies
  schema, command, status, data and error code; each must error after one request.
- Mode: focused-test. `completion_window_stops_on_terminal_error` serves timeout
  then conflict and checks two requests and the original terminal API code.
- Mode: focused-test. `ordinary_post_does_not_replay_completion_window` returns a
  valid window to ordinary post and checks one request plus REQUEST_TIMEOUT error.
Green command: `cargo test -p voom-fakes --lib completion_window`.

Steps:
1. Add the focused HTTP cases using an axum ephemeral listener, captured requests
   and deterministic response sequence; call private complete/post from sibling
   tests with typed ActiveWorker IDs. Bound each test request with a hang guard.
   No DB and no artificial sleep are required. Run focused command, require red
   on replay case's current REQUEST_TIMEOUT. Record output and exact input tree.
2. Route complete through post_request(..., true), post through false. Serialize
   body once with serde_json::to_vec, set JSON content type, preserve auth/key.
   Decode the existing API envelope with optional schema_version/command metadata.
   For opted-in HTTP408 require the spec's exact window fields before continuing;
   yield once between windows. For other responses retain existing result/error
   branch. No other caller changes or generic retry abstraction.
3. Run focused command green and original `just stress` green. Run the constrained
   baseline/disk stress recipes where local resource-controller prerequisites permit.
   Compare failures with the original symptom before correction; no guessed fixes.
4. Run `just test` assembled integration and `just ci` final local candidate; hooks
   remain mandatory. Commit only after required checks pass. Runtime rollback is
   reverting this isolated client change; there is no persisted state change.

Acceptance: each Success bullet in the spec maps to the cases above or the original
stress/full guardrails. Design findings: both independent passes identified the same false hard drain
bound; accepted-fixed by removing that promise and disclosing pending-window
consequences in Failure model. No lifecycle machinery added. No deferrals, rejected
findings, or follow-up candidates.
