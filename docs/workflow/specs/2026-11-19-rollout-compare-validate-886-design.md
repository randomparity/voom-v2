# Validate both records before rollout compare — design (#886)

## Problem

`media-evidence-rollout.py compare` parses its two records and passes them straight to
`compare_scans`. `validate_scan` runs only when `scan` captures a record, so a hand-edited,
truncated or wrong-kind record can still be judged and print verdicts.

## Scope

- `scripts/media-evidence-rollout.py`: a new module function `read_compared(path)` returns
  `(record, problems)`: `["not valid JSON"]` when the file does not decode or parse,
  `["not a JSON object"]` for a non-object, `["malformed sections (<error>)"]` when `validate_scan`
  raises `AttributeError`/`TypeError` on wrong nested types, else `validate_scan(record)`.
  `main` reads the baseline, then the candidate, and when either has problems raises
  `HarnessError` before `compare_scans` runs, naming each failing input by role and the path
  the operator gave, e.g. `baseline a.json failed validation: not_measured; candidate b.json
  failed validation: schema`. The existing handler prints it and exits 1. A construct or
  baseline-run record refuses on `schema`; an unreadable path keeps the existing `OSError`.
- `scripts/media-evidence-rollout-selftest.py`: new `CompareInputTests` cases call
  `rollout.main(["compare", ...])` (detailed under Validation).
- No ownership transition: `validate_scan` stays the scan contract's one owner.

### Failure model

1. Actors and deployments: a local operator running `compare` on committed or freshly captured
   records; `just media-evidence-rollout-selftest` on Linux and macOS CI.
2. Invariants: a record that breaks the scan contract `validate_scan` defines never produces a
   verdict; every committed scan record passes the check; the comparable pairs keep comparing.
3. Accepted: a shape `validate_scan` passes but `compare_scans` cannot read stays with
   `compare_scans`; `validate_scan` itself is unchanged.
4. Covered elsewhere: what makes two valid cells comparable stays with `compare_scans`'
   existing refusals and their `CompareTests`.

## Success

1. `compare` with an invalid baseline exits 1, prints `baseline <path> failed validation:` and
   its problems, and never calls `compare_scans`.
2. The same holds for an invalid candidate, named `candidate`.
3. Every committed `media-evidence-rollout-scan` record has no `read_compared` problems, and the
   four comparable pairs (rollout `g3-stage0-off-…-after-816` / `g2-g3-on-…-after-816`, canary
   off / on, net off / on; per-file stage-0 completion / per-file) exit 0 through `main`.

## Validation

- Contract: refusal of an invalid first record. Mode: focused-test.
  `test_compare_refuses_an_invalid_baseline` copies a committed baseline with `not_measured`
  removed into a temp file, patches `compare_scans` to fail if called, and asserts exit 1 and a
  stderr line naming `baseline`, the path and `not_measured`. Red without the check: exit 0.
- Contract: refusal of an invalid second record. Mode: focused-test.
  `test_compare_refuses_an_invalid_candidate` does the same with a JSON array as the candidate,
  expecting `candidate`, the path and `not a JSON object`. Red without the check: the array
  reaches `compare_scans` and raises `TypeError`, not exit 1.
- Contract: valid pairs still compare. Mode: focused-test.
  `test_committed_records_pass_compare_validation` asserts no problems for every committed scan
  record, and runs `main` on each Success-3 pair asserting exit 0 and a JSON verdict.
- Contract: unparseable and wrong-shape records refuse by name. Mode: focused-test.
  `test_compare_names_unparseable_and_malformed_records` (truncated baseline, `cell` as a list
  candidate) expects both roles with `not valid JSON` and `malformed sections`.
- Green command: `just media-evidence-rollout-selftest`; gate: `just source-checks`.
