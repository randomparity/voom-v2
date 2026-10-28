#!/usr/bin/env bash
set -euo pipefail
engine=${1:-docker}
case "$engine" in docker|podman) ;; *) echo 'Use docker or podman' >&2; exit 2 ;; esac
command -v "$engine" >/dev/null || { echo "Install and start $engine before building" >&2; exit 2; }
if [[ $engine == docker ]]; then
    architecture=$(docker info --format '{{.Architecture}}')
else
    architecture=$(podman info --format '{{.Host.Arch}}')
fi
case "$architecture" in
    amd64|x86_64) platform=linux/amd64 ;;
    arm64|aarch64) platform=linux/arm64 ;;
    *) echo "Unsupported native engine architecture: $architecture" >&2; exit 2 ;;
esac
context=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
image_file=$(mktemp)
trap 'rm -f "$image_file"' EXIT
"$engine" build --platform "$platform" --iidfile "$image_file" \
    --tag voom-ffmpeg:9.0.2-pcm-wavex "$context" >&2
image=$(cat "$image_file")
"$engine" image inspect "$image" --format '{{.Id}}'
