#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
chaos_dir="$repo_root/third_party/chaos-librarian"

scenario="${CHAOS_SCENARIO:-active-library-churn.yaml}"
duration="${CHAOS_DURATION:-10m}"
speed="${CHAOS_SPEED:-5x}"
checkpoint_interval="${CHAOS_CHECKPOINT_INTERVAL:-30s}"
execute_policy="${CHAOS_EXECUTE_POLICY:-0}"
policy_version_id="${CHAOS_POLICY_VERSION_ID:-}"
policy_container="${CHAOS_POLICY_CONTAINER:-mp4}"
policy_video_codec="${CHAOS_POLICY_VIDEO_CODEC:-h264}"
preserve="${CHAOS_PRESERVE_OUTPUT:-1}"
cleanup="${CHAOS_CLEANUP:-0}"

for tool in git uv jq cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "required tool not found: $tool" >&2
    exit 1
  fi
done

if [[ "$execute_policy" != "0" && -z "$policy_version_id" ]]; then
  echo "CHAOS_POLICY_VERSION_ID is required when CHAOS_EXECUTE_POLICY is nonzero" >&2
  exit 1
fi

case "$scenario" in
  */*) scenario_path="$scenario" ;;
  video-transcode-*.yaml) scenario_path="$repo_root/crates/voom-cli/tests/fixtures/chaos/$scenario" ;;
  *) scenario_path="$chaos_dir/tests/fixtures/scenarios/$scenario" ;;
esac

if [[ ! -f "$scenario_path" ]]; then
  echo "scenario not found: $scenario_path" >&2
  exit 1
fi

workdir="${CHAOS_WORKDIR:-$(mktemp -d -t voom-chaos-local.XXXXXX)}"
run_dir="$workdir/run"
db="$workdir/voom.db"
url="sqlite://$db"
summary="$workdir/summary.jsonl"
voom_bin="${VOOM_BIN:-$repo_root/target/debug/voom}"

chaos_pid=""
cleanup_run() {
  if [[ -n "$chaos_pid" ]] && kill -0 "$chaos_pid" 2>/dev/null; then
    kill "$chaos_pid" 2>/dev/null || true
    wait "$chaos_pid" 2>/dev/null || true
  fi
  if [[ "$preserve" = "1" ]]; then
    echo "preserved chaos E2E workdir: $workdir" >&2
  elif [[ "$cleanup" = "1" ]]; then
    rm -rf "$workdir"
  else
    echo "workdir left in place: $workdir" >&2
  fi
}
trap cleanup_run EXIT INT TERM
mkdir -p "$workdir"

git -C "$repo_root" submodule status third_party/chaos-librarian | grep -E '^ 057a4033a3a9ae14fef664ab82f2c31e1a223544 ' >/dev/null
if [[ -n "$(git -C "$chaos_dir" status --short --untracked-files=no)" ]]; then
  echo "Chaos Librarian submodule has tracked modifications" >&2
  exit 1
fi
cd "$chaos_dir"
uv sync --locked
uv run chaos-librarian capabilities --json | jq -e '.ready_for.materialize_media_mutations == true' >/dev/null

cd "$repo_root"
cargo build -p voom-cli -p voom-ffprobe-worker -p voom-verify-artifact-worker -p voom-ffmpeg-worker
cargo run -q -p voom-cli -- --database-url "$url" init >/dev/null

cd "$chaos_dir"
uv run chaos-librarian run "$scenario_path" --out "$run_dir" --duration "$duration" --speed "$speed" --json > "$workdir/chaos-run.json" &
chaos_pid=$!

started_at="$(date +%s)"
while [[ ! -d "$run_dir" ]] || ! find "$run_dir" -type f \( -name '*.mkv' -o -name '*.mp4' -o -name '*.avi' -o -name '*.mov' \) -print -quit | grep -q .; do
  if ! kill -0 "$chaos_pid" 2>/dev/null; then
    wait "$chaos_pid" || true
    echo "chaos-librarian exited before creating scannable media under $run_dir" >&2
    exit 1
  fi
  now="$(date +%s)"
  if (( now - started_at > 30 )); then
    echo "timed out waiting for scannable media under $run_dir" >&2
    exit 1
  fi
  sleep 0.1
done

checkpoint=0
while kill -0 "$chaos_pid" 2>/dev/null; do
  checkpoint=$((checkpoint + 1))
  scan_out="$workdir/scan-$checkpoint.json"
  set +e
  "$voom_bin" --database-url "$url" scan --path "$run_dir" > "$scan_out"
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
  if [[ "$execute_policy" != "0" && "$status" = "ok" ]]; then
    scanned_row="$(jq -c '.data.files[]? | select(.status == "scanned" and .file_version_id and .media_snapshot_id) | {file_version_id, media_snapshot_id}' "$scan_out" | head -n 1)"
    if [[ -n "$scanned_row" ]]; then
      file_version_id="$(jq -r '.file_version_id' <<<"$scanned_row")"
      media_snapshot_id="$(jq -r '.media_snapshot_id' <<<"$scanned_row")"
      input_slug="chaos-local-$checkpoint"
      policy_input_out="$workdir/policy-input-$checkpoint.json"
      "$voom_bin" --database-url "$url" policy input create-from-scan \
        --slug "$input_slug" \
        --file-version-id "$file_version_id" \
        --media-snapshot-id "$media_snapshot_id" \
        --container "$policy_container" \
        --video-codec "$policy_video_codec" > "$policy_input_out"
      input_set_id="$(jq -r '.data.input_set.input_set_id' "$policy_input_out")"
      policy_report_out="$workdir/policy-report-$checkpoint.json"
      if [[ "$execute_policy" = "execute" ]]; then
        mkdir -p "$workdir/staging-$checkpoint" "$workdir/output-$checkpoint"
        "$voom_bin" --database-url "$url" compliance execute \
          --policy-version-id "$policy_version_id" \
          --input-set-id "$input_set_id" \
          --staging-root "$workdir/staging-$checkpoint" \
          --output-dir "$workdir/output-$checkpoint" > "$policy_report_out"
        policy_status="executed"
      else
        "$voom_bin" --database-url "$url" compliance report \
          --policy-version-id "$policy_version_id" \
          --input-set-id "$input_set_id" > "$policy_report_out"
        policy_status="reported"
      fi
    else
      policy_status="skipped"
      policy_reason="scan had no scanned file with file_version_id and media_snapshot_id"
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
    '{checkpoint:$checkpoint,status:$status,error_code:$error_code,scan_out:$scan_out,policy_status:$policy_status,policy_reason:$policy_reason,policy_input_out:$policy_input_out,policy_report_out:$policy_report_out}' >> "$summary"
  sleep "$checkpoint_interval"
done

wait "$chaos_pid"
echo "chaos local summary: $summary"
