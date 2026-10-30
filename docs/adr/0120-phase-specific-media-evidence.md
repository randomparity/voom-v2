# 0120 — Phase-specific media identity evidence

## Status

Proposed (2026-10-29). Maintainer approval is required before any production trust change.
Until this record is Accepted, production keeps the conservative full-hash behaviour in every
phase.

## Context

Epic #787 asks which identity evidence each phase needs, from measurements taken under the
frozen [protocol](../media-evidence-protocol.md). Today each scanned file is read in full
three times (owner hash, then the probe's BLAKE3 re-reads before and after ffprobe), and
ADR 0077 publishes identity only at scan completion. The evidence is summarised in the
[decision report](../media-evidence-decision-report.md):

- A 1 MiB owner hash buffer cut hash CPU 77% (684.5 to 157.1 s) and summed hash time 11%,
  raising hash peak RSS from 5.2 to 14.9 MiB, with no trust change (#789).
- Removing the probe's two re-reads cut a local full pass from 619.2 s to 212.5 s and
  physical reads from 2.93× to 1.00× the corpus (#801, #797). On the network subset the
  re-reads were served by the client cache, so only probe CPU fell.
- A metadata guard of size, mtime ns and file key misses a same-size interior edit with
  mtime restored (#796 oracle, case O1). Adding ctime detects it on btrfs and tmpfs, not on
  the network mount, where the reported ctime follows mtime (#798).
- Every fingerprint and sampling candidate has constructed misses (#796, #798). Lazy
  hashing defers the one full read without removing it (#797). No storage here exposes a
  content version.
- First durable observation was never measured with a publishing instrument.

## Decision

Per phase, with the full table, frozen gates and contracts in the decision report:

| Phase | Decision |
|---|---|
| Discovery | Unchanged: metadata-only enumeration; no identity evidence. |
| Probe and plan | The owner hash stays the authoritative BLAKE3 full read, with a 1 MiB buffer. **Trust change:** the scan probe may reuse that hash in place of its two re-reads, guarded by size, mtime ns, ctime ns and file key before and after ffprobe, only on btrfs and tmpfs and only when an owner-node setting enables it (default off). Any other filesystem keeps both re-reads. Concurrency stays 4. Publication stays completion-gated (ADR 0077). |
| Transform input | Unchanged: full hash before and after the operation. |
| Output verification | Unchanged: full hash by the verify-artifact worker; output probing always re-reads. |
| Promotion/commit | Unchanged: commit authorization against verified facts. |
| Repeat scan | Unchanged: full rescan; no cached evidence is reused. |

The guarded reuse amends ADR 0077's agreement predicate for scan probes only. The hash
worker adds ctime to its pre/post stability check and reports the pre-read ctime. The scan
pump alone may send the guard (that ctime and file key) as a new optional field of the probe
request; `ExpectedFileFacts` and `ObservedFileFacts` are unchanged, and output probes never
send it. With a guard, the worker reads no file bytes, compares the guard before and after
ffprobe, echoes the expected hash and marks the result's hash source `reused`. The source is
omitted when the hash was recomputed. The pump accepts a reused hash only from a guarded
request. A guard mismatch fails closed with the existing `ARTIFACT_CHECKSUM_MISMATCH` class
and a fact-free payload, leaving the observation without evidence. Fingerprints, sampled
digests and the reuse guard are never stored as, or compared as, a content hash. ADR 0050
byte ownership, ticket and lease authority, and absence reconciliation at completion are
unchanged.

If the maintainer rejects the trust change, the probe-and-plan row keeps both re-reads and
only the 1 MiB buffer proceeds.

## Consequences

- Locally, the first durable observation and complete pass under the barrier are expected to
  fall by about two thirds. This is modelled, not measured; #794's gates measure it.
- A same-size edit made between hash and probe in the same ctime tick, a privileged forger,
  or damage to the bytes that keeps size, mtime and ctime can leave probe facts describing
  bytes other than the hashed ones. The transform input's full hash still refuses to act on
  such a file. A file that is never transformed keeps the mismatched facts until the next
  full rescan.
- One worker protocol version bump (ADR 0016), a flag day for the control plane, every
  node agent and every bundled worker: the hash result gains ctime ns, the probe request an
  optional guard, the probe result an optional hash source. Scan evidence (the batch route
  and `scan_observations.evidence_json`) records the source as an optional field (ADR 0013),
  read by #794's gates and operator audit to count reused observations. Rows written with
  recomputed hashes, including every output probe in `artifact_verifications.report`, stay
  byte-identical. A fleet-wide binary downgrade is unsupported only once reused rows exist;
  rollback is the owner-node setting.
- Adding a filesystem to the reuse list requires the hash-to-probe O1 and mid-read O10
  constructions to be detected on it; #794 gates both on btrfs and tmpfs.

## Considered & rejected

- **Keep all three reads (no-go).** judgment: forgoes the measured two-thirds cut on local
  storage, where the ctime guard detected every constructed mtime-restored edit; it remains
  the fallback if this record is not accepted.
- **Reuse with a size and mtime guard only.** verified: the committed #796 oracle
  (`fingerprint/2026-10-29-fingerprint-oracle.json`) records `meta` missing O1; #801's
  operator-recorded manual check in `single-read/README.md` corroborates it for the probe.
- **Reuse on the network mount.** verified: `reuse/README.md` records ctime following mtime
  there, so O1 and O13 are missed; `lazy/README.md` records no wire saving (24.66 GB fetched
  in repetition 1 under both).
- **Fingerprints or random samples as identity.** verified: `media-evidence-790-report.md`
  lists constructed O1 and O13 misses for every `ht*` and `rnd64` candidate.
- **Lazy provisional evidence.** verified: `lazy/README.md` shows deferral leaves eventual
  logical bytes at one full read and misses interior O13 and O15 constructions.
- **Transform input reuse (R3).** verified: `media-evidence-790-report.md` records its cost
  as not measured (#788 does not replay transforms); the input's full hash is also the
  backstop for the probe-reuse residual.
- **64 KiB, 4 MiB or batched 8 KiB hash reads.** verified: `stages-1-2.md` records buf1m's
  summed-hash improvement as the best of the four and selects it.
- **Cached evidence for repeat scans.** judgment: needs a durable cache and a scrub interval
  no measurement supports; silent corruption would persist until the scrub.
- **Per-file publication readiness.** verified: first durable observation is `not_measured`
  in every #788–#801 record; publishing before completion would also supersede ADR 0077.
- **Concurrency 8.** verified: `stages-1-2.md` records a slower first file (17.1 s vs 10.3 s)
  and no shorter pass.
