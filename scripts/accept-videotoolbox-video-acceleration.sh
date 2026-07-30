#!/usr/bin/env bash
set -euo pipefail

max_sessions=${1:-1}
if [[ ! "$max_sessions" =~ ^([1-9]|1[0-6])$ ]]; then
  echo "usage: $0 [max-sessions: 1..16]" >&2
  exit 2
fi

require_text() {
  local label=$1
  local pattern=$2
  local value=$3

  if ! rg -q "$pattern" <<<"$value"; then
    echo "acceptance evidence is missing $label" >&2
    exit 1
  fi
}

if [[ $(uname -s) != "Darwin" || $(uname -m) != "arm64" ]]; then
  echo "VideoToolbox acceptance requires Apple silicon macOS" >&2
  exit 2
fi

for command_name in cargo ffmpeg ffprobe mkfifo rg sqlite3 trash; do
  if ! command -v "$command_name" >/dev/null; then
    echo "required command is missing: $command_name" >&2
    exit 2
  fi
done

task_tmp=$(mktemp -d /tmp/voom-videotoolbox-acceptance.XXXXXX)
mkfifo "$task_tmp/worker.stdin"
exec 3<>"$task_tmp/worker.stdin"
exec 4>"$task_tmp/worker.stdin"
exec 5<"$task_tmp/worker.stdin"
exec 3>&-
worker_input_open=true
worker_pid=""

close_worker_input() {
  if [[ "$worker_input_open" == "true" ]]; then
    exec 4>&-
    worker_input_open=false
  fi
}

cleanup() {
  close_worker_input
  exec 5<&-
  if [[ -n "$worker_pid" ]] && kill -0 "$worker_pid" 2>/dev/null; then
    kill -TERM "$worker_pid" 2>/dev/null || true
    wait "$worker_pid" 2>/dev/null || true
  fi
  trash "$task_tmp"
}
trap cleanup EXIT

cargo test -p voom-ffmpeg-worker \
  real_videotoolbox_preflight_proves_host_pipelines \
  -- --ignored
cargo test -p voom-control-plane videotoolbox_requirement_
cargo test -p voom-control-plane videotoolbox_evidence_
cargo build -p voom-cli -p voom-ffmpeg-worker

database_url="sqlite://$task_tmp/voom.db"
target/debug/voom --database-url "$database_url" init >"$task_tmp/init.json"
target/debug/voom \
  --database-url "$database_url" \
  worker run-local \
  --kind ffmpeg \
  --videotoolbox \
  --videotoolbox-max-sessions "$max_sessions" \
  <&5 4>&- 5<&- \
  >"$task_tmp/worker.stdout" \
  2>"$task_tmp/worker.stderr" &
worker_pid=$!

deadline=$((SECONDS + 405))
while ! rg -q '"status":"ready"' "$task_tmp/worker.stdout"; do
  if ! kill -0 "$worker_pid" 2>/dev/null; then
    wait "$worker_pid" || true
    echo "VideoToolbox worker exited before readiness" >&2
    cat "$task_tmp/worker.stderr" >&2
    exit 1
  fi
  if ((SECONDS >= deadline)); then
    echo "VideoToolbox worker did not become ready within 405 seconds" >&2
    exit 1
  fi
  sleep 1
done

claim=$(
  sqlite3 -json "$task_tmp/voom.db" \
    "SELECT backend, hardware_token, supervisor_start_identity, capacity
		 FROM accelerator_claims"
)
capability=$(
  sqlite3 -json "$task_tmp/voom.db" \
    "SELECT hardware, extra FROM worker_capabilities
		 WHERE operation = 'transcode_video'"
)

require_text "VideoToolbox claim backend" '"backend":"video_toolbox"' "$claim"
require_text "declared claim capacity" "\"capacity\":$max_sessions" "$claim"
require_text "null supervisor start identity" \
  '"supervisor_start_identity":null' "$claim"
require_text "H.264 VideoToolbox encoder" 'h264_videotoolbox' "$capability"
require_text "HEVC VideoToolbox encoder" 'hevc_videotoolbox' "$capability"
require_text "H.264 hardware decoder" '\\"codec\\":\\"h264\\"' "$capability"
require_text "HEVC hardware decoder" '\\"codec\\":\\"hevc\\"' "$capability"
require_text "AV1 hardware decoder" '\\"codec\\":\\"av1\\"' "$capability"
require_text "8-bit and 10-bit pixel formats" \
  '\\"pixel_formats\\":\[\\"yuv420p\\",\\"yuv420p10le\\"\]' "$capability"

if rg -q '[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}' \
  "$task_tmp/worker.stdout" "$task_tmp/worker.stderr" \
  <(printf '%s\n%s\n' "$claim" "$capability"); then
  echo "raw UUID-shaped platform identity escaped into acceptance evidence" >&2
  exit 1
fi

kill -TERM "$worker_pid"
close_worker_input
wait "$worker_pid"
worker_pid=""

if [[ $(wc -l <"$task_tmp/worker.stdout") -ne 2 ]]; then
  echo "run-local stdout did not contain exactly readiness and retirement lines" >&2
  exit 1
fi
rg -q '"status":"retired"' "$task_tmp/worker.stdout"

claims_after_shutdown=$(
  sqlite3 "$task_tmp/voom.db" "SELECT COUNT(*) FROM accelerator_claims"
)
if [[ "$claims_after_shutdown" != "0" ]]; then
  echo "VideoToolbox claim remained after clean retirement" >&2
  exit 1
fi

echo "VideoToolbox video acceleration acceptance passed at capacity $max_sessions"
