# VOOM - Video Orchestration Operations Manager

## Purpose

This document specifies a from-first-principles architecture for a Rust-based
video library manager. The product manages video libraries through policy-driven
planning, durable job execution, out-of-process providers, remote nodes, and
agent-friendly interfaces. It supports CLI workflows, daemon operation, a web
interface, remote workers, plugin extensibility, durable media identity, and
production-grade observability. It also treats asset bundles, external-system
sync, durable issues, quality scoring, and runtime use locks as control-plane
concepts.

## Product Influences

TDarr demonstrates the value of distributed transcoding nodes, worker pools,
health checks, and operational controls around scan/watch behavior. Its model
shows that node scheduling and resource specialization must be early design
concerns, not later deployment details.

FileFlows demonstrates the power of reusable processing flows, branches, and
pluggable processing nodes. Its visual-flow model is flexible, but this design
chooses declarative policies and typed plans because they are easier to validate,
review, diff, test, and operate through CLI or agents.

Unmanic demonstrates a useful staged processing lifecycle: detect work, process
through worker plugins, use a cache, and then post-process. This design borrows
the lifecycle discipline and cache/staging safety, while replacing plugin-stack
execution with durable tickets and typed plans.

VOOM demonstrates the right product neighborhood: Rust, declarative media
policies, FFmpeg/MKVToolNix execution, SQLite state, CLI, web UI, and plugin
extensibility. This design deliberately avoids making an event bus the primary
work-routing mechanism. Work is routed through durable jobs and leases; events
record facts.

Plex and Jellyfin demonstrate the importance of playback-aware variants,
optimized versions, active playback state, and library refresh integration. A
video manager should not replace, move, or delete files that users are actively
watching.

Radarr, Sonarr, and Bazarr demonstrate that external applications often hold
useful identity, quality, subtitle, and wanted/missing state. This design treats
those systems as evidence and sync providers, not as hidden side effects inside
plugins.

tinyMediaManager and related media managers demonstrate that sidecar assets
such as NFO files, posters, trailers, and artwork are part of the managed
library experience. This design models those as bundle members instead of
untracked files next to the video.

Autoscan-style tools demonstrate that external library refresh and remote path
mapping are operational state. Refreshes, path maps, visibility, and sync
failures belong in the control plane.

Reference URLs:

- TDarr: <https://docs.tdarr.io/docs/>
- FileFlows: <https://fileflows.com/docs>
- Unmanic: <https://docs.unmanic.app/docs/using_unmanic/getting_started/>
- VOOM: <https://github.com/randomparity/voom>
- Plex Media Optimizer:
  <https://support.plex.tv/articles/214079318-media-optimizer-overview/>
- Plex Webhooks: <https://support.plex.tv/articles/115002267687-webhooks/>
- Jellyfin movie versions: <https://jellyfin.org/docs/general/server/media/movies>
- Bazarr setup: <https://wiki.bazarr.media/Getting-Started/Setup-Guide/>
- Autoscan docs: <https://docs.saltbox.dev/apps/autoscan/>
- tinyMediaManager features: <https://www.tinymediamanager.org/features/>

## Selected Architecture

Use a distributed control-plane-first architecture.

The control plane owns durable coordination. It stores policies, plans, jobs,
leases, nodes, artifacts, bundles, events, issues, scores, external-system
links, approvals, and audit history. It never directly executes media
operations. Every provider, including bundled providers, runs out of process and
speaks the same versioned worker protocol.

The core lifecycle is:

```text
Policy -> Plan DAG -> Durable Tickets -> Scheduler Leases -> Worker Results -> Host Commit
                                      |
                                      v
                              Append-only Events
```

Jobs and tickets are the source of execution truth. Events are append-only facts
for audit, metrics, UI updates, debugging, and optional reactive behavior. Events
do not claim, lease, or execute primary work.

This architecture is intentionally more explicit than a pure event bus. It keeps
CLI, daemon, API, web UI, local workers, remote nodes, synthetic workers, and
future plugins on one execution path.

## Design Principles

- Durable jobs route work; events record facts.
- All providers are out-of-process workers from day one.
- Built-in workers and third-party workers use the same protocol.
- Remote nodes are an early milestone, not a future migration.
- Synthetic providers are first-class contract clients and test infrastructure.
- Durable media identity is separate from paths, hashes, and storage locations.
- Identity evidence is stored as product data, not discarded as temporary match
  state.
- Asset bundles make sidecars, generated files, and primary media part of one
  durable user-facing unit.
- External systems, path mappings, and refresh jobs are durable control-plane
  state.
- Issues are durable findings with severity and priority, not transient log
  messages.
- Quality scoring uses named profiles so libraries can define "best"
  differently.
- Runtime use leases protect active playback, scans, manual locks, and risky
  commits.
- Policies describe desired media outcomes.
- Scheduling policies describe operational preferences.
- Safety policies describe approval, backup, verification, and rollback rules.
- Artifact handles abstract over local paths, shared mounts, object stores, and
  staged files.
- The host owns final commit by default. Workers produce staged artifacts.
- The first executable milestone proves the control plane without real media
  tools.

## Non-Goals For The First Product Line

- A visual workflow engine as the primary policy model.
- Event-bus-based work claiming.
- In-process executor fast paths.
- Worker-specific special cases in the scheduler.
- Direct original-file mutation by default.
- Plugin-defined arbitrary untyped JSON operations in the first DSL.
- External-system writes hidden inside provider-specific side effects.
- Automatic deletion or archiving based only on identity suggestions.
- A mandatory external database for home deployments.

## Core Components

### Control Plane

The control plane is the durable source of truth. It owns:

- SQLite database and migrations.
- In-memory SQLite test mode using the same schema and repositories.
- Library configuration.
- Durable media identity registry.
- Identity evidence registry.
- Asset bundle registry.
- External system registry.
- Issue registry.
- Quality scoring registry.
- Policy registry.
- Job queue and leases.
- Node registry (Sprint 4 — see Sprint 1 design for the interim
  `workers` table that absorbs remote/local/synthetic distinctions
  via its `kind` column).
- Artifact catalog.
- Runtime use leases.
- Event log.
- Approval and safety state.
- REST API.
- CLI command handlers.
- Web UI backend.

The control plane should expose clean storage boundaries for jobs, leases,
events, artifacts, policies, nodes, and library state. SQLite is the default
database, but the schema and transaction model should preserve a credible path
to Postgres if a future deployment profile needs it. Stable media, asset,
version, and location identifiers must be used throughout the control plane so
renames, moves, storage-provider changes, remuxes, transcodes, archives, and
restores do not break history.

### Policy Compiler

The policy compiler parses declarative media policies into a validated phase
DAG. V1 uses a small fixed operation vocabulary:

- scan library
- probe file
- hash file
- identify media
- score quality
- sync external system
- back up file
- remux/containerize
- transcode video
- edit tracks
- extract audio
- transcribe audio
- verify artifact
- commit artifact
- delete artifact

Later plugin packages may register namespaced typed operation schemas, such as
`whisper.transcribe_audio` or `acme.detect_commercials`. The compiler validates
those operations against registered schemas. Extensibility must stay typed,
inspectable, and reject invalid policies before execution.

### Planner

The planner compares a stored `MediaSnapshot` for a `FileVersion` to a compiled
policy and emits:

- `ComplianceReport`: whether the file satisfies the desired state and why.
- `ExecutionPlan`: a full phase DAG of durable tickets.
- Resource estimates: CPU, GPU, disk, network, expected duration, and temporary
  storage.
- Artifact expectations: inputs, outputs, checksums when known, durability, and
  commit targets.
- Safety gates: backup required, approval required, verification required, and
  rollback behavior.

The planner builds the full DAG upfront so the scheduler can reason about
resources and future dependency unlocks. The system revalidates at phase
boundaries and supports bounded replanning when produced artifacts change
downstream assumptions.

### Identity Registry

The identity registry separates logical media identity from file lineage,
content revisions, and storage locations. It owns:

- `MediaWork`: a logical title or recording, such as a movie, episode, or
  personal video.
- `MediaVariant`: a user-meaningful retained variant of a work, such as HD, 4K,
  theatrical cut, director's cut, remaster, mobile encode, or custom label.
- `AssetBundle`: the user-facing bundle of primary media and sidecar assets for
  a media variant.
- `FileAsset`: a durable managed file lineage that survives rename, move,
  remux, transcode, archive, restore, and storage-provider migration.
- `FileVersion`: an immutable byte/content revision under a file asset.
- `FileLocation`: current and historical locations for a file version.
- `IdentityEvidence`: auditable assertions from users, policies, paths,
  external databases, hashes, runtime similarity, frame matching, and plugins.

The default posture is conservative. Ingest preserves every discovered
`FileAsset`. The system may create provisional `MediaWork` and `MediaVariant`
records when identity is uncertain, but it must not archive, delete, or collapse
variants unless an explicit policy authorizes that action.

Identity evidence providers can suggest work matches, variant labels, duplicate
relationships, and external IDs. Policies decide which suggestions are
actionable.

### Asset Bundle Registry

The asset bundle registry tracks the files that together represent a
`MediaVariant`. A bundle can contain:

- primary video assets
- internal container tracks recorded in media snapshots
- external subtitle assets
- external audio assets such as commentary tracks
- posters and artwork
- NFO and metadata files
- trailers
- thumbnails
- transcripts
- generated logs and reports that should survive policy runs

Bundle identity belongs to `MediaVariant`, not to a single primary video file.
If a better encode replaces the primary video, the bundle survives. This allows
posters, commentary tracks, subtitles, and other sidecars to remain attached to
the retained variant.

Policies can target either a bundle or individual assets within a bundle. Bundle
transactions let the host commit or roll back related sidecar changes together,
such as renaming a primary video, subtitle, NFO, and poster as one operation.

### External System Registry

External systems are durable integrations, not private plugin state. The
registry tracks:

- system kind: Plex, Jellyfin, Emby, Radarr, Sonarr, Bazarr, filesystem, S3, or
  future providers
- connection profile and auth reference
- health status
- rate limits and budgets
- path mappings
- linked libraries
- linked media works, variants, bundles, assets, or external IDs
- supported sync operations
- visibility constraints for local and remote workers

Read-only imports from external systems can create identity evidence, quality
facts, issues, and external links. Writes such as library refresh, tag updates,
or remote deletion are durable jobs and must pass safety policy. External
path mappings are inputs to artifact placement and scheduler eligibility.

### Issue Registry

Issues are durable findings that may need attention, policy, or future work.
They are separate from jobs. A job is work to execute; an issue is a condition
that should be tracked until resolved, suppressed, or accepted.

Issue examples:

- `unknown_identity`
- `missing_subtitle`
- `duplicate_candidate`
- `policy_noncompliant`
- `health_failed`
- `external_sync_failed`
- `artifact_unavailable`
- `variant_retention_conflict`
- `worker_untrusted`

Issues have both severity and priority. Severity describes how bad the condition
is. Priority describes how soon the system should act. System defaults can rank
health failures high, storage-pressure duplicate candidates high, and optional
metadata gaps low. Users and policies can override priority.

### Quality Scoring Registry

Quality scoring compares assets, versions, and variants using named profiles.
Each library can choose its active scoring profile. For example:

- `balanced-home`
- `small-direct-play`
- `preserve-source`
- `anime-subtitle-focused`
- `4k-plus-hd-fallback`

Scores can include resolution, codec, HDR, audio layout, language match,
subtitle coverage, source quality, bitrate efficiency, file size, health status,
identity confidence, external custom-format scores, and user preference.

Scores are evidence-backed and versioned by provider, scoring profile, and
policy version. Retention policies can use them to keep the best version, keep
multiple purposeful variants, or archive lower-quality duplicates.

### Runtime Activity Registry

Runtime activity records active use of assets, bundles, versions, or locations.
These records are use leases. They may be advisory or blocking.

Use lease examples:

- playback from Plex, Jellyfin, Emby, or another client
- manual user lock
- external library scan
- active copy or backup
- worker-owned operation
- CLI or web UI maintenance lock

Playback leases block delete, archive, replace, or move commits by default;
the authoritative behavior lives in the `Commit Safety Gate` under
`## Runtime Use Lease Model`. They can also reduce scheduling score for heavy
reads from the same location. Manual blocking locks override automation.
Expired leases are cleaned up by the control plane.

### Scheduler

The scheduler leases ready tickets to workers. It considers:

- worker capabilities and grants
- node health and heartbeat freshness
- ticket priority
- issue severity and priority
- dependency unlock order
- artifact locality
- external path visibility
- storage and transfer cost
- quality scoring and retention goals
- runtime use leases
- measured throughput
- concurrency limits
- dynamic throttles
- scheduling windows
- safety requirements
- user policy overrides

The scheduler uses full-plan visibility for lookahead but does not permanently
bind every ticket to a node at plan creation. It leases dynamically at ticket
boundaries so nodes stay busy and failures can be handled without changing the
media policy.

### Artifact Resolver

Workers receive logical `ArtifactHandle`s, not raw assumptions about where bytes
live. The artifact resolver turns handles into access plans based on worker
capabilities and system policy.

An artifact handle includes:

- artifact identity
- media work identity when known
- media variant identity when known
- asset bundle identity when known
- file asset identity
- file version identity
- size
- checksum when known
- privacy class
- durability class
- allowed access modes
- mutability
- source lineage

The resolver ranks viable placements using:

- same-node locality
- shared mount availability
- object-store availability
- measured throughput
- latency
- current congestion
- monetary cost
- egress cost
- storage class
- safety constraints
- user-defined limits

The fastest, closest, least expensive safe backing store should be selected.
User policy may override the default optimizer.

### Worker Runtime

Every provider runs as an out-of-process worker. The worker protocol is
network-capable from day one, even when workers run on the same machine.

Workers:

- register with the control plane
- advertise capabilities
- receive grants from the host
- heartbeat
- accept leases
- resolve artifact access plans
- stream structured logs and progress
- produce staged artifacts
- return typed results
- report failures with actionable categories

The control protocol should optimize for human and agent inspectability:
versioned HTTP plus JSON for commands and responses, with NDJSON or SSE for
progress streams. Large media bytes move through artifact handles, not through
the control protocol.

### Event Log

The event log records facts:

- library scan started
- file discovered
- file missing
- file modified
- file asset created
- file version created
- file location changed
- asset bundle changed
- identity evidence recorded
- media work linked
- media variant linked
- external system linked
- external sync requested
- issue opened
- issue priority changed
- issue resolved
- quality score recorded
- use lease acquired
- use lease released
- probe completed
- policy evaluated
- plan created
- ticket ready
- ticket leased
- worker heartbeat missed
- artifact produced
- artifact verified
- commit completed
- job failed
- job completed

