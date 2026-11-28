# Implementation plan — #916

Expected implementation size: 180–300 changed lines (M).
Basis: baseline selector ~25; five consumer migrations ~40; regression fixtures and
cases ~130; protocol clarification ~20. Design artifacts excluded.
Branch: feat/network-physical-bytes-916. BASE_BRANCH: main; review base origin/main.
Guardrails: focused recipes below; assembled `just source-checks`; final `just ci`.
No owned deferrals or review findings at design entry.

## Task 1: Shared physical-read selection

Modify scripts/media-evidence-baseline.py and scripts/media-evidence-baseline-selftest.py.
Interfaces: consumes existing corpus.filesystem_type, local read_bytes and HOST_SOURCES
counter mapping; exports physical_read_bytes(corpus: dict[str, Any], local_bytes: int | str,
counters: dict[str, Any]) -> int | str for Task 2. Existing imports baseline/UNAVAILABLE
and LOCAL_FILESYSTEMS are confirmed in source.
Verification — Mode: focused-test. Add PhysicalReadTests source matrix including every
NETWORK_FILESYSTEMS type, local ext4, missing/unknown type, zero and unavailable counters,
positive unrelated CIFS traffic, array-valued metadata, stderr cause and preservation
of input mappings.
Red: helper absent or unsupported type returns numeric traffic. Green:
`just media-evidence-baseline-selftest`, expected all tests pass.

Implementation after red proof:

```python
def physical_read_bytes(corpus: dict[str, Any], local_bytes: int | str,
                        counters: dict[str, Any]) -> int | str:
    fs_type = corpus.get("filesystem_type")
    if isinstance(fs_type, str) and fs_type in LOCAL_FILESYSTEMS:
        return local_bytes
    if isinstance(fs_type, str) and fs_type in {"cifs", "smb3"}:
        return counters.get("cifs_bytes_read", UNAVAILABLE)
    print("media-evidence: physical read bytes unavailable: "
          "no supported counter for the corpus filesystem", file=sys.stderr)
    return UNAVAILABLE
```

Update host_counters docstring to distinguish raw diagnostic counters from selected
physical reads. Do not alter its capture or diagnostics. Criterion: unsupported filesystems
never consume CIFS traffic. No cleanup or state mutation.

## Task 2: Migrate physical-read consumers

Modify scripts/media-evidence-{candidates,lazy,rollout,observe,copy}.py and sibling
*-selftest.py files. All except candidates already import baseline. Add importlib.util to candidates
and load the sibling baseline module using lazy's existing relative-__file__ pattern:

```python
_spec = importlib.util.spec_from_file_location(
    "media_evidence_baseline", Path(__file__).with_name("media-evidence-baseline.py")
)
baseline = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(baseline)
```
Interfaces: consumes Task 1 helper and each record's corpus; preserve repetition_metrics
output names. candidates.physical_bytes(record, repetition) and rollout.scan_metrics(record,
repetition) keep signatures. lazy.baseline_physical and lazy.lazy_physical take corpus mapping
instead of network bool; lazy.compare passes each record's own corpus. observe/copy
repetition_metrics take (record, repetition); series supplies record for every repetition.
Verification — Mode: focused-test. Add unsupported-network tests with positive CIFS traffic
for candidates physical_bytes/series, lazy.compare, rollout.scan_metrics/compare_scans,
observe.series and copy.series. Include as-found repetition 1 and local/CIFS controls.
Add an asymmetric lazy as-found pair (CIFS candidate, NFS reference): first reads
are numeric/unavailable respectively, aggregate inconclusive with unavailable reason.
For other consumers assert the existing unavailable verdict, preserving timing metrics.
Rollout has no repetition-1 output; preserve its skip and test compared repetitions.
For copy assert destination physical_write_bytes remains the process value and raw mappings
are unchanged. Red: numeric physical reads on unsupported network; green: all five commands:
`just media-evidence-candidates-selftest media-evidence-lazy-selftest media-evidence-rollout-selftest media-evidence-observe-selftest media-evidence-copy-selftest`.

Complete replacement expressions (retain existing unavailability handling for local totals):

```python
# candidates.physical_bytes: remove storage_class-only branch, retain values collection.
return baseline.physical_read_bytes(record["corpus"],
    UNAVAILABLE if UNAVAILABLE in values else sum(values), repetition["host_counters"])
# rollout.scan_metrics: retain local worker values; remove network override.
"physical_bytes": baseline.physical_read_bytes(record["corpus"],
    UNAVAILABLE if UNAVAILABLE in physical else sum(physical), repetition["host_counters"]),
# lazy.baseline_physical: retain worker values aggregation, remove network branch.
return baseline.physical_read_bytes(corpus,
    sum(values) if all(isinstance(value, int) for value in values) else UNAVAILABLE,
    repetition["host_counters"])
# lazy.lazy_physical:
return baseline.physical_read_bytes(corpus, repetition["process"]["read_bytes"],
                                    repetition["host_counters"])
# observe.repetition_metrics physical_bytes and copy.repetition_metrics physical_read_bytes:
baseline.physical_read_bytes(record["corpus"], process["read_bytes"],
                             repetition["host_counters"])
```

Update direct call sites in each file, including first repetition and lazy baseline/candidate
sides; remove obsolete network bool. Add filesystem_type ext4 to synthetic local fixtures
that previously omitted the required recorded field. Criterion: every derived physical-read
consumer follows recorded filesystem type. Leave raw process/worker/host records and copy
physical_write_bytes unchanged. No compatibility wrapper: these are script-internal helpers.

## Task 3: Protocol and assembled verification

Modify docs/media-evidence-protocol.md physical-byte row: local process/worker read_bytes;
cifs/smb3 CIFS Stats; all other network types unavailable because no supported corpus counter.
Explain network_rx_bytes is host traffic diagnostic, never a physical-read substitute, and
historical numeric CIFS fields on non-CIFS records stay raw diagnostics. Clarify observe/copy
physical-read comparison selection while preserving destination process-write metrics.
State that an unavailable physical-read measurement leaves the physical budget unverified;
keep the threshold and logical scheduling limits unchanged.
Verification — Mode: task-test-not-applicable for prose: no executable consumer parses the
protocol wording; review its counter mapping against Task 1 and each Task 2 metric.
Run `just source-checks` for assembled integration (all guards/selftests pass), then final
`just ci` before pushing (all CI checks passed). Revert the branch commits to roll back.
