# Issue #419 — Durable scan sessions and observation reconciliation design

## Frozen charter

- **Interaction:** interactive.
- **Scope identity:** issue #419, token `C7BC1E00-731C-4095-99D3-97E3E96A78AE`.
- **Outcome:** durable manual scan sessions with ordered idempotent observation batches and
  completion-only absence reconciliation.
- **Completion criteria:** same-key batch replay creates no duplicate observations or events;
  out-of-order, skipped, conflicting, and cross-session batches fail clearly; disconnect,
  cancellation, timeout, and scan failure never retire unseen locations; only a successful
  complete traversal with all batches accepted can reconcile absence; a successful empty-root
  scan retires all locations live before that scan began; concurrent sessions for one root are
  serialized or rejected deterministically; CLI and API expose status, progress, terminal
  outcome, and reconciliation evidence.
- **Provenance:** issue #419, its accepted campaign dispatch, and the tokenized `WORK:SCOPE`
  comment. ADRs 0050 and 0055 constrain the existing root/incarnation and rooted-location model.
- **Exclusions:** remote discovery, hashing, and probing owned by #421; scheduler locality owned
  by #420.
- **Surface:** migration 0036, ADR 0067, scan domain/store/control-plane/API/CLI surfaces, focused
  tests, event and durable-payload inventories.
- **Ambiguities:** none. Concrete wire and persistence shapes are design outputs constrained by
  the issue criteria and existing repository contracts.

This design is governed by
[ADR 0067](../../adr/0067-durable-scan-sessions-and-completion-gated-reconciliation.md).

## Success conditions

The implementation is complete when all seven charter criteria are automated tests, migration
0036 is the only schema change, the new API bodies and durable payloads reject unknown fields,
all location retirement happens in the successful-completion transaction, and the repository's
focused gates plus `just ci` pass without warnings or skipped checks.

No implementation in this issue traverses, hashes, probes, or opens a storage root. Existing
local synchronous scanning remains unchanged and transitional until #421 replaces that byte
access. The new session protocol is independently exercisable through typed control-plane use
cases and authenticated API routes; it does not claim that the current node agent already runs
scan workers.

## Approaches considered

### Chosen: normalized session, batch, and observation ledgers

Store session state and counters in one row, batch replay identity in one row per accepted batch,
and observations in scalar rows. Successful completion tags each retained location row that it
retires. SQLite can then enforce session/sequence and session/locator uniqueness, join
observations to rooted locations without decoding JSON, and paginate evidence deterministically.

### Rejected: provisional stamps on `file_locations`

Writing a session ID or `last_seen` marker onto a live location during batch ingestion reduces
table count, but it lets a failed traversal mutate authoritative catalog state. Rolling those
writes back across many HTTP requests is not transactional and makes crash recovery a correctness
requirement. Observations remain separate until success.

### Rejected: one durable JSON document per batch

A blob preserves each HTTP request exactly, but it makes locator uniqueness and reconciliation
application-level scans over potentially large JSON. It also widens the ADR 0013 typed closure
without improving the wire contract. The request hash plus normalized rows preserves replay
identity with better database enforcement.

## Domain vocabulary

`ScanSessionId` is a SQLite-generated `u64` newtype in `voom-core`. `ScanSessionStatus` is a
closed enum with durable and JSON tokens `requested`, `running`, `succeeded`, `failed`,
`cancelled`, and `stale`. `succeeded` matches ADR 0050; the issue's prose term “completed” means
that terminal outcome and does not introduce a second token.

A `ScanObservation` contains:

- `provider_relative_locator: ProviderRelativeLocator`;
- `provider_object_identity: String`, 1–4096 UTF-8 bytes, no NUL;
- `size_bytes: u64`, rejected before SQLite if it exceeds signed 64-bit storage;
- `modified_at: OffsetDateTime` in the existing ISO-8601 wire form;
- `stability_started_at: OffsetDateTime`; and
- `stability_confirmed_at: OffsetDateTime`, which must not precede
  `stability_started_at`.