Events feed UI, metrics, audit, debugging, and optional reactive plugins. They
do not replace durable jobs or leases.

### Interfaces

All interfaces are clients of the same control plane.

The CLI must be agent-friendly:

- JSON input and output for all core commands.
- Dry-run mode.
- Plan inspection.
- Stable error codes.
- Machine-readable diagnostics.
- Human-readable table/plain modes where useful.

The daemon continuously monitors libraries, schedules jobs, manages remote
nodes, applies throttles, and recovers from crashes.

The web UI shows activity, queue state, library contents, compliance status,
plans, issues, asset bundles, external-system sync, node health, provider
capabilities, and library statistics over time.

## Worker Trust And Capability Grants

Workers advertise what they can do. The host grants what they are allowed to do.
The scheduler must use both.

Example:

```text
worker: basement-gpu-01
advertises:
  operations: probe_file, transcode_video, verify_artifact
  codecs: h264, hevc, av1
  hardware: nvidia_nvenc
  artifact_access: shared_mount, http

grants:
  can_execute: transcode_video, probe_file, verify_artifact
  can_access: library.movies.read, staging.local.write
  cannot_access: originals.write, backups.delete
  max_parallel:
    transcode_video: 2
    probe_file: 8
```

The scheduler enforces `max_parallel` per worker as a counting
semaphore at lease acquisition. The check-and-increment of the
worker's active-lease count for the requested operation happens in the
same transaction as the `ticket.leased` transition, so two schedulers
or two concurrent acquire calls cannot race past the same grant cap.
The SQL shape inside the IMMEDIATE acquire transaction is:

```sql
SELECT 1
  FROM workers w
 WHERE w.id = :worker_id
   AND (
        SELECT COUNT(*)
          FROM leases l
          JOIN tickets t ON t.id = l.ticket_id
         WHERE l.worker_id = :worker_id
           AND l.release_reason IS NULL
           AND t.kind = :operation
       ) < :max_parallel_for_operation;
```

An empty result aborts the acquire with `no eligible worker` (capacity
exhausted) and the scheduler tries the next candidate.

Original-file write access is never implicit. Default execution produces staged
artifacts. The host verifies and commits.

In-place mutation is exceptional. It requires:

- explicit worker grant
- backup first
- pre-mutation snapshot
- post-mutation snapshot
- audit event
- rollback metadata
- policy permission

## Policy Model

Media policy describes desired file and bundle outcomes. Scheduling policy
describes operational behavior. Safety policy describes approval, backup,
verification, and rollback requirements. Node policy describes what workers may
access or execute. Retention policy describes which variants or duplicates to
keep, archive, or delete. External-system policy describes which integrations
may be read automatically and which writes require approval. Issue policy
describes severity, priority, suppression, and escalation rules. Scoring policy
selects the named quality profile used by a library.

Example media policy shape:

```text
policy "english-x265-mkv" {
  phase containerize {
    container mkv
  }

  phase transcode {
    depends_on: [containerize]
    video codec hevc {
      encoder auto
      quality crf 20
    }
  }

  phase audio {
    depends_on: [transcode]
    keep audio where language == "eng" and not commentary
  }

  phase verify {
    depends_on: [audio]
    require quick_decode
  }
}
```

Example scheduling policy shape:

```text
schedule "home-library-default" {
  priority newest_first
  prefer local_gpu_for transcode_video
  copy_window "00:00-08:00"
  large_jobs night_only
  cloud_egress_budget "5 USD/day"
  pause_when node.health == degraded
}
```

The policy language will use a block-oriented text format that can be parsed,
formatted, diffed, and validated without executing worker code. Its required
property is that policies compile to a typed phase DAG with explicit operations,
dependencies, guards, inputs, outputs, and safety gates.

## Durable Identity Model

Durable identity is a core product feature. A path is not an identity. A content
hash is not an identity. A storage-provider key is not an identity. These values
are evidence and locations attached to durable records.

### Identity Layers

The model has four primary layers:

- `MediaWork`: the logical title or recording. External IDs such as TVDB, TMDB,
  IMDB, AniDB, Radarr, Sonarr, or user-created catalog IDs usually attach here.
- `MediaVariant`: a retained user-meaningful version of a work. Variants cover
  release/editorial differences and service-quality differences: theatrical,
  director's cut, remaster, HD, 4K, mobile, commentary, open matte, broadcast,
  unknown, or custom labels.
- `AssetBundle`: the user-facing bundle for a media variant. It groups primary
  media, sidecars, generated assets, and metadata that should move through
  organization and reporting together.
- `FileAsset`: a managed file lineage. It receives a stable UID when first
  ingested and keeps that UID through rename, move, remux, transcode, archive,
  restore, and storage-provider migration.
- `FileVersion`: an immutable byte revision under a file asset. A transcode,
  remux, restore, or other content-changing operation creates a new version
  under the same file asset unless policy explicitly creates a new asset.

`FileLocation` records where a version lives or lived. One file version can have
multiple locations: library primary path, staging path, shared mount path,
object-store key, backup location, remote cache, or historical path.

### Ingest Behavior

On first ingest, the system computes a content hash after file-stability rules
pass. Every newly discovered filesystem object gets a new `FileAsset` by
default, unless it resolves to the same physical-object identity as an
existing tracked `FileLocation` (in which case it is recorded as an
additional `FileLocation` on the existing `FileVersion` — see
`Ingest Identity Invariants`). For an object that is not an alias, the
hash, path rules, external metadata, and existing evidence produce
`IdentityEvidence` describing how the new asset relates to what is already
known, not a decision to merge identity. For each newly discovered
independent object, the control plane creates:

- a `FileAsset` UID
- an initial `FileVersion`
- an active `FileLocation`
- provisional `MediaWork` and `MediaVariant` links when needed
- identity evidence explaining how those links were chosen

The hash helps match known content and detect exact duplicates, but it does not
become the durable identity. Policy actions can change bytes while preserving
the file asset UID.

### Ingest Identity Invariants

- Each newly discovered filesystem object is treated as an independent
  copy by default and receives its own `FileAsset` UID. Ingest never
  merges two distinct independent copies into one `FileAsset`.
