# Media evidence: consolidated #790 report

Inputs for the phase decision [#791](https://github.com/randomparity/voom-v2/issues/791),
merging the three #790 experiments under the frozen
[protocol](media-evidence-protocol.md). This report decides nothing. It changes no production
trust: no experiment value entered a store, worker, lease or production hash field, and ADR
0050 byte ownership, ticket and lease authority, and absence reconciliation are untouched.
A no-go is a valid result.

| Experiment | Issue | Detail and records |
|---|---|---|
| Baseline (stage 0) | [#788](https://github.com/randomparity/voom-v2/issues/788) | [media-evidence-baseline/README.md](media-evidence-baseline/README.md) |
| Partial fingerprints (stage 3) | [#796](https://github.com/randomparity/voom-v2/issues/796) | [fingerprint/README.md](media-evidence-baseline/fingerprint/README.md) |
| Lazy full-hash lifecycle (stage 4) | [#797](https://github.com/randomparity/voom-v2/issues/797) | [lazy/README.md](media-evidence-baseline/lazy/README.md) |
| Change time, random sampling, reuse | [#798](https://github.com/randomparity/voom-v2/issues/798) | [reuse/README.md](media-evidence-baseline/reuse/README.md) |

Every figure below comes from those records or READMEs. All are one host's measurements, on
a local btrfs corpus (34 files, 935.3 GB) and a three-file subset (24.7 GB) of a CIFS corpus
shared with an active rehearsal.

## Summary for #791

1. **No candidate substitutes for the full content hash.** Every sampling candidate misses a
   same-size edit in bytes it does not read (constructed misses), and every metadata
   candidate misses a change that leaves its fields intact.
2. **The one large saving is reuse, not cheaper fingerprints.** Today each file is read three
   times: the owner hash and the probe's pre- and post-ffprobe hashes. Replacing the probe's
   two re-hashes with a metadata comparison cuts logical bytes by 66.7% on both corpora
   (#797, and the same arithmetic on #788's records below). The fingerprint itself costs
   0–76 MB per local pass, against 2,805.9 GB.
3. **Change time closes the O1 construction only on local storage.** On tmpfs and btrfs,
   `meta-ctime` detected every mtime-restored edit (O1, O13), because the `utime()` that
   restores mtime moves ctime and an unprivileged writer cannot set ctime. On the network
   mount the reported ctime follows the mtime, so the same edit is missed there.
4. **Seeded-random sampling detects an interior edit with probability equal to the fraction
   sampled.** At corpus scale that is under 0.04% per check. Its benefit is that an editor
   without the seed cannot aim at unread bytes, not detection power.
5. **Silent corruption is undetectable by every candidate except a full read.** Only periodic
   full verification (a scrub) catches it.

## Candidates and cost

Logical bytes per local, evicted pass over 34 files; network is the 3-file subset.

| Candidate | Source | Reads | Local logical | Network logical |
|---|---|---|---:|---:|
| Full hash, three reads (current) | #788 | whole file ×3 | 2,805.9 GB | 74.0 GB |
| Owner hash once, probe reuses it | #797, #798 | whole file ×1 + metadata | 935.3 GB | 24.7 GB |
| `meta` | #796 | stat | 0 | 0 |
| `meta-ctime` | #798 | stat | 0 | 0 |
| `ht64` | #796 | head and tail 64 KiB | 4.46 MB | 0.39 MB |
| `ht1m` | #796 | head and tail 1 MiB | 71.30 MB | 6.29 MB |
| `ht64-i8` / `rnd64-i8` | #796 / #798 | `ht64` + 8 interior 64 KiB blocks | 22.28 MB | 1.97 MB |
| `ht64-i32` / `rnd64-i32` | #796 / #798 | `ht64` + 32 interior 64 KiB blocks | 75.76 MB | 6.68 MB |

- Fingerprint wall times were 1–116 ms per local pass, against 613.9 s for #788's three full
  reads. The instruments differ (Python in-process versus release workers over HTTP); compare
  orders of magnitude only.
- `rnd64` additionally needs a per-block digest table built by one full read: 457 MB for the
  local corpus, 12 MB for the network subset.
- #797's lazy lifecycle defers the one full read without removing it. Its eventual logical
  bytes equal one full read plus fingerprints, with zero wasted speculative work observed.
- On the network subset, #788's probe re-reads were already served by the client cache, so
  the logical saving did not reach the wire (#797: repetition 1 fetched 24.7 GB under both).

## Constructed outcomes (mutation oracle)

Constructed misses show that a candidate *can* miss. They are not rates, and they are never
combined with the observed results in the next section. #796 ran O1–O13 on tmpfs; #798 ran its
candidates on tmpfs and btrfs with identical outcomes.

| Case | Full hash | `meta` | `meta-ctime` (local) | `ht64`, `ht1m`, `ht64-i8`, `ht64-i32` | `rnd64-i8`, `rnd64-i32` |
|---|---|---|---|---|---|
| O1 interior edit, mtime restored | detected | missed | detected | missed | missed (seed known) |
| O2 head edit, mtime restored | detected | missed | detected | detected | detected |
| O3 tail edit, mtime restored | detected | missed | detected | detected | detected |
| O4 short file | detected | missed | detected | detected | detected |
| O5 empty: append, truncate | equal | false-change | false-change | false-change | false-change |
| O6 edit in sample overlap | detected | missed | detected | detected | detected |
| O7 mtime differs only in ns | detected | detected | detected | detected | detected |
| O8 rename | equal | equal | false-change | equal | equal |
| O9 replacement | detected | detected | detected | detected | detected |
| O10 write between reads | drift rejected | not-applicable | not-applicable | detected | detected |
| O11 ordinary write before use | detected | detected | detected | detected | detected |
| O12 sidecar edit, mtime restored | detected | missed | detected | detected | detected |
| O13 stale cache, mtime restored | not applicable | missed | detected | missed | missed (seed known) |

`meta-ctime` on the network mount was not run through the oracle. Its probe showed the
reported ctime returning to its prior value after an mtime-restored write and after a `chmod`,
and not moving on a rename, so O1 and O13 there are **missed** and O8 is `equal`.

#797's lifecycle cases, with its head/tail fingerprint as the provisional evidence:

| Case | Outcome | Plan / output |
|---|---|---|
| O14 verifier killed mid-read, then resumed | provisional; verified on resume | held / kept |
| O15 late disagreement, head edit | detected | revoked / quarantined |
| O15 late disagreement, interior edit, mtime restored | **missed** | committable / kept |
| O13 stale evidence, interior edit, mtime restored | **missed** | committable / kept |
| O10 concurrent mutation | detected | revoked / quarantined |

## Observed results (finite corpora)

Kept apart from the oracle, as the protocol requires. In every #796 and #798 cell every file
gave the same fingerprint in all three repetitions, with no failure, and every candidate told
apart all 34 local and all 3 network files on size and sampled bytes. In #797 every file ended
verified in every repetition. No file changed during any run, so no mutation-detection rate
was observed, and a clean finite corpus is no general guarantee.

The detection probability of random sampling was measured on synthetic fixtures, 1000 trials
per cell, and matched the sampled fraction f = k/M within 3 standard errors (for example
0.273 observed against f = 0.254 for `rnd64-i32` on 8 MiB). n checks with fresh seeds detect
one edited block with probability 1 − (1 − f)^n. At the median local file, f = 7.0 × 10⁻⁵ for
`rnd64-i32`, so a 50% chance takes about 9,900 checks.

## Evidence-reuse candidates

| Candidate | Saving | Deployment assumption | Threat assumption |
|---|---|---|---|
| R1. The probe reuses the owner hash and compares metadata before and after ffprobe, instead of re-hashing twice | Local: 2,805.9 → 935.3 GB logical per pass (−66.7%); #788's probe workers fetched about 1.83 TB per pass. Network: 74.0 → 24.7 GB logical; the wire saving was not observed (cache) | The probe runs on the storage-owner node (ADR 0050), against the same file the owner hashed, shortly afterwards | A change in the gap is ordinary (moves mtime) or local-adversarial (moves ctime). On the network mount an mtime-restored edit in the gap is missed. Today's probe re-hash catches any change in the gap |
| R2. A repeat scan reuses cached full evidence keyed by file key, size, mtime ns and ctime ns | A repeat scan of unchanged files costs a stat instead of three full reads (0 logical bytes and about 1–3 ms for 34 files) | Cache keyed per storage node; a key mismatch, including a false change (rename, `chmod`, append-then-truncate), costs one full re-hash | Detects ordinary and local unprivileged edits. Misses the O13 construction on the network mount and any silent corruption, indefinitely |
| R3. Transform workers reuse verified input evidence instead of hashing the input before and after the operation | Two full input reads per transform; not measured (#788 does not replay transforms) | Transform and observation on the same node | As R1 for the operation's window |

Each reuse candidate trades a full re-read for a metadata guard, so its detection is exactly
the guard's. Where that guard is `meta-ctime`, local storage gains the O1 detection and the
network mount does not.

## Storage-version signals

What an unprivileged reader can obtain on these filesystems, measured by #798's probe:

| Signal | tmpfs | btrfs | Network mount | Use |
|---|---|---|---|---|
| ctime | moves on every write, `utime()`, `chmod`, rename | same | follows mtime; restored with it | change hint on local storage only |
| inode number | stable across writes, `chmod`, rename | same | same | detects replacement (O9) |
| inode generation | not exposed | unchanged by writes | not exposed | not a content version |
| content version or change counter | not exposed | not exposed | not exposed | none available |

Snapshot and immutable version evidence stay excluded, as the protocol froze them; #791 may
reopen that only where storage semantics justify it.

## Threat classes and residual risk

| Class | Example | Caught by |
|---|---|---|
| Ordinary tool edit | an editor or remuxer rewrites a file; mtime moves | every metadata candidate (O11) |
| Adversarial edit, local storage | a same-size write with mtime restored by `utime()` | `meta-ctime` (ctime cannot be set without root, the clock, or the raw device); missed by every other candidate with constructed misses |
| Adversarial edit, network storage | the same edit through the share | missed by every metadata and sampling candidate; whether a client-sent ChangeTime is honoured was not measured |
| Adversarial edit with seed knowledge | an editor who knows the sampling seed | missed by `rnd64` |
| Privileged forgery | root sets the clock or writes the block device | missed by every candidate except a full read |
| **Silent corruption** | media or controller damage with same size, mtime and ctime unchanged | **missed by every metadata and sampling candidate; caught only by a full read** |

Silent corruption is the residual class no cheaper-than-full-read candidate detects. Random
sampling catches it only at the rate f per check, and the full hash today catches it only when
a phase happens to re-read the file. If #791 adopts reuse, the full content hash is computed
once and then trusted until a metadata change, so silent corruption persists until a
**periodic full verification** (a scrub) re-reads the bytes. The scrub interval bounds how
long corrupted media can be trusted, and its I/O is one full read per file per interval.

## Inputs for #791

- Full content hash remains the only identity evidence that holds against every class above.
- Probe reuse of the owner hash (R1) is the measured saving, 66.7% of logical bytes, at the
  cost of the guard's detection in the gap between hashing and probing.
- `meta-ctime` is a stronger guard than `meta` on local storage and no stronger on the network
  mount. Any trust rule that depends on ctime must be per storage class.
- A cache of full evidence (R2) needs a scrub to bound the life of silent corruption.
- Random sampling does not beat reading the same fraction, and needs a stored block table.
- Not measured anywhere in #790: first durable observation (no publishing instrument), changes
  made on the server side or by another client, transform-input reuse, and scheduling and
  buffer effects (#789).
