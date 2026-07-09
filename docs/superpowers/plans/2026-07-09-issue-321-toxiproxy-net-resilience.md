# Toxiproxy Network-Resilience Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove `voom_worker_protocol::HttpClient` honors its timeout/error contract under real TCP faults by placing Toxiproxy between a live `HttpServer` and the client, as an opt-in `#[ignore]`d harness that never touches production code or per-PR CI.

**Architecture:** New `#[ignore]`d integration tests in `crates/voom-worker-protocol/tests/net_resilience.rs` drive Toxiproxy's REST API (via `reqwest`, already a workspace dep) to inject `timeout`, `reset_peer`, and `latency` toxics on the downstream path, asserting `ProtocolError::Timeout` / `InvalidPayload(request:)` / `Ok`. A `scripts/net-resilience.sh` harness provisions and reaps `toxiproxy-server` (pinned v2.12.0, per-platform SHA256), two `just` recipes wrap it, and a dispatch+weekly `.github/workflows/net-resilience.yml` mirrors `chaos-e2e.yml`. No `src/` change.

**Tech Stack:** Rust, tokio (`full`, test-util in dev), `reqwest` (json), `serde_json`; bash harness; GitHub Actions. External binary: Toxiproxy v2.12.0.

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-07-09-issue-321-toxiproxy-net-resilience-design.md`. **ADR:** `docs/adr/0033-toxiproxy-network-resilience-harness.md`. Every task's requirements implicitly include the spec; the scenario table (§4.3) is normative.
- **No production change.** Nothing under `crates/voom-worker-protocol/src/` is modified. If a scenario surfaces a real `HttpClient` defect, file a follow-up issue and `--skip` that scenario (see Task 2) — do not fix the client in this plan.
- **Style:** `[workspace.lints]` pedantic on; `unwrap`/`panic` denied, `expect` warns (and `warnings = deny` + clippy `-D warnings` promotes it to an error). Test code is linted by `cargo clippy --all-targets --all-features -D warnings`. Put `#![expect(clippy::unwrap_used, reason = "net-resilience tests fail loudly and are opt-in")]` at the top of `net_resilience.rs` (matching `crates/voom-cli/tests/chaos_librarian_e2e.rs`) and use `.unwrap()`, not `.expect()`. Functions ≤100 lines, ≤8 complexity, ≤5 positional params, 100-char lines, absolute imports only.
- **Ignore gating:** every `#[test]` in the file carries `#[ignore = "run via just net-resilience; requires a toxiproxy-server process"]`, so `just test` / `just ci` never run them and stay green with no Toxiproxy present. The file must still *compile* cleanly under `cargo build --all-targets` (that is what keeps it lint-gated).
- **Test layout:** the file lives in `crates/voom-worker-protocol/tests/` (integration test, exempt from the sibling-`_test.rs` convention). Its name does **not** end in `_test.rs`, so `just check-test-layout` does not require a `#[path]` link.
- **No paused time:** these tests use real wall-clock time and short client deadlines via `HttpClient::with_timeouts`; they never call `tokio::time::pause`. (No `SqlitePool` is involved, so `check-paused-time-db` is moot regardless.)
- **Guardrails.** Per commit touching Rust: `just fmt`, `just lint`, and a compile check `cargo test -p voom-worker-protocol --no-run`. Do **not** rely on `just test` to exercise these (they are `#[ignore]`d). Behavioral verification is `just net-resilience` (needs `toxiproxy-server`). For shell: `shellcheck` + `shfmt -i 2`. For the workflow: `actionlint` + `zizmor`. Before push: full `just ci` (must stay green and must not run the ignored suite).
- **Toxiproxy pin:** v2.12.0. Per-platform `toxiproxy-server-<platform>` SHA256:
  - linux-amd64 `556d891134a3c582dc1e1a3f7335fd55142e5965769855a00b944e13e48302fc`
  - linux-arm64 `53e770c1c3035b5a9f1bc629fce537db1f95f62b26f4ebe6e756afd701cf077c`
  - darwin-amd64 `9625bba4bd96117eedae49f982aba4c2f462b268dd406c9ff18186f9b1ef8afe`
  - darwin-arm64 `aa299966b52f16a8594f1cd0d1e9049dc2e8fe2c04a90c19860e2719b2b95d15`