- A newly discovered path is recorded as an additional `FileLocation` on
  an existing `FileVersion` only when ingest can prove immutable
  physical-object identity, not merely a matching reusable name. For
  local filesystems, that proof requires a stable file identity that
  survives delete/recreate (a generation-stamped file ID or the
  equivalent for the filesystem in use), current liveness of the
  previously tracked location, and hash and size validation against the
  recorded `FileVersion`; a matching `(device, inode)` alone is not
  sufficient because inode numbers are reused after delete/recreate. For
  object stores, that proof requires the bucket and key plus an
  immutable provider generation or version ID (for example an S3 version
  ID or a GCS generation number) that names the specific object
  generation rather than its current contents. A matching key without
  immutable-generation proof becomes a new `FileAsset`; the key match is
  recorded as `IdentityEvidence` (`path_rule_match` or
  `external_id_match` as appropriate) but does not attach a new
  `FileVersion` to an existing `FileAsset`, because object-store keys
  are reusable after overwrite or delete/recreate and a key match alone
  cannot prove the bytes belong to the existing lineage. A
  content-addressed ETag or hash match proves byte equality, not object
  identity — delete/recreate with identical bytes can produce the same
  value across distinct objects — and is recorded as `IdentityEvidence`
  (`hash_match`) rather than alias proof. The only path that adds a new
  `FileVersion` to an existing `FileAsset` is a host-committed lineage
  operation that produces new bytes from a prior `FileVersion` of the
  same asset — for example transcode, remux, or a restore that yields
  content-different output. Operations that preserve bytes (rename,
  move, archive, storage-provider migration, immutable-generation
  alias attachment, and external rename/move reconciliation) operate
  at the `FileLocation` level on an existing `FileVersion` and do not
  create a new `FileVersion`: alias attachment adds a new
  `FileLocation` to the existing `FileVersion`, rename/move
  reconciliation retires one `FileLocation` and records another on the
  same `FileVersion`, and storage-provider migration records the same
  bytes at a new `FileLocation` while keeping the `FileVersion`
  unchanged. A discovered object that cannot prove
  immutable identity against any existing tracked location follows the
  default rule and becomes a new `FileAsset`. Whenever a `FileLocation`
  is recorded as an alias of an existing `FileVersion`, the commit
  safety gate's affected-scope closure must include every `FileLocation`
  of that `FileVersion`, including aliased paths, so destructive work on
  one location cannot bypass a blocking lease held against another
  location of the same bytes.
- Lineage continuity across a `FileAsset` (rename, move, remux, transcode,
  archive, restore, storage-provider migration) is only established through
  a host-committed operation that produced the new path or bytes from a
  prior `FileVersion` of the same asset.
- External rename and move reconciliation is one such host-committed
  operation, but it is a reconciliation of durable state to observed
  reality, not a destructive commit the host is choosing to initiate.
  When a watcher or rescan discovers a path that proves the same
  immutable physical-object identity as a specific `FileLocation` on an
  existing `FileVersion`, and that `FileLocation` is no longer live at
  its recorded path, the host commits a rename/move event that retires
  the missing `FileLocation` and records the new one on the same
  `FileAsset` and `FileVersion`. Other `FileLocation`s of the same
  `FileVersion` are unaffected. The `Commit Safety Gate` does not block
  reconciliation: the physical move has already happened outside the
  host's authority, and refusing to record it would only leave durable
  state stale. Inside the same commit transaction, blocking leases
  scoped to the retired `FileLocation` are re-anchored to the new
  `FileLocation` — preserving `lease_id`, `issuer`, `acquired_at`,
  `expires_at`, `last_heartbeat_at`, and `blocking_mode` — so the
  protection invariant continues to hold on the moved bytes; an
  `external move re-anchored lease` event is recorded for each, and the
  issuer is notified through the same channel used for other lease
  lifecycle transitions but does not need to re-acquire. An issuer that
  wants to release after the move uses the normal release path; a
  permissioned force-release remains available as an audited override
  that terminates the lease with `force_released` and writes its own
  override event, and forced release does not bypass the safety gate on
  any later destructive commit. Advisory leases scoped to the retired
  location are re-anchored in the same way, as are other per-location
  durable records that logically follow the physical bytes (path-based
  metrics, transfer history, and similar). Accepted `IdentityEvidence`
  records are immutable: pinned `FileVersion` IDs, hashes, and observed
  locations are not rewritten by the reconciliation. The reconciliation
  appends a new evidence record linked to the move event that observes
  the new `FileLocation`, but the original accepted-evidence pin is
  preserved as written. The `Commit Safety Gate`'s evidence
  revalidation continues to evaluate the original pinned facts, so any
  later destructive action that depended on evidence accepted against
  the retired location fails with `stale identity evidence` until the
  evidence is re-collected against the current state and re-accepted by
  policy or user. Append-only event records keep their original
  references, since they are historical facts rather than durable
  per-location state. If immutable identity cannot be proven, ingest
  falls back to the default rule and creates a new `FileAsset`.
- Hash matches, path-rule matches, and external-metadata matches between a
  newly discovered object and an existing asset are recorded as
  `IdentityEvidence` (`hash_match`, `path_rule_match`, `external_id_match`,
  `duplicate_of_asset`, `same_as_asset`). They never collapse identity at
  ingest. Duplicate and same-as evidence is pinned to the specific
  `FileVersion` IDs, observed hashes, and observed locations that produced
  it; evidence does not auto-carry forward to new `FileVersion`s of either
  asset.
- Acting on duplicate or same-as evidence (archive, delete, replace)
  requires an accepted retention policy or explicit user confirmation, and
  produces a host-committed event referencing the evidence used. The host
  commit transaction revalidates that the pinned `FileVersion` IDs,
  hashes, and locations the evidence was accepted against still describe
  the current state; if any pinned attribute has changed, the action
  aborts with a stale-identity-evidence error and the evidence must be
  re-collected and re-accepted before retry.
- This spec deliberately does not define a `merge` operation that
  collapses two `FileAsset` lineages into one; introducing merge requires
  a follow-on specification covering its transaction semantics,
  original-history preservation, lease and event-link handling, conflict
  resolution, and rollback behavior. Until then, no policy or user action
  may collapse distinct `FileAsset` UIDs.

### Variant Retention

The default policy is to keep all discovered assets and variants. The system may
identify likely duplicates or variant relationships, but it does not archive or
delete them by default.

Retention policies may later express rules such as:

```text
keep variants ["4k", "hd"]
keep best 1 where work == same and variant == "hd"
archive duplicates where same_work and same_variant and quality < best_quality
require approval for delete when confidence < 0.98
```

This distinction prevents the system from treating a theatrical cut, director's
cut, remaster, HD fallback, and 4K version as disposable duplicates simply
because they share an external movie ID.

### Identity Evidence

Identity evidence is an auditable assertion about a work, variant, asset,
version, or location. It can be produced by users, policies, path rules, hashes,
external applications, external databases, frame matching, audio matching,
subtitle matching, or future plugins.

An evidence record includes:

- target type and target ID
- assertion type
- candidate ID or candidate value
- provider name and version
- confidence score
- provenance payload
- observed timestamp
- superseded timestamp when applicable
- accepted policy ID when a policy acts on it
- accepted user ID when a user confirms it

Example assertions:

- `belongs_to_work`
- `belongs_to_variant`
- `same_as_asset`
- `duplicate_of_asset`
- `preferred_variant`
- `user_label`
- `external_id_match`
- `path_rule_match`
- `hash_match`
- `runtime_similarity_match`
- `frame_fingerprint_match`
- `audio_fingerprint_match`

Identity evidence is reportable. The product should be able to explain why a
file is believed to be a specific movie, why two assets were grouped, why a
variant was retained, why a duplicate was archived, and which plugin or policy
made a useful recommendation.

## Asset Bundle Model

`AssetBundle` is the durable user-facing container for one `MediaVariant`. A
bundle groups the primary media file with the sidecar and generated files that
belong with that variant.

Bundle members include:

- primary video file assets
- external audio assets, such as a commentary track
- external subtitle assets
- poster and fanart assets
- NFO or metadata assets
- trailer assets
- transcript assets
- generated thumbnail or preview assets
- policy reports that should remain attached to the variant

Internal container tracks are recorded in `MediaSnapshot` and `FileVersion`
facts. External files are separate `FileAsset`s linked to the bundle. Both can
help define the variant. For example, a director's commentary may be an internal
audio track in one file or an external audio file in another bundle.

