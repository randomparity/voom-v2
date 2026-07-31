---
status: accepted
date: 2026-07-31
deciders: [VOOM core]
---

# 0053 — Artifact access mode is core domain vocabulary

## Context

Remote worker capability rows advertise artifact-access modes as JSON strings.
The scheduler currently accepts those strings, selects another string, and the
control plane converts the selection into the store-owned `ArtifactAccessMode`
enum before creating an artifact access plan. This duplicates the vocabulary at
the scheduler/control-plane seam and makes an impossible unsupported scheduler
selection a runtime error path.

The three supported modes and their snake-case tokens are shared domain facts.
They are used by scheduling policy, durable artifact access plans, and remote
dispatch payloads; they are not properties of SQLite.

Worker capability advertisements remain an extensible external and persistence
surface. They may contain tokens this binary does not understand, and the current
scheduler ignores those tokens while still selecting a supported token in the
same advertisement.

## Decision

`ArtifactAccessMode` lives in `voom-core` as the canonical closed vocabulary. It
owns the stable serde tokens, `as_str`, wire recognition, and contextual database
parsing, following the existing core execution-vocabulary pattern.

`voom-store` uses the core type for durable artifact access plans.
`voom-scheduler::WorkerCandidate` contains `Vec<ArtifactAccessMode>`, and
`SelectedCandidate` contains one `ArtifactAccessMode`. Scheduler selection,
scoring, and explanation rendering operate on the enum.

Persisted worker advertisements remain `Vec<String>`. The control plane projects
only recognized tokens into scheduler candidates with `from_wire`; unknown tokens
are ignored. This conversion is the boundary between the extensible persisted
advertisement and the scheduler's closed supported vocabulary.

The enum is also the durable-known vocabulary needed to decode historical plans.
Retiring a mode therefore happens in two steps: first exclude it from the
control-plane scheduler projection while retaining the variant for durable reads;
remove the variant only after a coordinated data/schema migration eliminates its
historical rows and `CHECK` token.

## Consequences

- Scheduler and artifact-plan code cannot disagree about supported modes or pass
  an unsupported selected string.
- Existing database values, JSON tokens, selection priority, scores, explanations,
  and remote payloads remain unchanged.
- A capability containing only unknown modes remains ineligible. A capability
  containing both unknown and known modes can still select the known mode.
- Merely persisting a new advertisement token does not make it schedulable.
  Activating a supported mode requires the core enum, scheduler priority and score,
  a migration widening the `artifact_access_plans.selected_access_mode` `CHECK`,
  and coordinated dispatch/worker consumer support.
- Disabling a mode for new scheduling does not immediately remove its enum variant;
  durable rows remain readable until the retirement migration is complete.

## Considered and rejected

- **Keep the store-owned enum and make the scheduler depend on `voom-store`.**
  Rejected because it reverses the one-way crate layering and makes a domain
  decision depend on SQLite infrastructure.
- **Keep the current store enum and raw-string scheduler seam.** Rejected because
  the scheduler can return a token that only a later control-plane conversion
  rejects, while the shared vocabulary remains owned by persistence code.
- **Move the enum to core but type only `SelectedCandidate`.** The scheduler could
  retain `WorkerCandidate.artifact_access: Vec<String>`, recognize tokens during
  selection, and return the enum. Rejected because persisted-token projection
  belongs at the application boundary; scheduler inputs should contain only domain
  values the scheduler supports, not external vocabulary it must parse defensively.
- **Type every worker capability advertisement.** Rejected because advertisements
  are an extensible worker/persistence contract; rejecting unknown future tokens
  would replace the current forward-compatible behavior. An open known/unknown
  wrapper could retain them, but adds a state the scheduler deliberately does not
  use and is more complex than projecting recognized modes at its boundary.
