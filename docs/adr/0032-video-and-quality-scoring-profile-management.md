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
all — even though the design doc (§*Quality Scoring Registry*) and the payload
contract inventory already reserve `quality_scoring_profiles.definition` (Class
P passthrough JSON) and each library's active scoring profile. The 0019
libraries migration comment explicitly defers the per-library scoring-profile
default column to this issue (#285, T16).

Two independent deliverables plus one linkage:

1. **Named video profile create/update/retire**, applying the *same* field
   validation the policy compiler applies to inline `transcode video` profiles
   (encoder family, CRF range, preset, tune, codec profile/level, pixel format,
   container, dimensions). That validation already lives as a reusable,
   protocol-neutral function, `voom_core::validate_profile_against_descriptor`,
   over `EncoderDescriptor` — the worker request path and the compiler both use
   it. Durable create/update must reuse it rather than re-deriving the rules.
2. **Quality-scoring-profile CRUD** over a new durable registry. Scoring
   profiles are open-ended (the design lists a dozen candidate dimensions with
   weights) and have **no reader yet** — exactly the shape of scheduling policy
   (ADR 0028): named, slug-keyed operator configuration a future daemon reads
   rather than invents.
3. **Per-library default scoring-profile** selection, the linkage the 0019
   migration reserved.

## Decision

### Schema — migration 0021 (`0021_quality_scoring_profiles.sql`)

One new `STRICT` table plus two additive `ALTER`s:

```sql
CREATE TABLE quality_scoring_profiles (
    id           INTEGER PRIMARY KEY,
    slug         TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    definition   TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(definition)),
    retired_at   TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;

ALTER TABLE video_profiles ADD COLUMN retired_at TEXT;

ALTER TABLE libraries ADD COLUMN default_scoring_profile_slug TEXT
    REFERENCES quality_scoring_profiles (slug);
```

- **Table name `quality_scoring_profiles`, column `definition`** match the
  already-checked-in payload contract inventory. `definition` is a **passthrough
  JSON object** (Class P): validated only as `json_valid` at the DB boundary and
  as "is a JSON object" in the repo, never deserialized into a typed
  `deny_unknown_fields` struct. Typing a dozen speculative dimension weights with
  no consumer would be premature; the daemon-era reader will type it when it
  exists.
- The `libraries` FK column is nullable with a `NULL` default, so the `ALTER`
  is legal on existing rows and FK-safe (foreign keys are enabled;
  `slug` is `UNIQUE`, a valid FK target).

### Retire is soft, delete is not offered

A durable video-profile name can be pinned by a compiled policy version
(`transcode video hevc using profile <name>`); a scoring-profile slug can be a
library default. Hard delete would orphan those references. Both registries get
a nullable `retired_at`: `retire` stamps it, `list` hides retired rows by
default, and `show`/`get_by_name`/`get_by_slug` still resolve a retired row so
pinned references and plan resolution are unaffected. Retire is idempotent.

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
new `SqliteQualityScoringProfileRepo`; `SqliteLibraryRepo` gains
`set_default_scoring_profile`) → control-plane thin delegations stamping the
injected clock → CLI command trees emitting one JSON envelope. Config CRUD emits
no events (operator state, not a state machine), consistent with ADR 0027/0028.

CLI surface: `voom profile create|update|retire`, `voom scoring-profile
create|list|show|update|retire`, and an additive `voom library
set-default-scoring-profile` variant (`--scoring-profile <slug>` / `--clear`).

## Consequences

- Operators can author durable video and scoring profiles; a future daemon and
  the retention planner have a real registry and per-library default to read.
- No reader consumes scoring `definition` yet; its internal shape is
  intentionally unconstrained until one does.
- `retired_at` is additive; no existing query changes behavior (seeded rows have
  `NULL`). The two `ALTER`s keep migration 0021 self-contained.

## Alternatives considered

- **Typed scoring-profile columns** (mirror scheduling policy exactly): rejected
  — no consumer fixes the dimension schema, so any typing is speculative and
  churn-prone. Passthrough JSON honors the reserved contract.
- **Hard `delete`**: rejected — orphans policy-pinned and library-default
  references. Soft `retire` preserves resolvability.
- **Separate `scoring_profiles` table name** (as first sketched by the
  orchestrator): rejected in favor of `quality_scoring_profiles`, the name the
  payload inventory and design doc already reserve — Architecture Trumps All.
