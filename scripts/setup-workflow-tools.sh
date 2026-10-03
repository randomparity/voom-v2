#!/usr/bin/env bash
# Official release digests, checked against GitHub release metadata on 2026-10-02.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
case "$(uname -s)" in
  Darwin) os=darwin; zizmor_os=apple-darwin ;;
  Linux) os=linux; zizmor_os=unknown-linux-gnu ;;
  *) echo 'workflow tools require macOS or Linux' >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) arch=arm64; native_arch=aarch64 ;;
  x86_64|amd64) arch=amd64; native_arch=x86_64 ;;
  *) echo 'workflow tools require arm64 or x86_64' >&2; exit 1 ;;
esac
case "$os-$arch" in
  darwin-arm64)
    actionlint_sha=aba9ced2dee8d27fecca3dc7feb1a7f9a52caefa1eb46f3271ea66b6e0e6953f
    shellcheck_sha=339b930feb1ea764467013cc1f72d09cd6b869ebf1013296ba9055ab2ffbd26f
    zizmor_sha=e28d22b087f9ebb8d99da6e740d348c930f559961c7c3f12badda54f882195a2 ;;
  darwin-amd64)
    actionlint_sha=5b44c3bc2255115c9b69e30efc0fecdf498fdb63c5d58e17084fd5f16324c644
    shellcheck_sha=c2c15e08df0e8fbc374c335b230a7ee958c313fa5714817a59aa59f1aa594f51
    zizmor_sha=10e6b18b11ea07e515a16f0f0518c7b07527bc9977c1fd5698181ce7f3554202 ;;
  linux-arm64)
    actionlint_sha=325e971b6ba9bfa504672e29be93c24981eeb1c07576d730e9f7c8805afff0c6
    shellcheck_sha=68a8133197a50beb8803f8d42f9908d1af1c5540d4bb05fdfca8c1fa47decefc
    zizmor_sha=7ff1dce33bdd18fd2a4affe63bdd47efcccca97b2cec1c1863ec26e9e2647540 ;;
  linux-amd64)
    actionlint_sha=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
    shellcheck_sha=b7af85e41cc99489dcc21d66c6d5f3685138f06d34651e6d34b42ec6d54fe6f6
    zizmor_sha=e65324f4430c2717591937edcec90ccbefaf14c174f8ec9415e03ca875b46e1a ;;
esac
tools="$repo_root/target/workflow-tools"
mkdir -p "$tools"
staging=$(mktemp -d "$tools/.install.XXXXXX")
trap 'rm -rf "$staging"' EXIT
install_tool() {
  local tool=$1 url=$2 expected=$3 member=$4 actual
  echo "Installing $tool"
  curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL --max-time 120 \
    "$url" -o "$staging/archive.tar.gz"
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$staging/archive.tar.gz")
  else
    actual=$(shasum -a 256 "$staging/archive.tar.gz")
  fi
  if [[ "${actual%% *}" != "$expected" ]]; then
    echo "SHA256 mismatch for $tool; refusing installation" >&2
    exit 1
  fi
  tar -xzOf "$staging/archive.tar.gz" "$member" > "$staging/$tool"
  test -s "$staging/$tool"
  chmod 755 "$staging/$tool"
  mv -f "$staging/$tool" "$tools/$tool"
}
install_tool actionlint \
  "https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_${os}_${arch}.tar.gz" \
  "$actionlint_sha" actionlint
install_tool shellcheck \
  "https://github.com/koalaman/shellcheck/releases/download/v0.11.0/shellcheck-v0.11.0.${os}.${native_arch}.tar.gz" \
  "$shellcheck_sha" shellcheck-v0.11.0/shellcheck
install_tool zizmor \
  "https://github.com/zizmorcore/zizmor/releases/download/v1.30.1/zizmor-${native_arch}-${zizmor_os}.tar.gz" \
  "$zizmor_sha" zizmor