---

## File Structure

- Modify: `crates/voom-worker-protocol/Cargo.toml` — add `reqwest = { workspace = true }` and `serde_json = { workspace = true }` under `[dev-dependencies]` (serde_json is a normal dep already but dev use is explicit; confirm it resolves without duplication — if already inherited, skip).
- Create: `crates/voom-worker-protocol/tests/net_resilience.rs` — the five scenarios + the Toxiproxy REST control helper.
- Create: `scripts/net-resilience.sh` — provision, start, run, reap.
- Modify: `justfile` — add `net-resilience` and `net-resilience-ci` recipes (not part of `ci`).
- Create: `.github/workflows/net-resilience.yml` — dispatch + weekly, mirrors `chaos-e2e.yml`.

---

### Task 1: Write the `#[ignore]`d net-resilience scenarios and Toxiproxy control helper

Where it fits: the core deliverable — the executable resilience contract. Everything else provisions or schedules it.

TDD note: these are `#[ignore]`d integration tests against an external process, so the red→green loop is driven by a **locally running toxiproxy**, not `just test`. Before/while implementing, install it (`brew install toxiproxy` on macOS, or download the pinned binary) and start it: `toxiproxy-server &` (listens on `127.0.0.1:8474`), then run `TOXIPROXY_ADDR=127.0.0.1:8474 cargo test -p voom-worker-protocol --test net_resilience -- --ignored --test-threads=1 --nocapture`. Once Task 2 lands, `just net-resilience` does this end-to-end. Write each scenario as a failing/observing test first, run it, then pin the asserted variant to observed behavior per spec §4.3.

**Files:**
- Modify: `crates/voom-worker-protocol/Cargo.toml`
- Create: `crates/voom-worker-protocol/tests/net_resilience.rs`

**Toxiproxy control helper (test-only, in the test file):**
- `fn toxiproxy_base() -> String` — read `TOXIPROXY_ADDR` (no default); panic with "set TOXIPROXY_ADDR or run `just net-resilience`" when unset.
- `struct Toxiproxy { base: String, http: reqwest::Client }` with:
  - `async fn create_proxy(&self, name: &str, upstream: SocketAddr) -> SocketAddr` — `POST {base}/proxies` with `{"name", "listen":"127.0.0.1:0", "upstream":"<upstream>", "enabled":true}`; parse the resolved `listen` from the JSON response and return it as a `SocketAddr`.
  - `async fn add_toxic(&self, name: &str, toxic: serde_json::Value)` — `POST {base}/proxies/{name}/toxics` with the given body (each caller sets `type`, `stream:"downstream"`, `toxicity:1.0`, `attributes`).
  - `async fn delete_proxy(&self, name: &str)` — `DELETE {base}/proxies/{name}` (best-effort cleanup at end of test).
  - On any non-success HTTP status or transport error, panic with the status/body (fail loud — a broken control call must not look like a passing scenario).
- Reachability: the first `create_proxy` call naturally fails loud if the server is unreachable; optionally add a `GET {base}/version` assertion in a shared setup helper for a clearer message.

**Upstream server helper:** reuse the pattern from `crates/voom-worker-protocol/src/http_test.rs` (`running_server`) — build `HttpServer::new(creds, handler)` and `.serve("127.0.0.1:0")`, returning the bound `SocketAddr` and the `ServerRunning` handle. `creds()`, an `OperationHandler` that returns a valid streaming/one-shot `OperationResponse`, `HandshakeRequest`/offered version, and `request(...)` builders mirror the existing `http_test.rs` helpers. Offer `voom_core::PROTOCOL_VERSION` so scenario 5's handshake yields `agreed == offered`.