The provider object identity is opaque outside the owner provider. It can identify hardlinked
entries without making a device/inode spelling part of the cross-provider contract. Issue #421
defines how the local-filesystem scanner constructs these facts and how hash/probe results bind
to them.

A batch contains 1–1000 observations. The existing API-wide 1 MiB body limit remains the harder
byte bound. An empty traversal sends no batch and completes with a null last sequence.

## Persistence: migration 0036

### `scan_sessions`

| Column | Contract |
|---|---|
| `id` | `INTEGER PRIMARY KEY`, surfaced as `ScanSessionId` |
| `storage_root_id` | required FK to `library_roots`, `ON DELETE RESTRICT` |
| `root_epoch` | checked non-negative snapshot taken at request time |
| `owner_node_id` | required FK to the root owner at request time |
| `owner_incarnation_id` | nullable until start, then required FK to `node_incarnations` |
| `status` | closed six-token lifecycle vocabulary |
| `next_sequence` | next zero-based batch sequence, initially `0` |
| `batch_count`, `observation_count` | checked non-negative progress counters |
| `idle_timeout_seconds` | `1..=86400`; default selected by CLI/use case is `300` |
| `progress_deadline_at` | requested/start/latest-new-batch time plus idle timeout |
| `location_high_watermark_id` | nullable until start; highest pre-start rooted live location ID |
| `requested_at`, `started_at`, `terminal_at` | lifecycle timestamps with state/NULL checks |
| `terminal_reason` | null while active; 1–1024 UTF-8 bytes with no NUL for non-success terminal states |
| `retired_location_count` | zero until successful completion, checked non-negative |

The state-shape CHECK requires:

- `requested`: no incarnation, start time, high-water mark, terminal time, or reason;
- `running`: incarnation, start time, and high-water mark shape established, with no terminal
  fields;
- `succeeded`: all running bindings retained, terminal time set, reason null;
- `failed`, `cancelled`, `stale`: terminal time and nonblank reason set; a terminal state reached
  from running retains its bindings, while a requested terminal may keep them null.

The high-water mark uses nullable `FileLocationId`: null means there were no live rooted
locations when the session started. A partial unique index on `storage_root_id` where status is
`requested` or `running` is the database backstop for single-root concurrency. Indexes support
status/deadline recovery and root/time inspection.

### `scan_observation_batches`

The primary key is `(scan_session_id, sequence)`. Each row stores a lowercase SHA-256 request
hash, observation count, accepted timestamp, and cumulative count returned to the caller. CHECKs
bound sequence and counts to non-negative signed integers. The row is inserted only in the same
transaction as all its observation rows, the session counter advance, its event, and the remote
idempotency completion.

### `scan_observations`

The primary key is `(scan_session_id, batch_sequence, ordinal)`, with a foreign key to the batch.
The six observation fields above are scalar columns. A unique constraint on
`(scan_session_id, provider_relative_locator)` rejects duplicate traversal entries across batch
boundaries. The repository validates every locator, size, and timestamp read from SQLite before
returning a domain row or using it in reconciliation.

### Reconciliation pointers

After creating the session table, migration 0036 adds nullable
`library_roots.last_scan_session_id REFERENCES scan_sessions(id) ON DELETE RESTRICT`. Only the
successful completion transaction updates it. Existing roots receive null; no historical scan is
invented.

Migration 0036 also adds nullable
`file_locations.retired_by_scan_session_id REFERENCES scan_sessions(id) ON DELETE RESTRICT`.
Existing locations receive null. A CHECK requires the pointer to be null while `retired_at` is
null. Successful completion sets the pointer, `retired_at`, and `epoch = epoch + 1` together.
Filtering by the pointer yields stable per-location evidence; its prior epoch is the checked
persisted epoch minus one.

