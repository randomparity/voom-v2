# URL security patch (#919)

## Problem and authority

Issue #919 identifies RUSTSEC-2026-0592 in locked `url` 2.5.8. The operator approved the minimal 2.5.9 patch, excluding unrelated dependency maintenance and exploit/reproduction work. Scope: q919-162135ba on #919.

## Design

Use `cargo update -p url --precise 2.5.9`. Registry metadata requires `form_urlencoded ^1.2.3`, so also minimally advance its locked 1.2.2 to 1.2.3 with `cargo update -p form_urlencoded --precise 1.2.3` if the first command cannot resolve. Preserve manifests, package dependency edges, and other locked packages unless resolution proves a change strictly necessary. Inspect the complete lockfile diff before accepting it. Cargo owns registry resolution and checksums; no application ownership or caller migration is needed. Existing reqwest/sqlx consumers continue using the same dependency edge.

Doing nothing leaves the advisory unresolved. A broad update adds unrelated risk and violates scope. Application workarounds would duplicate an upstream repair. No architectural decision, ADR, or migration is needed.

## Success

The workspace lockfile contains exactly one `url`, at 2.5.9; only its version/checksum and the required `form_urlencoded` 1.2.2 → 1.2.3 version/checksum change. `just deny`, `just audit`, and `just ci` pass. A reviewed PR reaches exact-head MERGE-READY; the orchestrator owns merging and cleanup.

## Failure model

1. Actors and deployments: local operators and CI build this Rust workspace; its services and workers consume URL parsing transitively.
2. Invariants and assets: locked dependency integrity and application availability; the vulnerable version must leave the resolved graph without unrelated upgrades.
3. Accepted failure classes: none for the requested dependency repair. Exploit construction is excluded by the operator; advisory detection and lockfile inspection establish repair evidence without invoking the vulnerable operation.
4. Covered elsewhere: repository regression behavior by `just ci`; registry checksum validation by Cargo; unrelated advisories by separate operator maintenance scope.

## Threat model

1. Boundary inventory: existing registry package input enters the build; existing URL parsing consumes runtime input. No entry point or permission is added or widened.
2. Actor model: untrusted URL sources can reach dependent parsers; Cargo/crates.io and the upstream patched release are trusted for this bounded dependency repair.
3. Control per boundary: Cargo verifies registry checksums; cargo-deny and cargo-audit evaluate the resolved package against advisory policy; upstream 2.5.9 supplies the origin fix. Existing runtime validation remains in its present owners.
4. Out of scope: exploit construction and unrelated dependency maintenance, explicitly excluded by the operator; no claim is made about other upstream defects.

## Validation and rollback

The pre-change `just audit` must report RUSTSEC-2026-0592 for url 2.5.8; after the patch it and `just deny` must pass. Inspect lockfile structure and run the repository suite. Reverting this commit restores the vulnerable lockfile and therefore is not the preferred operational fallback; hold the PR if compatibility fails.
