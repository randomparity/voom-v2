#!/usr/bin/env bash
set -Eeuo pipefail

max_sessions=${1:-1}
if [[ ! "$max_sessions" =~ ^([1-9]|1[0-6])$ ]]; then
  echo "usage: $0 [max-sessions: 1..16]" >&2
  exit 2
fi

require_text() {
  local label=$1
  local pattern=$2
  local value=$3

  if ! rg -q -- "$pattern" <<<"$value"; then
    echo "acceptance evidence is missing $label" >&2
    exit 1
  fi
}

if [[ $(uname -s) != "Darwin" || $(uname -m) != "arm64" ]]; then
  echo "VideoToolbox acceptance requires Apple silicon macOS" >&2
  exit 2
fi

for command_name in cargo ffmpeg ffprobe jq mkfifo rg sqlite3 trash; do
  if ! command -v "$command_name" >/dev/null; then
    echo "required command is missing: $command_name" >&2
    exit 2
  fi
done

task_tmp=$(mktemp -d /tmp/voom-videotoolbox-acceptance.XXXXXX)
task_tmp=$(cd "$task_tmp" && pwd -P)
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
trap 'echo "VideoToolbox acceptance failed: $BASH_COMMAND" >&2' ERR

voom() {
  local exit_code
  if target/debug/voom --database-url "$database_url" "$@"; then
    return 0
  else
    exit_code=$?
    echo "voom command failed ($exit_code): $*" >&2
    return "$exit_code"
  fi
}

run_voom_json() {
  local output=$1
  shift
  if ! voom "$@" >"$output"; then
    cat "$output" >&2
    return 1
  fi
  jq -e '.status == "ok"' "$output" >/dev/null
}

create_fixture() {
  local spec=$1
  local name encoder pixel_format codec_profile
  IFS='|' read -r name encoder pixel_format codec_profile <<<"$spec"
  local args=(
    -hide_banner -loglevel error -nostdin
    -f lavfi -i testsrc2=size=256x256:rate=30
    -t 1 -an -vf "format=$pixel_format"
    -c:v "$encoder"
  )
  case "$encoder" in
  h264_videotoolbox)
    args+=(-allow_sw 0 -b:v 4M -profile:v "$codec_profile" -level 4.1)
    ;;
  hevc_videotoolbox)
    args+=(-allow_sw 0 -b:v 4M -profile:v "$codec_profile")
    ;;
  libsvtav1)
    args+=(-crf 35 -preset 8)
    ;;
  esac
  if ! ffmpeg "${args[@]}" -f matroska -y "$task_tmp/$name.mkv" \
    >"$task_tmp/fixture-$name.log" 2>&1; then
    echo "failed to create VideoToolbox acceptance fixture $name" >&2
    cat "$task_tmp/fixture-$name.log" >&2
    exit 1
  fi
}

create_profile() {
  local spec=$1
  local name encoder codec_profile codec_level pixel_format decode max_width max_height
  IFS='|' read -r name encoder codec_profile codec_level pixel_format decode \
    max_width max_height <<<"$spec"
  local args=(
    profile create
    --name "$name"
    --encoder "$encoder"
    --bitrate-kbps 4000
    --preset default
    --codec-profile "$codec_profile"
    --pixel-format "$pixel_format"
    --decode "$decode"
  )
  if [[ "$codec_level" != "-" ]]; then
    args+=(--codec-level "$codec_level")
  fi
  if [[ "$max_width" != "-" ]]; then
    args+=(--max-width "$max_width" --max-height "$max_height")
  fi
  if ! voom "${args[@]}" >"$task_tmp/profile-$name.json"; then
    echo "failed to create VideoToolbox profile $name" >&2
    cat "$task_tmp/profile-$name.json" >&2
    exit 1
  fi
  jq -e '.status == "ok"' "$task_tmp/profile-$name.json" >/dev/null
}

assert_production_execution() {
  local case_name=$1
  local execute_json=$2

  if ! jq -e \
    '.status == "ok"
      and .data.summary.dispatch_count == 1
      and .data.summary.failure_count == 0
      and .data.file_phases[0].outcome == "committed"' \
    "$execute_json" >/dev/null; then
    echo "production workflow assertions failed for $case_name" >&2
    cat "$execute_json" >&2
    return 1
  fi
}