Foreign keys do not prove reconciliation semantics. Every inspection read that follows either
pointer uses checked joins. A root pointer must reference the highest-ID `succeeded` session for
that same root; single-root serialization makes session ID the total order of successful scans,
while historical session inspection remains valid independently of the pointer. A location
pointer must reference a `succeeded` session for the location's root, the location must be retired
with epoch at least one, its `retired_at` must equal the session's `terminal_at`, its ID must not
exceed that session's high-water mark, and its locator must be absent from that session's
observations. Session inspection also checks that the count of attributed location rows equals
`retired_location_count`. Any violation is `VoomError::Database`, not an empty result or domain
state.

Migration 0036 is additive and runs under the normal migrator transaction. It adds the new SQL to
`voom-store`'s embedded migrator and updates migration-count/schema tests. There is no down
migration; rollback uses the pre-migration database and prior binary.

## Session lifecycle and ordering

### Request

`request_scan_session(storage_root_id, idle_timeout_seconds)` validates the timeout, begins
`BEGIN IMMEDIATE`, and samples the injected control-plane clock after the writer lock is held. It
first transitions expired active sessions to `stale`, then loads the effective root. Missing or
corrupt roots fail before classification; disabled, unavailable, unassigned, retired, or
ownerless roots return the existing fail-closed blocked/configuration result. The inserted row
captures the current root epoch and owner node, appends `scan_session.requested`, and commits.

A concurrent request for the same root either observes the first active session and returns an
actionable conflict naming it or loses at the partial unique index and is mapped to the same
conflict. No request creates a ticket or lease.

### Start

An authenticated owner node calls `start_scan_session`. In one immediate transaction the use case
authenticates the bearer token, requires the request's incarnation to be the node's validated
current incarnation, reserves the remote idempotency key, loads the session and root, and checks:

1. session is `requested` and not expired;
2. path node equals the session and root owner;
3. root ID and epoch still match;
4. library/root are enabled and root is effectively active; and
5. root owner and current incarnation are unchanged.

It captures the maximum ID of currently live rooted locations for that root, transitions to
`running`, binds the incarnation, resets the progress deadline from the authoritative clock,
appends `scan_session.started`, stores the replay outcome, and commits.

The request transaction initializes `progress_deadline_at`. A successful start and each newly
accepted contiguous batch reset it to authoritative `now + idle_timeout_seconds`. After bearer,
current-incarnation, and session-owner checks, an already-accepted exact replay—whether found by
the HTTP key or by session/sequence request hash—returns its stored outcome before deadline or
root-availability fencing. It does not extend the deadline, terminalize the session, or emit an
event. Rejected requests and inspection also do not extend it. At
`now >= progress_deadline_at`, every genuinely new mutation first persists `stale` and its event,
so expiry wins over a new start, batch, success, failure, or operator cancel. Before the boundary,
immediate transactions serialize terminal outcomes and the first one to obtain the writer lock
wins.

### Accept batch

The batch route repeats the bearer/current-incarnation and session-owner checks, then looks up
`(session_id, sequence)` before mutable root, availability, epoch, deadline, or `running` fences:

- same request hash returns the stored accepted outcome, including after later terminalization,
  and emits nothing;
- different request hash is a conflict;
- no row requires `status = running` and `sequence = next_sequence`.

All observation validation occurs before insertion. A duplicate locator inside the request or in
an earlier batch is a conflict. A new accepted batch inserts the batch and observations, advances
`next_sequence`, increments both counters using checked conversions, extends the inactivity
deadline, appends one `scan_session.observation_batch_accepted` event, stores the remote replay,
and commits.

The remote idempotency key is namespaced by incarnation, as on existing node routes. Reusing one
key for another session or sequence produces a different route-instance request hash and fails as
an idempotency conflict. Thus both same-key and same-sequence replays are safe.

### Successful completion

The authenticated completion request contains:

```json
{"incarnation_id":"<32 lowercase hex>","last_sequence":7,"observation_count":1234}
```

