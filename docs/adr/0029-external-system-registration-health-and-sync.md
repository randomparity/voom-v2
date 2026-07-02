---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0029 — External-system registration, health, path mappings, and read-only sync

## Context

Sprint 17 workstream C (issue #284, T15) requires the **external-system** state
family to become CLI-manageable before the Sprint 20 daemon can run its health
and sync loops: register a system, probe its health, manage path mappings, run a
read-only sync, and read a sync report.

The three tables already exist (migration 0004): `external_systems`,
`external_system_links`, `external_path_mappings`. They had **no repo, no
production code, and no CLI** — only the `fake-external-system` worker binary.
This ADR adds the repos, domain events, control-plane cases, and CLI over the
**existing** tables. No new migration is created.

Two facts from the current tree constrain the design:

- **There is no production external-system provider.** `sync_external_system`
  (`OperationKind::SyncExternalSystem`) appears only in workflow plan binding.
  The one binary that answers it — `crates/voom-fakes/src/bin/fake_external_system.rs`
  — is a voom **worker-protocol** provider (an `HttpServer` answering the internal
  job protocol), not a Plex/Radarr REST surface. It returns `{"refresh_status":…}`
  and carries no catalog of items. So "the external read path" in V1 is the
  worker-protocol dispatch contract, not a REST client, and a real catalog-match
  engine that resolves external items to internal targets does not exist yet.
- **Automated health/sync *loops* are Sprint 20 (daemon).** Sprint 17 delivers
  the durable state and the one-shot CLI operations the loops will later call.

## Decision

Repos over the three existing tables, five durable domain events, one
control-plane case module, and one `voom external-system` CLI tree. External
**writes** remain out of scope (policy-gated jobs); everything here is read-only
against the external world.

### Identifiers (`voom-core`)

`ExternalSystemId`, `ExternalPathMappingId`, `ExternalSystemLinkId` — DB-generated
`u64` newtypes via `define_id!`.

### Repos (`voom-store`, `repo/external/`)

One `SqliteExternalSystemRepo` spanning the family, mirroring the library
directory layout (`repo/external/{systems,path_mappings,links}.rs` + `mod.rs`):

- **systems** — `register_in_tx` (health starts `unknown`), `get` / `get_in_tx`,
  `list` (active, `retired_at IS NULL`, id order), `set_health_in_tx`.
- **path_mappings** — pure CRUD (no events; path mappings are operator config):
  `create` (rejects an unknown parent system with `NotFound`), `get`,
  `list(system_id)`, `update` (partial), `retire` (soft-delete via `retired_at`).
- **links** — the durable primitives a sync reconciles: `record_in_tx`,
  `list` / `list_in_tx`, `retire_in_tx`.

Enum columns become `str_enum!`-style newtype-free enums validated on read and
write, so an out-of-vocabulary value is unrepresentable in Rust and fail-loud on
decode: `ExternalSystemKind`, `ExternalSystemHealth`, `ExternalLinkTargetType`,
`PathVisibility`. `connection_profile` and `rate_limit_config` are opaque JSON
(`serde_json::Value`) validated as JSON on write.

### Events (`voom-events`)

External-system health and sync are **stateful facts**, so — unlike the pure
config families (libraries/policies) — this family emits durable events (ADR 0001:
events record facts). `SubjectType::ExternalSystem` (`"external_system"`); five
`EventKind`s, each carrying a `#[serde(deny_unknown_fields)]` payload (ADR 0013):

| Event | When |
|---|---|
| `external_system.registered` | a system is registered |
| `external_system.health_changed` | a health probe changes the recorded status |
| `external_system.linked` | a sync records an external→internal link |
| `external_system.unlinked` | a sync retires a link no longer present |
| `external_system.synced` | a sync run completes (the sync-report source) |

Each write + its event share one transaction (exactly one event row per
transition), matching the node/worker case pattern.

### Health check (V1)

`health-check` performs a real read-only probe and records the result
(emitting `health_changed` only on an actual change):

- **filesystem-kind:** probe the active path mappings' `external_prefix`
  directories. No mappings → `unknown`; all present and readable → `healthy`;
  some missing → `degraded`; none present → `unreachable`.
- **other kinds:** V1 has no provider to probe, so the status is recorded as
  `unknown` with a CLI warning. Real per-kind probes arrive with their providers.

### Read-only sync (V1)

`sync` reconciles `external_system_links` from a read of the external system and
records the run as an `external_system.synced` event (no new table — the report
is reconstructed from that durable event plus the system's active links, honoring
the no-migration constraint). New external refs are recorded (`linked`); refs no
longer present are retired (`unlinked`).

The external read is driven through the existing `fake-external-system`
worker over the worker protocol in tests (spawned as `voom-fakes`'
`fake_providers.rs` does), proving the dispatch contract end-to-end. Because the
fake returns no catalog, a default sync records zero links and a `synced` event
with zero counts — still exercising the full dispatch → reconcile → report path.
`sync-report` returns the latest `synced` event's summary plus the active links.

The scheduled sync **loop** and real per-provider catalog reads remain Sprint 20.

### CLI (`voom-cli`)

`voom external-system register | list | show | health-check | sync | sync-report`
plus `voom external-system path-mapping create | list | show | update | delete`.
Standard single-JSON-envelope contract; `insta` snapshots per subcommand. Systems
and path mappings are addressed by their `u64` id (no unique natural key on
`external_systems`), matching the `backup` command's id addressing.

## Consequences

- The daemon (Sprint 20) inherits durable registration, health, path-mapping, and
  link primitives plus the sync-report event stream; it adds only the loop.
- `linked`/`unlinked` events and the link repo primitives are exercised by the
  sync path and repo/case tests; they are the API the daemon's sync loop calls.
- No external mutation and no catalog-match engine ship here; both are explicitly
  deferred. A real Plex/Radarr read surface, when it exists, plugs in behind the
  same worker-protocol dispatch the fake already models.
- No new migration; the family is additive over migration 0004's tables.
</content>
