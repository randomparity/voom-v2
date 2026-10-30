# Media evidence rollout report (#794)

ADR 0120 selected two changes: a 1 MiB owner hash buffer (#810), and guarded probe reuse of the
owner hash on btrfs and tmpfs behind the owner-node setting `probe_hash_reuse`, default off
(#811). This report runs the [rollout gates](media-evidence-decision-report.md#rollout-gates)
G1–G9 on one host. It uses a local btrfs corpus (34 files, 935.3 GB) and a three-file subset
(24.66 GB) of a CIFS corpus shared with an active rehearsal. Thresholds are the
[protocol's](media-evidence-protocol.md): improvement `max(10%, 2×CV)`, regression
`max(5%, 2×CV)`, and inconclusive when the baseline CV exceeds 20%. Every number comes from
the records and README in [media-evidence-baseline/rollout/](media-evidence-baseline/rollout/README.md).

**Recommendation: keep `probe_hash_reuse` off.** G7 and G9 fail, and G2 and G3 cannot be judged
on the gating full pass. The 1 MiB buffer (G1) passes and needs no setting.

## Verdicts

| Gate | Verdict | Evidence |
|---|---|---|
| G1 Hash buffer | **pass** | Full local, evicted, ×3, 8 KiB vs 1 MiB. Hash CPU 685.2 → 153.5 s (−77.6%, improvement). Summed hash 899.1 → 795.2 s (improvement). Wall 611.7 → 617.3 s (+0.9%, inconclusive, not a regression). Hash peak RSS 5.2 → 16.5 MiB, under the +256 MiB guardrail. All rows observed. |
| G2 Reuse I/O | **inconclusive** | The gating full pass cannot run: every whole-root scan of the local corpus fails ([#816](https://github.com/randomparity/voom-v2/issues/816)). Canary (6 files, ×5, reported): physical reads 1.000× the subset, probe `rchar` 0.026% of the subset, complete 101.2 → 26.2 s (improvement). |
| G3 First durable observation | **inconclusive** | The stage-0 cell (full local, off, ×3) was captured before any setting-on scan cell. Every repetition failed at 179–184 s on #816 with nothing published, so first durable observation is `unavailable` and has no baseline. Canary (reported, ×5): 5.60 → 1.69 s (−69.8%, improvement). Network subset (reported, as-found, repetitions 2–3): 9.37 → 8.97 s (inconclusive; CIFS is outside the reuse list, so nothing was reused). |
| G4 Constructed detection | **pass** | Real-media copies on btrfs (reflink, evicted) and tmpfs. (a) restored-mtime edit before probe: `pre_probe_guard`. (b) edit during the hash read: `hash_drift`. (c) mtime-moving write: `pre_probe_guard`. All six are `ARTIFACT_CHECKSUM_MISMATCH` with no evidence. #813's tests cover the same cases with a fake ffprobe. |
| G5 Fallback | **pass** | ext4 image mounted with `fuse2fs` (the kernel reports `fuse`; no native ext4 mount was available without privileges). With a guard sent, the unchanged copy recomputes with no hash source, and case (a) is detected by the recomputed hash. Network subset with the setting on: 0 of 3 reused in each of 3 repetitions. #813's CI ran the native-ext4 arm. |
| G6 Transform backstop | **pass** | Setting on, reused observation, then a restored-mtime edit, then a remux policy. The transform input failed with `ArtifactChecksumMismatch`. There is no remux file version, no commit intent, and no staged or output file. |
| G7 Recovery | **fail** | Every setting-off scan, including those after setting-on cells, wrote no reused row (also #813's pump test). Worker killed mid-hash: all evidence identical to clean runs. Killed mid-probe: that file's session failed with nothing published. Guardless `reused` refusal is covered by #813's test. Mixed versions: an agent refuses a worker on another protocol at startup, but **the control plane and node agent run leases across protocols 5 and 6 in both directions** ([#817](https://github.com/randomparity/voom-v2/issues/817)). |
| G8 Unchanged readers | **pass** | Every recomputed scan row in every cell has no hash source (`other_source` 0). The four G9 output-verification reports carry no hash source. #813's tests pin the encodings. |
| G9 Acceptance | **fail** | Canary: a repeat scan of the six files published an identical content-hash set. The repeat application re-dispatched none of the four completed files. Two files fail every application on remux defects unrelated to reuse ([#819](https://github.com/randomparity/voom-v2/issues/819), [#820](https://github.com/randomparity/voom-v2/issues/820)), so the repeat still dispatches transforms. The flagship policy's `defaults` block all six canary files, so the canary used a defaults-free normalize phase. Complete library: not run, blocked by #816. |

## Before and after

| Measure | Setting off | Setting on | Cell |
|---|---:|---:|---|
| First durable observation (median) | 5.60 s | 1.69 s | canary, evicted, ×5 |
| Complete (median) | 101.2 s | 26.2 s | canary, evicted, ×5 |
| Physical reads | 240.5 GB | 106.8 GB | canary, evicted, ×5 |
| Probe worker CPU | 90.2 s | 0.4 s | canary, evicted, ×5 |
| First durable observation | failed | not run | full local (#816) |
| First durable observation, reps 2–3 | 9.37 s | 8.97 s | network subset, as-found |

On the canary, reuse removes the probe's two full re-reads, as ADR 0120 modelled. On CIFS the
setting changes nothing, as designed.

## Path to enabling the setting

1. Fix #816, so that an oversized probe result cannot fail a whole-root scan.
2. Fix #817, so that protocol skew between the control plane and agent is refused before
   lease execution.
3. Fix #819 and #820, so that repeat application on the canary converges.
4. Rerun with `scripts/media-evidence-rollout.py`: the full-local stage-0 cell first, then
   G2 and G3 with the setting on, G7's mixed-version arm, and G9's canary and complete-library
   arms. Then follow the [node-agent runbook](runbooks/operator-node-agent.md#probe-hash-reuse).

## Exclusions and limits

- One host, one local and one network corpus. The network arm is a three-file subset read
  as-found and disclosed to the rehearsal. Its "on" cell exceeded the 100 GB per-cell
  physical budget by 23.3 GB on a cold client cache.
- Subset scans run one session per file. Their first-durable times are the first session to
  finish, not the whole-library completion barrier.
- Not measured: discovery, first actionable plan, first transformation start, first
  verified output, per-file stage timings, retries, and ffprobe child memory and I/O.
- The complete-library separate-output application was not run, because it needs a
  successful whole-root scan.
