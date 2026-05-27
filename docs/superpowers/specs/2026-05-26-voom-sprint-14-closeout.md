---
name: voom-sprint-14-closeout
description: Sprint 14 closeout evidence for policy-driven audio transcode and exactly-one audio extraction through durable tickets, FFmpeg workers, staged artifacts, sidecar bundles, events, and reporting.
---

# VOOM Sprint 14 Closeout

| Requirement | Evidence |
|---|---|
| DSL accepts supported audio mutation policy shapes | `cargo test -p voom-policy`; fixture `crates/voom-policy/fixtures/policies/audio-transcode-extract.voom`. |
| Planner emits audio transcode and exact-one extraction nodes | `cargo test -p voom-plan audio`; planner coverage in `crates/voom-plan/src/planner_test.rs`. |
| Already-compliant audio streams no-op and selector blockers are visible | `cargo test -p voom-plan audio`; blockers for zero, multiple, and unknown commentary matches in `crates/voom-plan/src/planner_test.rs` and `crates/voom-plan/src/audio_test.rs`. |
| Worker protocol carries typed audio requests/results | `cargo test -p voom-worker-protocol audio`; request/result coverage in `crates/voom-worker-protocol/src/audio_test.rs`. |
| FFmpeg worker preflight fails loudly for missing capabilities | `cargo test -p voom-ffmpeg-worker preflight`; encoder and muxer checks in `crates/voom-ffmpeg-worker/src/preflight_test.rs`. |
| FFmpeg worker writes only staged audio outputs and validates selected stream facts | `cargo test -p voom-ffmpeg-worker`; audio execution and metadata preservation tests in `crates/voom-ffmpeg-worker/src/ffmpeg_test.rs` and `handler_test.rs`. |
| Control plane stages, verifies, commits, and snapshots audio transcodes | `cargo test -p voom-control-plane audio`; control-plane audio module tests under `crates/voom-control-plane/src/audio/`. |
| Control plane commits extracted audio as sidecar bundle members | `cargo test -p voom-control-plane audio`; sidecar commit and recovery coverage in `crates/voom-control-plane/src/audio/commit_test.rs`. |
| Compliance execution routes audio operations through durable tickets | `cargo test -p voom-control-plane workflow compliance`; workflow and compliance bridge tests in `crates/voom-control-plane/src/workflow/executor_test.rs` and `cases/compliance_test.rs`. |
| Real audio transcode end-to-end flow works through out-of-process workers | `cargo test -p voom-control-plane --test audio_transcode_flow`; real FFmpeg fixture in `crates/voom-control-plane/tests/audio_transcode_flow.rs`. |
| Real audio extraction end-to-end flow works through out-of-process workers | `cargo test -p voom-control-plane --test audio_extract_flow`; sidecar bundle assertions in `crates/voom-control-plane/tests/audio_extract_flow.rs`. |
| Events and reporting expose audio start/progress/success/failure facts | `cargo test -p voom-events audio`; typed event payload coverage in `crates/voom-events/src/payload_test.rs` and control-plane audio event tests. |
| Test layout and lint contracts hold | `just check-test-layout`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`. |
| Full suite passes | `just ci`. |
