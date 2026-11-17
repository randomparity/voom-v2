# Full-pass scan at concurrency 2 against 4 — design (#839)

## Problem

The decision report rejects concurrency 2 only because no full-pass c2 cell exists. The #788
replay (`scripts/media-evidence-baseline.py`) takes `--concurrency` but never sends ADR 0120's
probe guard, so it cannot measure reuse.

## Scope

- **Replay.** `run --probe-reuse {off,on}` (default `off`). With `on`, the probe request carries
  `guard = {changed_at_ns, file_key}` from the hash result when both are present; otherwise no
  guard is sent. Row `hash_source` is `reused` when a probe result reports it, `recomputed` for
  another probe result, and `null` without one. A `reused` row counts one whole-file read.
- **Record.** `SCHEMA` keeps naming `/1`; new captures write `SCHEMA_V2`
  (`media-evidence-baseline-run/2`), which additionally requires `parameters.probe_hash_reuse`
  and row `hash_source`. `validate_run` accepts both. `reads_per_file` stays the budget's
  no-reuse bound; `logical_bytes_read` is actual.
- **Compare.** `media-evidence-candidates.py compare` refuses cells whose reuse setting
  differs (`/1` reads as `off`).
- **Capture.** Local corpus (34 files), evicted, 3 repetitions, c4/c2 × reuse off/on, all on
  one commit. Order alternates: off c4 then c2, on c2 then c4. Network subset
  `f0049,f0074,f0063`, as-found, c4 and c2, reuse off only: CIFS is outside the reuse list, so
  `on` would recompute (G5).
- **Report.** `docs/media-evidence-baseline/concurrency/README.md` with `compare` verdicts per
  reuse setting; amend the decision-report row; update the protocol's record format (a reuse-on
  record is not a three-read baseline).

### Failure model

1. Actors and deployments: one local operator on one Linux host; CI runs selftests only.
2. Invariants and assets at stake: media content, size, mtime and ctime never change (reads
   may update atime); committed records carry no host path or file name; `/1` records keep
   validating; c2/c4 verdicts compare only cells with equal build and host sections.
3. Accepted failure classes: drive/controller cache and other host activity are not
   controlled, and network counters include rehearsal traffic: disclosed.
4. Covered elsewhere: first durable observation (#841); ext4/xfs guard (#840); transform reads
   (#838); default change; SLOs (#471).

## Success

- Four local records (c4/c2 × off/on), valid `/2`, with identical `build` and `host` sections.
  A non-`observed` row is reported as recorded and marks its comparison failed.
- Per reuse setting, `compare` verdicts for wall, first file done, physical bytes and
  guardrails, in the README and decision row.
- Two network records, reported as not gating.

## Validation

| Contract | Mode | Evidence |
|---|---|---|
| Guard and read accounting | focused-test | baseline selftest: stub reuses only when guarded; `on` counts 1 read per reused row, `off` 3; red before the flag |
| Guard omitted; probe error | focused-test | baseline selftest: hash without `file_key` gives `recomputed`; probe error gives `null` |
| `/2` validation, `/1` unchanged | focused-test | baseline selftest: `/2` lacking either key rejected; committed records, including `concurrency/`, pass |
| Compare reuse match | focused-test | candidates selftest: `off` vs `on` refused; `/1` vs `/2` `off` compares |
| Measurements and verdicts | task-test-not-applicable | real-media timings cannot run in CI |

Green: `just media-evidence-baseline-selftest media-evidence-candidates-selftest
media-evidence-rollout-selftest`.
