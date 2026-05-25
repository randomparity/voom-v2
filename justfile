# Default action: list available recipes
default:
    @just --list

# Bootstrap a fresh checkout for development
setup:
    @echo "==> Verifying Rust toolchain"
    @command -v rustup >/dev/null || { echo "Install rustup: https://rustup.rs"; exit 1; }
    rustup show active-toolchain || rustup toolchain install stable
    rustup component add clippy rustfmt
    @echo "==> Installing cargo tools (idempotent)"
    cargo install --locked cargo-audit cargo-deny prek cargo-llvm-cov ast-grep
    @echo "==> Verifying uv + Python 3.13"
    @command -v uv >/dev/null || { echo "Install uv: https://docs.astral.sh/uv/"; exit 1; }
    uv python install 3.13
    @echo "==> Installing git hooks"
    prek install
    prek auto-update --cooldown-days 7
    @echo "==> Warming cargo cache"
    cargo fetch
    @echo "==> Setup complete. Try: just ci"

# Run the exact set of checks GitHub Actions runs
ci: fmt-check lint check-test-layout test doc deny audit
    @echo "==> All CI checks passed"

# Individual checks (also called by `ci`)
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --workspace --all-features

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items

audit:
    cargo audit --deny warnings

deny:
    cargo deny check

# Generate workspace coverage in lcov format (consumed by SonarCloud and other readers)
coverage:
    cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info

# Generate workspace coverage as a browsable HTML report
coverage-html:
    cargo llvm-cov --workspace --all-features --html

# Enforce the sibling-test layout: no inline tests in src/, every *_test.rs is linked
check-test-layout:
    ./scripts/check-test-layout.sh

# Run the CLI binary
run *ARGS:
    cargo run -p voom-cli -- {{ARGS}}

# Run version + init + health end-to-end against an ephemeral on-disk DB
smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    workdir=$(mktemp -d -t voom-smoke.XXXXXX)
    db="$workdir/voom.db"
    missing="$workdir/never-created.db"
    url="sqlite://$db"
    missing_url="sqlite://$missing"
    trap 'rm -rf "$workdir"' EXIT

    # Helper: run an expected-failing voom command, capturing stdout + exit code
    # separately so `set -o pipefail` doesn't trip the script on the deliberate
    # non-zero CLI exit code.
    expect_fail() {
        local expected_code="$1"; shift
        local expected_err_code="$1"; shift
        set +e
        local out
        out=$("$@")
        local rc=$?
        set -e
        if [[ "$rc" -ne "$expected_code" ]]; then
            echo "expected CLI exit code $expected_code, got $rc"
            echo "stdout: $out"
            return 1
        fi
        echo "$out" | jq -e --arg code "$expected_err_code" \
            '.status == "error" and .error.code == $code' >/dev/null
    }

    # version: no DB touch
    cargo run -q -p voom-cli -- --database-url "$url" version | jq -e '.status == "ok"'

    # health on missing file: must exit 2 with DB_UNREACHABLE AND leave the
    # filesystem untouched (no file, no parent dir creation).
    expect_fail 2 DB_UNREACHABLE \
        cargo run -q -p voom-cli -- --database-url "$missing_url" health
    test ! -e "$missing" || { echo "health created a file at $missing"; exit 1; }

    # init: creates the DB and applies migrations (idempotent)
    cargo run -q -p voom-cli -- --database-url "$url" init | \
        jq -e '.status == "ok" and .data.already_initialized == false' >/dev/null
    cargo run -q -p voom-cli -- --database-url "$url" init | \
        jq -e '.status == "ok" and .data.already_initialized == true' >/dev/null

    # health after init: ok
    cargo run -q -p voom-cli -- --database-url "$url" health | \
        jq -e '.status == "ok" and .data.db.status == "current"' >/dev/null

    echo "==> smoke OK"

# Remove build artifacts
clean:
    cargo clean

# Run deterministic Chaos Librarian E2E tests. Not part of default `just ci`.
chaos-e2e-ci:
    cargo test -p voom-cli --test chaos_librarian_e2e -- --ignored --nocapture

# Run a short local-only Chaos Librarian wall-clock churn scenario.
chaos-e2e-local:
    ./scripts/chaos-e2e-local.sh

# Exercise the local Chaos Librarian shell harness with faked external tools.
chaos-e2e-local-script-test:
    ./scripts/test-chaos-e2e-local.sh

# Run an extended local-only Chaos Librarian wall-clock soak.
chaos-e2e-soak:
    CHAOS_DURATION=${CHAOS_DURATION:-2h} CHAOS_SPEED=${CHAOS_SPEED:-10x} CHAOS_PRESERVE_OUTPUT=1 ./scripts/chaos-e2e-local.sh