Bundle transactions allow the host to commit or roll back coordinated changes.
For example, renaming a movie may update a primary video, subtitles, poster, and
NFO together. Generating subtitles or transcripts adds new bundle assets with
provenance rather than leaving unmanaged outputs next to the video file.

## External System Model

`ExternalSystem` records durable integrations with applications and storage
providers. Examples include Plex, Jellyfin, Emby, Radarr, Sonarr, Bazarr, S3,
shared filesystems, and future catalog systems.

Each external system records:

- kind and display name
- connection profile and auth reference
- health status
- rate limits and budgets
- visibility model
- path mappings
- linked libraries
- linked media works, variants, bundles, assets, and external IDs
- supported read and write operations

Read-only imports can produce identity evidence, quality facts, issues, and
external links. Writes are durable jobs and must pass safety policy. Examples
include refreshing a Plex library, updating a Radarr tag, notifying Jellyfin
about a moved file, or deleting a remote object.

Path mappings affect both scheduling and artifact resolution. A worker or
external system is eligible for a ticket only when it can safely see the required
artifact location or receive a resolved artifact transfer plan.

## Issue Model

`Issue` records a durable condition that should remain visible until resolved,
suppressed, accepted, or converted into planned work.

Issues include:

- unknown identity
- missing subtitle
- duplicate candidate
- policy noncompliance
- health failure
- external sync failure
- artifact unavailable
- variant retention conflict
- untrusted worker
- terminal failure

When a ticket transitions to `failed` terminally — whether because its
`FailureClass` is non-retriable or operator-required, or because its
retry budget is exhausted — the host opens a `terminal_failure` issue
linked to the ticket and its last lease in the same transaction that
records the terminal transition. Severity and priority are both
derived from the failure's category in the taxonomy (see Error
Handling And Recovery → Failure taxonomy): operator-required and
non-retriable failures default to higher severity and priority than a
retriable failure that simply exhausted its retries. Exactly one
`terminal_failure` issue is opened per terminal transition: the
ticket state machine treats `failed` as terminal, so a given ticket
transitions to that state at most once. This is the DLQ analogue:
terminal failures do not silently disappear into the event log.

Issues have both severity and priority:

```text
severity: critical | high | medium | low | info
priority: urgent | high | normal | low | someday
priority_source: system | user | policy | external
priority_reason: text
```

Severity describes impact. Priority describes when to act. A health failure can
default to high severity and high priority. A duplicate candidate consuming
large storage on a nearly full disk can be high priority even if the media is
still playable. Users and policies can override priority.

Noncompliance should create or update an issue even when a plan can be created
automatically. Its status becomes `planned` when an execution plan exists,
`open` when user or policy input is needed, and `resolved` when the desired
state is reached.

## Quality Scoring Model

`QualityScoringProfile` defines what "better" means for a library or policy.
The control plane supports multiple named profiles. Each library chooses a
default profile.

Example profiles:

- `balanced-home`
- `small-direct-play`
- `preserve-source`
- `anime-subtitle-focused`
- `4k-plus-hd-fallback`

`QualityScore` can attach to a `MediaVariant`, `AssetBundle`, `FileAsset`, or
`FileVersion`. It records:

- profile ID and version
- provider name and version
- target type and target ID
- total score
- dimension scores
- provenance
- observed timestamp

Dimensions can include resolution, codec, HDR, audio layout, language match,
subtitle coverage, source quality, bitrate efficiency, file size, health
status, identity confidence, external custom-format scores, and user
preference.

Retention policies should reference named scoring profiles rather than a single
global score. This allows one library to prefer small direct-play files while
another preserves source quality.

## Runtime Use Lease Model

`AssetUseLease` records active use of an asset, bundle, version, or location.
Use leases may be advisory or blocking.

Lease kinds include:

- playback
- scan
- copy
- manual lock
- external lock
- worker operation

Playback leases block delete, archive, replace, and move commits by default.
Manual blocking locks override automation. Advisory leases influence scheduler
score but do not make work impossible. Expired leases are cleaned up by the
control plane.

External systems can create use leases from playback or scan activity. For
example, Plex or Jellyfin activity can protect an asset from replacement while a
user is watching it.

### Lease Fields

- `lease_id`, `kind` (playback, scan, copy, manual lock, external lock,
  worker operation)
- `scope`: target type and target ID (asset, bundle, version, or location)
- `issuer`: user, control plane subsystem, worker, or named external system
- `blocking_mode`: advisory or blocking
- `acquired_at`, `expires_at`, `last_heartbeat_at`
- `clock_source`: the monotonic-plus-wall clock the control plane uses to
  evaluate freshness (named explicitly so external issuers cannot supply a
  drifting clock)
- `release_reason` when terminated (released, expired, issuer_lost,
  superseded, force_released)

### Lease Lifecycle

- Automated, external, and worker-issued leases have a finite TTL. A
  lease without renewal is treated as expired at `expires_at` evaluated
  against the control-plane clock.
- Long-running issuers (playback, worker operations) renew with a heartbeat
  on a cadence shorter than the TTL. A missed heartbeat past `expires_at`
  is lease expiry, regardless of what the issuer believes.
- A lease transitions to `issuer_lost` only after heartbeat timeout past
  `expires_at` confirms the issuer is gone. A transient process disconnect,
  dropped network, or paused client is not by itself issuer loss; the lease
  continues to block until its TTL elapses without renewal. An issuer that
  wants to release early sends an authenticated explicit release, which
  terminates the lease with `release_reason = released`. Stale external
  locks are bounded by the same TTL as internal leases; external systems
  that want a longer hold must keep renewing.
- Manual maintenance locks (CLI or web UI) are explicit-release. They do
  not expire on TTL alone, because they encode a user's deliberate intent
  to hold the scope. They release only by explicit user release,
  permissioned force-release with a recorded reason, or stale-owner
  recovery that confirms the issuing user or process is gone; an
  external rename or move re-anchors the lock to the new `FileLocation`
  rather than releasing it (see `Ingest Identity Invariants`).
  Long-lived manual locks stay visible in the lease inventory until
  released, with age surfaced so operators can spot forgotten holds.
- A lease in any terminal state — regardless of its `release_reason`
  (`released`, `expired`, `issuer_lost`, `superseded`, or
  `force_released`) — stops blocking work immediately. Cleanup is
  bookkeeping; it never grants safety on its own.

### Commit Safety Gate

- Delete, archive, replace, and move commits perform their lease check
  inside the host-side transaction that records the commit. The check
  evaluates lease freshness against the control-plane clock immediately
  before any irreversible filesystem mutation.
- The affected scope for a destructive commit is the full closure of
  identifiers it touches: the target `FileAsset`, the affected
  `FileVersion`(s), every `FileLocation` of those `FileVersion`(s) —
  including hardlinks, bind-mount paths, shared-mount paths, object-store
  aliases, and any other path the host can resolve to the same physical
  object — the `AssetBundle` the asset belongs to, and any other
  locations being written, moved, or removed. The gate checks every
  blocking lease attached to any scope in that closure (asset, bundle,
  version, or location), not just the commit target, so a playback lease
  taken on one location or `FileVersion` cannot be bypassed by
  destructive work on an aliased location or sibling scope of the same
  bytes.
- When a destructive commit is acting on identity evidence (for example,
  archive, delete, or replace based on accepted `duplicate_of_asset` or
  `same_as_asset` evidence), the gate also revalidates the pinned
  `FileVersion` IDs, observed hashes, and observed locations from the
  accepted evidence inside the same transaction. If any pinned attribute
  has changed since acceptance, the gate aborts the commit with a
  stale-identity-evidence error and the evidence must be re-collected
  and re-accepted before retry.
