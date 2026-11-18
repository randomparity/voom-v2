# ext4/xfs reuse oracle and network ChangeTime — plan (#840)

Goal: implement [the design](../specs/2026-11-17-reuse-fs-evidence-840-design.md): oracle schema
`/2` (tick wait, coarse-tick witness, metadata O10 guard window), probe schema `/2`
(`client_change_time`), then capture and document ext4, xfs and network evidence.

Architecture: two experiment scripts gain a schema version each; prior versions keep validating.
Records go under `docs/media-evidence-baseline/reuse/`. No production crate changes.

Tech stack: Python 3 standard library only; `unittest` selftests run by `just`.

Expected implementation size: 230–300 changed lines (M) — two script edits (~70 lines each),
selftest additions (~70), README and report prose (~60), excluding generated JSON records.

## Global Constraints

- No new dependency. Selftests must pass on Linux and macOS CI without root, loop devices,
  `/proc` or a network share (`time.CLOCK_REALTIME_COARSE` is absent on macOS).
- Committed records carry no path, timestamp, inode, host or share name (`privacy_problems`).
- Every existing record validates unchanged; historical records are not rewritten.
- sudo only for `losetup`, `mkfs.ext4`, `mkfs.xfs`, `mount`, `umount`, `losetup -d`, and `chown`
  of the created mount roots. Network writes only inside one fresh directory under the designated
  scratch share, removed afterwards. Never touch the media share.
- Guardrails: `just media-evidence-fingerprint-selftest`, `just media-evidence-reuse-selftest`,
  `just check-media-evidence-fingerprint-guard`, `just source-checks`, `just ci` before push.
- Lines ≤ 100 chars; functions ≤ 100 lines.

## File map

| File | Change |
|---|---|
| `scripts/media-evidence-fingerprint.py` | oracle `/2`: `--tick-wait`, `same_coarse_tick`, metadata O10 window, `/1`+`/2` validation |
| `scripts/media-evidence-fingerprint-selftest.py` | `/2` behaviour tests; committed-record filter accepts both versions |
| `scripts/media-evidence-reuse.py` | probe `/2`: `client_change_time`, new `not_measured`, `/1`+`/2` validation |
| `scripts/media-evidence-reuse-selftest.py` | `/2` tests; committed-record coverage for new records |
| `docs/media-evidence-baseline/reuse/*.json` | five new records (Task 3) |
| `docs/media-evidence-baseline/reuse/README.md`, `docs/media-evidence-decision-report.md` | results and verdicts |

## Task 1 — oracle schema `/2`

Files: `scripts/media-evidence-fingerprint.py`, `scripts/media-evidence-fingerprint-selftest.py`.

Interfaces (later tasks rely on): `ORACLE_SCHEMA_V1`, `ORACLE_SCHEMA` (`/2`), `ORACLE_SCHEMAS`,
`run_oracle(scratch_parent, candidates=None, tick_wait=CLOCK_TICK_SECONDS)`,
`validate_oracle(record)`, CLI `oracle --tick-wait SECONDS`.

Verification:
- Contract: `/2` row carries `same_coarse_tick` and parameters carry `tick_wait_seconds`. Mode:
  focused-test — `OracleTests.test_v2_rows_carry_tick_witness`; red: `KeyError`/assert on the
  missing key; green: `python3 scripts/media-evidence-fingerprint-selftest.py`.
- Contract: metadata O10 is constructed; `meta` misses, `meta-ctime` detects. Mode: focused-test
  — `test_metadata_o10_is_the_guard_window`; red: outcome `not-applicable`.
- Contract: `/1` records still validate; `/2` rejects a missing or non-bool witness and a
  negative wait. Mode: focused-test — `test_validator_versions`; red: `schema` problem on `/1`.

