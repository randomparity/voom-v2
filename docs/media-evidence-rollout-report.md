# Media evidence rollout report (#794)

ADR 0120 selected two changes. The first is a 1 MiB owner hash buffer (#810). The second is
guarded probe reuse of the owner hash on btrfs and tmpfs, behind the owner-node setting
`probe_hash_reuse`, which defaults to off (#811).

This report runs the [rollout gates](media-evidence-decision-report.md#rollout-gates)
G1–G9 on one host. The corpora are a local btrfs corpus (34 files, 935.3 GB) and a
three-file subset (24.66 GB) of a CIFS corpus shared with an active rehearsal. Thresholds
are the [protocol's](media-evidence-protocol.md): improvement `max(10%, 2×CV)`, regression
`max(5%, 2×CV)`, and inconclusive when the baseline CV exceeds 20%. Every number comes from
the records and README in
[media-evidence-baseline/rollout/](media-evidence-baseline/rollout/README.md).

**Recommendation: keep `probe_hash_reuse` off.** Reuse delivers its modelled gain on real
storage: first durable observation over the full local corpus fell from 673.1 s to 223.7 s,
and the guard caught every constructed edit. The setting still stays off until two gates
pass on reruns:

- G7: the control plane and node agent run leases across protocol versions. Its
  mixed-version arm reruns once the #817 fix merges.
- G9: two remux defects kept the canary's repeat application from converging. The canary
  and complete-library arms rerun once the #819 and #820 fixes merge.

Neither open failure comes from the reuse path. The 1 MiB buffer (G1) needs no setting.

**Ordering rule.** The stage-0 cell precedes its paired setting-on cell per scan
configuration, and worker-level constructions are not scan cells (operator-approved
2026-10-30). Each off/on pair below ran on one build, with a fresh control-plane store and
an evicted cache for every repetition.

**Builds.**
- G1 through G5, G7's kill arms, and G8's scan rows ran on `main` at `3092e7c3`, which
  includes #821.
- G6, the G9 canary so far, the network cells and G7's version pairings ran on
  `f8be728c`.
- #821 (ADR 0122) caps the probe snapshot at 60 KiB. On the post-#821 build, 3 of the 34
  local files (`f0010`, `f0011`, `f0012`) carry `ffprobe_json_omitted_bytes` in place of
  their raw ffprobe JSON. The rest of their evidence is unchanged.

## Verdicts

| Gate | Verdict | Evidence |
|---|---|---|
| G1 Hash buffer | **pass** | Full local, evicted, ×3, 8 KiB vs 1 MiB harness cells. Hash CPU 688.8 → 162.6 s (−76.4%, improvement). Summed hash 965.2 → 870.8 s (−9.8%, inconclusive, not a regression). Wall 669.3 → 676.6 s (+1.1%, inconclusive, not a regression). Hash peak RSS 5.1 → 16.2 MiB, within +256 MiB. All rows observed. Outside the gate: the harness's first file done rose 16.0 → 18.2 s (+13.7%, a regression by the protocol threshold). The earlier, superseded pair showed 10.9 → 10.5 s, so this is not consistent. |
| G2 Reuse I/O | **pass** | Full local, evicted, ×3, setting on. Physical reads 935.56 GB, 1.0003× the corpus (limit 1.05×). Probe worker `rchar` 149.6 MB, 0.016% of corpus bytes (limit 1%). Wall (session complete) 673.1 → 223.7 s, −66.8%, baseline CV 0.3% (improvement; worst on-repetition 224.1 s). |
| G3 First durable observation | **pass** | The stage-0 cell (full local, setting off, ×3) was captured before any setting-on cell on that build: 670.8, 674.6, 673.1 s. Setting on: 224.1, 223.7, 222.8 s. Median 673.1 → 223.7 s, −66.8%, CV 0.3% (improvement). One session publishes all 34 files at completion (ADR 0077), so first durable observation equals complete. Network subset (reported, as-found, repetitions 2–3): 9.37 → 8.97 s, inconclusive, as expected because CIFS is outside the reuse list. A first stage-0 on the pre-fix build failed on every repetition ([#816](https://github.com/randomparity/voom-v2/issues/816), fixed by #821 before these cells). |
| G4 Constructed detection | **pass** | Real-media copies on btrfs (reflink, evicted) and on tmpfs, with the guard sent. (a) restored-mtime edit before probe: `pre_probe_guard`. (b) edit during the hash read: `hash_drift`. (c) mtime-moving write: `pre_probe_guard`. All six are `ARTIFACT_CHECKSUM_MISMATCH` with no evidence. #813's tests cover the same cases with a fake ffprobe. |
| G5 Fallback | **pass, FUSE stand-in** | An ext4 image mounted through `fuse2fs` (the kernel reports `fuse`; no native ext4 mount was writable without privileges). With the guard sent, the unchanged copy recomputes with no hash source, and case (a) is detected by the recomputed hash. Network subset with the setting on: 0 of 3 reused in each of 3 repetitions. The native-ext4 arm is #813's CI only. |
| G6 Transform backstop | **pass** | Setting on: a reused observation, then a restored-mtime edit, then a remux policy. The transform input failed with `ArtifactChecksumMismatch`. There was no remux file version, no commit intent, and no staged or output file. |
| G7 Recovery | **fail (mixed-version arm; rerun after #817)** | Arms that held: every setting-off cell wrote no reused rows. A worker SIGKILLed 3 or 8 s into a canary scan, with the setting on or off, published nothing partial: affected sessions either failed whole or published clean-run hashes. Only the pre-#816 setting-off probe kill demonstrably hit an in-flight probe (its session failed). With the setting on the probe is nearly idle, so the 8 s kill most likely landed between operations. A guardless `reused` is refused (#813's test). **Arm that failed:** the control plane and node agent run leases across protocols 5 and 6 in both directions ([#817](https://github.com/randomparity/voom-v2/issues/817)); only the agent refuses a worker on another protocol. Not exercised: turning the setting off on a database that already holds reused rows (each cell uses a fresh database; #813's pump test covers it). |
| G8 Unchanged readers | **pass** | In the #816-fixed cells, recomputed scan rows carry no hash source field at all, and no row carries a value other than `reused`. The pre-#816 cells' classifier counted a null as absent; serde omits the field (`skip_serializing_if`), so none can occur. The four G9 output-verification reports carry no hash source. #813's tests pin the encodings. |
| G9 Acceptance | **pending (rerun after #819 and #820)** | Approved scope: the canary runs `reference-user` without its two `defaults` lines (accepted by the operator; evidence in [#827](https://github.com/randomparity/voom-v2/issues/827)), including transcode, on copies of the six files, then re-applies it. The complete library gets a read-only scan, a normalize-only remux into a separate output root outside the corpus, a re-apply, and a repeat-scan hash identity check. Transcode is proven on the canary only; the full library runs remux-only. Evidence so far: six full-library scans and every canary repetition published identical content-hash sets. On `f8be728c` a defaults-free normalize canary completed four files and never re-dispatched them, while two files failed every application on [#819](https://github.com/randomparity/voom-v2/issues/819) and [#820](https://github.com/randomparity/voom-v2/issues/820). |

## Before and after

| Measure | Setting off | Setting on | Cell |
|---|---:|---:|---|
| First durable observation (median) | 673.1 s | 223.7 s | full local, evicted, ×3 |
| Physical reads (median) | 2,805.6 GB | 935.6 GB | full local, evicted, ×3 |
| Probe worker CPU (median) | 757.5 s | 2.5 s | full local, evicted, ×3 |
| Hash worker CPU (median) | 164.0 s | 152.3 s | full local, evicted, ×3 (inconclusive) |
| First durable observation (median) | 5.90 s | 1.69 s | 6-file canary, evicted, ×5 |
| Complete (median) | 103.4 s | 26.0 s | 6-file canary, evicted, ×5 |
| First durable observation, reps 2–3 | 9.37 s | 8.97 s | network subset, as-found |

Reuse removes the probe's two full re-reads on btrfs, as ADR 0120 modelled (wall about −65%,
physical 1.00× the corpus). On CIFS the setting changes nothing, as designed.

## Path to enabling the setting

1. Merge #817's fix, then rerun G7's mixed-version pairings.
2. Merge #819's and #820's fixes, then rerun G9 on that build: canary first, then the
   complete-library remux and its repeat.
3. If both pass, follow the
   [node-agent runbook](runbooks/operator-node-agent.md#probe-hash-reuse).

## Exclusions and limits

- One host, with one local corpus and one network corpus. The network arm is a three-file
  subset read as-found and disclosed to the rehearsal. Its setting-on cell exceeded the
  100 GB per-cell physical budget by 23.3 GB.
- Network cells, the setting-off kill cells, G6, G9 and G7's version pairings ran on the
  pre-#816 build. #821 changes only
  the probe snapshot's size bound.
- Subset scans run one session per file. Their first-durable times are the first session to
  finish, not the whole-library completion barrier.
- Not measured: discovery, first actionable plan, first transformation start, first
  verified output, per-file stage timings, retries, and ffprobe child memory.
