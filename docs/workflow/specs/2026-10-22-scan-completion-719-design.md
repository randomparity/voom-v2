# Validated owner-scan completion (#719)

## Problem

The custom scan pump publishes sessions but omits the terminal access evidence required
by accepted ADRs 0071/0073. Its direct tests use incomplete synthetic plans and inspect
sessions rather than ticket settlement. The control-plane validator is correct.

## Scope and architecture

Keep the existing owner-node scan flow (ADR 0077) and wire contracts. Before starting a
session or dispatching discovery, validate the acquired plan against the scan payload:
nonzero plan identity, acquiring owner, payload node identity, typed source root, and
exactly the canonical read-only storage-root declaration for that source. Require the
payload declaration and selected evidence declaration to agree. Existing typed evidence
validation enforces the epoch set; authenticated session start/complete and terminal
completion retain current control-plane epoch/incarnation fences.

Resolve the source root through the agent's configured storage-root map, reusing
`commit::canonical_root`. Canonicalize the payload provider locator, require an existing
directory and equality with that configured root, and dispatch the resolved root path.
A missing binding, different path, malformed declaration or mismatched owner fails the
lease before session start or worker invocation. Do not turn a root declaration into
proof of individual-file locality; the configured root and current session authorize
this existing enumeration path.

Pass configured roots from the runtime into the scan pump. On successful scan-session
publication, add the existing strict `artifact_access` consumption shape to its summary:
validated true, the selected owner, and selected typed access evidence. Construct this
only after the preceding validation succeeds. Keep failed outcomes free of consumption
claims. No new endpoint, schema, evidence type, compatibility path or ownership move.

Compared alternatives: attaching an unvalidated echo would bypass the missing enforcement;
relaxing control-plane completion would violate ADR 0073. Moving scanning into the generic
media executor adds a second orchestration migration without helping this repair. This is
an extension of the existing owner-agent responsibility, not a new architecture decision.

## Success

C1: A real acquired rooted scan publishes its session, completes the scanner ticket,
releases the lease, consumes its plan and creates no malformed-result issue.
C2: Selected owner/root/declaration or configured-locator mismatches stop before byte work;
completion rejects changed evidence through the existing strict validator.
C3: Existing session failure, epoch/incarnation fences and replay semantics remain intact.
C4: Focused regressions plus actual rooted scans and bounded Chaos inspect both session
and terminal ticket/plan outcomes. No production or original canary processing.

## Failure model

- Actors and deployments: configured owner agents, authenticated control-plane API,
  bundled scan/hash/probe children, local Linux acceptance and cross-platform CI.
- Invariants and assets: root authorization, original bytes, exact consumption evidence,
  session publication and durable ticket/lease/plan settlement.
- Accepted failure classes: existing session/lease separation can leave a published
  session if transport fails after publication; preserve existing failure/replay behavior.
  Concurrent hostile local filesystem replacement is outside the trusted-owner deployment.
- Covered elsewhere: control plane owns current epochs/incarnations and atomic terminal
  settlement (ADRs 0073/0077); worker owns candidate containment and scan bounds; #339 owns
  canary/resume/capacity and later production acceptance, operator owns retention decisions.

## Threat model

- Boundaries: existing API dispatch to agent, configured root to filesystem, agent to child,
  and completion evidence to control plane. No added or widened external boundary.
- Actors: malformed/stale dispatch and misconfigured roots are untrusted inputs; authenticated
  owner process and configured child binaries are trusted. No defense against a malicious owner.
- Controls: typed declaration/evidence and exact owner/root/payload agreement; configured-root
  canonical equality before discovery; existing session fences; unchanged exact terminal echo
  validation. Errors use existing failure classes and retain diagnostic evidence locally.
- Out of scope: hostile local path races, compromised children/owner, production execution,
  schema changes and artifact deletion, under the approved exclusions.

## Validation

Focused tests enforce fail-before-dispatch for wrong owner, root/right/declaration, absent
plan evidence and unbound/different/non-directory locator. Update pump fixtures to valid
rooted plans and assert successful summaries carry exact evidence. Add an acquired-lease
API regression checking session/ticket/lease/plan and rejecting altered evidence. Existing
scan failure/epoch tests remain behavioral protection. Use temporary directories owned by
fixtures. Prove new tests fail on the old implementation, then run focused tests, formatting,
source checks and full `just ci` before push. Build in an isolated target directory.

Live proof uses generated fixtures, exact committed binaries, repeated rooted scans and
20-second Chaos scan acceptance; inspect terminal states as well as sessions. Retain evidence
and outputs. Hardware transforms and full production are not needed for this scanner repair.