**Scenarios (spec §4.3 — one `#[ignore]`d `#[tokio::test]` each, unique proxy name):**
- [ ] **Scenario 1 — handshake timeout.** Start server; create proxy; `add_toxic(type:"timeout", attributes:{timeout:0}, stream:"downstream")`; `HttpClient::with_timeouts(proxy_addr, 500ms, 500ms)`; assert `handshake(PROTOCOL_VERSION)` returns `Err(ProtocolError::Timeout { .. })`.
- [ ] **Scenario 2 — dispatch timeout (response head blocked).** Same toxic; `dispatch(creds, key, request)`; assert `Err(ProtocolError::Timeout { .. })`. Document in a comment that the toxic blocks the response head, so the deadline fires at `request().await` (validates the dispatch-deadline wrapper, not `read_response_line`).
- [ ] **Scenario 3 — handshake reset.** `add_toxic(type:"reset_peer", attributes:{timeout:0}, stream:"downstream")`; `with_timeouts(2s, 2s)`; assert `handshake` returns `Err(ProtocolError::InvalidPayload { detail })` with `detail.starts_with("request:")`. If observed variant/detail differs, record the observed value and update the assertion + spec table (spec permits this; the invariant is typed, non-`Timeout`, non-hanging).
- [ ] **Scenario 4 — dispatch reset.** Same `reset_peer` toxic; `dispatch`; assert `Err(ProtocolError::InvalidPayload { detail })` with `detail.starts_with("request:")` (same recording rule).
- [ ] **Scenario 5 — latency tolerance (liveness).** `add_toxic(type:"latency", attributes:{latency:200, jitter:0}, stream:"downstream")`; use `HttpClient::new(proxy_addr)` (production 10 s handshake deadline); assert `handshake` returns `Ok(resp)` with `resp.agreed == PROTOCOL_VERSION`.
- [ ] Each test deletes its proxy and shuts down its server at the end.

**Acceptance criteria (reviewer-checkable):**
- File compiles under `cargo test -p voom-worker-protocol --no-run` and passes `just lint`.
- Running against a live toxiproxy (`just net-resilience`, or manual start) all five scenarios pass; a red scenario is either fixed (helper bug) or, if it is a real client defect, filed + `--skip`ped per Task 2.
- `just test` does not execute them (they are `#[ignore]`d) and `just ci` stays green with no Toxiproxy installed.
- No file under `crates/voom-worker-protocol/src/` changed.

**Rollback:** delete the test file and revert the `Cargo.toml` dev-dep lines; nothing else depends on this task.

**Commit:** `test(worker-protocol): add Toxiproxy network-resilience scenarios (#321)`.

---

### Task 2: Provisioning harness `scripts/net-resilience.sh`

Where it fits: turns the `#[ignore]`d tests into a one-command, self-contained run and is what CI invokes.

**Files:** Create `scripts/net-resilience.sh` (`#!/usr/bin/env bash`, `set -euo pipefail`, `shellcheck`-clean, `shfmt -i 2`). Model lifecycle/trap handling on `scripts/chaos-e2e-local.sh`.

**Behavior (spec §4.5):**
- [ ] Resolve `repo_root`. Pin `TOXIPROXY_VERSION=2.12.0` and a per-platform SHA256 map (the four values above).
- [ ] Resolve platform from `uname -s`/`uname -m` → one of `linux-amd64|linux-arm64|darwin-amd64|darwin-arm64`; unknown platform → fail loud.
- [ ] Obtain the binary: if `NET_RESILIENCE_DOWNLOAD=1`, download `https://github.com/Shopify/toxiproxy/releases/download/v${TOXIPROXY_VERSION}/toxiproxy-server-<platform>` to a cache path, `shasum -a 256 -c` against the pinned digest (fail loud on mismatch), `chmod +x`. Otherwise require `toxiproxy-server` on `PATH`, failing loud with an install hint (`brew install toxiproxy` / set `NET_RESILIENCE_DOWNLOAD=1`) if absent.
- [ ] Choose control address: `TOXIPROXY_ADDR` if set, else `127.0.0.1:8474`. **Unconditionally** probe the resolved address (`nc -z` or bash `/dev/tcp`) and fail loud with an actionable "address already in use — stop the other server or set a free TOXIPROXY_ADDR" message if it is already listening (guards the silent-wrong-server mode for both the default and operator-set address).
- [ ] Start the server bound to the chosen address in the background, capture its PID, and `trap` a cleanup that `kill`s that PID on `EXIT`/`INT`/`TERM`.
- [ ] Readiness: poll `GET http://<addr>/version` (bounded, e.g. 50×0.1s) but only while the spawned PID is alive; if the PID dies, fail loud with the server output.
- [ ] `export TOXIPROXY_ADDR="<addr>"` so the tests' `toxiproxy_base()` and the server agree.
- [ ] Run `cargo test -p voom-worker-protocol --test net_resilience -- --ignored --test-threads=1 --nocapture "$@"` — forwarding `"$@"` so a caller can pass `--skip <scenario>` to quarantine a known-defective scenario (the green-suite escape hatch; document this in a comment with a placeholder for the follow-up issue link).
- [ ] Propagate the cargo exit code as the script's exit code.