execute_case() {
  local spec=$1
  local case_name profile_name source_name source_codec source_container
  local expected_codec expected_profile expected_pixel_format expected_width expected_height
  IFS='|' read -r case_name profile_name source_name source_codec source_container \
    expected_codec expected_profile expected_pixel_format expected_width \
    expected_height <<<"$spec"

  local scan_json="$task_tmp/scan-$case_name.json"
  run_voom_json "$scan_json" scan --path "$task_tmp/$source_name"
  local file_version_id media_snapshot_id
  file_version_id=$(jq -er '.data.files[0].file_version_id' "$scan_json")
  media_snapshot_id=$(jq -er '.data.files[0].media_snapshot_id' "$scan_json")

  local policy_file="$task_tmp/policy-$case_name.voom"
  printf 'policy "%s" { phase encode { transcode video to %s using profile "%s" } }\n' \
    "$case_name" "$expected_codec" "$profile_name" >"$policy_file"
  local policy_json="$task_tmp/policy-$case_name.json"
  run_voom_json "$policy_json" policy create \
    --slug "$case_name" \
    --file "$policy_file"
  local policy_version_id
  policy_version_id=$(jq -er '.data.version.version_id' "$policy_json")

  local input_json="$task_tmp/input-$case_name.json"
  run_voom_json "$input_json" policy input create-from-scan \
    --slug "$case_name" \
    --file-version-id "$file_version_id" \
    --media-snapshot-id "$media_snapshot_id" \
    --container "$source_container" \
    --video-codec "$source_codec"
  local input_set_id
  input_set_id=$(jq -er '.data.input_set.input_set_id' "$input_json")

  mkdir -p "$task_tmp/staging-$case_name" "$task_tmp/output-$case_name"
  local execute_json="$task_tmp/execute-$case_name.json"
  run_voom_json "$execute_json" compliance execute \
    --policy-version-id "$policy_version_id" \
    --input-set-id "$input_set_id" \
    --max-in-flight-files 1 \
    --staging-root "$task_tmp/staging-$case_name" \
    --output-dir "$task_tmp/output-$case_name"
  assert_production_execution "$case_name" "$execute_json"

  local job_id location_id output_path
  job_id=$(jq -er '.data.summary.job_id' "$execute_json")
  location_id=$(jq -er '.data.file_phases[0].produced_file_location_id' "$execute_json")
  output_path=$(
    sqlite3 "$task_tmp/voom.db" \
      "SELECT value FROM file_locations WHERE id = $location_id"
  )

  local report_json="$task_tmp/report-$case_name.json"
  run_voom_json "$report_json" compliance report --job-id "$job_id"
  jq -e \
    --argjson job_id "$job_id" \
    '.status == "ok"
      and .data.summary.job_id == $job_id
      and .data.file_phases[0].outcome == "committed"' \
    "$report_json" >/dev/null

  local event
  event=$(
    sqlite3 "$task_tmp/voom.db" \
      "SELECT payload FROM events
       WHERE kind = 'artifact.transcode_succeeded'
         AND json_extract(payload, '$.job_id') = $job_id
       ORDER BY event_id DESC LIMIT 1"
  )
  jq -e \
    --arg resource_id "$resource_id" \
    --arg expected_codec "$expected_codec" \
    --arg expected_pixel_format "$expected_pixel_format" \
    '.hardware_backend == "video_toolbox"
      and .hardware_token == ("videotoolbox:" + $resource_id)
      and .hardware_resource_id == $resource_id
      and .hardware_device_uuid == null
      and .output_video_codec == $expected_codec
      and .output_pixel_format == $expected_pixel_format' <<<"$event" >/dev/null

  local facts
  facts=$(
    ffprobe -v error -select_streams v:0 \
      -show_entries stream=codec_name,profile,width,height,pix_fmt \
      -of json "$output_path"
  )
  jq -e \
    --arg codec "$expected_codec" \
    --arg profile "$expected_profile" \
    --arg pixel_format "$expected_pixel_format" \
    --argjson width "$expected_width" \
    --argjson height "$expected_height" \
    '.streams[0].codec_name == $codec
      and .streams[0].profile == $profile
      and .streams[0].pix_fmt == $pixel_format
      and .streams[0].width == $width
      and .streams[0].height == $height' <<<"$facts" >/dev/null
}

cargo test -p voom-ffmpeg-worker \
  real_videotoolbox_preflight_proves_host_pipelines \
  -- --ignored
