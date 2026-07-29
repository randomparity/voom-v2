#!/usr/bin/env bash
set -euo pipefail

if (($# == 0)); then
	echo "usage: $0 GPU-<uuid> [GPU-<uuid> ...]" >&2
	exit 2
fi

for device_uuid in "$@"; do
	if [[ ! "$device_uuid" =~ ^GPU-[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}$ ]]; then
		echo "invalid full NVIDIA UUID: $device_uuid" >&2
		exit 2
	fi
done

for command_name in cargo ffmpeg ffprobe nvidia-smi rg; do
	if ! command -v "$command_name" >/dev/null; then
		echo "required command is missing: $command_name" >&2
		exit 2
	fi
done

cargo build -p voom-ffmpeg-worker
worker_bin="target/debug/voom-ffmpeg-worker"
task_tmp=$(mktemp -d /tmp/voom-nvidia-acceptance.XXXXXX)

cleanup() {
	rm -f "$task_tmp"/*
	rmdir "$task_tmp"
}
trap cleanup EXIT

ffmpeg \
	-hide_banner \
	-loglevel error \
	-f lavfi \
	-i testsrc2=size=256x256:rate=30 \
	-t 1 \
	-c:v libx264 \
	-pix_fmt yuv420p \
	"$task_tmp/source-h264.mkv"

for device_uuid in "$@"; do
	nvidia-smi -i "$device_uuid" --query-gpu=uuid,name,driver_version --format=csv,noheader
	bound=$(
		env \
			VOOM_WORKER_ID=1 \
			VOOM_WORKER_EPOCH=0 \
			VOOM_WORKER_SECRET=0123456789abcdef0123456789abcdef \
			VOOM_NVIDIA_DEVICE="$device_uuid" \
			VOOM_NVIDIA_MAX_SESSIONS=1 \
			"$worker_bin" </dev/null
	)
	if [[ "$bound" != *"\"device_uuid\":\"$device_uuid\""* ]]; then
		echo "worker readiness did not report the configured UUID: $bound" >&2
		exit 1
	fi

	CUDA_VISIBLE_DEVICES="$device_uuid" ffmpeg \
		-hide_banner \
		-loglevel error \
		-nostdin \
		-n \
		-i "$task_tmp/source-h264.mkv" \
		-map 0:v:0 \
		-vf format=nv12,hwupload_cuda=device=0 \
		-c:v hevc_nvenc \
		-rc vbr \
		-cq 22 \
		-b:v 0 \
		-preset p5 \
		-f matroska \
		"$task_tmp/software-$device_uuid.mkv"

	CUDA_VISIBLE_DEVICES="$device_uuid" ffmpeg \
		-hide_banner \
		-loglevel error \
		-nostdin \
		-n \
		-hwaccel cuda \
		-hwaccel_device 0 \
		-hwaccel_output_format cuda \
		-c:v h264_cuvid \
		-i "$task_tmp/source-h264.mkv" \
		-map 0:v:0 \
		-vf scale_cuda=format=nv12 \
		-c:v hevc_nvenc \
		-rc vbr \
		-cq 22 \
		-b:v 0 \
		-preset p5 \
		-f matroska \
		"$task_tmp/cuda-$device_uuid.mkv"

	for output in \
		"$task_tmp/software-$device_uuid.mkv" \
		"$task_tmp/cuda-$device_uuid.mkv"; do
		facts=$(ffprobe \
			-v error \
			-select_streams v:0 \
			-show_entries stream=codec_name,width,height,pix_fmt \
			-of json \
			"$output")
		if ! rg -q '"codec_name": "hevc"' <<<"$facts"; then
			echo "unexpected acceptance output facts for $output: $facts" >&2
			exit 1
		fi
	done
done

echo "NVIDIA video acceleration acceptance passed for $# device(s)"
