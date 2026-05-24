# Sprint 7 Simplification Review Follow-up Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the branch simplification review by addressing the remaining stale-node scan recommendation.

**Architecture:** Keep the node repository behavior unchanged while reducing avoidable row work. `mark_stale_in_tx` should ask SQLite only for candidate statuses that can transition to stale, then preserve the existing per-row TTL and optimistic update guard.

**Tech Stack:** Rust, `sqlx`, SQLite, tokio tests, project `just` commands.

---

### Task 1: Narrow Stale Node Candidate Scanning

**Files:**
- Modify: `crates/voom-store/src/repo/nodes.rs`
- Verify: `crates/voom-store/src/repo/nodes_test.rs`
- Verify: `crates/voom-control-plane/src/cases/nodes_test.rs`

- [ ] **Step 1: Confirm existing behavior tests cover intent**

Run:

```bash
cargo test -p voom-store nodes_mark_stale
cargo test -p voom-control-plane mark_stale_nodes
```

Expected: both commands pass before the refactor.

- [ ] **Step 2: Simplify candidate query and iteration**

In `crates/voom-store/src/repo/nodes.rs`, update `mark_stale_in_tx` so the query uses:

```sql
FROM nodes WHERE status IN ('registered','active') ORDER BY last_seen_at ASC, id ASC
```

Then replace the intermediate `candidates` vector with direct row decoding:

```rust
let mut changed = Vec::new();
for row in &rows {
    let node = row_to_node(row)?;
    if let Some(node) = mark_stale_candidate_in_tx(tx, &node, now).await? {
        changed.push(node);
    }
}
Ok(changed)
```

- [ ] **Step 3: Verify focused behavior**

Run:

```bash
cargo test -p voom-store nodes_mark_stale
cargo test -p voom-control-plane mark_stale_nodes
```

Expected: both commands pass.

- [ ] **Step 4: Run project checks for touched code**

Run:

```bash
just fmt-check
just lint
```

Expected: both commands pass.

- [ ] **Step 5: Commit**

Run:

```bash
git add docs/superpowers/plans/2026-05-23-sprint-7-simplification-review-followup.md crates/voom-store/src/repo/nodes.rs
git commit -m "refactor: narrow stale node scan"
```
