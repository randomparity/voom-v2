#!/usr/bin/env bash
# Provision Toxiproxy, run the opt-in network-resilience suite, and reap the
# server. See docs/superpowers/specs/2026-07-09-issue-321-toxiproxy-net-resilience-design.md
# and docs/adr/0033.
#
# Env:
#   NET_RESILIENCE_DOWNLOAD=1  download + SHA256-verify the pinned toxiproxy-server
#                              (CI). Otherwise require toxiproxy-server on PATH.
#   TOXIPROXY_ADDR             control-API address (default 127.0.0.1:8474).
# Extra args are forwarded to `cargo test` (e.g. --skip <scenario> to quarantine
# a scenario against a filed follow-up issue).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

toxiproxy_version="2.12.0"
addr="${TOXIPROXY_ADDR:-127.0.0.1:8474}"
host="${addr%:*}"
port="${addr##*:}"

sha_for_platform() {
  case "$1" in
  linux-amd64) echo "556d891134a3c582dc1e1a3f7335fd55142e5965769855a00b944e13e48302fc" ;;
  linux-arm64) echo "53e770c1c3035b5a9f1bc629fce537db1f95f62b26f4ebe6e756afd701cf077c" ;;
  darwin-amd64) echo "9625bba4bd96117eedae49f982aba4c2f462b268dd406c9ff18186f9b1ef8afe" ;;
  darwin-arm64) echo "aa299966b52f16a8594f1cd0d1e9049dc2e8fe2c04a90c19860e2719b2b95d15" ;;
  *) return 1 ;;
  esac
}

detect_platform() {
  local os arch
  case "$(uname -s)" in
  Linux) os="linux" ;;
  Darwin) os="darwin" ;;
  *)
    echo "unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
  esac
  case "$(uname -m)" in
  x86_64 | amd64) arch="amd64" ;;
  arm64 | aarch64) arch="arm64" ;;
  *)
    echo "unsupported arch: $(uname -m)" >&2
    exit 1
    ;;
  esac
  echo "${os}-${arch}"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

resolve_server_bin() {
  if [[ "${NET_RESILIENCE_DOWNLOAD:-0}" != "1" ]]; then
    if ! command -v toxiproxy-server >/dev/null 2>&1; then
      echo "toxiproxy-server not found on PATH." >&2
      echo "Install it (macOS: brew install toxiproxy) or set NET_RESILIENCE_DOWNLOAD=1." >&2
      exit 1
    fi
    command -v toxiproxy-server
    return
  fi

  local platform expected cache_dir bin url actual tmp
  platform="$(detect_platform)"
  if ! expected="$(sha_for_platform "$platform")"; then
    echo "no pinned SHA256 for platform: $platform" >&2
    exit 1
  fi
  cache_dir="$repo_root/target/net-resilience"
  bin="$cache_dir/toxiproxy-server-${platform}-${toxiproxy_version}"
  mkdir -p "$cache_dir"
  if [[ ! -x "$bin" ]] || [[ "$(sha256_of "$bin")" != "$expected" ]]; then
    url="https://github.com/Shopify/toxiproxy/releases/download/v${toxiproxy_version}/toxiproxy-server-${platform}"
    echo "==> downloading toxiproxy-server ${toxiproxy_version} (${platform})" >&2
    # Download to a unique temp file, verify, then atomically rename into place
    # so an interrupted or concurrent download can never leave a torn binary at
    # the cache path.
    tmp="$(mktemp "${bin}.XXXXXX")"
    if ! curl -fsSL "$url" -o "$tmp"; then
      rm -f "$tmp"
      echo "download failed: $url" >&2
      exit 1
    fi
    actual="$(sha256_of "$tmp")"
    if [[ "$actual" != "$expected" ]]; then
      echo "SHA256 mismatch for $url" >&2
      echo "  expected $expected" >&2
      echo "  actual   $actual" >&2
      rm -f "$tmp"
      exit 1
    fi
    chmod +x "$tmp"
    mv -f "$tmp" "$bin"
  fi
  echo "$bin"
}

# Fail loud if the chosen control address is already serving — for the default
# AND an operator-set TOXIPROXY_ADDR — so the suite never runs against a
# toxiproxy this script did not start and cannot reap.
if (exec 3<>"/dev/tcp/${host}/${port}") 2>/dev/null; then
  exec 3>&- 3<&-
  echo "control address ${addr} is already listening." >&2
  echo "Stop that server or set TOXIPROXY_ADDR to a free address." >&2
  exit 1
fi

server_bin="$(resolve_server_bin)"

# Keep the server's (verbose) logs out of the run output; surface them only when
# the server fails to come up, so CI shows just the cargo test result on success.
server_log="$(mktemp -t net-resilience-toxiproxy.XXXXXX)"
server_pid=""
cleanup() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -f "$server_log"
}
trap cleanup EXIT INT TERM

"$server_bin" -host "$host" -port "$port" >"$server_log" 2>&1 &
server_pid=$!

ready=""
for _ in $(seq 1 50); do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    wait "$server_pid" || true
    echo "toxiproxy-server exited before becoming ready:" >&2
    cat "$server_log" >&2
    exit 1
  fi
  if curl -fsS "http://${addr}/version" >/dev/null 2>&1; then
    ready="1"
    break
  fi
  sleep 0.1
done
if [[ -z "$ready" ]]; then
  echo "timed out waiting for toxiproxy-server at ${addr}:" >&2
  cat "$server_log" >&2
  exit 1
fi

export TOXIPROXY_ADDR="$addr"
cargo test -p voom-worker-protocol --test net_resilience -- \
  --ignored --test-threads=1 --nocapture "$@"
