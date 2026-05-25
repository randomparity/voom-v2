# Issue 74 Chaos Policy Summary Design

## Goal

Finish the local Chaos Librarian churn runner's policy execution mode now that
public scan-to-policy-input creation exists. `CHAOS_EXECUTE_POLICY=1` should run
report-only policy flow, `CHAOS_EXECUTE_POLICY=execute` should run execution
flow, and every checkpoint should write enough JSONL data to inspect what
happened without opening the per-command output files.

## Current State

`scripts/chaos-e2e-local.sh` already accepts `CHAOS_EXECUTE_POLICY`, requires
`CHAOS_POLICY_VERSION_ID` when it is nonzero, scans the materialized library,
uses public `voom policy input create-from-scan`, and runs either
`compliance report` or `compliance execute`. The remaining gap is the checkpoint
summary contract: it records command output paths and a coarse policy status,
but not the created policy input ID, report status, execution ticket counts, or
policy command error codes requested by #74.

## Chosen Approach

Keep the shell runner as the integration boundary. Add small `jq` extraction
blocks after each public VOOM command and append structured fields to
`summary.jsonl`.

The runner will not call Rust test helpers, private SQL, or control-plane APIs.
It will continue to use only these public commands:

- `voom scan`
- `voom policy input create-from-scan`
- `voom compliance report`
- `voom compliance execute`

## Policy Modes

- `CHAOS_EXECUTE_POLICY=0` or unset: scan-only, unchanged default.
- `CHAOS_EXECUTE_POLICY=1`: create a policy input set from the first scanned
  media row and run `compliance report`.
- `CHAOS_EXECUTE_POLICY=report`: same as `1`, accepted as a clearer alias.
- `CHAOS_EXECUTE_POLICY=execute`: create a policy input set and run
  `compliance execute` with staging/output directories under the workdir.

Any other nonzero value fails before the Chaos run starts.

## Checkpoint Summary Shape

Each checkpoint JSON object will retain the existing fields and add:

- `scan_status`
- `scan_error_code`
- `policy_input_set_id`
- `policy_report_status`
- `policy_report_summary_status`
- `policy_error_code`
- `policy_execution_job_id`
- `policy_execution_submitted_node_count`
- `policy_execution_dispatch_count`
- `policy_execution_failure_count`
- `policy_ticket_count`

Fields that do not apply to the current mode or outcome are `null` or empty
strings, matching the script's current string-heavy summary style. Existing
`status` and `error_code` are retained for compatibility and continue to mirror
scan status/error.

## Failure Handling

The scan allowlist remains unchanged and narrow:

- `ARTIFACT_UNAVAILABLE`
- `MALFORMED_WORKER_RESULT`
- `ARTIFACT_CHECKSUM_MISMATCH`

Policy command failures are not allowlisted in this cycle. If policy input
creation, report, or execute returns nonzero, the runner records the command's
public error code in the checkpoint summary, prints a checkpoint-specific
message, and exits nonzero. This keeps policy execution mode fail-loud while
still leaving enough JSONL context in the preserved workdir.

## Tests

Update `scripts/test-chaos-e2e-local.sh` to exercise report mode using a fake
`voom` binary. The fake command should emit realistic JSON envelopes for:

- `scan --path`
- `policy input create-from-scan`
- `compliance report`

The test will assert that `summary.jsonl` contains a checkpoint with:

- `policy_status = "reported"`
- `policy_input_set_id = "101"`
- `policy_report_summary_status = "compliant"`
- `policy_ticket_count = 0`

This keeps the test fast and platform-independent while pinning the public
summary contract.

## Out of Scope

- Seeding policy documents inside the runner.
- Running the real Chaos Librarian ignored E2E suite in routine CI.
- Retrying policy failures.
- Multi-file policy input selection beyond the existing first scanned media row.
