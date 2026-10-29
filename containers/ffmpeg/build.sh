#!/usr/bin/env bash
set -euo pipefail
engine=${1:-docker}
case "$engine" in docker|podman) ;; *) echo 'Use docker or podman' >&2; exit 2 ;; esac
source_dir=${2:-}
recipe=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
if [[ -n $source_dir ]]; then
    [[ -d $source_dir ]] || { echo 'Prefetched source directory is missing' >&2; exit 2; }
    shopt -s nullglob dotglob
    inputs=("$source_dir"/*)
    [[ ${#inputs[@]} == 3 ]] || { echo 'Supply exactly the three pinned source files' >&2; exit 2; }
    while read -r expected filename; do
        input="$source_dir/$filename"
        [[ -f $input && ! -L $input ]] || { echo "Missing regular source file: $filename" >&2; exit 2; }
        if command -v sha256sum >/dev/null 2>&1; then
            digest=$(sha256sum "$input")
        else
            digest=$(shasum -a 256 "$input")
        fi
        [[ ${digest%% *} == "$expected" ]] || { echo "Source digest mismatch: $filename" >&2; exit 2; }
    done < "$recipe/source-sha256"
fi
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
context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
cp "$recipe/Dockerfile" "$recipe/pcm-wavex.patch" "$recipe/source-sha256" \
    "$recipe/.dockerignore" "$context/"
mkdir "$context/sources"
if [[ -n $source_dir ]]; then
    cp "${inputs[@]}" "$context/sources/"
fi
"$engine" build --platform "$platform" --iidfile "$context/image-id" \
    --tag voom-ffmpeg:9.0.2-pcm-wavex "$context" >&2
image=$(cat "$context/image-id")
"$engine" image inspect "$image" --format '{{.Id}}'
