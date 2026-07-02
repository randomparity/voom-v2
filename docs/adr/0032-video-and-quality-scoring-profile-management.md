---
status: accepted
date: 2026-07-02
deciders: [VOOM core]
---

# 0032 — Video profile and quality-scoring profile management

## Context

`voom profile list|show` is read-only over the six *seeded* video encode
profiles (migration 0014). There is no way to author a durable named video
profile from the CLI, and there is no quality-scoring-profile command family at
all — even though the durable `quality_scoring_profiles` **table already exists**
(migration 0004 §10.3: `id, name, version, definition, created_at, retired_at`,
`definition` validated `json_valid` and listed Class P passthrough in the payload
inventory) and the 0019 libraries migration comment explicitly defers the
per-library scoring-profile default column to this issue (#285, T16).

Two independent deliverables plus one linkage:

1. **Named video profile create/update/retire**, applying the *same* field
   validation the policy compiler applies to inline `transcode video` profiles
   (encoder family, CRF range, preset, tune, codec profile/level, pixel format,
   container, dimensions). That validation already lives as a reusable,
   protocol-neutral function, `voom_core::validate_profile_against_descriptor`,
   over `EncoderDescriptor` — the worker request path and the compiler both use
   it. Durable create/update must reuse it rather than re-deriving the rules.
2. **Quality-scoring-profile CRUD** over the *existing* 0004 registry. Scoring
   profiles are open-ended (the design lists a dozen candidate dimensions with
   weights) and have **no reader/scorer wired yet** — like scheduling policy
   (ADR 0028), named operator configuration a future daemon reads rather than
   invents. This issue adds the repo, control-plane, and CLI over that table; it
   does **not** create a table.
3. **Per-library default scoring-profile** selection, the linkage the 0019
   migration reserved.

## Decision

### Schema — migration 0021 (`0021_profile_management.sql`)

Two additive `ALTER`s only; the scoring registry already exists:

```sql
ALTER TABLE video_profiles ADD COLUMN retired_at TEXT;
ALTER TABLE libraries ADD COLUMN default_scoring_profile_name TEXT;
```

- **Reuse the 0004 `quality_scoring_profiles` table** rather than creating a new
  one. It is keyed by `name` (`UNIQUE`), carries an integer `version`, a JSON
  `definition` (passthrough Class P — validated `json_valid` at the DB boundary
  and "is a JSON object" in the repo, never deserialized into a typed
  `deny_unknown_fields` struct), and already has its own `retired_at`.
- **`video_profiles.retired_at`** adds the same soft-retire marker to the
  seeded video-profile registry, which lacked one.
- The **`libraries` linkage is a plain nullable TEXT column keyed by profile
  `name`, not a declared foreign key.** Referential integrity is enforced at
  write time by the repository (setting an unknown or retired profile is
  refused) and is safe because scoring profiles are soft-retired, never
  hard-deleted (`quality_scores.profile_id` is `ON DELETE RESTRICT`), so a
  referenced name is never removed.

### Retire is soft, delete is not offered

A durable video-profile name can be pinned by a compiled policy version
(`transcode video hevc using profile <name>`); a scoring-profile name can be a
library default; `quality_scores` reference a profile by id under `ON DELETE
RESTRICT`. Hard delete would orphan those references. Both registries carry a
nullable `retired_at`: `retire` stamps it, `list` hides retired rows by default,
and `show`/`get_by_name` still resolve a retired row so pinned references and
plan resolution are unaffected. Retire is idempotent.

### Reuse, don't duplicate, the profile validation

`voom profile create|update` builds a `TranscodeVideoProfile` from its inputs and
calls `validate_profile_against_descriptor`; `output_container` and
`max_width`/`max_height` (not carried by that type) are validated alongside.
`target_codec` is **derived** from the encoder's descriptor rather than taken as
a separate, redundant, disagreement-prone argument. Validation failures surface
as `CONFIG_INVALID`, the same code inline-profile errors use. The generated row
`id` is `vp-{name}` — 1:1 with the `UNIQUE` name, matching the seed convention.

### Layering

Store repos (`SqliteVideoProfileRepo` gains `create`/`update`/`retire`;
new `SqliteQualityScoringProfileRepo` over the 0004 table; `SqliteLibraryRepo`
gains `set_default_scoring_profile`) → control-plane thin delegations stamping
the injected clock → CLI command trees emitting one JSON envelope. Config CRUD
emits no events (operator state, not a state machine), consistent with ADR
0027/0028.

CLI surface: `voom profile create|update|retire`, `voom scoring-profile
create|list|show|update|retire`, and an additive `voom library
set-default-scoring-profile` variant (`--scoring-profile <name>` / `--clear`).

## Consequences

- Operators can author durable video and scoring profiles; a future daemon and
  the retention planner have a real registry and per-library default to read.
- No reader consumes scoring `definition` yet; its internal shape is
  intentionally unconstrained until one does.
- `retired_at` on `video_profiles` is additive; no existing query changes
  behavior (seeded rows have `NULL`). Migration 0021 is two `ALTER`s, no table.

## Alternatives considered

- **A second, separate `scoring_profiles` table** (as first sketched by the
  orchestrator): rejected — `quality_scoring_profiles` already exists (0004) and
  is the name the payload inventory and design doc reserve. Reuse it; Architecture
  Trumps All.
- **Typed scoring-profile columns**: rejected — no consumer fixes the dimension
  schema, so any typing is speculative and churn-prone. The 0004 passthrough
  JSON `definition` honors the reserved contract.
- **Hard `delete`**: rejected — orphans policy-pinned, library-default, and
  `quality_scores` references. Soft `retire` preserves resolvability.
