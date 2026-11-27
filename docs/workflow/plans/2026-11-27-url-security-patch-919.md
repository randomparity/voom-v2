# URL security patch implementation (#919)

Base: main. Branch: feat/url-security-patch-919. Scope: q919-162135ba.
Expected implementation size: 8–8 changed lines (S), based on the url and required form_urlencoded version/checksum replacements in Cargo.lock. Design denominator: 100 changed lines from the live #919 assessment, independently of this estimate.

## Task 1 — update the existing locked package

Files: modify `Cargo.lock` only. Existing architecture and caller ownership remain unchanged.
Interfaces: consumes Cargo.lock registry package `url` 2.5.8; produces the same package/dependency edges at 2.5.9 for existing reqwest/sqlx consumers. No Rust signatures change.

Verification:
- Contract: remove the advisory-bearing URL version. Mode: focused-test. Red: `just audit` reports RUSTSEC-2026-0592 for url 2.5.8. Green: `just audit` exits 0 after the update.
- Contract: preserve unrelated locked packages and edges. Mode: focused-test. Compare parsed pre/post Cargo.lock package records; exactly url and form_urlencoded version/checksum differ, with final versions 2.5.9 and 1.2.3. The same assertion fails before the update because url is 2.5.8.

Steps:
1. Run `just audit` and record the expected advisory failure; caller also observed `just deny` failure on clean b4f450a4 for the same advisory.
2. Run `cargo update -p url --precise 2.5.9`. Expected: url 2.5.8 -> 2.5.9 and required form_urlencoded 1.2.2 -> 1.2.3. If Cargo refuses the minimum dependency change, run `cargo update -p form_urlencoded --precise 1.2.3` then retry the url command. Inspect complete `git diff -- Cargo.lock`; stop for any unrelated resolution change.
3. Parse pre/post lockfiles with Python tomllib, assert one url 2.5.9 and one form_urlencoded 1.2.3 and identical package records except these two version/checksum pairs. Expect success.
4. Run `just deny`, `just audit`, then `just ci`; expect exit 0. Preserve real-time SQLite test execution and repository-local temporary storage.
5. Commit with installed hooks enabled; complete independent branch/security reviews and simplification, deliver green CI, then hand off the exact head for orchestrator merge.

Acceptance: the design's Success criteria hold. No new tests call the vulnerable URL operation. On failure, retain evidence and diagnose before changing scope; do not upgrade other packages speculatively.
Deferrals and follow-up candidates: none.