**Acceptance criteria:** `shellcheck scripts/net-resilience.sh` and `shfmt -d -i 2 scripts/net-resilience.sh` are clean; running it locally (with `toxiproxy-server` available) provisions, runs the five scenarios green, and always reaps the server; a pre-occupied control port fails loud before starting.

**Rollback:** delete the script.

**Commit:** `test(worker-protocol): add net-resilience provisioning harness (#321)`.

---

### Task 3: `just` recipes

Where it fits: the ergonomic entry points; keeps these tests out of `just ci`.

**Files:** Modify `justfile`.

- [ ] Add (near the `chaos-e2e-*` recipes, with matching comment style), and do **not** add either to the `ci` recipe:
  ```
  # Run the opt-in Toxiproxy network-resilience suite (server from PATH). Not part of `just ci`.
  net-resilience *ARGS:
      ./scripts/net-resilience.sh {{ARGS}}

  # Hermetic net-resilience run: download + SHA256-verify the pinned toxiproxy-server. Used by CI.
  net-resilience-ci *ARGS:
      NET_RESILIENCE_DOWNLOAD=1 ./scripts/net-resilience.sh {{ARGS}}
  ```
- [ ] Verify `just ci` is unchanged (grep the `ci:` line — neither recipe appears in it).

**Acceptance criteria:** `just --list` shows both recipes; `just net-resilience` runs the suite (with a local toxiproxy); `just ci` still runs only its existing sub-recipes and stays green.

**Rollback:** revert the `justfile` hunk.

**Commit:** `build: add net-resilience just recipes (#321)`.

---

### Task 4: Scheduled workflow `.github/workflows/net-resilience.yml`

Where it fits: prevents the opt-in suite from silently rotting (the reason chaos-e2e.yml has a weekly cron).

**Files:** Create `.github/workflows/net-resilience.yml`, mirroring `chaos-e2e.yml` but lighter (no media tools — the suite only needs the worker-protocol crate).

- [ ] `on: [workflow_dispatch, schedule (cron "0 6 * * 1")]`; `permissions: contents: read`; a `concurrency` group keyed on `github.ref` with `cancel-in-progress: false`.
- [ ] Job `net-resilience` on `ubuntu-latest`, `timeout-minutes: 20`:
  - Checkout (pin `actions/checkout` to the same SHA as `chaos-e2e.yml`, `persist-credentials: false`, no submodules needed).
  - `Swatinem/rust-cache` (same pinned SHA as existing workflows).
  - `extractions/setup-just` (same pinned SHA).
  - `run: just net-resilience-ci` (the recipe downloads + SHA-verifies toxiproxy; no separate install step).
- [ ] Job `notify-failure` (`needs: net-resilience`, `if: failure() && github.event_name == 'schedule'`, `permissions: issues: write`) that opens a tracking issue via `gh issue create`, copied from `chaos-e2e.yml`.

**Acceptance criteria:** `actionlint .github/workflows/net-resilience.yml` and `zizmor .github/workflows/net-resilience.yml` are clean; all action refs are SHA-pinned; the workflow does not run on push/PR (dispatch + schedule only), so it never gates PRs.

**Rollback:** delete the workflow file.

**Commit:** `ci: add scheduled net-resilience workflow (#321)`.

---

## Post-implementation verification

- `just ci` green and did **not** run the ignored suite.
- `just net-resilience` green end-to-end (server reaped) on the dev machine.
- `shellcheck`/`shfmt`, `actionlint`/`zizmor`, `just lint`, `just fmt-check` all clean.
- `git grep -n toxiproxy -- crates/voom-worker-protocol/src` returns nothing (no production change).
