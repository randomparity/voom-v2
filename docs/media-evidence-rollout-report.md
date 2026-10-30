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
and the guard caught every constructed edit. The setting still stays off, because two gates
fail:

- G7: the control plane and node agent run leases across protocol versions.
- G9: two remux defects keep repeat application from converging.

Neither failure comes from the reuse path. The 1 MiB buffer (G1) needs no setting.

## Verdicts

| Gate | Verdict | Evidence |
|---|---|---|
| G1 Hash buffer | *(fill after rerun)* | Full local, evicted, ×3, 8 KiB vs 1 MiB harness cells. |
| G2 Reuse I/O | **pass** | Full local, evicted, ×3, setting on. Physical reads 935.56 GB, 1.0003× the corpus (limit 1.05×). Probe worker `rchar` 149.6 MB, 0.016% of corpus bytes (limit 1%). Wall (session complete) 673.1 → 223.7 s, −66.8%, baseline CV 0.3% (improvement; worst on-repetition 224.1 s). |
| G3 First durable observation | **pass** | The stage-0 cell (full local, setting off, ×3) was captured before any setting-on cell on that build: 670.8, 674.6, 673.1 s. Setting on: 224.1, 223.7, 222.8 s. Median 673.1 → 223.7 s, −66.8%, CV 0.3% (improvement). One session publishes all 34 files at completion (ADR 0077), so first durable observation equals complete. Network subset (reported, as-found, repetitions 2–3): 9.37 → 8.97 s, inconclusive, as expected because CIFS is outside the reuse list. A first stage-0 on the pre-fix build failed on every repetition ([#816](https://github.com/randomparity/voom-v2/issues/816), fixed by #821 before these cells). |
| G4 Constructed detection | **pass** | Real-media copies on btrfs (reflink, evicted) and on tmpfs, with the guard sent. (a) restored-mtime edit before probe: `pre_probe_guard`. (b) edit during the hash read: `hash_drift`. (c) mtime-moving write: `pre_probe_guard`. All six are `ARTIFACT_CHECKSUM_MISMATCH` with no evidence. #813's tests cover the same cases with a fake ffprobe. |
| G5 Fallback | **pass, FUSE stand-in** | An ext4 image mounted through `fuse2fs` (the kernel reports `fuse`; no native ext4 mount was writable without privileges). With the guard sent, the unchanged copy recomputes with no hash source, and case (a) is detected by the recomputed hash. Network subset with the setting on: 0 of 3 reused in each of 3 repetitions. The native-ext4 arm is #813's CI only. |
| G6 Transform backstop | **pass** | Setting on: a reused observation, then a restored-mtime edit, then a remux policy. The transform input failed with `ArtifactChecksumMismatch`. There was no remux file version, no commit intent, and no staged or output file. |
| G7 Recovery | **fail** | Arms that held: every setting-off cell wrote no reused rows. A worker SIGKILLed mid-scan, with the setting on or off, published nothing partial; affected sessions failed whole or published clean-run hashes. A guardless `reused` is refused (#813's test). **Arm that failed:** the control plane and node agent run leases across protocols 5 and 6 in both directions ([#817](https://github.com/randomparity/voom-v2/issues/817)); only the agent refuses a worker on another protocol. Not exercised: turning the setting off on a database that already holds reused rows (each cell uses a fresh database; #813's pump test covers it). |
| G8 Unchanged readers | **pass** | No stored scan row in any cell carries a hash source other than `reused`, and recomputed rows carry no field at all. The four G9 output-verification reports carry no hash source. #813's tests pin the encodings. |
| G9 Acceptance | **fail** | Repeat scan: six full-library scans and every canary repetition published identical content-hash sets. Repeat application on the canary: the four completed files were never re-dispatched, but two files fail every application on remux defects unrelated to reuse ([#819](https://github.com/randomparity/voom-v2/issues/819), [#820](https://github.com/randomparity/voom-v2/issues/820)), so the repeat still dispatches transforms. The flagship policy's `defaults` block all six canary files, so the canary used a defaults-free normalize phase. The complete-library separate-output application was not started, because the canary did not pass (#339's procedure runs the full library only after the canary). |

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

1. Fix #817, so that protocol skew between the control plane and an agent is refused before
   lease execution.
2. Fix #819 and #820, so that repeat application on the canary converges.
3. Rerun G7's mixed-version arm and G9 with `scripts/media-evidence-rollout.py`: canary,
   then complete-library separate-output application and repeat. Then follow the
   [node-agent runbook](runbooks/operator-node-agent.md#probe-hash-reuse).

## Exclusions and limits

- One host, with one local corpus and one network corpus. The network arm is a three-file
  subset read as-found and disclosed to the rehearsal. Its setting-on cell exceeded the
  100 GB per-cell physical budget by 23.3 GB.
- Network cells, G6, G9 and G7's version pairings ran on the pre-#816 build. #821 changes only
  the probe snapshot's size bound.
- Subset scans run one session per file. Their first-durable times are the first session to
  finish, not the whole-library completion barrier.
- Not measured: discovery, first actionable plan, first transformation start, first
  verified output, per-file stage timings, retries, and ffprobe child memory.
