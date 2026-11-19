# Kernel release in oracle and ctime-probe records — design (#891)

## Problem

The oracle's same-tick claim depends on the kernel's ctime granularity, but oracle and
ctime-probe records do not say which kernel produced them. For the #840 records the release
is stated only in `docs/media-evidence-baseline/reuse/README.md` prose.

## Scope

- `scripts/media-evidence-fingerprint.py`: oracle schema `media-evidence-fingerprint-oracle/3`
  adds `parameters.kernel_release`, the value of `platform.release()`, or `unavailable` when
  that is empty. `/2` keeps its own constant (`ORACLE_SCHEMA_V2`). `validate_oracle` keeps
  the `/2` checks (tick wait, coarse-tick witness) for `/2` and `/3`, and requires a
  non-empty string `kernel_release` for `/3` only.
- `scripts/media-evidence-reuse.py`: probe schema `media-evidence-reuse-ctime-probe/3` adds
  the same field from the same helper. `/2` keeps `PROBE_SCHEMA_V2`. `validate_probe` keeps
  the `/2` checks for `/2` and `/3`, and requires `kernel_release` for `/3` only.
- `/1` and `/2` records stay valid as written; no committed record is changed and no
  measurement is re-run. A `/3` record is written by the next oracle or probe run.
- The OS name is already `host.os`; it is not repeated. `host` is shared with the baseline
  harness and keeps its keys.
- `docs/media-evidence-protocol.md` record-format section gains one paragraph naming both
  `/3` schemas and the field.
- No ownership transition: both validators keep their structure and gain one check each.

### Failure model

1. Actors: an operator running `oracle` or `ctime-probe`; `just` selftests on Linux and macOS
   CI.
2. Invariants: every committed `/1` and `/2` record validates unchanged; a new record cannot
   validate without the kernel release; no host path or private value enters a record.
3. Accepted: `platform.release()` names the kernel of the host running the harness. For a
   network filesystem the server's kernel is not recorded; `storage_class` already marks it.

## Success

- A fresh oracle and probe record carry `/3` and a non-empty `parameters.kernel_release`, and
  validate.
- `/3` records lacking or with a non-string or empty `kernel_release` fail with
  `kernel_release`; `/1` and `/2` records without it validate.
- Every committed record under `docs/media-evidence-baseline/` validates unchanged.
- `just source-checks` exits 0.

## Validation

- focused-test: fingerprint selftest — the fresh oracle record is `/3` with the field; the
  validator rejects a `/3` record with it missing, `""`, or a non-string, and accepts `/2`
  without it. Red: dropping the `/3` check lets the mutants validate.
- focused-test: reuse selftest — the same for the probe record, plus the committed-record
  tests select `/2` records through the `_V2` constants.
- guardrail: `just source-checks`.
