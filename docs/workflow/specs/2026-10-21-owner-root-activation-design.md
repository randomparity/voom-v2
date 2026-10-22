# Authenticated owner-root activation (#436)

## Problem and scope
Configured roots cannot become usable through a production owner-agent path.
The three acceptance scripts also use removed path scans and local-worker bootstrap.
The operator approved atomic root activation as part of existing node activation.
Scope authority is issue #436 WORK:SCOPE q436-1e9e32c2, including its exclusions.
[ADR 0106](../../adr/0106-atomic-owner-root-activation.md) records the interface choice.

## Architecture and data flow
The owner agent validates its configured storage-root bindings before activation.
For each binding it canonicalizes the directory locally, requires a directory and
readable directory access, and derives the existing Unix device/inode identity format.
The configured locator remains the immutable provider locator; canonicalization and
filesystem metadata never run in the control plane. Root proofs carry typed root ID,
configured locator and activation identity. The existing activation request gains a
bounded list, defaulting empty for worker-only clients. No response field is needed.
Reject duplicate IDs, zero IDs, more than 64 roots, malformed identities and locators.
Keep stable retry bytes and the existing idempotency key/hash machinery.

After bearer authentication and activation replay handling, the control plane starts
the requested incarnation using the existing transaction. It loads each root by ID,
validates persisted data, and requires the authenticated node to own that root,
local_filesystem provider, matching configured locator, and activatable lifecycle state.
It composes existing root activation through a transaction-aware control-plane helper
so state and fact events retain one owner. A failed root rolls back incarnation,
workers, root mutations, events and idempotency reservation. Successful activation
commits them together. Replays return the original result without activating roots
again or duplicating events. Existing root identity/epoch rules remain unchanged.
A stale replay cannot resurrect a retired root or superseded incarnation.
No operator activation command or storage schema migration is introduced.

## Shared acceptance bootstrap
Use one shared helper for the three known scripts, driven through their existing
Bash entrypoints. It creates/configures a dedicated node, library, separate input,
staging and output roots through CLI commands, sets defaults and exact local node
identity, and starts the actual API and owning agent. It supports already initialized
acceptance databases and resolves binaries from the effective Cargo target directory.
Use API bind port zero and its structured listening record rather than a port race.
Wait for active roots and ready workers with a deadline and child-liveness checks;
failed activation terminates setup before scan. Return stable IDs to callers.
Register scan/hash/probe/verification and needed transform workers on the owner.
Explicit dependency paths use existing node-agent configuration.

Capture registration stdout in memory; write the token only into a private temporary
credential outside the evidence tree, mode 0600 under a 0700 directory. Evidence gets
only explicitly selected nonsecret fields. Do not log command environments or bearer
values. Remove the helper-owned credential at shutdown; retain nonsecret evidence.
Signals/failures stop and reap helper-owned processes with bounded shutdown.

Migrate Chaos and both hardware scripts to scans by root ID and root-scoped policy
inputs. Register staging/output roles and defaults required by ADR 0097. Hardware
media runs use owner-agent workers with declarations obtained from the existing
bundled-worker accelerator readiness probe. Preserve encoder/decode/convergence checks.
Keep the existing run-local VideoToolbox claim/crash/retirement contract assertions
as a separate dedicated-database arm: the node agent does not currently own that
local-supervisor claim table. Do not silently replace those assertions with agent logs.
Update the operator runbook and provisioning instructions to implemented commands.

## Success
The three named workflows contain no path scans and do not write lifecycle SQL.
An authenticated owning agent activates its configured roots and completes a real
software scan through CLI/API/agent/workers, without test-only activation.
Malformed, wrong-owner, wrong-locator, retired-root and failed-local-validation cases
cannot create a usable new activation. Retry does not duplicate mutations or events.
Helper failure/redaction tests establish no scan follows failed activation and the
registration token is absent from retained evidence on success and failure.
Run hardware-independent self-tests on this host; real VAAPI/VideoToolbox encode arms
require their matching devices/OS and are reported explicitly if unavailable.

## Failure model
- Actors and deployments: authenticated operator/agent on Unix Linux or macOS;
  bounded ephemeral local acceptance stacks; anonymous or wrong-node API callers.
- Invariants and assets: owner authority; current incarnation and root epochs;
  atomic state/events/replay; source media; bearer credentials; bounded child lifetime.
- Accepted failure classes: startup proof is a snapshot, not a filesystem watcher;
  later filesystem failure remains an execution failure under existing rooted access
  guards. Hardware-specific encode runs cannot execute on a host lacking that backend;
  this limitation is reported, with software integration and self-tests still required.
- Covered elsewhere: root state/epoch lifecycle ADR0055; path containment and commit
  fencing ADR0074/0097; existing activation authentication, quota and stale-incarnation
  replay guards; fixture execution races ADR0105. Full production selection remains
  the operator's #339 decision after measured canary results.

## Threat model
Boundary inventory: widen the existing bearer-authenticated activation JSON boundary
with owner proofs; add local filesystem/config-to-proof and registration-to-private-
credential/evidence boundaries. Actor model: local operator controls config, the
authenticated owner controls proofs, other nodes and anonymous callers are untrusted.
Controls per boundary: authenticate and transact ownership checks; validate/bound
proofs; canonicalize only on owner; capture token without evidence logging.
Explicitly out of scope: hostile owner attestation and continuous mount monitoring
are not promised by the accepted ownership/startup model; existing rooted execution
guards own subsequent path failures.
Bearer credentials authorize only the declared node; request body IDs and locators
are untrusted even with a valid token. Fetch/decode roots before comparing ownership.
No filesystem proof from another node, retired root, duplicate ID or replay may widen
access. The control plane trusts the authenticated owner to report its local identity;
this does not attest against a compromised owner. Bound root count and string lengths
under existing API body limits. Paths are data, never shell fragments. Secrets are
excluded at capture, not scrubbed from an already published log. Evidence is private;
public workflow summaries must redact host/user/network identifiers.

## Validation
The private implementation plan maps each material contract to a focused test and
red/green proof. Run relevant structural/lint checks, then one final just ci for the
assembled candidate. Verify actual binary identity before the software end-to-end run.
