# Issue 89 Shared Worker Dispatch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove duplicated remux/transcode worker dispatch stream handling and bundled-worker binary discovery without changing observable behavior.

**Architecture:** `artifact::worker` already owns `WorkerCommand` and bundled worker process launch, so shared dispatch and discovery helpers live there. Remux and transcode keep operation-specific request construction, validation, progress handling, and result type selection while delegating common protocol mechanics.

**Tech Stack:** Rust 2024, tokio timeout, serde JSON, `voom-worker-protocol::ClientHandle`, existing sibling unit-test layout, `just`.

---

## Files

- Modify: `crates/voom-control-plane/src/artifact/worker.rs`
- Modify: `crates/voom-control-plane/src/artifact/worker_test.rs`
- Modify: `crates/voom-control-plane/src/scan/worker.rs`
- Modify: `crates/voom-control-plane/src/remux/dispatch.rs`
- Modify: `crates/voom-control-plane/src/transcode/dispatch.rs`
- Create if needed: `crates/voom-control-plane/src/transcode/dispatch_test.rs`

## Tasks

- [x] Add characterization tests for transcode bundled worker discovery, matching the existing remux/scan/verify sibling and `deps` behavior.
- [x] Add shared bundled-worker command discovery in `artifact::worker` and convert remux, transcode, scan, and verify-artifact command builders to use it.
- [x] Add shared `VoomError` worker operation dispatch helper in `artifact::worker`, with labels preserving current messages.
- [x] Convert transcode dispatch to the shared helper and verify transcode dispatch tests.
- [x] Convert remux dispatch to the shared helper with a progress handler and verify remux dispatch tests.
- [x] Run adversarial code review, address material findings, then run simplification review and address the highest-leverage safe recommendation.
- [x] Run targeted tests, `just fmt-check`, and `just ci`.

## Test Commands

```bash
cargo test -p voom-control-plane transcode::dispatch
cargo test -p voom-control-plane remux::dispatch
cargo test -p voom-control-plane scan::worker
cargo test -p voom-control-plane artifact::worker
just fmt-check
just ci
```
