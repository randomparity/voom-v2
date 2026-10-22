#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
chaos_dir="$repo_root/third_party/chaos-librarian"
source "$repo_root/scripts/acceptance-bootstrap.sh"

scenario="${CHAOS_SCENARIO:-active-library-churn.yaml}"
duration="${CHAOS_DURATION:-10m}"
speed="${CHAOS_SPEED:-5x}"
checkpoint_interval="${CHAOS_CHECKPOINT_INTERVAL:-30s}"
execute_policy="${CHAOS_EXECUTE_POLICY:-0}"
policy_version_id="${CHAOS_POLICY_VERSION_ID:-}"

for tool in git uv jq cargo python3 ffmpeg ffprobe mkvmerge; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "required tool not found: $tool" >&2
    exit 1
  fi
done

if [[ "$execute_policy" != "0" && -z "$policy_version_id" ]]; then
  echo "CHAOS_POLICY_VERSION_ID is required when CHAOS_EXECUTE_POLICY is nonzero" >&2
  exit 1
fi
case "$execute_policy" in
  0|1|report|execute) ;;
  *)
    echo "CHAOS_EXECUTE_POLICY must be 0, 1, report, or execute" >&2
    exit 1
    ;;
esac

case "$scenario" in
  */*) scenario_path="$scenario" ;;
  *) scenario_path="$chaos_dir/tests/fixtures/scenarios/$scenario" ;;
esac

if [[ ! -f "$scenario_path" ]]; then
  echo "scenario not found: $scenario_path" >&2
  exit 1
fi

workdir="${CHAOS_WORKDIR:-$(mktemp -d -t voom-chaos-local.XXXXXX)}"
mkdir -p "$workdir"
workdir=$(cd "$workdir" && pwd -P)
run_dir="$workdir/run"
library_dir="$run_dir/library"
db="$workdir/voom.db"
url="sqlite://$db"
summary="$workdir/summary.jsonl"
voom_bin="${VOOM_BIN:-${CARGO_TARGET_DIR:-$repo_root/target}/debug/voom}"

chaos_pid=""
cleanup_run() {
  acceptance_stop || echo "acceptance shutdown failed" >&2
  if [[ -n "$chaos_pid" ]] && kill -0 "$chaos_pid" 2>/dev/null; then
    kill "$chaos_pid" 2>/dev/null || true
    wait "$chaos_pid" 2>/dev/null || true
  fi
  echo "preserved chaos E2E workdir: $workdir" >&2
}
trap cleanup_run EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

git -C "$repo_root" submodule status third_party/chaos-librarian | grep -E '^ 9f4c3bf7b7908484ad179d288dd59f3f85185053 ' >/dev/null
if [[ -n "$(git -C "$chaos_dir" status --short --untracked-files=no)" ]]; then
  echo "Chaos Librarian submodule has tracked modifications" >&2
  exit 1
fi
cd "$chaos_dir"
uv sync --locked
uv run chaos-librarian capabilities --json | jq -e '
  .ready_for.materialize_static == true and
  .ready_for.materialize_filesystem_mutations == true and
  .ready_for.materialize_media_mutations == true
' >/dev/null

cd "$repo_root"
cargo build -p voom-cli -p voom-api -p voom-node-agent -p voom-scan-worker -p voom-hash-worker -p voom-ffprobe-worker \
  -p voom-verify-artifact-worker -p voom-ffmpeg-worker -p voom-mkvtoolnix-worker
voom_bin="$(cd "$(dirname "$voom_bin")" && pwd -P)/$(basename "$voom_bin")"

cd "$chaos_dir"
uv run chaos-librarian run "$scenario_path" --out "$run_dir" --duration "$duration" --speed "$speed" --json > "$workdir/chaos-run.json" &
chaos_pid=$!

started_at="$(date +%s)"
while [[ ! -d "$library_dir" ]] || ! find "$library_dir" -type f \( -name '*.mkv' -o -name '*.mp4' -o -name '*.avi' -o -name '*.mov' \) -print -quit | grep -q .; do
  if ! kill -0 "$chaos_pid" 2>/dev/null; then
    wait "$chaos_pid" || true
    echo "chaos-librarian exited before creating scannable media under $library_dir" >&2
    exit 1
  fi
  now="$(date +%s)"
  if (( now - started_at > 30 )); then
    echo "timed out waiting for scannable media under $library_dir" >&2
    exit 1
  fi
  sleep 0.1
done

acceptance_start "$workdir" --input "$library_dir" \
  --staging "$workdir/staging" --output "$workdir/output" --bin-dir "$(dirname "$voom_bin")"
input_root=$(acceptance_root input "$library_dir")

checkpoint=0
while kill -0 "$chaos_pid" 2>/dev/null; do
  checkpoint=$((checkpoint + 1))
  scan_out="$workdir/scan-$checkpoint.json"
  set +e
  "$voom_bin" --database-url "$url" scan --root "$input_root" > "$scan_out"
  scan_rc=$?
  set -e
  error_code="$(jq -r '.error.code // empty' "$scan_out")"
  status="$(jq -r '.status' "$scan_out")"
  if [[ "$scan_rc" -ne 0 && "$error_code" != "ARTIFACT_UNAVAILABLE" && "$error_code" != "MALFORMED_WORKER_RESULT" && "$error_code" != "ARTIFACT_CHECKSUM_MISMATCH" ]]; then
    echo "non-allowlisted scan failure at checkpoint $checkpoint: $error_code" >&2
    exit 1
  fi
  policy_status="skipped"
  policy_reason=""
  policy_input_out=""
  policy_report_out=""
  fatal_error=""
  policy_input_set_id=""
  policy_report_status=""
  policy_report_summary_status=""
  policy_error_code=""
  policy_execution_job_id=""
  policy_execution_submitted_node_count=""
  policy_execution_dispatch_count=""
  policy_execution_failure_count=""
  policy_ticket_count=""
  if [[ "$execute_policy" != "0" && "$status" = "ok" ]]; then
    if [[ "$(jq -r '.data.status // empty' "$scan_out")" == "succeeded" ]]; then
      input_slug="chaos-local-$checkpoint"
      policy_input_out="$workdir/policy-input-$checkpoint.json"
      set +e
      "$voom_bin" --database-url "$url" policy input create-from-scan \
        --slug "$input_slug" \
        --root "$input_root" > "$policy_input_out"
      policy_input_rc=$?
      set -e
      policy_error_code="$(jq -r '.error.code // empty' "$policy_input_out")"
      if [[ "$policy_input_rc" -ne 0 ]]; then
        policy_status="failed"
        policy_reason="policy input creation failed"
        fatal_error="policy input creation failed at checkpoint $checkpoint: $policy_error_code"
      else
        policy_input_set_id="$(jq -r '.data.input_set.input_set_id // empty' "$policy_input_out")"
        policy_report_out="$workdir/policy-report-$checkpoint.json"
        if [[ "$execute_policy" = "execute" ]]; then
          set +e
          "$voom_bin" --database-url "$url" compliance execute \
            --policy-version-id "$policy_version_id" \
            --input-set-id "$policy_input_set_id" \
            --staging-root "$workdir/staging" \
            --output-dir "$workdir/output" > "$policy_report_out"
          policy_report_rc=$?
          set -e
          policy_status="executed"
        else
          set +e
          "$voom_bin" --database-url "$url" compliance report \
            --policy-version-id "$policy_version_id" \
            --input-set-id "$policy_input_set_id" > "$policy_report_out"
          policy_report_rc=$?
          set -e
          policy_status="reported"
        fi
        policy_report_status="$(jq -r '.status // empty' "$policy_report_out")"
        policy_report_summary_status="$(jq -r '.data.report.summary.status // empty' "$policy_report_out")"
        policy_error_code="$(jq -r '.error.code // empty' "$policy_report_out")"
        policy_execution_job_id="$(jq -r '.data.summary.job_id // empty' "$policy_report_out")"
        policy_execution_submitted_node_count="$(jq -r '.data.summary.submitted_node_count // empty' "$policy_report_out")"
        policy_execution_dispatch_count="$(jq -r '.data.summary.dispatch_count // empty' "$policy_report_out")"
        policy_execution_failure_count="$(jq -r '.data.summary.failure_count // empty' "$policy_report_out")"
        policy_ticket_count="$(jq -r '(.data.tickets // []) | length' "$policy_report_out")"
        if [[ "$policy_report_rc" -ne 0 ]]; then
          policy_reason="policy $policy_status failed"
          fatal_error="policy $policy_status failed at checkpoint $checkpoint: $policy_error_code"
        fi
      fi
    else
      policy_status="skipped"
      policy_reason="scan session did not complete"
    fi
  elif [[ "$execute_policy" = "0" ]]; then
    policy_reason="CHAOS_EXECUTE_POLICY is unset or zero"
  else
    policy_reason="scan status was $status"
  fi
  jq -n \
    --argjson checkpoint "$checkpoint" \
    --arg status "$status" \
    --arg error_code "$error_code" \
    --arg scan_out "$scan_out" \
    --arg policy_status "$policy_status" \
    --arg policy_reason "$policy_reason" \
    --arg policy_input_out "$policy_input_out" \
    --arg policy_report_out "$policy_report_out" \
    --arg policy_input_set_id "$policy_input_set_id" \
    --arg policy_report_status "$policy_report_status" \
    --arg policy_report_summary_status "$policy_report_summary_status" \
    --arg policy_error_code "$policy_error_code" \
    --arg policy_execution_job_id "$policy_execution_job_id" \
    --arg policy_execution_submitted_node_count "$policy_execution_submitted_node_count" \
    --arg policy_execution_dispatch_count "$policy_execution_dispatch_count" \
    --arg policy_execution_failure_count "$policy_execution_failure_count" \
    --arg policy_ticket_count "$policy_ticket_count" \
    '{
      checkpoint:$checkpoint,
      status:$status,
      error_code:$error_code,
      scan_status:$status,
      scan_error_code:$error_code,
      scan_out:$scan_out,
      policy_status:$policy_status,
      policy_reason:$policy_reason,
      policy_input_out:$policy_input_out,
      policy_report_out:$policy_report_out,
      policy_input_set_id:$policy_input_set_id,
      policy_report_status:$policy_report_status,
      policy_report_summary_status:$policy_report_summary_status,
      policy_error_code:$policy_error_code,
      policy_execution_job_id:$policy_execution_job_id,
      policy_execution_submitted_node_count:$policy_execution_submitted_node_count,
      policy_execution_dispatch_count:$policy_execution_dispatch_count,
      policy_execution_failure_count:$policy_execution_failure_count,
      policy_ticket_count:($policy_ticket_count | if . == "" then null else tonumber end)
    }' >> "$summary"
  if [[ -n "$fatal_error" ]]; then
    echo "$fatal_error" >&2
    exit 1
  fi
  sleep "$checkpoint_interval"
done

wait "$chaos_pid"
if (( checkpoint == 0 )); then
  echo "Chaos ended before a scan checkpoint; increase CHAOS_DURATION for owner startup" >&2
  exit 1
fi
acceptance_stop
echo "chaos local summary: $summary"