- The host serializes destructive commits against both blocking-lease
  acquisition and `FileLocation`/`FileVersion` mutations on the affected
  scope. While a commit is in progress, new blocking leases on the scope
  are rejected or held until the commit resolves, and new `FileLocation`s
  that alias discovery would attach to an in-scope `FileVersion` are
  likewise blocked or held; while a fresh blocking lease exists on the
  scope, destructive commits on that scope wait or fail. Immediately
  before the irreversible filesystem mutation, the host recomputes the
  affected-scope closure under the same isolation — picking up any
  `FileLocation`s that have been attached as aliases of in-scope
  `FileVersion`s since the initial gate check — and re-evaluates blocking
  leases against the recomputed closure. The commit aborts if a fresh
  blocking lease has appeared on any member of the recomputed closure or
  if the closure itself has grown to include a location with an existing
  blocking lease.
- Closure resolution is fail-closed. If the host cannot fully resolve
  the affected scope — an alias provider is unreachable, a shared or
  remote mount is unavailable, a path-mapping translation fails, an
  object-store identity probe times out, or any other in-scope identity
  check is incomplete — the commit aborts with a `closure resolution
  incomplete` safety-gate error rather than proceeding on a partial
  view. Operators who need to commit despite incomplete resolution use a
  separately audited, permissioned force path that records its own
  override event and reason.
- A lease in any terminal state (any non-null `release_reason`) does
  not block the commit, even if cleanup has not yet run. A TTL-bound
  lease whose `expires_at` has passed without a renewing heartbeat is
  treated as expired and does not block. Manual maintenance locks are
  not TTL-bound; they continue to block until they reach a terminal
  state through one of the release paths defined under
  `Lease Lifecycle`.
- A blocking lease that is still fresh at the gate fails the commit with
  the existing "blocked by active use lease" error. Advisory leases never
  fail the gate.
- The gate result, the affected scope closure, and the lease IDs the gate
  considered are written as part of the commit's event record, so audit
  can later prove which leases were evaluated against which clock value
  and across which scopes.

## Primary Workflow

A library change follows one common lifecycle:

```text
Library watcher or CLI scan
  -> ScanLibrary job
  -> ProbeFile / HashFile jobs
  -> MediaWork / MediaVariant / AssetBundle / FileAsset records updated
  -> FileVersion / FileLocation records updated
  -> IdentityEvidence recorded
  -> QualityScore and Issue records updated when applicable
  -> MediaSnapshot stored
  -> EvaluatePolicy job
  -> ComplianceReport + ExecutionPlan
  -> tickets become ready as dependencies unlock
  -> scheduler leases tickets to workers
  -> workers produce staged artifacts and structured results
  -> host verifies, records events, and advances the plan
  -> host commits final artifact or records failure/rollback
```

For a multi-phase policy like "containerize to MKV, transcode to x265, strip
non-English audio, verify," the planner emits a full DAG upfront. The scheduler
uses that DAG for resource lookahead and dynamically leases ready work.

Phase boundaries are revalidation points. The system checks whether produced
artifacts still satisfy assumptions such as track IDs, codecs, duration, file
size, checksums, and health-check results. If assumptions changed, bounded
replanning updates downstream tickets while preserving the audit trail.

Plans, tickets, artifacts, and events reference stable IDs. Path strings and
content hashes may appear in payloads, but they are never the only link between
state records.

## Synthetic Provider Suite

Synthetic providers are first-class provider packages. They validate the
architecture before real media tools are introduced and remain part of the
ongoing test suite.

Required synthetic providers:

- `fake-scanner`: emits deterministic file discovery scenarios.
- `fake-prober`: returns canned media snapshots.
- `fake-transcoder`: simulates duration, progress, output size, codec changes,
  and failures.
- `fake-remuxer`: simulates container and track mutations.
- `fake-backup-store`: simulates local and object-store backup behavior.
- `fake-health-checker`: returns pass, fail, and degraded results.
- `fake-object-store`: simulates upload/download, egress cost, latency, and
  corruption.
- `fake-transcriber`: simulates transcript and subtitle generation.
- `fake-identity-provider`: simulates path, external ID, runtime, and duplicate
  evidence.
- `fake-external-system`: simulates Plex/Jellyfin/Radarr/Sonarr-style reads,
  writes, path mappings, rate limits, and refresh failures.
- `fake-quality-scorer`: emits named-profile quality scores.
- `fake-issue-provider`: emits durable issues with severity and priority.
- `fake-use-lease-provider`: simulates playback, external scans, and manual
  locks.
- `chaos-worker`: crashes, stalls, corrupts output, misses heartbeats, returns
  malformed results, and exceeds deadlines.
- `benchmark-worker`: measures scheduler throughput without media tools.

These providers are not test doubles hidden inside unit tests. They are normal
workers that speak the real protocol and can be used by CLI, daemon, API, web
UI, integration tests, benchmarks, and demos.

## Data Storage

The default database is SQLite on disk. Tests use in-memory SQLite with the same
migrations and repository code.

Initial storage areas:

- `libraries`
- `library_roots`
- `media_works`
- `media_variants`
- `asset_bundles`
- `asset_bundle_members`
- `file_assets`
- `file_versions`
- `file_locations`
- `identity_evidence`
- `external_systems`
- `external_system_links`
- `external_path_mappings`
- `issues`
- `issue_links`
- `quality_scoring_profiles`
- `quality_scores`
- `asset_use_leases`
- `media_snapshots`
- `policies`
- `compiled_policies`
- `compliance_reports`
- `execution_plans`
- `tickets`
- `ticket_dependencies`
- `leases`
- `workers`
- `worker_capabilities`
- `worker_grants`
- `artifact_handles`
- `artifact_locations`
- `artifact_lineage`
- `events`
- `approvals`
- `backups`
- `scheduling_policies`
- `safety_policies`
- `retention_policies`
- `external_system_policies`
- `issue_policies`

The schema must support crash recovery, stale lease detection, event retention,
plan auditability, durable identity history, evidence reporting, and idempotent
ticket execution. It must also support bundle-level transactions,
external-system sync history, issue lifecycle, quality-score provenance, and
runtime use leases.

SQLite serializes concurrent writers through `BEGIN IMMEDIATE`: there
is no `SKIP LOCKED` primitive, and one writer at a time holds the
database write lock. Sprint 1 deliberately does not need a non-
blocking dequeue — there is no scheduler yet, and the single-writer
boundary is sufficient for lease-acquire, lease-release, and
commit-safety-gate transactions. The Postgres deployment profile
already hinted at above is the natural place to migrate the
ticket-dequeue path to `SELECT ... FOR UPDATE SKIP LOCKED`, which lets
multiple scheduler processes claim disjoint ready tickets without
lock-escalation contention. Sprint 4's multi-scheduler reasoning
starts from this fact rather than re-deriving it.

## Error Handling And Recovery

Errors should be classified at the boundary where they occur:

- policy parse error
- policy validation error
- missing capability
- no eligible worker
- artifact unavailable
- artifact checksum mismatch
- blocked by active use lease
- stale identity evidence
- closure resolution incomplete
- external system unavailable
- external system rate limited
- worker timeout
- worker crash
- malformed worker result
- verification failure
- backup failure
- commit failure
- approval required
- priority policy conflict
- user cancellation

