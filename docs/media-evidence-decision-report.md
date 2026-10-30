# Media evidence: phase decision report (#791)

This report holds the comparison, phase table, rollout gates, implementation contracts and
re-scope drafts behind [ADR 0120](adr/0120-phase-specific-media-evidence.md), for epic
[#787](https://github.com/randomparity/voom-v2/issues/787). ADR 0120 is **Accepted**
(2026-10-30); the selected changes land in #810 and #811. Measurements follow the frozen
[protocol](media-evidence-protocol.md). They come from one host, on a local btrfs corpus
(34 files, 935.3 GB) and a three-file subset (24.7 GB) of a network corpus shared with an
active rehearsal. Every figure is copied from the committed records and READMEs named in each
row. Nothing was re-measured for this report.

## Evidence

Verdicts use the protocol thresholds. "Local" is the evicted full pass; "network" is the
as-found subset. A constructed miss shows a candidate *can* miss. It is not a rate.

| Candidate | Source | Measured cost | Verdict | Constructed misses | Decision |
|---|---|---|---|---|---|
| Current: three full reads, 8 KiB hash, concurrency 4 | #788 [README](media-evidence-baseline/README.md) | Local 2,805.9 GB logical, 2,765.8 GB physical, wall 613.9 s, hash CPU 684.5 s, probe CPU 805.0 s; network 74.0 GB logical | baseline | none in O1–O12 | Kept in every phase except where a row below replaces it |
| 64 KiB, 1 MiB, 4 MiB hash buffer; batched 8 KiB loop | #789 [stages 1–2](media-evidence-baseline/stages-1-2.md) | Hash CPU 222.9 / 157.1 / 170.5 / 230.4 s; summed hash 805.8 / 798.5 / 803.9 / 818.2 s; hash peak RSS 5.7 / 14.9 / 38.8 / 4.8 MiB | CPU improvement for all four; summed hash improvement except batch8k; wall inconclusive | none (no trust change) | **1 MiB selected** for the owner hash; others rejected |
| Concurrency 8 (local) | #789 | Wall 642.3 vs 619.2 s; first file 17.1 vs 10.3 s; summed hash 1,814.9 vs 798.5 s | wall inconclusive; first file and summed hash regression | none | Rejected |
| Concurrency 1, 2 (6-file local subset; network subset) | #789 | Subset c2: first file 7.0 vs 10.8 s, physical 257.6 vs 306.2 GB, wall 74.2 vs 78.1 s; c1 wall 98.6 s | c2 wall inconclusive; c1 wall regression; network inconclusive (CV above 20%) | none | Not selected: no full-pass c2 cell exists; concurrency stays 4 |
| Bounded incremental publication, flush cadence | #789 (modelled) | Under the completion barrier no cadence moves first durable observation | modelled only | none | Not selected |
| Per-file publication readiness | #789, #801 (modelled) | Modelled first ready 6.2 s vs barrier 212.5 s with single read | first durable observation `not measured` | none | Not selected: unmeasured, and needs an ADR superseding ADR 0077 publication |
| Single read: probe trusts owner hash under a size + mtime stat check | #801 [single-read](media-evidence-baseline/single-read/README.md) | Local wall 212.5 s; physical 935.5 GB (1.00× corpus); probe CPU 2.5 s; summed hash +5.1% vs buf1m; network: speed inconclusive, probe CPU 17.5 → 0.24 s | wall, physical, probe CPU improvement locally; summed hash regression | Same-size interior edit with mtime restored between hash and probe (#801 operator-recorded manual check; `meta` misses O1 in the committed #796 oracle) | Rejected with this guard |
| R1: probe reuses owner hash; re-hashes replaced by a metadata guard | #797 [lazy](media-evidence-baseline/lazy/README.md), #798 [790 report](media-evidence-790-report.md) | Logical bytes −66.7% on both corpora; network wire saving not observed (24.66 GB fetched in repetition 1 under both) | logical improvement | Exactly the guard's misses | **Selected** with the `meta-ctime` guard on btrfs and tmpfs, setting default off |
| `meta` (size, mtime ns, file key) | #796 [fingerprint](media-evidence-baseline/fingerprint/README.md) | 0 bytes | n/a | O1–O4, O6, O12, O13 missed | Rejected as identity evidence |
| `meta-ctime` (`meta` + ctime ns) | #798 [reuse](media-evidence-baseline/reuse/README.md) | 0 bytes; stat of 34 files about 1–3 ms | Added after the freeze; no stage-0 verdict claimed | Local: no miss in O1–O13 (O10 not applicable); false change on O5, O8. Network: ctime follows mtime, O1 and O13 missed | Selected only as R1's guard on btrfs and tmpfs |
| `ht64`, `ht1m`, `ht64-i8`, `ht64-i32` | #796 | Local 4.46 / 71.30 / 22.28 / 75.76 MB per pass | n/a | O1, O13 missed | Rejected |
| `rnd64-i8`, `rnd64-i32` | #798 | As `ht64-i8`/`-i32`, plus a 457 MB block table built by one full read | Detection probability equals the sampled fraction: under 0.04% per check | O1, O13 missed when the seed is known | Rejected |
| Lazy provisional evidence with deferred full hash | #797 | Eventual logical 935.31 GB local (one full read); provisional within 0.03 s; zero wasted work observed | logical improvement comes from dropping the re-reads, not from deferral | Interior O13 and O15 missed; plan becomes committable | Rejected |
| R2: repeat scan reuses cached full evidence | #798 | A stat instead of three full reads for unchanged files | not measured end to end | Network O13 missed; silent corruption persists until a scrub | Rejected |
| R3: transform reuses verified input evidence | #798 | `not measured` (#788 does not replay transforms) | none | As R1 for the operation window | Rejected |
| Snapshot or storage content version | protocol, #798 | No filesystem here exposes a content version or change counter | excluded at freeze | n/a | Excluded |

## Phase table

Normative for ADR 0120 once accepted. "Full hash" is an authoritative BLAKE3 `blake3:`
content hash over every byte. A guard, fingerprint or sampled digest is never stored or
compared as one.

| Phase | Required evidence | Permitted before full verification | Revalidation triggers | Mismatch handling | Supported deployments | Residual risk | Performance tradeoff |
|---|---|---|---|---|---|---|---|
| Discovery | None; metadata-only enumeration | Listing candidates only | n/a | n/a | All | None added | Unchanged |
| Probe and plan | Owner full hash (1 MiB buffer) with a pre/post stat including ctime. Probe: full hash before and after ffprobe; or, with the setting on and the root on btrfs or tmpfs, the reused owner hash plus the guard (size, mtime ns, ctime ns, file key) before and after ffprobe | Nothing: evidence attaches only when hash and probe agree, and publishes at session completion (ADR 0077) | Any guard field change; stat drift during the hash read; a filesystem outside the reuse list | Fail closed: `ARTIFACT_CHECKSUM_MISMATCH` with a fact-free payload; observation recorded without evidence | Reuse: btrfs and tmpfs owner roots with the setting on. All others: re-hash | Same-tick ctime edit, privileged forgery, or damage keeping size, mtime and ctime: probe facts may describe other bytes. The transform input full hash refuses to act on such a file; an untransformed file keeps them until the next full rescan | Local: one full read instead of three, wall about −65%, physical 1.00× corpus. Network: CPU only |
| Transform input | Full hash before and after the operation, equal to the planned facts | None | Every operation | Existing `ARTIFACT_CHECKSUM_MISMATCH`; no output | All | Unchanged | Unchanged: two input reads per transform |
| Output verification | Full hash by the verify-artifact worker; output probes always re-read | None | Every output | Existing verification failure; output not committed | All | Unchanged | Unchanged |
| Promotion/commit | Commit authorization against verified facts | None | Every commit | Existing commit-gate refusal | All; relocation is #413/#528 | Unchanged | Unchanged |
| Repeat scan | Full rescan, as probe and plan | As probe and plan | Every scan | As probe and plan | As probe and plan | Full rescan still re-reads every byte, so damage is caught on the next scan | Local saving as probe and plan; no cached reuse |

ADR 0050 byte ownership, ticket and lease authority, and absence reconciliation at completion
are unchanged in every row.

## Rollout gates

Frozen here for [#794](https://github.com/randomparity/voom-v2/issues/794). These are rollout
acceptance measurements on the operator's storage. They are not service-level targets or CI
timing checks, which stay with #471. Thresholds are the protocol's: improvement is
`max(10%, 2×CV)`, regression `max(5%, 2×CV)`, inconclusive if the baseline CV exceeds 20%,
3 repetitions. Each comparison is against the same build with the change off, on the same host
and cache state. Constructions use fixtures or copies only.

1. **G1 Hash buffer.** Local corpus, evicted, full pass: hash CPU improves; summed hash
   seconds and wall do not regress; hash peak RSS stays within baseline + 256 MiB.
2. **G2 Reuse I/O.** Local corpus, evicted, full pass, setting on: physical reads at most
   1.05× corpus bytes; the probe worker's `rchar` below 1% of corpus bytes; wall improves.
3. **G3 First durable observation.** Local corpus, evicted, full pass, through a real
   control-plane scan session timed to its first durable observation (the publishing
   instrument #794 builds). Capture the stage-0 cell, setting off, *before* any setting-on
   cell, then the same cell with the setting on. It must improve. The network subset is
   reported, not gating. Without the stage-0 cell the metric is inconclusive,
   and the setting stays off.
4. **G4 Constructed detection, btrfs and tmpfs.** At least 3 cases each: (a) same-size
   interior edit with mtime restored between hash and probe fails closed with no evidence;
   (b) the same edit during the hash read, mtime restored before the post-read stat, fails the
   hash stability check; (c) an ordinary write that moves mtime fails. Every case is detected.
5. **G5 Fallback.** Setting on, a root outside the reuse list (an ext4 fixture, and the
   network subset of at most 3 files, disclosed): every probe recomputes its hash (no hash
   source in the result), and case (a) is detected.
6. **G6 Transform backstop.** An edit with mtime restored after a reused observation and
   before a transform fails at transform input with `ARTIFACT_CHECKSUM_MISMATCH` and commits
   no output.
7. **G7 Recovery.** Setting off: the next scan writes no reused rows. A control plane, node
   agent or worker on a different protocol version is refused before lease execution
   (ADR 0016). A worker killed mid-hash or mid-probe publishes no partial evidence (O14).
   The pump refuses a `reused` result from a request that carried no guard.
8. **G8 Unchanged readers.** Output-probe rows in `artifact_verifications.report` and scan
   rows with recomputed hashes carry no hash source field.
9. **G9 Acceptance.** Canary on the 6-file local subset, then complete-library separate-output
   acceptance and repeat application with #794's existing facilities. A repeat scan of the
   unchanged library yields identical content hashes for every file, and the repeat
   application dispatches no new transform.

If the trust change is rejected, only G1 and G9 apply.

## Implementation contracts

Each takes effect only after ADR 0120 is accepted.

1. **Hash buffer.** The hash worker reads 1 MiB per read, as #789's `buf1m` cell (hash worker
   SHA-256 `f73aa370…`). No wire change.
2. **Hash stability.** The hash worker adds ctime ns to its pre/post stat comparison (no
   wire change) and reports the pre-read ctime ns in its result (wire change, contract 9).
3. **Probe guard.** `ProbeFileRequest` gains an optional guard carrying ctime ns and the file
   key (device, inode) from the hash result. Only the scan pump sends it, and only when the
   setting is on. Output probes and media-child probes never send it. `ExpectedFileFacts` and
   `ObservedFileFacts` do not change, so ticket and publication payloads embedding them are
   unchanged.
4. **Probe behaviour.** Without a guard the worker behaves as today. With one, it opens the
   file without following links, checks `statfs` type against the reuse list (btrfs, tmpfs),
   and recomputes as today if the type is not listed. Otherwise it compares size, mtime ns,
   ctime ns and file key before and after ffprobe, echoes the expected hash, and sets the
   result's optional hash source to `reused`. The source is omitted when the hash was
   recomputed.
5. **Guard failure.** A guard mismatch uses the existing `ARTIFACT_CHECKSUM_MISMATCH`
   failure class with a fact-free payload (a stage marker only, like `hash_drift`). No stat
   value, filesystem type or path enters the failure detail.
6. **Pump predicate.** ADR 0077's agreement predicate accepts `reused` only from a request it
   sent with a guard; otherwise it compares as today.
7. **Durable evidence.** `ScanObservationEvidence` gains the optional hash source under
   ADR 0013 (inventory row updated), omitted when recomputed. Its readers are the batch route
   (`voom-api/src/scan.rs::typed_observation`) and `scan_observations.evidence_json`; its
   consumers are #794's gates and operator audit. `artifact_verifications.report` rows are
   unchanged.
8. **Setting.** One owner-node setting enables guarded reuse, default off.
9. **Compatibility.** All wire changes (contracts 2–4 and 7) ship under one protocol version
   bump (ADR 0016). That is a flag day: the control plane, every node agent and every bundled
   worker upgrade together, with the setting off. Rows with recomputed hashes stay
   byte-identical, so a fleet-wide binary downgrade is supported until reused rows exist and
   unsupported afterwards. Rollback is the setting.
   A crash or restart publishes nothing partial, and the next scan re-observes.

## Re-scope drafts for #792–#794

Drafts only; the campaign orchestrator applies them after operator approval.

**If ADR 0120 is accepted:**

- **#792 → split into two PR-sized issues, then close #792 as superseded by them.**
  - *A. Adopt the 1 MiB hash buffer and check ctime for hash stability* (`type:feature`,
    `effort:S`): contract 1 and contract 2's stability check, with no wire change; a test
    for the stability failure on a ctime-only change. Depends on ADR 0120 acceptance.
  - *B. Probe reuses the owner hash under a local ctime guard* (`type:feature`, `effort:M`):
    contract 2's reported ctime and contracts 3–9 under the single protocol bump, with G4–G8
    implemented as tests where executable. Depends on A.
- **#793 → close as not planned.** ADR 0120 selects no change for transform input, output
  verification, promotion or repeat scan, and rejects R2 and R3. Reconsider if a later
  decision selects either after measuring it.
- **#794 → rewrite body** to gates G1–G9 above, including building G3's publishing
  instrument, in order: G3's stage-0 cell first, canary, full local pass, network subset.
  Depends on #792's A and B; drop #793.

**If the trust change is rejected:** #792 is rewritten to the 1 MiB hash buffer alone
(contract 1, `effort:S`), #793 closes as not planned, and #794 is rewritten to G1 and G9,
depending on #792.

## Not measured and exclusions

Carried from the records:

- Not measured anywhere: discovery, first durable observation, first actionable plan, first
  transformation start, first verified output, complete library, sidecar hashing, retries,
  and ffprobe child memory and I/O (`not_measured` in the #788, #789, #797 and #801 records).
  #796 and #798 also did not measure per-file physical bytes, peak memory or probing.
- Not measured in #790: server-side or other-client changes on the network share, whether a
  client-sent ChangeTime is honoured, and transform-input reuse.
- ctime behaviour was measured on btrfs and tmpfs only, on a kernel with fine-grained ctime.
  The oracle waited 20 ms before each edit, so same-tick edits on a coarse-clock kernel were
  not constructed.
- The `meta-ctime` and `rnd64` candidates were added after the protocol froze, so no verdict
  against stage 0 is claimed for them (#798).
- The #801 constructed miss is an operator-recorded manual check, not a committed record.
- Excluded at freeze: snapshot and immutable version evidence, and concurrency above 8.
  Piping the hash pass's bytes into ffprobe was excluded by the operator (#801).
- Network cells are not cache-equivalent, and their host counters include rehearsal traffic.
