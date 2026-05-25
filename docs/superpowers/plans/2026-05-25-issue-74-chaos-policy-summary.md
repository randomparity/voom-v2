# Chaos Policy Summary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add durable policy/report/execution summary fields to `scripts/chaos-e2e-local.sh` checkpoints for #74.

**Architecture:** Keep the existing shell runner as the only implementation surface. Use public `voom` CLI JSON envelopes and `jq` extraction to enrich each checkpoint JSONL row.

**Tech Stack:** Bash, `jq`, existing `voom` CLI JSON envelopes, existing script-level smoke test.

---

## File Structure

- Modify `scripts/chaos-e2e-local.sh`: validate policy mode, capture policy command return codes, extract public IDs/status/counts, and write enriched checkpoint JSON.
- Modify `scripts/test-chaos-e2e-local.sh`: extend the fake `voom` binary and run the local script in report mode to assert the new summary contract.

## Task 1: Enrich Local Chaos Policy Summaries

**Files:**
- Modify: `scripts/test-chaos-e2e-local.sh`
- Modify: `scripts/chaos-e2e-local.sh`

- [ ] **Step 1: Write the failing script test**

Update the fake `voom` command in `scripts/test-chaos-e2e-local.sh` to branch on the public command words:

```bash
cat >"$fake_voom" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  if [[ "${args[$i]}" == "--database-url" ]]; then
    unset "args[$i]" "args[$((i + 1))]"
    break
  fi
done
args=("${args[@]}")
case "${args[*]}" in
  scan\ --path\ *)
    library="${args[$((${#args[@]} - 1))]}"
    if ! find "$library" -type f -name '*.mkv' -print -quit | grep -q .; then
      echo '{"status":"error","error":{"code":"MISSING_LIBRARY"}}'
      exit 2
    fi
    echo '{"status":"ok","data":{"files":[{"status":"scanned","file_version_id":11,"media_snapshot_id":22}]}}'
    ;;
  policy\ input\ create-from-scan\ *)
    echo '{"status":"ok","data":{"input_set":{"input_set_id":101}}}'
    ;;
  compliance\ report\ *)
    echo '{"status":"ok","data":{"report":{"summary":{"status":"compliant"}}}}'
    ;;
  *)
    echo "unexpected voom invocation: ${args[*]}" >&2
    exit 1
    ;;
esac
SH
```

Set these environment variables when invoking the script:

```bash
CHAOS_EXECUTE_POLICY=1 \
CHAOS_POLICY_VERSION_ID=7 \
```

After the existing `test -s` assertion, add:

```bash
jq -e '
  select(.policy_status == "reported")
  | select(.policy_input_set_id == "101")
  | select(.policy_report_summary_status == "compliant")
  | select(.policy_ticket_count == 0)
' "$chaos_workdir/summary.jsonl" >/dev/null
```

- [ ] **Step 2: Run RED**

Run:

```bash
scripts/test-chaos-e2e-local.sh
```

Expected: fail because `policy_input_set_id`, `policy_report_summary_status`, and `policy_ticket_count` are missing from summary rows.

- [ ] **Step 3: Implement policy mode validation**

In `scripts/chaos-e2e-local.sh`, add validation after the existing policy version check:

```bash
case "$execute_policy" in
  0|1|report|execute) ;;
  *)
    echo "CHAOS_EXECUTE_POLICY must be 0, 1, report, or execute" >&2
    exit 1
    ;;
esac
```

- [ ] **Step 4: Add summary variables before policy execution**

Inside the checkpoint loop, after `policy_report_out=""`, initialize:

```bash
policy_input_set_id=""
policy_report_status=""
policy_report_summary_status=""
policy_error_code=""
policy_execution_job_id=""
policy_execution_submitted_node_count=""
policy_execution_dispatch_count=""
policy_execution_failure_count=""
policy_ticket_count=""
```

- [ ] **Step 5: Extract policy input ID and fail loud on create errors**

Replace the raw `policy input create-from-scan` invocation with:

```bash
set +e
"$voom_bin" --database-url "$url" policy input create-from-scan \
  --slug "$input_slug" \
  --file-version-id "$file_version_id" \
  --media-snapshot-id "$media_snapshot_id" \
  --container "$policy_container" \
  --video-codec "$policy_video_codec" > "$policy_input_out"
policy_input_rc=$?
set -e
policy_error_code="$(jq -r '.error.code // empty' "$policy_input_out")"
if [[ "$policy_input_rc" -ne 0 ]]; then
  echo "policy input creation failed at checkpoint $checkpoint: $policy_error_code" >&2
  exit 1
fi
policy_input_set_id="$(jq -r '.data.input_set.input_set_id // empty' "$policy_input_out")"
```

- [ ] **Step 6: Extract report/execute status and counts**

For execute mode, wrap the command with `set +e`, capture `policy_report_rc`,
then extract:

```bash
policy_report_status="$(jq -r '.status // empty' "$policy_report_out")"
policy_report_summary_status="$(jq -r '.data.report.summary.status // empty' "$policy_report_out")"
policy_error_code="$(jq -r '.error.code // empty' "$policy_report_out")"
policy_execution_job_id="$(jq -r '.data.execution.job_id // empty' "$policy_report_out")"
policy_execution_submitted_node_count="$(jq -r '.data.execution.submitted_node_count // empty' "$policy_report_out")"
policy_execution_dispatch_count="$(jq -r '.data.execution.dispatch_count // empty' "$policy_report_out")"
policy_execution_failure_count="$(jq -r '.data.execution.failure_count // empty' "$policy_report_out")"
policy_ticket_count="$(jq -r '(.data.tickets // []) | length' "$policy_report_out")"
```

For report mode, run `compliance report`, set `policy_status="reported"`, and
extract the same fields. If `policy_report_rc` is nonzero in either mode, print
the `policy_error_code` and exit 1.

- [ ] **Step 7: Extend checkpoint JSON**

Extend the final `jq -n` invocation with `--arg` bindings for every new field
and include these keys in the object:

```jq
scan_status:$status,
scan_error_code:$error_code,
policy_input_set_id:$policy_input_set_id,
policy_report_status:$policy_report_status,
policy_report_summary_status:$policy_report_summary_status,
policy_error_code:$policy_error_code,
policy_execution_job_id:$policy_execution_job_id,
policy_execution_submitted_node_count:$policy_execution_submitted_node_count,
policy_execution_dispatch_count:$policy_execution_dispatch_count,
policy_execution_failure_count:$policy_execution_failure_count,
policy_ticket_count:($policy_ticket_count | if . == "" then null else tonumber end)
```

- [ ] **Step 8: Run GREEN**

Run:

```bash
scripts/test-chaos-e2e-local.sh
```

Expected: pass.

- [ ] **Step 9: Verify and commit**

Run:

```bash
shellcheck scripts/chaos-e2e-local.sh scripts/test-chaos-e2e-local.sh || true
git diff --check
```

If `shellcheck` is unavailable, record that it was skipped. Then commit:

```bash
git add scripts/chaos-e2e-local.sh scripts/test-chaos-e2e-local.sh
git commit -m "feat: summarize chaos policy execution"
```