For an empty traversal, both `last_sequence` and `observation_count` are respectively null and
zero. Otherwise `last_sequence + 1` must equal `next_sequence`, and the claimed count must equal
the durable session count. These checks are the complete-traversal watermark; a skipped batch can
never be hidden by a later sequence.

After all start/batch fences pass, completion computes retirement candidates ordered by location
ID:

- rooted, live `file_locations` for the session root;
- `id <= location_high_watermark_id`; and
- no session observation with the same provider-relative locator.

Before any update it rejects the whole completion if an existing pending, authorized, or
recovery-required commit scope contains a candidate location. This extends the shared commit-lock
query rather than copying its vocabulary. A conflict leaves the session running so the exact
completion request can be retried.

The transaction updates exactly those live rows with one `retired_at`,
`retired_by_scan_session_id`, and `epoch + 1`, verifies affected-row counts, marks the session
`succeeded`, updates the root's `last_scan_session_id`, appends one
`scan_session.succeeded` summary, completes replay state, and commits. Any error rolls back every
part, including location provenance and events.

Locations with IDs above the high-water mark are never retired by that session, even if their
locators are absent from its observations. This conservative rule prevents concurrent
publication from being mistaken for absence. A later complete session can reconcile them.

Completion necessarily performs an O(number of pre-start live root locations) anti-join and
update under SQLite's single writer. The supported bound for this slice is 100,000 live rooted
locations. A release-mode scale gate creates one root with 100,000 distinct live rooted locations,
accepts an empty traversal, and measures only the completion call; fixture creation and assertions
are outside the timer. On both existing `ubuntu-latest` and `macos-latest` CI runners, three fresh-
database repetitions must each complete within 25 seconds, leaving five seconds of the 30-second
API deadline for routing and response work. Each repetition verifies 100,000 retirements, the
session/root summary, and zero partial rows. Failure stops implementation for a design checkpoint;
chunking is not an acceptable fallback because it would expose partially reconciled catalog state.

### Other terminal outcomes and stale recovery

- The owner node may mark a running session `failed` with a 1–1024-byte UTF-8 reason containing
  no NUL.
- The operator may cancel a requested or running session through the local CLI/control-plane
  method with the same reason contract.
- Ending or superseding the bound incarnation marks its running sessions `stale` in the same
  node-lifecycle transaction.
- `remote_recover(now)` marks requested/running sessions past `progress_deadline_at` stale after
  stale-node recovery and before returning its expanded report.
- A later start, batch, completion, or failure call that observes expiry, root epoch drift,
  owner/incarnation drift, or unavailable root persists `stale` plus its event and replayable
  conflict before returning the conflict.

No non-success path queries retirement candidates or mutates `file_locations` or
`library_roots.last_scan_session_id`. Terminal rows reject further transitions. Operator cancel
and successful completion serialize under immediate transactions; exactly one wins.

## Repository and control-plane boundaries

`voom-core` owns `ScanSessionId` and `ScanSessionStatus`. `voom-store` adds
`repo/scan/{mod,sessions}.rs` and owns checked row decoding, state-transition SQL, batch insertion,
candidate/provenance queries, and keyset pagination. Its APIs preserve `StorageRootId`, `NodeId`,
`NodeIncarnationId`, `FileLocationId`, `ProviderRelativeLocator`, and `ScanSessionId` across every
boundary; they never flatten IDs into an intermediate primitive struct.

`voom-control-plane::scan::sessions` owns request-scoped transaction ordering, authentication,
root availability, event composition, remote replay outcomes, stale classification, and the
completion transaction. `ControlPlane` receives one `SqliteScanSessionRepo`. Existing discovery,
hash, probe, and `scan::persist` code is not moved or extended.

The shared in-flight commit-lock query becomes callable by this control-plane transaction and
recognizes `pending`, `authorized`, and `recovery_required`. Its existing callers retain their
behavior; tests prove the scan completion checks the whole candidate set before mutation.

## API contract