cargo test -p voom-control-plane videotoolbox_requirement_
cargo test -p voom-control-plane videotoolbox_evidence_
cargo test -p voom-ffmpeg-worker videotoolbox_
cargo build \
  -p voom-cli \
  -p voom-ffmpeg-worker \
  -p voom-ffprobe-worker \
  -p voom-verify-artifact-worker

database_url="sqlite://$task_tmp/voom.db"
run_voom_json "$task_tmp/init.json" init

for fixture in \
  "base-h264|h264_videotoolbox|yuv420p|high" \
  "base-hevc8|hevc_videotoolbox|yuv420p|main" \
  "base-hevc10|hevc_videotoolbox|yuv420p10le|main10" \
  "base-av18|libsvtav1|yuv420p|-" \
  "base-av110|libsvtav1|yuv420p10le|-"; do
  create_fixture "$fixture"
done
cp "$task_tmp/base-h264.mkv" "$task_tmp/source-sw-h264.mkv"
cp "$task_tmp/base-h264.mkv" "$task_tmp/source-sw-hevc.mkv"
cp "$task_tmp/base-h264.mkv" "$task_tmp/source-hw-h264.mkv"
cp "$task_tmp/base-h264.mkv" "$task_tmp/source-hw-scale.mkv"
ffmpeg -hide_banner -loglevel error -nostdin \
  -i "$task_tmp/base-hevc10.mkv" \
  -map 0 -c copy -y "$task_tmp/base-hevc10.mp4"

for profile in \
  "accept-sw-h264|h264_videotoolbox|high|4.1|yuv420p|software|-|-" \
  "accept-sw-hevc|hevc_videotoolbox|main|-|yuv420p|software|-|-" \
  "accept-hw-h264|h264_videotoolbox|high|4.1|yuv420p|video-toolbox|-|-" \
  "accept-hw-hevc|hevc_videotoolbox|main|-|yuv420p|video-toolbox|-|-" \
  "accept-hw-hevc10|hevc_videotoolbox|main10|-|yuv420p10le|video-toolbox|-|-" \
  "accept-hw-scale|hevc_videotoolbox|main|-|yuv420p|video-toolbox|128|128"; do
  create_profile "$profile"
done

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

deadline=$((SECONDS + 465))
while ! rg -q '"status":"ready"' "$task_tmp/worker.stdout"; do
  if ! kill -0 "$worker_pid" 2>/dev/null; then
    wait "$worker_pid" || true
    echo "VideoToolbox worker exited before readiness" >&2
    cat "$task_tmp/worker.stderr" >&2
    exit 1
  fi
  if ((SECONDS >= deadline)); then
    echo "VideoToolbox worker did not become ready within 465 seconds" >&2
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
capability_extra=$(
  sqlite3 "$task_tmp/voom.db" \
    "SELECT extra FROM worker_capabilities
		 WHERE operation = 'transcode_video'"
)
resource_id=$(jq -er '.accelerator.resource_id' <<<"$capability_extra")

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
jq -e \
  '.accelerator.model_identifier != ""
    and .accelerator.chip_name != ""
    and .accelerator.macos_version != ""
    and .accelerator.macos_build != ""
    and .accelerator.max_sessions >= 1' <<<"$capability_extra" >/dev/null

ffmpeg -hide_banner -buildconf >"$task_tmp/ffmpeg-build.txt" 2>&1
require_text "FFmpeg VideoToolbox build flag" \
  '--enable-videotoolbox' "$(cat "$task_tmp/ffmpeg-build.txt")"

for acceptance_case in \
  "sw-h264|accept-sw-h264|source-sw-h264.mkv|h264|mkv|h264|High|yuv420p|256|256" \
  "sw-hevc|accept-sw-hevc|source-sw-hevc.mkv|h264|mkv|hevc|Main|yuv420p|256|256" \
  "hw-h264|accept-hw-h264|source-hw-h264.mkv|h264|mkv|h264|High|yuv420p|256|256" \
  "hw-hevc8|accept-hw-h264|base-hevc8.mkv|hevc|mkv|h264|High|yuv420p|256|256" \
  "hw-hevc10|accept-hw-hevc10|base-hevc10.mp4|hevc|mp4|hevc|Main 10|yuv420p10le|256|256" \
  "hw-av18|accept-hw-hevc|base-av18.mkv|av1|mkv|hevc|Main|yuv420p|256|256" \
  "hw-av110|accept-hw-hevc10|base-av110.mkv|av1|mkv|hevc|Main 10|yuv420p10le|256|256" \
  "hw-scale|accept-hw-scale|source-hw-scale.mkv|h264|mkv|hevc|Main|yuv420p|128|128"; do
  execute_case "$acceptance_case"
