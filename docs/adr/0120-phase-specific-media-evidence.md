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

- A 1 MiB owner hash buffer cut hash CPU 67–77% with no trust change (#789).
- Removing the probe's two re-reads cut a local full pass from 619.2 s to 212.5 s and
  physical reads from 2.93× to 1.00× the corpus (#801, #797). On the network subset the
  re-reads were served by the client cache, so only probe CPU fell.
- A stat guard of size and mtime misses a same-size edit with mtime restored between hash
  and probe (#801). Adding ctime detects it on btrfs and tmpfs, not on the network mount,
  where the reported ctime follows mtime (#798).
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

The guarded reuse amends ADR 0077's agreement predicate for scan probes only. The probe
result says whether its content hash was recomputed or reused, and the pump accepts a reused
hash only when the guard held. A guard mismatch fails closed, as a hash mismatch does today,
leaving the observation without evidence. Fingerprints, sampled digests and the reuse guard
are never stored as, or compared as, a content hash. ADR 0050 byte ownership, ticket and
lease authority, and absence reconciliation at completion are unchanged.

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
- The worker protocol changes (ADR 0016 version bump): the hash result gains ctime ns, the
  probe request a guard mode, the probe result its hash source. The durable evidence records
  the hash source as an additive field (ADR 0013), so a binary downgrade after such rows exist
  is unsupported; rollback is the owner-node setting.
- Adding a filesystem to the reuse list requires the O1 and O10 constructions to pass on it.

## Considered & rejected

- **Keep all three reads (no-go).** judgment: forgoes the measured two-thirds cut on local
  storage, where the ctime guard detected every constructed mtime-restored edit; it remains
  the fallback if this record is not accepted.
- **Reuse with a size and mtime guard only.** verified: #801's manual construction (one
  interior byte flipped, mtime restored) was probed as unchanged, recorded in
  `media-evidence-baseline/single-read/README.md`.
- **Reuse on the network mount.** verified: `reuse/README.md` records ctime following mtime
  there, so O1 and O13 are missed; `lazy/README.md` records no wire saving (24.66 GB fetched
  in repetition 1 under both).
- **Fingerprints or random samples as identity.** verified: `media-evidence-790-report.md`
  lists constructed O1 and O13 misses for every `ht*` and `rnd64` candidate.
- **Lazy provisional evidence.** verified: `lazy/README.md` shows deferral leaves eventual
  logical bytes at one full read and misses interior O13 and O15 constructions.
- **Cached evidence for repeat scans.** judgment: needs a durable cache and a scrub interval
  no measurement supports; silent corruption would persist until the scrub.
- **Per-file publication readiness.** verified: first durable observation is `not_measured`
  in every #788–#801 record; publishing before completion would also supersede ADR 0077.
- **Concurrency 8.** verified: `stages-1-2.md` records a slower first file (17.1 s vs 10.3 s)
  and no shorter pass.
