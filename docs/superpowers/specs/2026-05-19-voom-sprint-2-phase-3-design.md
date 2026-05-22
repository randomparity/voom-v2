---
name: voom-sprint-2-phase-3-design
description: Sprint 2 Phase 3 combined design + plan — fake provider suite. Adds voom-fake-support (shared helpers) and voom-fakes (eleven binaries) as new workspace crates. Phase 3 scaffold ships the crate layout and one representative fake (fake-scanner) so the architecture is on disk; the other ten fakes are documented placeholders deferred to follow-up commits.
status: proposed
date: 2026-05-19
sprint: 2
phase: 3
branch: feat/sprint-2
parent_spec: docs/superpowers/specs/2026-05-19-voom-sprint-2-design.md
parent_sections: §2 Phase 3, §3 (voom-fake-support + voom-fakes rows)
scope: Historical Phase 3 architectural surface + one representative fake; all eleven Sprint 2 fake providers are completed and accepted by the Phase 6 closeout doc
---

# Sprint 2 Phase 3 — Fake Provider Suite (combined design + plan)

> Supersession note: this is a historical scaffold design. The TODO fake
> binaries described here were follow-up placeholders only. The current
> Sprint 2 acceptance source for active fake-provider behavior is
> `docs/superpowers/specs/2026-05-21-voom-sprint-2-phase-6-fake-providers-conformance-closeout-design.md`.

## 1. Goal

Place the two new crates the overview specifies — `voom-fake-support`
(shared helpers consumed only by the eleven `fake-*` binaries) and
`voom-fakes` (the binaries themselves) — on disk with their public
shape pinned. Ship one representative fake binary (`fake-scanner`)
so the architecture is end-to-end exercisable. The other ten fakes
are documented in this spec and shipped as TODO scaffolds in the
binaries crate.

## 2. Crate layout

```
crates/
├── voom-fake-support/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── scenario.rs        — scripted scenario loader (JSON)
│       └── lease_loop.rs      — lease accept + progress emit helpers
└── voom-fakes/
    ├── Cargo.toml
    └── src/
        └── bin/
            ├── fake_scanner.rs           — Phase 3 representative (ships)
            ├── fake_prober.rs            — TODO (Phase 3 follow-up)
            ├── fake_transcoder.rs        — TODO
            ├── fake_remuxer.rs           — TODO
            ├── fake_backup_store.rs      — TODO
            ├── fake_health_checker.rs    — TODO
            ├── fake_identity_provider.rs — TODO
            ├── fake_external_system.rs   — TODO
            ├── fake_quality_scorer.rs    — TODO
            ├── fake_issue_provider.rs    — TODO
            └── fake_use_lease_provider.rs — TODO
```

Each TODO scaffold is a one-line `fn main()` that prints
`fake-<name> is a Phase 3 follow-up commit` to stderr and exits 0;
this keeps the binary targets compilable without false-passing as
real workers. The conformance harness (Phase 6 expansion) will skip
these scaffolds based on a `voom-fakes.toml` manifest.

Historical acceptance for this phase was limited to landing the crate
layout plus `fake-scanner`. Sprint 2 release acceptance requires all
eleven fake providers to be active manifest-backed workers under the
Phase 6 closeout criteria.

## 3. Scenario format (voom-fake-support)

```jsonc
{
  "scenario": "fake-scanner-basic",
  "events": [
    { "kind": "discover_file", "path": "/library/movies/example.mkv", "size": 1234567890 },
    { "kind": "discover_file", "path": "/library/movies/example.srt", "size": 4096 },
    { "kind": "scan_complete", "duration_ms": 1500 }
  ]
}
```

The scenario file is JSON (matches the rest of the Sprint 2 wire
format) and each `kind` maps to a typed scenario event the fake's
operation handler consumes. The shared library exposes
`load_scenario(path) -> Result<Scenario, ScenarioError>` and
`ScenarioPlayer::next() -> Option<Event>`.

## 4. Phase 3 commits

1. `voom-fake-support` skeleton: lib.rs with `Scenario`, `ScenarioPlayer`,
   `lease_loop` helpers — minimal traits + a `noop` scenario test fixture.
2. `voom-fakes` skeleton: ten TODO bins + `fake-scanner` real bin
   driving a hard-coded scenario.

Adversarial review: one round after both commits land.