All node mutation routes use the existing HTTPS listener, bearer authentication,
`Idempotency-Key`, 1 MiB body limit, 30-second request deadline, standard JSON envelope, and
stable request hashing. Bodies carry `#[serde(deny_unknown_fields)]`.
In the route table, `...` expands exactly to `/v1/scan`.

| Method and path | Purpose |
|---|---|
| `POST .../node/{node_id}/session/{session_id}/start` | bind to current incarnation |
| `POST .../node/{node_id}/session/{session_id}/batch/{sequence}` | accept one batch |
| `POST /v1/scan/node/{node_id}/session/{session_id}/complete` | atomically succeed and reconcile |
| `POST .../node/{node_id}/session/{session_id}/fail` | fail without reconciliation |
| `GET .../node/{node_id}/session/{session_id}` | inspect as current owner incarnation |
| `GET .../node/{node_id}/session/{session_id}/reconciliation` | page reconciliation evidence |

Mutation bodies include `incarnation_id`; the batch body additionally contains `observations`,
completion contains the watermark, and failure contains `reason`. Authenticated GETs receive
`incarnation_id`, `after_id`, and `limit` query parameters, require the same bearer/current-
incarnation fence, and expose only sessions whose owner is the authenticated node. Limits follow
ADR 0031: default 50, maximum 100, ordered by `file_location_id ASC`, with an optional exclusive
`after_id` cursor.

The API does not add a scan-session acquisition route. That would route work outside tickets.
Issue #421 will dispatch a scan ticket whose payload names the requested session.

HTTP classifications reuse public codes: malformed input is `BAD_ARGS`/400, missing session is
`NOT_FOUND`/404, bad credentials are `UNAUTHORIZED`/401, stale fences/order/replay conflicts are
`CONFLICT`/409, unavailable root is the existing `BLOCKED` classification, payload bounds are
`PAYLOAD_TOO_LARGE`/413, and unexpected storage failures are server errors. Credentials are
validated before session existence is revealed.

## CLI contract

The existing `voom scan --root` synchronous transitional command remains unchanged in #419.
New top-level `voom scan-session` commands expose only the durable session surface:

- `request --root <id> [--idle-timeout-seconds <1..=86400>]` (default 300);
- `show --id <session-id>`;
- `list [--root <id>] [--status <status>] [--after <id>] [--limit <1..=100>]`;
- `reconciliation --id <session-id> [--after <file-location-id>] [--limit <1..=100>]`; and
- `cancel --id <session-id> --reason <nonblank text>`.

Every invocation emits exactly one existing CLI envelope. Session DTOs include IDs, root epoch,
owner/incarnation, persisted status, progress counters, deadline, lifecycle timestamps, terminal
reason, high-water mark, retired count, and whether reconciliation was applied. List and
reconciliation commands return the established keyset cursor shape. The CLI never prints bearer
tokens or provider object identities, and it does not claim that requesting a session schedules
or completes scan work.

## Event and durable-payload contract

`SubjectType::ScanSession` and these event kinds are added:

- `scan_session.requested`;
- `scan_session.started`;
- `scan_session.observation_batch_accepted`;
- `scan_session.succeeded`;
- `scan_session.failed`;
- `scan_session.cancelled`; and
- `scan_session.stale`.

Payloads use core newtypes and `ScanSessionStatus`, reject unknown fields, and contain only the
minimum lifecycle or count evidence. The observation and retained location rows carry detail;
events do not duplicate locator lists.

Remote idempotency responses for start, batch, completion, and failure are typed strict roots.
`docs/payload-contract-inventory.md` records their `remote_idempotency_keys.response_json`
closure and the new event family. `scripts/payload-contract-scope.txt` adds the defining scan
event and control-plane session files. No new durable JSON column is introduced by migration
0036.

## Failure ordering and corruption handling

Public mutation ordering is:

1. parse path/query/body and validate bounded typed values;
2. authenticate the bearer token without revealing session existence;
3. require the current incarnation fence;
4. reserve or resolve remote replay identity;
5. decode session/root/location rows with checked numeric, enum, locator, and timestamp parsing;
6. validate session ownership, lifecycle, deadline, root epoch, and root availability;
7. validate sequence, body hash, locator uniqueness, or completion watermark;
8. check every completion candidate against in-flight commit locks;
9. mutate session/catalog state and append facts in one transaction; and
10. complete replay state and commit.

An exact completed remote replay returns after steps 1–4 while still requiring current node
authority. A batch replay under a new HTTP key continues through checked session ownership and
batch identity, then returns the existing outcome before mutable fences and without another
event. Corrupt persisted state is
`VoomError::Database`, never `NotFound`, `Blocked`, or an ordering conflict. Checked conversions
reject negative or oversized SQLite integers before classification.

## Threat model

### Boundary inventory and actors

- **Existing widened boundary — authenticated owner node to control plane.** A legitimate but
  buggy or compromised node process controls session IDs, sequences, locator/object facts,
  stability timestamps, completion watermarks, failure reasons, and idempotency keys. It may not
  act for another logical node or incarnation.
- **New local operator boundary — CLI to SQLite-backed control-plane methods.** A local operator
  can request, inspect, and cancel sessions. This is the existing trusted local CLI deployment
  boundary; no unauthenticated HTTP operator mutation is added.
- **New persisted-data boundary — SQLite to typed readers.** Session, observation, and location
  provenance rows may be corrupt or manually edited and are untrusted on every read.
- **Existing log/output boundary.** Session metadata and failures reach API/CLI envelopes and
  tracing. Bearer tokens, request hashes, provider object identities, and full locator sets must
  not be logged.

### Controls

- Existing constant-time bearer verification plus current-incarnation checks authenticate every
  node API call; root/session owner equality authorizes it.
- Provider-relative locator validation, length/count/body bounds, checked numeric conversion,
  strict request DTOs, and chronological stability checks constrain untrusted input.
- One shared terminal-reason validator enforces 1–1024 UTF-8 bytes with no NUL for failure and
  cancellation API/use-case inputs, replay decoding, and persisted rows; migration CHECKs enforce
  the byte length and nonblank/no-NUL shape as a database backstop. The SQLite byte bound uses
  `length(CAST(terminal_reason AS BLOB))`, never `length(terminal_reason)`, which counts Unicode
  characters rather than encoded bytes.
- Immediate transactions, database unique constraints, request hashes, and immutable terminal
  states constrain replay and races.
- Root epoch/incarnation/availability/deadline checks and the high-water mark prevent stale or
  partial traversal evidence from widening retirement.
- Complete-watermark checks, candidate preflight, commit-lock checks, and one transaction prevent
  partial reconciliation.
- Authenticated API inspection exposes summary and retired location IDs only to the current root
  owner. Observation object identities and locators remain store/internal evidence for #421 and
  are not added to this API or CLI.
- Errors identify the operation, session/root ID, expected next sequence, and retry action where
  safe; authentication errors remain deliberately generic.

### Explicitly out of scope

This design does not protect a root from its legitimately authenticated owner submitting false
filesystem facts; #421's worker/agent evidence contract and provider implementation establish
those facts. It adds no token revocation, operator HTTP authentication, cross-tenant isolation,
object-store credentials, scanning sandbox, hash/probe validation, scheduler ownership gate, or
continuous cleanup loop. Those threats are owned by existing auth/deployment controls or the
explicitly excluded issues.

## Test strategy

Tests use real SQLite and the injected `ManualClock`; they never pause Tokio while a `SqlitePool`
is live.

### Domain and migration tests

- `ScanSessionId` round-trips without flattening and every status token is exhaustive.
- Migration 0036 creates all constraints/indexes, adds the root and location pointers, preserves
  existing root and location rows, rejects invalid state shapes, and appears in schema probes.