Steps:
1. Add the three tests to `OracleTests` (setUpClass gains a second record:
   `run_oracle(parent, candidates=["meta", "meta-ctime"], tick_wait=0)`). Assertions: every row has
   `same_coarse_tick` in `{True, False, None}`; when `sys.platform.startswith("linux")`,
   `experiment.COARSE_CLOCK == 5` and every `meta-ctime` row's witness is a bool, else
   `COARSE_CLOCK is None` and every witness is `None`;
   `parameters["tick_wait_seconds"] == 0`; zero-wait outcome `("O10","meta") == "missed"` (the
   control holds on any kernel). Zero-wait `meta-ctime` detection is not asserted: CI kernels
   before 6.13 stamp ctime from the coarse clock and may legitimately miss. The default-wait
   record (`cls.record` from `run_oracle(parent, candidates=list(REUSE_CANDIDATES))`) asserts
   `("O10","meta-ctime") == "detected"` beside the existing O1/O12/O13 checks. Validator: a copy with
   `schema` set to `ORACLE_SCHEMA_V1` and the witness/param removed validates `[]`; a `/2` copy
   with one row's witness set to `"yes"` reports `"case same_coarse_tick"`; with the key deleted
   reports `"case same_coarse_tick"`; with `tick_wait_seconds = -1` reports `"tick_wait_seconds"`.
   Update existing asserts that `O10`/`meta` and `O10`/`meta-ctime` are `not-applicable` to
   `missed` and `detected`. Run; expect failures.
2. Implement in the script:
   ```python
   ORACLE_SCHEMA_V1 = "media-evidence-fingerprint-oracle/1"
   ORACLE_SCHEMA = "media-evidence-fingerprint-oracle/2"
   ORACLE_SCHEMAS = {ORACLE_SCHEMA_V1, ORACLE_SCHEMA}
   # Linux CLOCK_REALTIME_COARSE (linux/time.h id 5), the clock coarse ctime stamps come from;
   # Python's time module does not export the name.
   COARSE_CLOCK = 5 if sys.platform.startswith("linux") else None

   def coarse_now() -> int | None:
       return None if COARSE_CLOCK is None else time.clock_gettime_ns(COARSE_CLOCK)
   ```
   O10 construction text appends: `"; for meta and meta-ctime, a same-size edit with mtime
   restored while a reader holds the file, between the stat before and the stat after"`.
   `Oracle.__init__(self, scratch, large, tick_wait)` stores `tick_wait`; `run` sets
   `self.same_tick = None` before the case and adds `"same_coarse_tick": self.same_tick` to the
   row. New method:
   ```python
   def note_tick(self, start: int | None) -> None:
       self.same_tick = None if start is None else start == coarse_now()
   ```
   New `prime(path)`: `start = coarse_now()`; `set_mtime(path, os.stat(path).st_mtime_ns)`
   (stamps ctime, keeps mtime and bytes); return `start`.
   `compare`: `reference = reference_digest(path)`; `start = self.prime(path)`;
   `before = self.fingerprint(...)`; `time.sleep(self.tick_wait)`; `after_path = change(path)`;
   `self.note_tick(start)`; rest unchanged. `stale_cache`: digest first, then `start = self.prime(path)`, cache
   fingerprint, sleep `tick_wait`, edit, `note_tick(start)`; require `current != reference`.
   `change_during_observation`: metadata candidates call a new `guard_window(directory,
   candidate)` that builds the large fixture, picks the interior first-unread offset, and runs
   `compare` with a change that opens the file, reads `SAMPLE_BYTES`, then
   `edit_and_return(target, offset)` while the handle is open.
   `run_oracle(..., tick_wait=CLOCK_TICK_SECONDS)` passes it on and records
   `"tick_wait_seconds": tick_wait`. CLI: `--tick-wait` with a type that rejects negatives
   (`argparse.ArgumentTypeError("--tick-wait must be >= 0")`).
   `validate_oracle`: accept `ORACLE_SCHEMAS`; when `/2`, require `tick_wait_seconds` a non-bool
   number ≥ 0 (problem `"tick_wait_seconds"`) and each row's `same_coarse_tick` present and in
   `(True, False, None)` (problem `"case same_coarse_tick"`).
