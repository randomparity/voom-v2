# Full-pass scan at concurrency 2 against 4 — design (#839)

## Problem

The decision report rejects concurrency 2 only because no full-pass c2 cell exists. The #788
replay (`scripts/media-evidence-baseline.py`) takes `--concurrency` but never sends ADR 0120's
probe guard, so it cannot measure the selected reuse strategy.

## Scope

- **Replay.** `run --probe-reuse {off,on}` (default `off`). With `on`, the probe request carries
  `guard = {changed_at_ns, file_key}` from the hash result when both are present; otherwise no
  guard is sent and the worker recomputes. Each row records `hash_source`: `reused` when the
  probe reports it, `recomputed` for any other probe result, `null` when no probe result
  exists. A `reused` row counts one completed whole-file read; other rows keep the existing
  per-outcome counts.
- **Record.** New captures write schema `media-evidence-baseline-run/2`: `/1` plus
  `parameters.probe_hash_reuse` and row `hash_source`, both required. `validate_run` checks each
  schema's exact key set, so committed `/1` records stay valid unchanged.
- **Compare.** `media-evidence-candidates.py compare` refuses cells whose reuse setting
  differs (`/1` reads as `off`).
- **Capture.** Release workers from this branch. Local corpus (34 files), evicted,
  3 repetitions: c4/c2 × reuse off/on, run c4 then c2 per setting, no builds during capture.
  Network subset `f0049,f0074,f0063`, as-found, c4 and c2 with reuse off only: CIFS is outside
  the reuse list, so `on` would recompute (G5).
- **Report.** `docs/media-evidence-baseline/concurrency/README.md` with `compare` verdicts per
  reuse setting; amend the decision-report row; update the protocol's record-format section.
  No production default change.

### Failure model

1. Actors and deployments: one local operator on one Linux host; CI runs selftests only.
2. Invariants and assets at stake: media bytes and timestamps never change; committed records
   carry no host path or file name; `/1` records keep validating; c2/c4 verdicts compare only
   matched cells.
3. Accepted failure classes: drive/controller cache and other host activity are not controlled
  , and network counters include rehearsal traffic: disclosed.
4. Covered elsewhere: first durable observation (#841); ext4/xfs guard (#840); transform reads
   (#838); default change (future issue); SLOs (#471).

## Success

- Four committed local records (c4/c2 × off/on), valid `/2`, every row `observed`; `on` rows
  `reused` on btrfs.
- Per reuse setting, `compare` verdicts for wall, first file done, physical bytes and resource
  guardrails, in the README and decision row.
- Two network records, reported as not gating.

## Validation

| Contract | Mode | Evidence |
|---|---|---|
| Guard sent and reuse read accounting | focused-test | baseline selftest: stub probe echoes `reused` only when guarded; `on` record has 1 read per reused row, `off` has 3; red before the flag exists |
| Guard omitted when hash lacks ctime or key | focused-test | baseline selftest: stub hash without `file_key` yields `recomputed` |
| `/2` validation, `/1` unchanged | focused-test | baseline selftest: `/2` missing `hash_source` or `probe_hash_reuse` rejected; committed `/1` records pass |
| Compare refuses mixed reuse | focused-test | candidates selftest: `off` vs `on` raises `CandidateError` |
| Measurements and verdicts | task-test-not-applicable | real-media timings; the committed-record validation test covers their shape |

Green: `just media-evidence-baseline-selftest media-evidence-candidates-selftest`.
