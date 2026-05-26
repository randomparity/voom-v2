# Issue 87 Planner Traversal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `snapshot_operations` the only recursive planner operation traversal used for both normal expansion and remux grouping.

**Architecture:** Keep the existing flattened traversal helper in `planner.rs`; remove the now-redundant recursive arms from leaf expansion. Add focused planner tests for rule traversal so ordering and unknown-condition blocking remain pinned.

**Tech Stack:** Rust 2024, `voom-plan` unit tests with sibling test files, `just`.

---

## Files

- Modify: `crates/voom-plan/src/planner.rs`
- Modify: `crates/voom-plan/src/planner_test.rs`
- Add: `docs/superpowers/specs/2026-05-26-issue-87-unify-planner-traversal-design.md`
- Add: `docs/superpowers/plans/2026-05-26-issue-87-unify-planner-traversal.md`

## Tasks

- [x] Add characterization tests for `Rules` `First`, `Rules` `All`, and unknown rule conditions producing blocked leaf operations.
- [x] Remove duplicate `Conditional`/`Rules` recursive arms from `expand_operation_for_snapshot` and delete `expand_rules_for_snapshot`.
- [x] Run adversarial review and address material findings.
- [x] Run simplification review and address the most relevant safe recommendation.
- [x] Run targeted planner tests, `just fmt-check`, and `just ci`.

## Test Commands

```bash
cargo test -p voom-plan planner_test::rules
cargo test -p voom-plan planner_test::remux
cargo test -p voom-plan fixtures::tests::remux_track_selection
just fmt-check
just ci
```