3. Committed-record tests: the fingerprint selftest filters on `record["schema"] in
   experiment.ORACLE_SCHEMAS`; the reuse selftest's `validators` map gains
   `fingerprint.ORACLE_SCHEMA_V1: fingerprint.validate_oracle` and its `oracles` list reads
   `by_schema[fingerprint.ORACLE_SCHEMA_V1]` (the committed tmpfs/btrfs records), so both stay
   green after this task. Run `python3 scripts/media-evidence-reuse-selftest.py` too.
4. Run `python3 scripts/media-evidence-fingerprint-selftest.py` — expect `OK`; commit
   `feat(media-evidence): oracle schema 2 with tick witness and guard-window O10 (#840)`.

## Task 2 — probe schema `/2`

Files: `scripts/media-evidence-reuse.py`, `scripts/media-evidence-reuse-selftest.py`.

Interfaces: `PROBE_SCHEMA_V1`, `PROBE_SCHEMA` (`/2`), `classify_client_change_time(ctime_ns,
requested_mtime_ns, sent_from_ns, sent_to_ns) -> str`, `validate_probe(record)`.

Verification:
- Contract: classification. Mode: focused-test — `ClientChangeTimeTests.test_classification`
  (ctime == requested → `follows-mtime`; inside bracket ± 2 s →
  `client-time-or-server-stamp`; 10 s outside → `other`); red: `AttributeError`.
- Contract: local filesystems record `not-applicable`; network storage runs the case. Mode:
  focused-test — existing `ProbeTests` asserts `not-applicable`; a new test patches
  `reuse.baseline.storage_class` to return `"network"` and expects `client-time-or-server-stamp`
  on the local temp dir (a local kernel stamps its own time) and a record that validates.
- Contract: `/1` still validates; `/2` rejects a network filesystem with `not-applicable`, a
  local one with `follows-mtime`, and the old `not_measured` keys. Mode: focused-test —
  `test_probe_validator_versions`.

Steps:
1. Write the tests; run `python3 scripts/media-evidence-reuse-selftest.py`; expect failures.
2. Implement:
   ```python
   PROBE_SCHEMA_V1 = "media-evidence-reuse-ctime-probe/1"
   PROBE_SCHEMA = "media-evidence-reuse-ctime-probe/2"
   NOT_MEASURED_V1 = {<the current NOT_MEASURED dict, unchanged>}
   NOT_MEASURED = {
       "server_side_change": NOT_MEASURED_V1["server_side_change"],
       "client_set_chosen_change_time": "the kernel SMB client sends only its current time as "
           "ChangeTime; a chosen value needs an authenticated SMB client this host lacks",
   }
   CLIENT_CHANGE_TIMES = {"follows-mtime", "client-time-or-server-stamp", "other",
                          "not-applicable"}
   CLOCK_SKEW_NS = 2_000_000_000

   def classify_client_change_time(ctime_ns, requested_mtime_ns, sent_from_ns, sent_to_ns):
       if ctime_ns == requested_mtime_ns:
           return "follows-mtime"
       if sent_from_ns - CLOCK_SKEW_NS <= ctime_ns <= sent_to_ns + CLOCK_SKEW_NS:
           return "client-time-or-server-stamp"
       return "other"

   def client_change_time(path: Path, settle: float) -> str:
       """utime() through a kernel SMB client also sends ChangeTime = the client's time."""
       sent_from = time.time_ns()
       fingerprint.set_mtime(path, fingerprint.BUMPED_MTIME_NS)
       sent_to = time.time_ns()
       time.sleep(settle)
       return classify_client_change_time(os.stat(path).st_ctime_ns,
                                          fingerprint.BUMPED_MTIME_NS, sent_from, sent_to)
   ```
   `probe_filesystem`: after the steps loop and `fs_type`, set `change_time =
   client_change_time(path, settle)` when `baseline.storage_class(fs_type) == "network"`, else
   `"not-applicable"`; add `"client_change_time": change_time` to the result. `run_ctime_probe`
   writes `PROBE_SCHEMA` and `NOT_MEASURED`. `validate_probe`: pick schema, `not_measured` keys and
   the filesystem key set by version; for `/2` require `client_change_time` in
   `CLIENT_CHANGE_TIMES` and `(value == "not-applicable") == (storage_class != "network")`
   (problem `"filesystem client_change_time"`). Update the module docstring. In the reuse
   selftest's committed-record test add `reuse.PROBE_SCHEMA_V1: reuse.validate_probe` and read
   probes from `by_schema[reuse.PROBE_SCHEMA_V1]`, so it stays green before Task 3.