- Corrupt negative counters/epochs, unknown status, malformed timestamps, invalid locator/object
  identity, and impossible terminal shapes surface as database errors.
- Empty, ASCII 1024-byte, ASCII 1025-byte, and NUL-containing terminal reasons are exercised
  through failure API and cancellation CLI/use-case paths. Multibyte cases accept exactly 1024
  encoded bytes and reject the next complete UTF-8 scalar above the limit through both the shared
  validator and direct SQL insertion. Persisted out-of-contract reasons decode as database errors.
- FK-valid reconciliation pointers to a wrong-root, non-succeeded, or older same-root successful
  session; mismatched terminal and retirement timestamps; an above-watermark or observed
  attributed location; and a mismatched retired count each surface as a database error before
  inspection returns domain data.

### Repository and transaction tests

- Two concurrent requests for one root serialize: one succeeds and one names the active session;
  requests for different roots both succeed.
- Start binds the current incarnation and captures the correct location high-water mark.
- A new batch advances exactly once. Replaying the same HTTP key or the same sequence/hash under a
  different key returns the same outcome with unchanged observation/event counts.
- An exact accepted replay after its session deadline remains read-only and returns the stored
  outcome; recovery or the next genuinely new mutation then persists `stale` exactly once.
- Gaps, regressions, conflicting body hashes, duplicate locators within/across batches, cross-
  session key reuse, oversized batches, and invalid stability order fail before mutation.
- Disconnect, explicit failure, cancellation, timeout recovery, incarnation supersession, root
  unavailability, and root-epoch drift yield terminal/non-success state with no retired locations
  and no root pointer update.
- Completion rejects a missing/skipped final sequence or wrong count without mutation.
- Empty successful completion retires every pre-start live rooted location for the root and no
  other root. A non-empty traversal keeps observed locations and retires unseen ones.
- A location created above the start high-water mark remains live. A pending, authorized, or
  recovery-required commit lock makes completion retryable and leaves all candidates live.
- Forced event, replay-completion, location-update, session-update, and commit failures each roll
  back the whole logical operation.
- Reconciliation evidence pages by `retired_by_scan_session_id` in stable location-ID order and
  reports the derived prior and persisted retired epochs exactly.
- A release-mode completion gate with 100,000 absent pre-start locations runs three fresh-database
  repetitions on both CI operating systems; every measured completion is at most 25 seconds and
  retires exactly 100,000 rows, or implementation returns to design.

### API and CLI tests

- Every route rejects missing/invalid credentials before revealing session existence, rejects
  unknown JSON fields, enforces the body/deadline layers, and maps errors to the documented status
  and envelope.
- Authenticated inspection rejects a non-owner/current-incarnation mismatch and paginates without
  duplicates.
- `scan-session request/show/list/reconciliation/cancel` each emits exactly one JSON envelope;
  snapshots cover progress and every terminal state.
- Secret-safety assertions prove tokens, request hashes, provider object identities, and
  observation locators are absent from logs and normal inspection envelopes.

### Proof that tests bite

During implementation, temporarily remove the completion status predicate and confirm the failed-
session reconciliation test fails; temporarily remove the sequence increment and confirm replay/
gap tests fail; temporarily drop the high-water predicate and confirm the concurrent-location test
fails. Restore each mutation before committing.

Focused commands are recorded in the implementation plan. Final verification is `just ci`,
`git diff --check origin/main...HEAD`, the issue workflow's adversarial review, and its threat scan.

## Durable handoff facts

- Branch: `feat/durable-scan-sessions-419`
- Base branch: `main` (`origin/main` at design start)
- Assigned migration: `0036`
- Assigned ADR: `0067`
- ADR index coupling: coupled; this change includes exactly the ADR 0067 README row.
- Guardrails: focused `cargo test -p ...` commands during TDD, then `just fmt-check`,
  `just check-test-layout`, `just check-paused-time-db`, `just lint`, relevant focused tests, and
  `just ci` before push.