Every failure records an event and updates durable state. Retriable failures
remain attached to tickets with attempt count, backoff, and reason. Non-retriable
failures stop the affected plan branch and surface actionable diagnostics.

### Retry policy

Retriable failures are rescheduled by setting the ticket's next-eligible
time to `now + wait(attempt)`, where
`wait(attempt) = random_between(0, min(cap, base * 2^attempt))`. The
distribution is capped-exponential with full jitter; full jitter is
required because tickets that share a lockstep retry schedule retry in
unison and create thundering-herd load spikes on whatever they share a
bottleneck with (worker pool, external system, scheduler). The `base`
and `cap` parameters are owned by scheduling policy (Sprint 4+); the
control-plane core supplies a deterministic-by-injection seam — the
RNG is passed through the same `Clock`-style boundary the identity
model already uses for time, so unit and integration tests remain
fully deterministic across builds.

### Failure taxonomy

Each error category maps to one of three retry classes. The mapping is
normative: it is the source the `FailureClass` enum compiles against,
the input the retry-policy decision uses, and the basis on which a
terminal-failure transition is surfaced as an `Issue` (see Issue
Model). `retriable` re-queues with backoff up to `max_attempts`;
`non_retriable` transitions the ticket to `failed` immediately and is
not unblocked by re-attempting the same input; `operator_required`
also transitions to `failed`, but the diagnostic names a concrete
operator action that, once taken, makes a fresh attempt viable.

| Failure category               | Class               |
|--------------------------------|---------------------|
| `worker_timeout`               | `retriable`         |
| `worker_crash`                 | `retriable`         |
| `no_eligible_worker`           | `retriable`         |
| `artifact_unavailable`         | `retriable`         |
| `artifact_checksum_mismatch`   | `retriable`         |
| `external_system_unavailable`  | `retriable`         |
| `external_system_rate_limited` | `retriable`         |
| `verification_failure`         | `retriable`         |
| `backup_failure`               | `retriable`         |
| `commit_failure`               | `retriable`         |
| `policy_parse_error`           | `non_retriable`     |
| `policy_validation_error`      | `non_retriable`     |
| `missing_capability`           | `non_retriable`     |
| `malformed_worker_result`      | `non_retriable`     |
| `user_cancellation`            | `non_retriable`     |
| `stale_identity_evidence`      | `operator_required` |
| `closure_resolution_incomplete`| `operator_required` |
| `blocked_by_active_use_lease`  | `operator_required` |
| `approval_required`            | `operator_required` |
| `priority_policy_conflict`     | `operator_required` |

A terminally-failed ticket is not allowed to vanish into the event log:
it is the durable analogue of a dead-letter queue. The host opens a
`terminal_failure` issue (see Issue Model) in the same transaction
that records the terminal transition, so the failure stays visible
until an operator or policy resolves, suppresses, or accepts it.

Stale leases are recovered by heartbeat timeout. Partially produced artifacts
are either promoted only after verification or marked abandoned and eligible for
cleanup. Host-owned commit ensures a worker crash does not leave the control
plane believing a final mutation succeeded.

## Observability

The product should expose:

- structured logs
- append-only event log
- job and ticket status
- worker health
- queue depth
- lease age
- retry counts
- throughput by operation type
- artifact transfer time and cost
- scheduling decisions and rejected candidates
- identity evidence history
- asset bundle history
- external-system sync history
- issue severity and priority trends
- quality score changes
- active and expired use leases
- variant retention decisions
- policy compliance trends
- library statistics over time

The web UI, CLI, and API should all be able to inspect why a ticket is waiting,
why a worker was selected, why an artifact placement was chosen, and why a file
asset is linked to a work or variant. It should also explain why an issue has a
given priority, why a quality score changed, and why an operation is blocked by
active use.

## Security And Safety

V1 security focuses on clear local and home-network boundaries:

- worker registration requires authentication
- worker grants are explicit
- artifact access is scoped
- original-file writes are denied by default
- remote artifact URLs are time-limited where supported
- destructive actions require policy permission
- external-system writes require policy permission
- approval gates are available for risky operations
- active blocking use leases prevent risky commit/delete/archive operations
  (see `Commit Safety Gate` under `## Runtime Use Lease Model`)
- every mutation is audited

Future plugin distribution can add package signing, marketplace trust metadata,
and stricter sandboxing. The worker protocol should not require those features
to be useful.

## CLI MVP Requirements

The CLI MVP must support:

- initialize config and database
- register synthetic workers
- scan with synthetic provider
- evaluate a policy
- show compliance report
- create an execution plan
- inspect plan JSON
- run plan with synthetic providers
- show events
- show media work, variant, asset, version, location, and identity evidence
  history
- show asset bundles and sidecar assets
- show issues with severity, priority, and linked evidence
- show external systems, path mappings, and sync history
- show quality scores and active scoring profiles
- show active use leases and blocked operations
- show workers and capabilities
- show jobs, tickets, leases, and artifacts
- emit JSON for every command

The CLI must be suitable for agent use: deterministic output, stable schemas,
dry-run mode, plan-only mode, and machine-readable errors.

## Daemon MVP Requirements

The daemon MVP must support:

- continuous library monitoring
- file stability/debounce rules
- scan reconciliation
- background scheduling
- issue lifecycle updates
- external-system health and sync jobs
- runtime use lease cleanup
- remote worker heartbeats
- stale lease recovery
- dynamic throttles
- scheduled copy windows
- crash recovery
- event streaming for UI/API clients

## Web UI MVP Requirements

The web UI MVP must show:

- current activity
- queue and ticket state
- plan details
- policy compliance status
- library contents
- file detail with media snapshot
- durable file asset history
- media work and variant views
- asset bundle and sidecar views
- identity evidence and match confidence
- issue board with severity and priority
- external-system sync and path mapping views
- quality score and retention views
- active use lease / playback lock indicators
- worker/node health
- provider capabilities
- artifact locations
- recent events
- library statistics over time

The web UI is an operational console, not the architectural source of truth.
Everything it does should be possible through CLI/API.

## Sprint Roadmap

Use two-week sprints. Each sprint should prove an architectural promise and
leave behind automated tests.

### Sprint 0: Spec And Skeleton

Goal: create the Rust workspace and engineering guardrails.

Deliverables:

- Rust workspace.
- Core crate boundaries.
- SQLite migration runner.
- In-memory SQLite test harness.
- CLI shell with JSON output mode.
- Initial REST API skeleton.
- Quality gates: format, lint, type/build, tests.
- Architecture decision records for job/event split and out-of-process workers.

Exit criteria:

- Empty app starts.
- Database initializes on disk and in memory.
- CLI can print version and health JSON.
- CI-equivalent local checks pass.

### Sprint 1: Durable Control Plane MVP

Goal: implement core durable state without media tooling.

Deliverables:

- job and ticket tables
- leases with stale lease recovery
- worker registry (the `workers` table carries a `kind` column that
  distinguishes local / remote / synthetic; a separate `nodes`
  registry is deferred to Sprint 4)
- artifact catalog
- media work, media variant, file asset, file version, file location, and
  identity evidence tables
- asset bundle, external system, issue, quality score, and use lease tables
- append-only event log
- repository interfaces
- migration tests
- JSON CLI for inspecting jobs, leases, workers, artifacts, identity records,
  and events (a `voom node` inspection command is deferred to Sprint 4
  alongside the `nodes` table; Sprint 1 ships `voom worker` against
  the `workers` table, whose `kind` column distinguishes local /
  remote / synthetic)

