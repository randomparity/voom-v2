# Scan child dispatch identity (#716)

## Problem and authority
The approved #716 scope is WORK:SCOPE q716-693f07a2 on the issue.
A fresh scan lease changes each child request body, but candidate dispatch keys use
only the agent incarnation and relative locator. A live worker rejects the second
body under that key. Real unchanged-file and Chaos runs reproduced this failure.
ADR 0077 already assigns scan orchestration and child dispatch to the owner agent.

## Design
Keep key construction in the existing node-agent candidate dispatcher. Prefix its
current locator tag with both `context.incarnation_id` and `context.lease_id`, matching
the existing discovery request. The scheduler's typed lease ID identifies one scan
execution, including its root. Hash and probe append their current operation suffixes;
sidecar hashes retain their existing primary/sidecar locator suffixes.
The same context and candidate produce the same request/key on retry. A later lease
gets fresh keys even when the root, relative locator and agent incarnation are unchanged.
Separate-root scans also have separate leases, so equal relative names cannot replay
each other's bytes. No protocol, cache or ownership change is required.

The alternative of adding only root identity still collides on a later scan of that
root. Adding only session identity would miss changed lease IDs on execution attempts.
Restarting children or weakening body-conflict checks would hide the defect and violate
scope. The existing discovery identity is the smallest reusable execution boundary.

## Success and validation
Record actual hash/probe dispatch keys and serialized request bodies at the scripted
child boundary. Exercise primary and sidecar hashing, a second lease on the same root,
a separate-root lease with equal locators, and an identical-context retry. A repeated
key must always carry an identical body in this named sequence; fresh leases must have
distinct keys. Existing pump failure, evidence and session tests remain unchanged.
Run the focused test red, apply the small key fix, then run the scan pump tests green.
Use real workers to prove two scans on one live agent and bounded Chaos checkpoints.
Because authenticated root activation is separately approved but unmerged in #436,
run those proofs in a dedicated sibling integration checkout combining #436 and #716;
retain its exact commit, source identities and binary hashes, and keep the #716 PR
based on main. Preserve failed evidence. Run relevant guards/hooks and final `just ci`.

## Failure model
- Actors and deployments: authenticated owner agent on supported Linux/macOS hosts;
  operator-initiated root scans, live long-lived hash/probe workers, bounded CI fixtures.
- Invariants and assets: fresh execution cannot replay prior file facts; identical
  requests may retry stably; byte ownership, root/lease fences and protocol body checks.
- Accepted failure classes: filesystem drift and disappearing files retain ADR 0077's
  existing evidence-less/absence behavior; this change promises no new retry policy.
- Covered elsewhere: worker replay/body-conflict and cache bounds remain in the worker
  protocol; root activation and script bootstrap remain #436; production/retention #339.

## Threat model
- Boundary inventory: existing agent-to-child idempotency header and request body;
  no new or widened endpoint, payload, authority or persistence surface.
- Actor model: operator controls root contents; relative locators are untrusted,
  validated discovery data. The agent owns typed incarnation and lease identities.
- Controls per boundary: retain existing validated locators, worker credential checks,
  bounded worker cache, request-body conflict detection and rooted execution fences.
- Out of scope: hostile storage-owner attestation and unrelated locator/header limits;
  existing rooted/protocol validation owns those boundaries and is not relaxed.