3. Run the selftest — expect `OK`; commit
   `feat(media-evidence): probe schema 2 records client ChangeTime (#840)`.

## Task 3 — capture and document

Files: five records in `docs/media-evidence-baseline/reuse/`, its `README.md`,
`docs/media-evidence-decision-report.md`, both selftests' committed-record tests.

Verification:
- Contract: committed records validate and cover ext4, xfs (both waits) and the network probe.
  Mode: focused-test — reuse `CommittedRecordTests` (validators keyed by all four oracle/probe
  schemas; `/2` oracles cover `{ext4, xfs}` × `{0, 0.02}` with no `invalid` row and
  `meta` missing O1 and O10; `meta-ctime` outcomes are not asserted (they are the finding); the `/2` probe covers ext4, xfs and one `network`
  filesystem); red before the records exist: missing schema key.
- Contract: README and report prose. Mode: task-test-not-applicable — human-readable evidence
  summary with no executable consumer.

Steps (host exclusive; no builds or tests while capturing). All provisioning and cleanup runs
from one private shell script in the session scratchpad with `trap cleanup EXIT`; `cleanup`
unmounts what it mounted, detaches the loop devices it attached, deletes its images and removes
its network directory, so a failed capture step still leaves nothing behind.
1. Commit Tasks 1–2 so records name a clean commit. `S=<session scratchpad>/q840`.
   `truncate -s 1G $S/ext4.img $S/xfs.img`; `sudo mkfs.ext4 -q $S/ext4.img` (use a loop
   device from `sudo losetup --find --show`); `sudo mkfs.xfs -q /dev/loopN`; mount each on
   `$S/mnt-ext4`, `$S/mnt-xfs`; `sudo chown $USER: <mount roots>`. Record inode size from
   `dumpe2fs -h $S/ext4.img` (user-owned image, no sudo) and `xfs_info <mount>` for the README.
2. For each fs and wait in (0.02, 0): `python3 scripts/media-evidence-fingerprint.py oracle
   --candidates meta,meta-ctime --tick-wait <w> --scratch-parent $S/mnt-<fs> --output
   docs/media-evidence-baseline/reuse/2026-11-17-reuse-oracle-<fs>[-no-wait].json`.
3. `mkdir` a fresh `q840-<random>` directory under the network scratch share.
   `python3 scripts/media-evidence-reuse.py ctime-probe --manifest <private local manifest>
   --manifest <private network manifest> --scratch-parent $S/mnt-ext4 --scratch-parent
   $S/mnt-xfs --scratch-parent <fresh share dir> --output
   docs/media-evidence-baseline/reuse/2026-11-17-reuse-ctime-probe-ext4-xfs-network.json`.
   Remove the fresh share directory; list its parent to confirm.
4. Cleanup: `sudo umount` both, `sudo losetup -d` both, delete images; confirm with
   `losetup -a`, `findmnt`, `ls $S`.
5. Update committed-record tests; run both selftests — expect `OK`.
6. README: new section with commands, mkfs defaults, kernel, results tables, count of
   `same_coarse_tick: true` rows, ChangeTime method and source citation, share disclosure.
   A zero-wait record with no `same_coarse_tick: true` row is written up as "same-tick not
   constructed" for that filesystem. Decision report: replace the "ctime behaviour was measured on btrfs and tmpfs only" and
   "Not measured in #790 … ChangeTime" lines with the new results and a per-filesystem
   ADR 0120 O1/O10 verdict that names the instrument (oracle stat model) and states #794's
   rollout guard cases were not run on ext4/xfs.
7. Commit records and docs separately; push.