Exit criteria:

- Tests can create jobs, lease tickets, expire leases, and recover work.
- Tests can create a file asset, add versions and locations, and report its
  event/evidence history.
- Tests can create a bundle, open and prioritize an issue, record a quality
  score, and block a commit with a use lease.
- Events are recorded for all state transitions.
- In-memory SQLite tests exercise the same repositories as disk mode.

### Sprint 2: Synthetic Provider Suite MVP

Goal: prove the worker protocol and scheduler with fake providers.

Deliverables:

- versioned HTTP/JSON worker protocol
- local worker supervisor
- fake scanner
- fake prober
- fake transcoder
- fake remuxer
- fake backup store
- fake health checker
- fake identity provider
- fake external system
- fake quality scorer
- fake issue provider
- fake use lease provider
- chaos worker
- benchmark worker
- structured progress stream
- provider conformance tests

Exit criteria:

- A synthetic end-to-end plan runs through the real scheduler.
- Chaos tests cover worker crash, timeout, malformed result, and missed heartbeat.
- Benchmark worker reports scheduler throughput.

### Sprint 3: Policy DAG MVP

Goal: implement core policy-to-plan behavior.

Deliverables:

- core media policy grammar
- parser and validator
- compiled policy model
- media snapshot model
- durable identity model in plans and reports
- asset bundle targets in plans and reports
- quality scoring profile selection
- issue creation for noncompliance
- compliance report
- plan DAG generation
- phase dependency handling
- plan dry-run and JSON inspection
- scheduling priority model

Exit criteria:

- Synthetic media snapshots can be evaluated against policies.
- Non-compliant files produce deterministic execution plans.
- Multi-phase policy plans execute with synthetic providers.

### Sprint 4: Remote Node MVP

Goal: make remote workers a real early deployment shape.

Deliverables:

- authenticated worker registration
- network worker leases
- heartbeat and health model
- remote synthetic workers
- artifact handle access plans
- locality/cost scoring
- node-level concurrency limits
- node registry (`nodes` table, `NodeRepo`, worker-to-node
  relationship, node heartbeat / locality / concurrency), alongside
  remote-node lease acquisition
- remote-node integration tests

Exit criteria:

- A remote synthetic worker can execute leased tickets.
- Scheduler chooses workers using capability, health, locality, and cost.
- Lost remote nodes trigger stale lease recovery.

### Sprint 5: CLI Media MVP

Goal: add the first real media path while preserving the provider contract.

Deliverables:

- ffprobe worker
- FFmpeg worker for one transcode path
- MKVToolNix worker for one remux/track-edit path
- backup worker
- verification worker
- staged artifact commit
- real ingest creates file assets, versions, locations, hashes, and media
  snapshots
- sidecar asset ingest for at least one generated or external asset type
- CLI scan/evaluate/plan/run commands
- JSON reports

Exit criteria:

- CLI can scan a real library path, evaluate policy compliance, create a plan,
  and execute a simple staged media change.
- CLI can show a bundle with primary media plus at least one sidecar asset.
- No real media worker bypasses the out-of-process protocol.

### Sprint 6: Daemon MVP

Goal: run continuously and manage changing libraries.

Deliverables:

- filesystem watcher
- file stability rules
- scan sessions and reconciliation
- background scheduler loop
- issue lifecycle updates
- external-system sync job loop
- use lease cleanup loop
- scheduling windows
- dynamic throttles
- recovery on restart
- daemon status API

Exit criteria:

- Adding, modifying, and removing files produces correct durable state changes.
- The daemon opens, updates, and resolves issues as library state changes.
- The daemon recovers from restart without losing queued work.
- Scheduling windows affect ticket leasing without changing media policies.

### Sprint 7: Web UI MVP

Goal: provide a usable operational console.

Deliverables:

- activity dashboard
- queue and ticket views
- plan detail view
- library browser
- file detail view
- media work and variant views
- asset bundle and sidecar views
- identity evidence timeline
- issue board with severity and priority
- external-system sync and path mapping views
- quality score and retention views
- active use lease / playback lock indicators
- worker/node health view
- capability view
- event stream
- basic library statistics over time

Exit criteria:

- A user can understand what is running, waiting, failed, and why.
- A user can inspect a file asset's versions, locations, evidence, and policy
  history from the web UI.
- UI actions use the same API as CLI/daemon workflows.

### Sprint 8: Plugin SDK And Extensible Operations

Goal: make third-party providers practical.

Deliverables:

- plugin package layout
- provider manifest
- operation schema registration
- result schema registration
- identity evidence provider schema examples
- external-system provider schema examples
- quality scorer provider schema examples
- SDK examples
- conformance test runner
- compatibility/version checks
- documentation for provider authors

Exit criteria:

- A sample third-party provider registers a namespaced operation schema.
- A sample identity provider emits evidence that can be validated and reported.
- The policy compiler validates the plugin-defined operation.
- The conformance suite verifies provider behavior.

### Sprint 9: Safety And Observability Hardening

Goal: make failure modes visible and recoverable.

Deliverables:

- approval gates
- backup policies
- rollback flows
- richer verification policies
- chaos test suite
- metrics endpoint
- trace IDs across plan, ticket, worker, artifact, and event records
- identity evidence reports
- variant retention reports
- issue lifecycle reports
- external-system sync reports
- use lease blocking reports
- scheduler decision logs
- artifact cleanup

Exit criteria:

- Common destructive operations can require approval.
- Chaos tests are part of the regular verification suite.
- Reports explain identity matches, variant retention, and duplicate actions
  from evidence and policy history.
- Reports explain issue priority, external sync results, quality-score changes,
  and operations blocked by active use.
- Operators can inspect why work was routed, paused, retried, or failed.

### Sprint 10: Production Readiness

Goal: prepare for real use and release.

Deliverables:

- installation packaging
- upgrade and migration tests
- security review
- sample policies
- user documentation
- provider author documentation
- benchmark gates
- release process
- backup/restore documentation

Exit criteria:

- A fresh user can install, configure, scan, plan, execute, monitor, and recover.
- Migrations are tested across released schema versions.
- Release artifacts and docs are ready for production users.

## Intermediate Milestones

- Control Plane MVP: Sprint 1 complete.
- Synthetic Worker MVP: Sprint 2 complete.
- Policy/CLI MVP: Sprint 3 complete.
- Remote Node MVP: Sprint 4 complete.
- Real Media CLI MVP: Sprint 5 complete.
- Daemon MVP: Sprint 6 complete.
- Web UI MVP: Sprint 7 complete.
- Extensible Plugin MVP: Sprint 8 complete.
- Production Candidate: Sprint 10 complete.

## Spec Review Notes

This spec intentionally keeps exact Rust crate names, DSL grammar details, API
schemas, and database column definitions for the implementation plan. The design
decisions fixed here are the architectural boundaries: durable jobs over
work-events, out-of-process workers, early remote nodes, artifact handles with
cost-aware placement, durable identity across work/variant/asset/version/location
layers, asset bundles as user-facing units, identity evidence as reportable
product data, external-system sync as durable state, issues with severity and
priority, named quality scoring profiles, runtime use leases, synthetic
providers as the first test spine, and separate media/scheduling/safety/node
policies.
