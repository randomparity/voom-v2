# #737 implementation plan

Base: main. Branch: feat/audio-companion-default-737. Scope: q737-0108ebe4. Artifact lane full-spec; complexity M, fixed denominator 250 changed lines, provenance issue #737 and live synthesis/shared-helper inspection. Hazards: generated-media disposition contract and shared-helper compatibility. Design: ../specs/2026-10-24-audio-companion-default-design.md.

## One coupled Cast unit
Add synthesis-specific command handling and output rejection without changing ordinary metadata copying. Expected implementation/test diff: approximately 260 lines. Initial estimate undercounted formatted command/validator regressions and the required worker HTTP/process proof plumbing; this corrects estimation only, with frozen scope and denominator unchanged.

Verification inventory (`focused-test`):
- Original default preservation and non-default companions for default/non-default sources and multiple selections: ffmpeg_test.rs command regressions; expected red inherited default/missing explicit disposition, green `cargo test -p voom-ffmpeg-worker --lib synthesize`.
- Unexpected companion default rejection: ffmpeg_test.rs validator regression; expected red accepts default, same green command.
- Actual generated media and truthful facts: tests/transcode_conformance.rs existing synthesis test extended to launch the worker binary and dispatch through HTTP/progress; expected red two default audio tracks, green `cargo test -p voom-ffmpeg-worker --test transcode_conformance add_track_synthesis -- --nocapture`.
- Compatibility: existing ordinary transcode metadata tests, green worker suite; shared helper stays unchanged.

No ownership transition, obsolete paths, or retained new compatibility paths. No numbered ADR/migration required. Final integration: worker tests, `just ci` (worker owns final run), normal hooks; then review/shipping with no merge authorization.

## Continuity
Step: design. Open findings: none. Deferrals: none. Follow-up candidates: none. Review design budget two bounded independent passes only if first has defensible blockers; final iterating review budget two. Guardrail timings unknown until measured. Parent baseline is comparison only. Architecture x86_64, no target declared; GNU userland, shell detector unknown (tool shell zsh), Bash helper interpreter 5.3.9. Clean primary with one sibling selects external isolated worktree. ADR index coupled, no mandatory per-PR edits. Prerequisites gh/git/jq/bash/just/cargo/python3/ffmpeg/ffprobe available; versions observed in private preflight output, no new floors declared. Existing prek pre-commit uses repository guard recipes; no pre-push hook, final `just ci` required. Prior tooling solution read: pipe-draining preflight deadlock; do not reintroduce undrained pipes.
