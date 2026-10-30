# Control-plane / node-agent protocol gate — implementation plan (#817)

Goal: refuse control-plane/node-agent protocol skew before any lease runs.
Architecture: one `voom-api` middleware gates every node-token route on the
`x-voom-protocol-version` request header and stamps its own version on the response; the node
agent's `ControlPlaneClient` sends the header and refuses a 2xx answer without a matching one.
Spec: `docs/workflow/specs/2026-10-30-protocol-skew-gate-design.md`; ADR 0123.
Tech stack: Rust 2024, axum 0.8.9, reqwest, tokio.

Expected implementation size: 330–420 changed lines (M) — file map below: ~60 lines of
production code, ~250 of tests, ~30 of test-helper header lines, ~25 of docs and script.

## Global Constraints

- Header name `x-voom-protocol-version`; value `voom_core::PROTOCOL_VERSION` in decimal.
  Tests derive skewed values as `PROTOCOL_VERSION + 1` and `PROTOCOL_VERSION - 1`, never a
  literal, so a future bump keeps them meaningful.
- No change to `voom-core`, `PROTOCOL_VERSION`, migrations, `ErrorCode`, or envelope shape.
- Not touched: `crates/voom-worker-protocol/src/wire/ndjson.rs`,
  `crates/voom-node-agent/src/scan_session.rs` (#816); `scripts/media-evidence-*`,
  `docs/media-evidence-*` (#794).
- Unit tests in sibling `<source>_test.rs` files (`just check-test-layout`).
- Workspace lints: no `unwrap`/`expect`/`panic` in production code.
- Guardrails: focused `cargo test -p <crate> <name>`, then `just ci` before push, all under
  `flock -s /home/dave/src/voom-v2/.agent/corpus.lock`.

## File map

| File | Change |
|---|---|
| `crates/voom-api/src/protocol.rs` (new) | header const, `gate` middleware |
| `crates/voom-api/src/protocol_test.rs` (new) | gate unit tests |
| `crates/voom-api/src/lib.rs` | `mod protocol;` and `route_layer` in `base_router` |
| `crates/voom-api/src/{commit,scan}_test.rs`, `tests/{remote_execution_route,scan_session_flow,server_tls,server_cutoff}.rs` | request helpers send the header; skew test in `remote_execution_route.rs` |
| `crates/voom-node-agent/src/client.rs` | send header; `check_protocol_version` on 2xx |
| `crates/voom-node-agent/src/client_test.rs` | raw server stamps / omits header; new cases |
| `crates/voom-node-agent/tests/client_tls.rs` | mock response stamps header |
| `crates/voom-node-agent/tests/lifecycle.rs` | synthetic replay stamps header; older-control-plane test |
| `crates/voom-fakes/src/remote_runner.rs` | send header |
| `scripts/issue-733-group-proof-fixture.py` | relay response header |
| `docs/runbooks/operator-control-plane-api.md`, `docs/runbooks/operator-node-agent.md`, `docs/release-process.md`, `docs/adr/README.md` | docs, index row |

## Task 1 — control-plane gate

Verification:
- `Mode: focused-test` — node routes refuse a missing, malformed or unequal version before the
  handler and stamp the version. `crates/voom-api/src/protocol_test.rs`; red: compile error
  (`protocol` absent), then 401/404 instead of 400. Green: `cargo test -p voom-api protocol`.
- `Mode: focused-test` — an unversioned activation or acquire changes no durable state.
  `crates/voom-api/tests/remote_execution_route.rs::unversioned_requests_take_no_lease`; red:
  activation answers 200. Green: `cargo test -p voom-api --test remote_execution_route unversioned`.

Steps:
1. Write `protocol_test.rs` with `unconfigured_app()` (copy from `scan_test.rs:1122`) and:
   - `every_node_route_refuses_a_request_without_a_version`: for each of the 20 (method, path)
     pairs from `commit::routes`, `execution::routes`, `scan::routes` (paths with ids `7`, `9`,
     `1`), send no header; assert 400, `error.code == "BAD_ARGS"`, message contains `missing
     x-voom-protocol-version`, response header equals `PROTOCOL_VERSION.to_string()`.
   - `skewed_versions_are_refused_by_number`: `PROTOCOL_VERSION + 1` and `- 1` on
     `/v1/execution/node/7/activate`; 400, message contains `offered={v}, expected
     {PROTOCOL_VERSION}`, hint contains `ADR 0123`.
   - `malformed_versions_are_refused`: `"six"`, `""`, `"6.0"`; 400, message contains `malformed`.
   - `the_gate_runs_before_authentication`: no bearer and no header → 400 (not 401).
   - `a_matching_version_reaches_the_handler`: header `PROTOCOL_VERSION`, no bearer → 401 with
     the response header stamped.
   - `health_is_not_gated`: `GET /health` without header → not 400, no response header.
2. Run it; expect compile failure.
3. Create `protocol.rs`:

```rust
//! Control-plane / node-agent protocol version gate (ADR 0123).

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use voom_core::{ErrorCode, PROTOCOL_VERSION};

use crate::err_response;

/// Carries the sender's [`PROTOCOL_VERSION`] on node-route requests and responses.
pub const PROTOCOL_VERSION_HEADER: &str = "x-voom-protocol-version";

const REFUSAL_HINT: &str = "run the control plane and every node agent from one release; \
                            mixed protocol versions are refused (ADR 0123)";

/// Refuse a node-route request from another protocol version before the handler runs, and
/// stamp this control plane's version on every response.
pub(crate) async fn gate(request: Request, next: Next) -> Response {
    let mut response = match check_offered(request.headers()) {
        Ok(()) => next.run(request).await,
        Err(message) => err_response(
            StatusCode::BAD_REQUEST,
            "api.request",
            ErrorCode::BadArgs.as_str(),
            message,
            Some(REFUSAL_HINT.to_owned()),
        ),
    };
    response
        .headers_mut()
        .insert(PROTOCOL_VERSION_HEADER, HeaderValue::from(PROTOCOL_VERSION));
    response
}

fn check_offered(headers: &HeaderMap) -> Result<(), String> {
    let Some(raw) = headers.get(PROTOCOL_VERSION_HEADER) else {
        return Err(format!(
            "missing {PROTOCOL_VERSION_HEADER} (control plane speaks protocol {PROTOCOL_VERSION})"
        ));
    };
    let offered = raw
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| format!("malformed {PROTOCOL_VERSION_HEADER}; expected a decimal version"))?;
    if offered != PROTOCOL_VERSION {
        return Err(format!(
            "unsupported protocol version: offered={offered}, expected {PROTOCOL_VERSION}"
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
```

4. In `lib.rs` add `mod protocol;` and change `base_router` to:

```rust
fn base_router(state: AppState) -> axum::Router {
    let node_routes = axum::Router::new()
        .merge(commit::routes())
        .merge(execution::routes())
        .merge(scan::routes())
        .route_layer(axum::middleware::from_fn(protocol::gate));
    axum::Router::new()
        .route("/health", get(health))
        .merge(node_routes)
        .with_state(state)
}
```

5. `cargo test -p voom-api protocol`; expect pass.
6. Add `.header("x-voom-protocol-version", voom_core::PROTOCOL_VERSION.to_string())` to every
   request builder hitting a node route in the files in the file map
   (`rg -n 'Request::(post|get)|Request::builder' crates/voom-api`). In
   `remote_execution_route.rs` add `unversioned_requests_take_no_lease`: with the fixture's
   ready ticket, POST activate for a fresh incarnation and POST acquire, both without the
   header → 400; then the ticket is still `Ready` and the node lists only the fixture's
   incarnation.
7. `cargo test -p voom-api`; expect pass. Commit
   `feat(api): refuse node requests from another protocol version`.

## Task 2 — node-agent check

Verification:
- `Mode: focused-test` — every request carries the header; a 2xx answer with a missing,
  malformed or unequal header is a terminal `Config` error after one attempt; error answers
  keep their classification. `crates/voom-node-agent/src/client_test.rs`; red: header absent
  in `CapturedRequest`, skewed success decodes. Green: `cargo test -p voom-node-agent --lib client`.

Steps:
1. In `client_test.rs`: add `protocol_version: Option<String>` to `CapturedRequest` (read with a
   non-panicking lookup); make `write_response` stamp `PROTOCOL_VERSION`; add
   Copy-compatible variants `RawResponse::SuccessWithVersion(Option<u32>)` (`None` omits the
   header), `RawResponse::SuccessMalformedVersion` (header `x`) and `RawResponse::ProtocolRefusal`
   (400 with body `{"schema_version":"0","command":"api.request","status":"error","data":null,
   "warnings":[],"error":{"code":"BAD_ARGS","message":"unsupported protocol version: offered=5,
   expected 6","hint":"... (ADR 0123)"}}`).
   Tests: `requests_offer_the_agent_protocol_version`; `skewed_success_is_refused_without_retry`
   for `SuccessWithVersion(Some(PROTOCOL_VERSION + 1))`, `SuccessWithVersion(None)` and
   `SuccessMalformedVersion`, each
   asserting one captured request and `ErrorCode::ConfigInvalid` with `ADR 0123` in the message;
   `protocol_refusal_is_terminal`: a client with `maximum_attempts: None` receiving the gate's
   400 `BAD_ARGS` refusal body (`RawResponse::ProtocolRefusal`) returns `ConfigInvalid` after
   exactly one captured request;
   `error_answers_are_not_version_checked`: a 400 with no header still yields the existing
   `Config("bad input")`.
2. Run; expect failures. Implement in `client.rs`:

```rust
const PROTOCOL_VERSION_HEADER: &str = "x-voom-protocol-version";
```

   in `send_attempt`, after `.bearer_auth(..)`:
   `.header(PROTOCOL_VERSION_HEADER, voom_core::PROTOCOL_VERSION.to_string())`; after the
   retry-status branch and before `read_bounded_body`:

```rust
        if status.is_success()
            && let Err(error) = check_protocol_version(response.headers())
        {
            return AttemptResult::Complete(Err(error));
        }
```

```rust
/// Refuse a control-plane answer from another protocol version (ADR 0123).
fn check_protocol_version(headers: &reqwest::header::HeaderMap) -> Result<(), VoomError> {
    let expected = voom_core::PROTOCOL_VERSION;
    let Some(raw) = headers.get(PROTOCOL_VERSION_HEADER) else {
        return Err(VoomError::Config(format!(
            "control-plane response omitted {PROTOCOL_VERSION_HEADER}; this node agent speaks \
             protocol {expected} and refuses a control plane from another release (ADR 0123)"
        )));
    };
    match raw.to_str().ok().and_then(|value| value.parse::<u32>().ok()) {
        Some(offered) if offered == expected => Ok(()),
        Some(offered) => Err(VoomError::Config(format!(
            "control plane speaks protocol {offered}; this node agent speaks protocol \
             {expected} (ADR 0123)"
        ))),
        None => Err(VoomError::Config(format!(
            "control-plane response carried a malformed {PROTOCOL_VERSION_HEADER} (ADR 0123)"
        ))),
    }
}
```

3. Stamp the header in the `client_tls.rs` mock handler response. `cargo test -p
   voom-node-agent --lib client` and `--test client_tls`; expect pass. Commit
   `feat(node-agent): refuse a control plane on another protocol version`.

## Task 3 — callers and end-to-end skew

Verification:
- `Mode: focused-test` — an agent facing an older control plane exits before any lease.
  `crates/voom-node-agent/tests/lifecycle.rs::agent_refuses_an_older_control_plane_before_any_lease`;
  red: delete only the `check_protocol_version` call in `send_attempt` locally — the agent
  then acquires and the ticket leaves `Ready`; restore it. Green: `cargo test -p voom-node-agent
  --test lifecycle older_control_plane`.
- `Mode: focused-test` — the synthetic remote runner still completes against the gated API.
  Existing `crates/voom-fakes/src/remote_runner_test.rs`; red: 400 `missing
  x-voom-protocol-version`. Green: `cargo test -p voom-fakes remote_runner`.
- `Mode: focused-test` — the fixture relays the header and its own requests send it.
  `just check-owner-publication <fresh scratch dir>` (chain mode, synthetic lavfi media); red
  before step 3: the agent refuses the relayed activation. Before that, `python3 -c` importing
  the module and calling `GroupProofProxy._protocol_version()` must print `PROTOCOL_VERSION`.
- `Mode: task-test-not-applicable` — the three docs: prose has no executable consumer.

Steps:
1. `lifecycle.rs`: add `response_version: Option<u32>` to `LiveFixture::start` (existing
   callers pass `None`); when set, add an outermost layer that overwrites the response header
   with that value. Stamp `PROTOCOL_VERSION` on `delay_acquire`'s synthetic replay response.
   New test: ready a probe ticket, start the fixture with `Some(PROTOCOL_VERSION - 1)`, run
   `AgentRuntime::run_until(pending)` under `HANG_GUARD`; assert it returns `Err` whose text
   contains `this node agent speaks protocol` (the agent-side message, not the gate's), the
   recorded paths contain no `/v1/execution/lease/acquire`, and
   the ticket is still `Ready`.
2. `remote_runner.rs` `post_request`: add the header after `bearer_auth`. Run the green
   commands above.
3. In `scripts/issue-733-group-proof-fixture.py`: `forward` returns `(status, body, version)`
   with `version` from `response.headers.get("x-voom-protocol-version")` (and the `HTTPError`
   branch alike). Update the two-value unpacks at `reject` (`status, body, _ = ...`) and
   `check_expired_group_api`; the synthetic `(500, ...)` tuple in `do_POST` gains `None`; tuple
   equality checks keep working. `do_POST` sends the header when `response[2]` is not `None`.
   Add `_protocol_version()`, which reads `pub const PROTOCOL_VERSION: u32 = N;` from
   `crates/voom-core/src/lib.rs` beside the script's repository and fails loudly if absent; the
   header dicts built in `check_reuse_acquire` and `check_late_publication_rollback` add
   `"x-voom-protocol-version": self._protocol_version()`.
4. Docs: header line in the curl example and the header named in the reverse-proxy sentence
   (`operator-control-plane-api.md`); troubleshooting entry naming both refusal messages
   (`operator-node-agent.md`); one sentence in `docs/release-process.md`'s protocol 6
   paragraph; ADR 0123 index row.
5. `just ci`; expect green. Run `just check-owner-publication <fresh scratch dir>` once (chain
   mode routes the agent through the relay); expect success, and report the result. Commit `test(node-agent): prove protocol skew stops the agent
   before a lease` and `docs: record the control-plane protocol gate`.
