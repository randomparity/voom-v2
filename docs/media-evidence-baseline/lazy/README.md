# Lazy full-hash lifecycle prototype (#797)

Captured on 2026-10-29 under the frozen [protocol](../../media-evidence-protocol.md), stage 4
(deferred full evidence) and oracle cases O13–O15. The design is
[the #797 spec](../../workflow/specs/2026-10-29-lazy-hash-lifecycle-design.md). These are
measurements of one host and one experiment-only prototype, not a decision; #791 owns the
decision. The raw records are the JSON files beside this note, and every number below comes
from them.

## What ran

`scripts/media-evidence-lazy.py` keeps one record per file in a scratch state file, replaced
atomically after each transition:

| State | Entered when |
|---|---|
| `unknown` | no observation yet |
| `provisional` | `observe` fingerprinted an unknown, stale or disagreeing file |
| `verified` | `verify` read the whole file, the stat did not drift, and the head/tail window recomputed from that read equals the provisional one |
| `stale` | `observe` saw a changed fingerprint, or `verify` saw a changed or drifting stat |
| `disagreement` | `verify` found the recomputed head/tail window unlike the provisional one |

The fingerprint is size, mtime ns, inode key, and a digest of the first and last 64 KiB. It is
a fixed instrument, not a selection: no stage 1–3 selection existed when this ran, and #796
owns the fingerprint search. Plans made on provisional evidence are `held`, become
`committable` only on `verified`, and are `revoked` (outputs `quarantined`) on `stale` or
`disagreement`. Digests are `exp-blake2b:` values over 1 MiB reads. They stay in the scratch
state file, which is deleted after each run; no record, store, worker, lease or production
hash field receives one.

`run` measures one lifecycle per repetition, 4 files in flight: observe every file, observe it
twice more as the probe's pre- and post-probe re-observations would under this candidate, then
verify every file. Eventual logical bytes are reported by component. Physical bytes are the
process's `read_bytes` locally and the host CIFS `Bytes read` delta on the network.

## Recovery matrix

Fixtures are 4 MiB synthetic files in a scratch directory the run removes. An oracle knows the
true content; its reads are not counted. Every case matched its stated expectation
([record](2026-10-29-lazy-matrix.json)).

| Case | Construction | State | Oracle | Plan / output | Wasted bytes |
|---|---|---|---|---|---:|
| clean | observe, plan, verify | verified | no change | committable / kept | 0 |
| restart (O14) | child verifier SIGKILLs itself at 50%; a fresh reader loads the state file | provisional, no digest; resumed verify → verified | no change | held / kept | 2 MiB |
| cancel | cooperative stop at 25% | provisional | no change | held / kept | 1 MiB |
| retry | first read fails with EIO at 50%, second attempt | verified | no change | committable / kept | 2 MiB |
| retry exhausted | both attempts fail at 25% | provisional | no change | held / kept | 2 MiB |
| concurrent mutation (O10) | a byte appended mid-read | stale | detected | revoked / quarantined | 4 MiB + 1 |
| unseen interior edit during read | interior bytes overwritten mid-read, mtime restored | verified | **missed** | committable / kept | 0 |
| late disagreement, head (O15) | head edit after planning, mtime restored | disagreement | detected | revoked / quarantined | 4 MiB |
| late disagreement, interior (O15) | interior edit after planning, mtime restored | verified | **missed** | committable / kept | 0 |
| stale evidence, head (O13) | verified, then head edit with mtime restored, then re-scan | stale | detected | revoked / quarantined | 0 |
| stale evidence, interior (O13) | verified, then interior edit with mtime restored, then re-scan | verified | **missed** | committable / kept | 0 |

Starvation is a 32-tick simulation with no I/O: 8 background files, a new on-demand file
every tick, one verification per tick. Serving on-demand work only leaves all 8 background
files provisional. Serving the oldest background file every 4th tick verifies all 8, at the
cost of an on-demand backlog of 8.

The three misses are **constructed misses**: a same-size edit outside the head/tail windows
with mtime restored is invisible to this fingerprint and to the stat, so verification hashes
the edited bytes and calls them verified. A plan made on the earlier bytes becomes
committable. Under current behavior the full hash precedes planning, so the same edit after
observation is caught only by transform re-observation (O11). A constructed miss shows the
candidate *can* miss; it is not a rate. The corpus runs observed no mutation at all.

## Eventual I/O

| Cell | Rep | Wall (s) | All provisional (s) | First verified (s) | Logical read (GB) | Physical read (GB) | CPU (s) | Peak RSS (MiB) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| local, evicted, 34 files (935.3 GB) | 1 | 336.0 | 0.029 | 8.9 | 935.31 | 935.23 | 1315.9 | 34.4 |
| | 2 | 335.8 | 0.028 | 8.6 | 935.31 | 935.23 | 1315.2 | 34.5 |
| | 3 | 335.2 | 0.028 | 8.6 | 935.31 | 935.23 | 1312.5 | 34.5 |
| network, as-found, 3 files (24.7 GB) | 1 | 23.5 | 0.013 | 15.0 | 24.66 | 24.66 (host CIFS) | 35.8 | 34.0 |
| | 2 | 16.2 | 0.009 | 10.0 | 24.66 | 13.06 (host CIFS) | 35.2 | 34.0 |
| | 3 | 16.2 | 0.011 | 10.0 | 24.66 | 13.06 (host CIFS) | 35.2 | 34.2 |

Logical bytes by component, identical in every repetition of a cell:

| Cell | Owner fingerprint | Probe re-observations | Deferred full hash | Wasted |
|---|---:|---:|---:|---:|
| local | 4,456,448 | 8,912,896 | 935,300,661,466 | 0 |
| network | 393,216 | 786,432 | 24,662,518,120 | 0 |

Every file ended `verified` in every repetition, with no failure, and the corpus stat facts
were unchanged after each run. Every local repetition started with 0 resident bytes for every
file, verified by `fincore`. In network repetitions 2 and 3 the largest file (11.6 GB) was
resident in the client page cache.

Comparison with #788 under the protocol thresholds (`compare` output):

| Cell | Metric | #788 median | Lazy median | Change | Verdict |
|---|---|---:|---:|---:|---|
| local, evicted (reps 1–3) | logical bytes | 2805.90 GB | 935.31 GB | −66.7% | improvement |
| | physical bytes | 2765.79 GB | 935.23 GB | −66.2% | improvement |
| network, as-found (reps 2–3) | logical bytes | 73.99 GB | 24.66 GB | −66.7% | improvement |
| | physical bytes | 9.46 GB (CV 53.8%) | 13.06 GB | +38.0% | inconclusive |

Network repetition 1, reported separately: 24.66 GB of host CIFS reads for both #788 and this
candidate, exactly the subset size.

What the numbers mean:

- **The saving comes from replacing the probe's two full re-reads with fingerprint
  comparisons, not from deferral.** Deferral moves the one full read later; it does not remove
  it. If the probe kept re-hashing, eventual logical bytes would be #788's three full reads
  plus the fingerprint bytes, slightly more than today.
- **On the network corpus the logical saving did not reach the wire.** #788's probe re-reads
  were already served by the client page cache, so repetition 1 fetched the subset once under
  both. Repetitions 2–3 are inconclusive, because the baseline varies with what the client
  cache kept.
- **Wasted speculative work was zero on both corpora**, because nothing changed during the
  runs. The matrix bounds the waste per fault: up to half a file for an interrupted or retried
  verification as constructed here, and one whole file for each drift or disagreement, before
  the file is observed and verified again.
- **Provisional evidence existed for every file within 0.03 s**, against #788's first
  hash-and-probe completion at 9.9–10.4 s locally. Neither publishes, so first durable
  observation stays inconclusive under the protocol: no stage-0 cell exists with a publishing
  instrument.
- Wall time, CPU and memory are reported, not compared. This prototype hashes BLAKE2b in one
  Python process with 1 MiB reads, while #788 ran the release Rust workers with 8 KiB BLAKE3
  reads. CPU covers every thread of the harness process; peak RSS is the process-lifetime
  peak.

## Exclusions

- Buffer size, concurrency, flush and scheduling tuning: #789. The run uses 4 in flight and
  1 MiB reads as fixed instrument choices.
- Fingerprint candidates and the O1–O12 oracle cases: #796.
- Evidence reuse across scans, storage-version evidence and the consolidated report: #798.
  The O13 cases re-scan a lifecycle record; they do not evaluate a reuse cache.
- The decision, production lifecycle, consumers, rollout and SLOs: #791, #792, #793, #794,
  #471. Storage relocation: #413/#528.
- No ticket, lease, absence-reconciliation, store or worker code runs or changes. ADR 0050
  byte ownership is untouched.
- Not measured, recorded in each run as `not_measured`: discovery, first durable observation,
  first actionable plan, first transformation start, first verified output, complete
  library, sidecar hashing, ffprobe's own reads, and retries.

## Commands

```sh
python3 scripts/media-evidence-lazy.py matrix \
  --output docs/media-evidence-baseline/lazy/2026-10-29-lazy-matrix.json
python3 scripts/media-evidence-lazy.py run <private-dir>/local-manifest.json \
  --max-bytes 3000000000000 --repetitions 3 --evict \
  --output docs/media-evidence-baseline/lazy/2026-10-29-lazy-local-corpus-evicted.json
python3 scripts/media-evidence-lazy.py run <private-dir>/network-manifest.json \
  --ids f0049,f0074,f0063 --max-bytes 80000000000 --repetitions 3 \
  --output docs/media-evidence-baseline/lazy/2026-10-29-lazy-network-corpus-as-found.json
python3 scripts/media-evidence-lazy.py compare \
  docs/media-evidence-baseline/lazy/2026-10-29-lazy-local-corpus-evicted.json \
  docs/media-evidence-baseline/2026-10-29-local-corpus-evicted.json
python3 scripts/media-evidence-lazy.py compare \
  docs/media-evidence-baseline/lazy/2026-10-29-lazy-network-corpus-as-found.json \
  docs/media-evidence-baseline/2026-10-29-network-corpus-as-found.json
```

The private manifests are #788's. Before the runs, a fresh listing of both corpora matched
them file for file, so manifest ids compare. `just media-evidence-lazy-selftest` validates
every record here and runs the matrix on any CI host.

## Disclosures

- Every corpus run held an exclusive host-wide lock shared with the sibling experiments
  (#789, #796), so no other corpus measurement ran during it. Other host activity was not
  controlled or recorded.
- The network corpus is shared with an active rehearsal. This candidate's load was about
  50.8 GB of CIFS reads at concurrency 4 over about one minute (21:22–21:23 UTC), within the
  100 GB cell budget. Host counters include any rehearsal traffic in that window. Repetition 1
  found the client cache cold (0 resident bytes for every file), but the same files were read
  earlier the same day by #788 and other experiments, and the server's cache is not
  observable, so repetition 1 is not a first touch on the server.
- The local capture ran 20:27–20:44 UTC. An earlier local capture (19:39–19:56 UTC) was
  discarded and not committed: a harness defect recorded process CPU as `unavailable`. The
  defect is fixed and covered by the self-test. Its logical byte counts equalled the committed run's.
- Local physical reads were about 70 MB (0.007%) below the deferred full-hash bytes in each
  repetition, although every file was evicted and verified non-resident first. The cause
  is not established, and no claim is made about the difference.
