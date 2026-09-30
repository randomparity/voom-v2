# Independent required source checks

Issue: #640. Scope: q640-7536ccdc, approved by the operator on 2026-09-30.
Decision: [ADR 0101](../../adr/0101-independent-required-source-checks.md).

## Problem

At baseline db063448c367e5809677560f1a470b2ee6b8e400, both CI test jobs invoke
`just ci`. Its source guards follow lint compilation and precede tests. #637 already optimized
the guards; the old two-minute interval is not the baseline for this scheduling change.
Live main protection is absent and inherited rulesets are empty. Independent jobs need
separately verified required-check administration before they can protect merging.

## Scope and ownership

Move seven source guards and their six existing self-tests into `source-checks`:
test-layout, paused-time-db, control-plane-sql-boundary, check-constraint-bypass,
payload-deny-unknown, transaction-openers, and adr-index. Test-layout has no self-test.
`platform-checks` retains fmt-check, lint, select-ffmpeg-asset-selftest,
run-constrained-selftest, constrained-recipes-selftest, test, doc, deny, and audit.
`ci` calls both shared recipes; leaf commands stay unchanged. Existing hooks keep calling
leaf recipes and are outside this change. No old inline source-check list remains in `ci`.

The new `source-guards` matrix runs on ubuntu-latest and macos-latest independently of
`test`; there is no `needs` dependency. Its stable names are `source guards (ubuntu-latest)`
and `source guards (macos-latest)`. Checkout, just, and ast-grep reuse the existing pins.
The test matrix keeps its runners/names/cache/media setup and runs `just platform-checks`.
Its ast-grep installer moves with the only consumers to the source job. Coverage is unchanged.
Both guard OSes remain because shell guards exercise GNU/BSD find/sed/sort/awk/mktemp;
there is no single-OS coverage claim or consolidation.

## Merge gate and rollout

The campaign root alone applies the operator-approved main protection after the new check
contexts exist: both guard names bound to GitHub Actions app 15368, strict=false,
enforce_admins=true, no required reviews or actor restrictions. No required checks existed at
preflight; this approval does not require platform jobs. Re-read settings before applying,
stop for a changed policy, and read back the exact effective configuration afterward.
The approved policy is retained in issue640 trajectory5920460337. The API rejected combining
`contexts: []` with `checks` (HTTP 422, no mutation); the same approved policy was applied
using the accepted `checks` representation alone and independently read back.
The worker never writes repository settings. Failed guard jobs must remain merge-blocking
for the administrator actor too; settings inspection alone is not the fault proof.

For one temporary proof commit, the source job creates an unused Rust source file with
an inline test module before its normal command. Existing test-layout must report it and
fail the source job on both OSes. The normal local checkout stays valid, so hooks remain
active. Observe required guard failures and the PR's BLOCKED merge state without attempting
merge. Remove the temporary workflow injection in a separate commit before final samples.
Do not add a reusable failure switch to production workflows. Retain both commits for audit.

## Success and evidence

- The local dependency graph contains each original CI leaf once, partitioned as above.
- Both independent source jobs run the shared guard recipe; neither test job reruns it.
- A real existing guard fails in CI and required checks block the proof PR head.
- After injection removal, full local ci and required final CI pass on recorded heads.
- Report three optimized-base and three unchanged-candidate workflow executions, plus the
  separate intentional failure, within the approved seven-execution cap. Existing base run
  36782121321 is sample one. No failed, incomplete, or cancelled sample is silently replaced.
- Record workflow and job IDs, SHA, timestamps, runner image and cache conditions, workflow
  attempt latency (validated attempt run_started_at to final job end), each job runtime, summed runner-seconds,
  and macOS test duration. Record queue delay separately where observable. Validate the
  first rerun timestamp against its jobs; never reuse original creation as a rerun origin.
  Report min/median/max; no guaranteed latency reduction.
- Compare source-step intervals and scheduling placement separately from whole-run variance.
  Added source-job setup counts toward total runner time; #637 savings are not credited again.

## Failure model

- Actors and deployments: local developer with installed project tools; GitHub Actions
  ubuntu-latest and macos-latest; repository administrator and PR contributors.
- Invariants and assets: complete local suite, both platform suites, GNU/BSD guards,
  required guard enforcement, bounded paid CI executions, truthful immutable-head evidence.
- Accepted failure classes: no measurable overall speedup due to runner/cache variance;
  report it. Administrative editing can later remove protection; proof covers observed policy.
- Covered elsewhere: guard parsing/performance #637; build/profile #638; runner/concurrency
  #639; sharding/artifacts #641; hook redesign #485. Existing media/constrained/supply-chain
  behavior stays in platform jobs. Unrelated failures are reported, not silently repaired.

## Validation

Use `just --dump --dump-format json` to compare the old and new recipe graphs and assert
exact leaf-set preservation, disjoint partition, and completeness. Read workflow structure
and use existing YAML validation; actual Actions runs prove job names, scheduling, and tools.
Local source checks run three times before and after; retain logs and exit status separately.
The one fault run is required for enforcement proof. Full local `just ci` and existing hooks
remain mandatory before shipping; an applicable result may be reused only with recorded input
identity. A mandatory additional full run is not a replacement CI measurement sample.
Publish timing and enforcement evidence in the PR/issue report so the measured candidate
stays unchanged across its three runs. Any implementation correction invalidates candidate
comparability and requires operator guidance before expanding the seven-execution cap.
