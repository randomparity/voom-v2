#!/usr/bin/env bash
# Make DEST hold the pinned FFmpeg inputs. Every URL is untrusted transport: a file is kept
# or installed only when its digest matches source-sha256, and the builder verifies the
# digests and the release signature again before extraction.
set -euo pipefail
dest=${1:-}
[[ -d $dest ]] || { echo 'Usage: fetch-sources.sh EXISTING-DIRECTORY' >&2; exit 2; }
recipe=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
digest_of() {
    local line
    if command -v sha256sum >/dev/null 2>&1; then
        line=$(sha256sum "$1")
    else
        line=$(shasum -a 256 "$1")
    fi
    echo "${line%% *}"
}
download=$(mktemp -d)
trap 'rm -rf "$download"' EXIT
while read -r expected filename; do
    target="$dest/$filename"
    if [[ -f $target && ! -L $target && $(digest_of "$target") == "$expected" ]]; then
        echo "Verified existing $filename" >&2
        continue
    fi
    rm -f "$target"
    fetched=
    while read -r name url; do
        [[ $name == "$filename" ]] || continue
        if curl --fail --silent --show-error --location --proto =https --proto-redir =https \
            --connect-timeout 20 --max-time 300 --retry 2 \
            --output "$download/$filename" "$url" </dev/null &&
            [[ $(digest_of "$download/$filename") == "$expected" ]]; then
            mv "$download/$filename" "$target"
            echo "Fetched $filename from $url" >&2
            fetched=yes
            break
        fi
        echo "Rejected $filename from $url" >&2
    done < "$recipe/source-urls"
    [[ -n $fetched ]] || {
        echo "No URL in source-urls served the pinned $filename; check network access" >&2
        exit 1
    }
done < "$recipe/source-sha256"
