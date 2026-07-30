#!/usr/bin/env bash
set -euo pipefail

# Real-device acceptance for Linux VAAPI video acceleration (issue #409).
#
# Deliberately not wired into `just ci`, exactly like
# scripts/accept-nvidia-video-acceleration.sh: it needs a VAAPI render node, a VA
# driver build that can encode HEVC, and membership in that node's group. Run it
# by hand on the acceptance host recorded in the design spec section 2.
#
# What it proves that no unit test can: a profile names a GPU *surface* format
# (nv12/p010) while the encoded file reports a *file* format (yuv420p /
# yuv420p10le), and the two are never equal for a conforming encode. Confusing
# them produced five bugs on this branch, four fatal to the happy path, and every
# one passed the unit tests. So every check below runs a real encode on the
# bound device and reads the real file back.
#
# Evidence is kept, not cleaned up: the run prints the directory holding the
# worker readiness lines, the executed ffmpeg argv, and the verified output
# facts for each device.

if (($# == 0)); then
  echo "usage: $0 <pci-address> [pci-address ...]   e.g. $0 0000:f4:00.0" >&2
  exit 2
fi

for pci_address in "$@"; do
  if [[ ! "$pci_address" =~ ^[[:xdigit:]]{4}:[[:xdigit:]]{2}:[[:xdigit:]]{2}\.[[:digit:]]$ ]]; then
    echo "invalid PCI address, want dddd:bb:dd.f: $pci_address" >&2
    exit 2
  fi
done

for command_name in cargo ffmpeg ffprobe jq; do
  if ! command -v "$command_name" >/dev/null; then
    echo "required command is missing: $command_name" >&2
    exit 2
  fi
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
cargo build \
  -p voom-cli \
  -p voom-ffmpeg-worker \
  -p voom-ffprobe-worker \
  -p voom-verify-artifact-worker

voom_bin="$repo_root/target/debug/voom"
worker_bin="$repo_root/target/debug/voom-ffmpeg-worker"
# Resolved once, before VOOM_FFMPEG_BIN is ever exported: source fixtures are
# encoded with libx264, and routing them through the argv-recording wrapper
# would plant a software encoder in the very log this script checks.
real_ffmpeg="$(command -v ffmpeg)"

task_tmp=$(mktemp -d /tmp/voom-vaapi-acceptance.XXXXXX)
worker_pid=""
worker_stdin_pid=""

# `worker run-local` is a foreground supervisor that retires on stdin EOF, so the
# FIFO writer is what holds it open and killing that writer is what shuts it down.
stop_worker() {
  if [[ -n "$worker_stdin_pid" ]]; then
    kill "$worker_stdin_pid" 2>/dev/null || true
    worker_stdin_pid=""
  fi
  if [[ -n "$worker_pid" ]]; then
    wait "$worker_pid" 2>/dev/null || true
    worker_pid=""
  fi
}

cleanup() {
  stop_worker
  echo "VAAPI acceptance evidence kept in: $task_tmp" >&2
}
trap cleanup EXIT

acceptance_failed=0

record_check() {
  local device=$1 outcome=$2 description=$3
  if [[ "$outcome" == "pass" ]]; then
    printf 'PASS  [%s] %s\n' "$device" "$description"
  else
    printf 'FAIL  [%s] %s\n' "$device" "$description"
    acceptance_failed=1
  fi
}

# Spec section 2.3: a testsrc source encoded without an explicit -pix_fmt lands
# in gbrp, which VAAPI cannot decode, and the resulting failures look like
# pipeline bugs rather than a bad fixture.
make_source_fixture() {
  local destination=$1
  "$real_ffmpeg" \
    -hide_banner \
    -loglevel error \
    -f lavfi \
    -i testsrc2=size=320x240:rate=30 \
    -t 1 \
    -c:v libx264 \
    -pix_fmt yuv420p \
    "$destination"
}

# Records every argv the worker executes. `-c:v` lives in the middle of a
# transcode command, so the only honest way to prove no software encoder ran is
# to read what was actually executed.
install_ffmpeg_recorder() {
  local bin_dir=$1 argv_log=$2
  mkdir -p "$bin_dir"
  cat >"$bin_dir/ffmpeg" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >>"$argv_log"
exec "$real_ffmpeg" "\$@"
EOF
  chmod +x "$bin_dir/ffmpeg"
}

start_worker() {
  local run_dir=$1 database_url=$2 pci_address=$3
  mkfifo "$run_dir/worker.stdin"
  sleep 900 >"$run_dir/worker.stdin" &
  worker_stdin_pid=$!
  "$voom_bin" --database-url "$database_url" worker run-local \
    --kind ffmpeg \
    --vaapi-device "$pci_address" \
    --vaapi-max-sessions 1 \
    <"$run_dir/worker.stdin" \
    >"$run_dir/worker.stdout" \
    2>"$run_dir/worker.stderr" &
  worker_pid=$!
  local waited=0
  while [[ ! -s "$run_dir/worker.stdout" ]]; do
    if ((waited >= 300)); then
      echo "worker did not report readiness within 300s: $run_dir/worker.stderr" >&2
      return 1
    fi
    sleep 1
    waited=$((waited + 1))
  done
}

# One hermetic pipeline: its own database, library copy, worker, and output
# directory, so a run can never inherit another run's committed artifacts or
# resume state. Writes the promoted artifact path to "$run_dir/artifact.path".
run_pipeline() {
  local run_dir=$1 pci_address=$2 argv_log=$3 profile_name=$4
  shift 4
  local profile_args=("$@")
  local database_url="sqlite://$run_dir/voom.db"

  mkdir -p "$run_dir/library" "$run_dir/staging" "$run_dir/output"
  make_source_fixture "$run_dir/library/source.mkv"
  install_ffmpeg_recorder "$run_dir/bin" "$argv_log"
  export VOOM_FFMPEG_BIN="$run_dir/bin/ffmpeg"

  "$voom_bin" --database-url "$database_url" init >"$run_dir/init.json"
  "$voom_bin" --database-url "$database_url" profile create \
    --name "$profile_name" \
    --encoder hevc_vaapi \
    --qp 24 \
    --output-container mkv \
    "${profile_args[@]}" >"$run_dir/profile.json"
  printf 'policy "%s" {\n  phase transcode {\n    transcode video to hevc using profile "%s"\n  }\n}\n' \
    "$profile_name" "$profile_name" >"$run_dir/policy.voom"
  "$voom_bin" --database-url "$database_url" policy create \
    --slug "$profile_name" \
    --file "$run_dir/policy.voom" >"$run_dir/policy.json"
  local policy_version_id
  policy_version_id=$(jq -r '.data.version.version_id' "$run_dir/policy.json")

  "$voom_bin" --database-url "$database_url" scan \
    --path "$run_dir/library" >"$run_dir/scan.json"
  "$voom_bin" --database-url "$database_url" policy input create-from-scan \
    --all --slug "$profile_name" >"$run_dir/input.json"
  local input_set_id
  input_set_id=$(jq -r '.data.input_set.input_set_id' "$run_dir/input.json")

  start_worker "$run_dir" "$database_url" "$pci_address"
  local execute_status=0
  "$voom_bin" --database-url "$database_url" compliance execute \
    --policy-version-id "$policy_version_id" \
    --input-set-id "$input_set_id" \
    --staging-root "$run_dir/staging" \
    --output-dir "$run_dir/output" >"$run_dir/execute.json" || execute_status=$?
  stop_worker
  unset VOOM_FFMPEG_BIN
  if ((execute_status != 0)); then
    jq -r '.error | "compliance execute failed: \(.code) \(.message)"' \
      "$run_dir/execute.json" >&2
    return 1
  fi

  find "$run_dir/output" -type f -name '*.mkv' >"$run_dir/artifact.path"
  if [[ ! -s "$run_dir/artifact.path" ]]; then
    echo "compliance execute promoted no artifact under $run_dir/output" >&2
    return 1
  fi
}

expect_output_facts() {
  local device=$1 run_dir=$2 expected=$3 description=$4
  local artifact observed
  artifact=$(head -n 1 "$run_dir/artifact.path")
  if ! observed=$(ffprobe -v error -select_streams v:0 \
    -show_entries stream=codec_name,profile,pix_fmt \
    -of csv=p=0 "$artifact"); then
    record_check "$device" fail "$description: ffprobe failed on $artifact"
    return
  fi
  printf '%s\n' "$observed" >"$run_dir/output-facts.txt"
  if [[ "$observed" == "$expected" ]]; then
    record_check "$device" pass "$description ($observed)"
  else
    record_check "$device" fail "$description: want $expected, got $observed"
  fi
}

# The check that would have caught the planner bug: a conforming VAAPI output
# was re-planned for transcode forever, because the planner compared the
# profile's surface format against the file's encoded format. Plans the same
# policy over the produced artifact in a database that has never seen the
# source, and requires the planner to want nothing.
expect_convergence() {
  local device=$1 source_run_dir=$2 converge_dir=$3 profile_name=$4 description=$5
  shift 5
  local profile_args=("$@")
  local database_url="sqlite://$converge_dir/voom.db"
  local artifact_dir
  artifact_dir=$(dirname "$(head -n 1 "$source_run_dir/artifact.path")")

  mkdir -p "$converge_dir"
  "$voom_bin" --database-url "$database_url" init >"$converge_dir/init.json"
  "$voom_bin" --database-url "$database_url" profile create \
    --name "$profile_name" \
    --encoder hevc_vaapi \
    --qp 24 \
    --output-container mkv \
    "${profile_args[@]}" >"$converge_dir/profile.json"
  cp "$source_run_dir/policy.voom" "$converge_dir/policy.voom"
  "$voom_bin" --database-url "$database_url" policy create \
    --slug "$profile_name" \
    --file "$converge_dir/policy.voom" >"$converge_dir/policy.json"
  local policy_version_id
  policy_version_id=$(jq -r '.data.version.version_id' "$converge_dir/policy.json")

  "$voom_bin" --database-url "$database_url" scan \
    --path "$artifact_dir" >"$converge_dir/scan.json"
  "$voom_bin" --database-url "$database_url" policy input create-from-scan \
    --all --slug "$profile_name" >"$converge_dir/input.json"
  local input_set_id
  input_set_id=$(jq -r '.data.input_set.input_set_id' "$converge_dir/input.json")

  "$voom_bin" --database-url "$database_url" compliance report \
    --policy-version-id "$policy_version_id" \
    --input-set-id "$input_set_id" >"$converge_dir/report.json"

  local executable_checks planned_nodes
  executable_checks=$(jq -r '.data.report.summary.executable_check_count' "$converge_dir/report.json")
  planned_nodes=$(jq -r '[.data.plan.nodes[] | select(.status != "no_op")] | length' \
    "$converge_dir/report.json")
  if [[ "$executable_checks" == "0" && "$planned_nodes" == "0" ]]; then
    record_check "$device" pass "$description"
  else
    record_check "$device" fail \
      "$description: $executable_checks executable check(s), $planned_nodes non-no-op node(s)"
  fi
}

# Spec section 7: no software encoder is ever substituted, a failure is a
# failure. This reads every argv the worker executed, probes included.
expect_no_software_encoder() {
  local device=$1 argv_log=$2
  local offenders
  if ! offenders=$(grep -F -n \
    -e libx265 \
    -e libsvtav1 \
    -e libaom-av1 \
    -e hevc_nvenc \
    "$argv_log"); then
    record_check "$device" pass "no software encoder in any executed ffmpeg argv"
    return
  fi
  printf '%s\n' "$offenders" >&2
  record_check "$device" fail "software encoder found in executed ffmpeg argv"
}

accept_device() {
  local pci_address=$1
  local device_dir="$task_tmp/${pci_address//[:.]/-}"
  local argv_log="$device_dir/ffmpeg-argv.log"
  mkdir -p "$device_dir"
  : >"$argv_log"

  # Identity, read off the worker's own readiness line exactly as the NVIDIA
  # script reads its UUID. `-vaapi_device <node>` is what binds the encode, so
  # this is the assertion that the node the worker resolved is the address the
  # operator configured.
  local bound
  bound=$(
    env \
      VOOM_WORKER_ID=1 \
      VOOM_WORKER_EPOCH=0 \
      VOOM_WORKER_SECRET=0123456789abcdef0123456789abcdef \
      VOOM_VAAPI_DEVICE="$pci_address" \
      VOOM_VAAPI_MAX_SESSIONS=1 \
      "$worker_bin" </dev/null
  )
  printf '%s\n' "$bound" >"$device_dir/bound.txt"
  if [[ "$bound" == *"\"pci_address\":\"$pci_address\""* ]]; then
    record_check "$pci_address" pass "worker bound the configured PCI address"
  else
    record_check "$pci_address" fail "worker readiness reported a different device: $bound"
    return 1
  fi

  # Main, 8-bit. The profile asks for the nv12 surface; a conforming file
  # reports yuv420p.
  run_pipeline "$device_dir/main8" "$pci_address" "$argv_log" hevc-vaapi-main8 \
    --pixel-format nv12 --codec-profile main
  expect_output_facts "$pci_address" "$device_dir/main8" "hevc,Main,yuv420p" \
    "8-bit nv12/main encode wrote the expected file facts"

  # Main10. The startup probe encodes 8-bit only (finding F9), so this run is
  # the only end-to-end proof that the Main10 half of issue #409 works.
  run_pipeline "$device_dir/main10" "$pci_address" "$argv_log" hevc-vaapi-main10 \
    --pixel-format p010 --codec-profile main10
  expect_output_facts "$pci_address" "$device_dir/main10" "hevc,Main 10,yuv420p10le" \
    "10-bit p010/main10 encode wrote the expected file facts"

  # Hardware decode into hardware encode: frames never leave the device, and
  # `-hwaccel_output_format vaapi` errors instead of falling back to software.
  run_pipeline "$device_dir/decode" "$pci_address" "$argv_log" hevc-vaapi-decode \
    --pixel-format nv12 --codec-profile main --decode vaapi
  expect_output_facts "$pci_address" "$device_dir/decode" "hevc,Main,yuv420p" \
    "VAAPI-decoded source encoded on the same node"

  expect_convergence "$pci_address" "$device_dir/main8" "$device_dir/converge-main8" \
    hevc-vaapi-main8 "re-planning the 8-bit output plans no transcode" \
    --pixel-format nv12 --codec-profile main
  expect_convergence "$pci_address" "$device_dir/main10" "$device_dir/converge-main10" \
    hevc-vaapi-main10 "re-planning the 10-bit output plans no transcode" \
    --pixel-format p010 --codec-profile main10

  expect_no_software_encoder "$pci_address" "$argv_log"
}

for pci_address in "$@"; do
  # Sequential, unlike the NVIDIA script's parallel fan-out: each device runs a
  # foreground worker supervisor and several full control-plane executions, and
  # interleaving those makes a failure unreadable.
  accept_device "$pci_address"
done

if ((acceptance_failed != 0)); then
  echo "VAAPI video acceleration acceptance FAILED" >&2
  exit 1
fi

echo "VAAPI video acceleration acceptance passed for $# device(s)"
