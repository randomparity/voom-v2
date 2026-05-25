---
name: voom-sprint-10-closeout
description: Sprint 10 closeout evidence for explicit-path real ingest, the bundled ffprobe worker boundary, scan envelopes, and docs.
status: complete
date: 2026-05-24
sprint: 10
references:
  - docs/specs/voom-control-plane-design.md
  - docs/superpowers/specs/2026-05-24-voom-sprint-10-design.md
---

# VOOM Sprint 10 Closeout

## Acceptance Matrix

| Requirement | Command | Observed result |
|---|---|---|
| scan command | `cargo test -p voom-cli --test scan_envelope` | passed: 5 tests; `scan_file_success_outputs_envelope_and_persists_snapshot`, `scan_directory_reports_unsupported_entries_as_skipped`, `scan_unsupported_explicit_file_is_bad_args`, `scan_reuses_builtin_ffprobe_worker_row`, and `scan_content_drift_fails_without_snapshot` all ok |
| discovery/hash | `cargo test -p voom-control-plane scan` | passed: 30 unit tests plus 1 filtered integration test; discovery ordering, unsupported extensions, symlink rejection, BLAKE3 facts, and canonical scan report path all ok |
| worker boundary | `cargo test -p voom-control-plane scan` and `cargo test -p voom-ffprobe-worker --test probe_worker` | passed: control-plane launch/dispatch tests use a caller-supplied worker id and child process; worker protocol test suite passed 6 tests |
| worker bootstrap reuse | `cargo test -p voom-cli --test scan_envelope` | passed: `scan_reuses_builtin_ffprobe_worker_row` ok and scan snapshots record one reused `builtin.ffprobe` worker id |
| snapshot persistence | `cargo test -p voom-cli --test scan_envelope` and `cargo test -p voom-control-plane scan` | passed: CLI success test persisted one media snapshot; control-plane persistence tests record file identity plus media snapshot with the selected worker |
| skipped unsupported files | `cargo test -p voom-cli --test scan_envelope` and `cargo test -p voom-control-plane scan` | passed: directory unsupported entry is reported as skipped; unsupported explicit file returns `BAD_ARGS` |
| failure envelope | `cargo test -p voom-cli --test scan_envelope` | passed: content drift returns a failure envelope and records zero media snapshots |
| CLI snapshots | `cargo test -p voom-cli --test scan_envelope` | passed: all five scan envelope insta snapshots matched |
| real ffprobe fixture | `cargo test -p voom-ffprobe-worker --test probe_worker` | passed: 6 tests; `real_ffprobe_success_returns_progress_and_probe_result` ran against `ffprobe` 7.1.4 and `crates/voom-ffprobe-worker/fixtures/media/tiny.mp4` |
| docs | `rg -n "T[B]D|T[O]DO|place[Hh]older|INCOM""PLETE" docs/specs/voom-control-plane-design.md docs/superpowers/specs/2026-05-24-voom-sprint-10-closeout.md` | passed: no matches; `rg` returned exit code 1 because the marker scan found no rows |
| `just ci` | `just ci` | passed: fmt-check, clippy, test-layout, workspace tests, docs, deny, and audit all completed; final output was `==> All CI checks passed` |

The docs row uses adjacent shell string literals for the final marker word so
the recorded command evaluates to the required marker scan without matching its
own closeout evidence.

## Deferred Work

Sprint 10 intentionally leaves durable library roots, scheduled scans, watch
loops, policy-driven scan selection, remote media transfer, daemon scan
surfaces, and mutation workers to later roadmap phases named in
`docs/specs/voom-control-plane-design.md`.