done

ffmpeg \
  -hide_banner \
  -loglevel verbose \
  -nostdin \
  -hwaccel videotoolbox \
  -hwaccel_output_format videotoolbox_vld \
  -i "$task_tmp/base-h264.mkv" \
  -frames:v 1 \
  -an \
  -vf scale_vt=w=128:h=128 \
  -c:v hevc_videotoolbox \
  -allow_sw 0 \
  -b:v 4M \
  -profile:v main \
  -f null \
  - >"$task_tmp/direct-frame.stdout" 2>"$task_tmp/direct-frame.stderr"
direct_frame_log=$(cat "$task_tmp/direct-frame.stderr")
require_text "VideoToolbox frame context" 'videotoolbox_vld' "$direct_frame_log"
require_text "direct input frame-context reuse" \
  'Using input frames context.*videotoolbox_vld' "$direct_frame_log"
if rg -q 'hwdownload|hwupload|auto_scale|libx264|libx265' <<<"$direct_frame_log"; then
  echo "hardware decode evidence contained a forbidden fallback or frame transition" >&2
  exit 1
fi

if rg -q '[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}' \
  "$task_tmp/worker.stdout" "$task_tmp/worker.stderr" \
  <(printf '%s\n%s\n%s\n' "$claim" "$capability" "$capability_extra"); then
  echo "raw UUID-shaped platform identity escaped into acceptance evidence" >&2
  exit 1
fi

claim_processes=$(
  sqlite3 -separator '|' "$task_tmp/voom.db" \
    "SELECT supervisor_pid, process_group_id FROM accelerator_claims"
)
IFS='|' read -r claim_pid claim_group <<<"$claim_processes"

kill -KILL "$worker_pid"
wait "$worker_pid" 2>/dev/null || true
worker_pid=""
close_worker_input

if [[ $(wc -l <"$task_tmp/worker.stdout") -ne 1 ]]; then
  echo "hard-stopped run-local stdout did not contain exactly one readiness line" >&2
  exit 1
fi
require_text "initial readiness line" '"status":"ready"' \
  "$(cat "$task_tmp/worker.stdout")"

owner_gone=false
for _attempt in {1..100}; do
  if ! /bin/ps -p "$claim_pid" -o pid= | rg -q '[[:digit:]]' &&
    ! /bin/ps -axo pgid= |
    rg -q "^[[:space:]]*${claim_group}[[:space:]]*$"; then
    owner_gone=true
    break
  fi
  sleep 0.1
done
if [[ "$owner_gone" != "true" ]]; then
  echo "hard-stopped VideoToolbox worker processes did not exit" >&2
  exit 1
fi

if ! target/debug/voom \
  --database-url "$database_url" \
  worker run-local \
  --kind ffmpeg \
  --videotoolbox \
  --videotoolbox-max-sessions "$max_sessions" \
  </dev/null \
  >"$task_tmp/recovery.stdout" \
  2>"$task_tmp/recovery.stderr"; then
  echo "VideoToolbox claim recovery failed" >&2
  cat "$task_tmp/recovery.stderr" >&2
  exit 1
fi
if [[ $(wc -l <"$task_tmp/recovery.stdout") -ne 2 ]]; then
  echo "recovered run-local stdout did not contain readiness and retirement lines" >&2
  exit 1
fi
require_text "recovery readiness line" '"status":"ready"' \
  "$(sed -n '1p' "$task_tmp/recovery.stdout")"
require_text "recovery retirement line" '"status":"retired"' \
  "$(sed -n '2p' "$task_tmp/recovery.stdout")"

claims_after_shutdown=$(
  sqlite3 "$task_tmp/voom.db" "SELECT COUNT(*) FROM accelerator_claims"
)
if [[ "$claims_after_shutdown" != "0" ]]; then
  echo "VideoToolbox claim remained after clean retirement" >&2
  exit 1
fi

echo "VideoToolbox video acceleration acceptance passed at capacity $max_sessions"
