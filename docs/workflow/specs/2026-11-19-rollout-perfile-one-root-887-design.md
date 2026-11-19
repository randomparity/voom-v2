# Refuse multi-root per-file publication cells — design (#887)

## Problem

`publication_problems` in `scripts/media-evidence-rollout.py` proves a `/2` per-file cell
from two per-repetition aggregates: `batches` equals the observation count, and
`published_before_complete` is above zero. `scan` requests one session per root, so with more
than one root both aggregates span every root. One root that publishes during its session can
lift `published_before_complete` above zero while another root publishes only at completion,
and a root that flushed one batch per observation can cover a root that did not. The record
then passes as a per-file cell it did not prove.

## Decision

Refuse a per-file cell with more than one root; do not prove per root.

The record cannot support a per-root proof. A repetition carries `batches`,
`published_before_complete`, `session_status` and `evidence` as cell-wide values;
`session_status` is a sorted list with no root or session identity, and the only per-root fact
the record holds is the count `corpus.roots`. A per-root proof would need new per-root fields,
a third scan schema, and a new capture path in `wait_for_publication`, `observation_batches` and
`stored_evidence`. Nothing needs a multi-root per-file cell: the committed per-file record is a
full pass with `corpus.roots == 1`, and a full pass always scans one root. `scan_roots` gives
each file of a subset its own root, so the refusal leaves per-file cells only for a full pass or a
one-file subset; no measurement uses a larger per-file subset.

## Scope

- `scripts/media-evidence-rollout.py`:
  - `publication_problems`: a per-file record whose `corpus.roots` is not exactly `1` adds the
    problem `per-file roots`. The check compares with `!= 1`, so a value that is not equal to 1
    (for example a hand-edited string) refuses instead of raising; `capture_scan` always writes
    an integer count. The two existing per-repetition checks are unchanged.
  - `capture_scan`: directly after `select_files`, before `corpus_filesystem` and the work
    directory is created, a `--publication per-file` capture whose selection yields more than
    one `scan_roots` root raises `HarnessError` naming the root count, so an operator does not
    run a long capture that validation then refuses, and a refused run leaves no work directory.
    The root list is computed once and reused for `corpus.roots`.
- `scripts/media-evidence-rollout-selftest.py`: `RecordTests` cases (under Validation).
- No ownership transition: `publication_problems` stays the per-file contract's one owner;
  `capture_scan` is its caller.

### Failure model

1. Actors and deployments: a local operator capturing or comparing rollout scan cells;
   `just media-evidence-rollout-selftest` on Linux and macOS CI.
2. Invariants: a `/2` per-file record validates only when its proof cannot be satisfied by
   one root on behalf of another; every committed scan record still validates.
3. Accepted: per-root proof is not offered; a multi-root per-file measurement needs a new
   schema first. Completion cells and `/1` records are unaffected.
4. Covered elsewhere: `compare` refuses invalid records through `read_compared` (#886), so a
   multi-root per-file record also cannot be compared.

## Success

1. A per-file record with `corpus.roots == 2` whose aggregates pass (`batches` equals
   observations, `published_before_complete > 0`) fails validation with `per-file roots`.
2. A single-root per-file record and every completion record validate as before, including the
   committed `per-file-publication/2026-11-17-per-file-local-corpus-evicted.json`.
3. A per-file `scan` over a selection that spans two roots refuses before starting a stack.

## Validation

- Contract: a multi-root per-file proof is refused. Mode: focused-test.
  `RecordTests.per_file_record` sets `corpus.roots = 1` (the shared fixture fills every corpus
  key with `"x"`, which the new check refuses).
  `test_a_multi_root_per_file_cell_is_refused` builds the passing per-file fixture with
  `corpus.roots = 2` (one root's publication would satisfy the aggregate) and asserts
  `per-file roots`; it also asserts a `"x"` root count refuses; the same record as a completion
  cell validates. Red without the check: `validate_scan` returns `[]`.
- Contract: capture refuses before any stack. Mode: focused-test.
  `test_a_per_file_capture_refuses_more_than_one_root` patches `select_files`,
  `corpus_filesystem` and `binaries_build` as the sibling capture test does, patches
  `scan_roots` to two roots and `scan_repetition` to `self.fail`, and asserts
  `assertRaisesRegex(HarnessError, "2 roots")` and that no work directory was created. Red
  without the check: `scan_repetition` is reached and the test fails.
- Contract: committed records still validate. Mode: existing focused-test
  `test_committed_records_validate`.
- Green command: `just media-evidence-rollout-selftest`; gate: `just source-checks`.
