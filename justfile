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
ci: fmt-check lint check-test-layout check-paused-time-db check-paused-time-db-selftest \
    check-control-plane-sql-boundary check-control-plane-sql-boundary-selftest \
    check-check-constraint-bypass check-check-constraint-bypass-selftest \
    check-payload-deny-unknown check-payload-deny-unknown-selftest \
    check-adr-index check-adr-index-selftest select-ffmpeg-asset-selftest \
    run-constrained-selftest \
    test doc deny audit
    @echo "==> All CI checks passed"

# Individual checks (also called by `ci`)
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
    # Build every binary to completion before running any test. The integration
    # tests build worker binaries on demand via `cargo_build_package`; pre-building
    # here keeps those calls no-ops so a worker binary is never relinked while a
    # concurrently-running test execs it (ETXTBSY). Same feature set as the test run.
    cargo build --workspace --all-features --all-targets
    # Guard test-target wiring without the workspace's --all-features override.
    VOOM_TEST_PREBUILT_WORKERS=1 cargo test -p voom-control-plane
    VOOM_TEST_PREBUILT_WORKERS=1 cargo test --workspace --all-features

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc \
        --workspace --all-features --no-deps --document-private-items

audit:
    cargo audit --deny warnings

deny:
    cargo deny check

# Generate workspace coverage in lcov format (consumed by SonarCloud and other readers)
coverage:
    cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info -- --test-threads=1

# Generate workspace coverage as a browsable HTML report
coverage-html:
    cargo llvm-cov --workspace --all-features --html -- --test-threads=1

# Enforce the sibling-test layout: no inline tests in src/, every *_test.rs is linked
check-test-layout:
    ./scripts/check-test-layout.sh

# Guard: no test pairs tokio paused time with a real SqlitePool
check-paused-time-db:
    ./scripts/check-paused-time-db.sh

# Self-test for the paused-time guard (keeps its ast-grep patterns honest)
check-paused-time-db-selftest:
    ./scripts/check-paused-time-db-selftest.sh

# Guard: production control-plane code delegates every SQL operation to voom-store
check-control-plane-sql-boundary:
    ./scripts/check-control-plane-sql-boundary.sh

# Self-test for the control-plane SQL boundary guard
check-control-plane-sql-boundary-selftest:
    ./scripts/check-control-plane-sql-boundary-selftest.sh

# Guard: check-constraint bypasses use the shared pinned-connection helper
check-check-constraint-bypass:
    ./scripts/check-check-constraint-bypass.sh

# Self-test for the check-constraint bypass source guard
check-check-constraint-bypass-selftest:
    ./scripts/check-check-constraint-bypass-selftest.sh

# Guard: every durable typed payload denies unknown fields (audit M4, ADR 0013)
check-payload-deny-unknown:
    ./scripts/check-payload-deny-unknown.sh

# Self-test for the payload contract guard (keeps its ast-grep rules honest)
check-payload-deny-unknown-selftest:
    ./scripts/check-payload-deny-unknown-selftest.sh

# Guard: every numbered ADR is listed in the ADR index and every index link exists
check-adr-index:
    ./scripts/check-adr-index.sh

# Self-test for the ADR index guard
check-adr-index-selftest:
    ./scripts/check-adr-index-selftest.sh

# Self-test for the ffmpeg asset selector (keeps its pattern, floor and ordering honest)
select-ffmpeg-asset-selftest:
    ./scripts/select-ffmpeg-asset-selftest.sh

# Self-test for the constrained-run wrapper (argument handling only; runs nothing)
run-constrained-selftest:
    ./scripts/run-constrained-selftest.sh

# Races in this suite are found by repetition, not by a single run: issue #546
# reproduces at roughly 1 run in 8 on idle hardware, so "it passed locally"
# after one run means very little.
#
#   just test-repeat voom-node-agent delayed_acquire_replay_never_dispatches
#   just test-repeat voom-control-plane chaos_ 50

# Repeat one filtered test up to COUNT times, stopping on the first failure
test-repeat PKG FILTER COUNT='25':
    #!/usr/bin/env bash
    set -uo pipefail
    for i in $(seq 1 {{ COUNT }}); do
        if ! cargo test -p {{ PKG }} {{ FILTER }} >/dev/null 2>&1; then
            echo "FAILED on run $i of {{ COUNT }}; rerunning it with output:"
            cargo test -p {{ PKG }} {{ FILTER }}
            exit 1
        fi
        printf '\rrun %s/%s ok' "$i" "{{ COUNT }}"
    done
    echo $'\nno failure in {{ COUNT }} runs'

# The two ends of the parallelism range CI already runs, named so either can be
# reproduced on purpose. Each end has found races the other missed, so try both
# before concluding a test is not flaky.

# Run the suite serialized, as the coverage job does
test-serial *ARGS:
    cargo test --workspace --all-features {{ ARGS }} -- --test-threads=1

# Run the suite at this host's default parallelism, as the test job does
test-parallel *ARGS:
    cargo test --workspace --all-features {{ ARGS }}

# Reach for `test-repeat` first: constraints did not separate the cells for the
# one race measured so far, and this is for what repetition cannot reach
# (memory pressure, slow storage). See `scripts/run-constrained.sh --help`.

# Run the suite under runner-like limits, 4 cpus and 16G (Linux cgroup v2 only)
test-constrained *ARGS:
    ./scripts/run-constrained.sh -- cargo test --workspace --all-features {{ ARGS }}

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
    cd third_party/chaos-librarian && uv sync --locked
    cargo build -p voom-cli -p voom-ffprobe-worker -p voom-verify-artifact-worker -p voom-ffmpeg-worker
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

# Run the opt-in Toxiproxy network-resilience suite (server from PATH). Not part of `just ci`.
net-resilience *ARGS:
    ./scripts/net-resilience.sh {{ARGS}}

# Hermetic net-resilience run: download + SHA256-verify the pinned toxiproxy-server. Used by CI.
net-resilience-ci *ARGS:
    NET_RESILIENCE_DOWNLOAD=1 ./scripts/net-resilience.sh {{ARGS}}
