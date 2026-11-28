# Network corpus physical-read measurements (#916)

## Problem and authority

Issue #916 identifies CIFS statistics attributed to unrelated network filesystems.
The operator selected unavailable measurements for unsupported network filesystems,
retention of raw host counters and diagnostics, and alignment of the protocol and
consumers. New filesystem telemetry is excluded (maintainer follow-up; no epic).
Frozen authority: issue #916 WORK:SCOPE token `q916-fca3438a`.

## Design

Keep record schemas and raw `process`, `workers`, and `host_counters` unchanged.
Baseline owns the filesystem allow-lists and will own physical-read selection:
`physical_read_bytes(corpus, local_bytes, counters) -> int | str`.
For an allow-listed local filesystem return the process/worker read_bytes value;
for `cifs`/`smb3` return `cifs_bytes_read`; otherwise return `unavailable` and
explain on stderr that no supported physical-read counter exists for the corpus
filesystem. Never substitute host interface traffic for storage physical bytes.

Migrate candidates, lazy, rollout, observe and copy physical-read comparison metrics
to this helper. Each side of a comparison selects using its own corpus metadata,
including as-found repetition 1. Keep raw process I/O and destination write metrics
intact. Fingerprint/reuse corpus records retain raw process and host diagnostics and have no
executable derived physical-read comparison. Their historical CIFS report remains intact.
The separate baseline probe-reuse records are compared by candidates.

No serialized field is added or removed; historical records retain validation and
are interpreted using their recorded filesystem type. Unknown or missing types
cannot establish a supported physical-read measurement. Existing minimal synthetic
local fixtures gain their actual filesystem type. CIFS and local controls retain
numeric measurements, including zero; unavailable selected counters stay unavailable.

A shared selector is smaller and safer than repeating filesystem policy across five
consumers. Adding NFS/other counters would require unapproved telemetry. No ADR or
migration is needed for this bounded correction under the operator's settled choice.

## Success

For every network type in baseline.NETWORK_FILESYSTEMS except cifs/smb3, derived
physical reads are unavailable even with a positive CIFS counter. CIFS and local
controls retain their appropriate counter. Candidates, rollout, observe and copy keep the existing `unavailable` verdict; lazy
keeps `inconclusive` with reason `unavailable`. Existing repetition-1 outputs use the
selector too; rollout continues skipping repetition 1. Useful timings remain available.
Protocol names backing counters and the unsupported cause. Raw records stay unchanged.

## Failure model

Actors/deployments: operators comparing committed evidence or capturing Linux local,
CIFS/SMB, NFS, Ceph, 9p and other allow-listed network corpora; non-Linux missing counters.
Assets: truthful physical-read conclusions and unchanged raw diagnostic evidence.
Required handling: unsupported/unknown filesystem, missing selected counter, and
unrelated positive host CIFS traffic cannot yield a corpus physical-read number.
Accepted residual: CIFS statistics and interface counters include unrelated traffic
on shared hosts; the protocol already discloses this, and per-mount attribution is
outside the approved correction. Destination write I/O remains a process metric.
Other-owner work: new per-filesystem instrumentation belongs to maintainer follow-up.

## Threat model

Saved JSON corpus metadata crosses into counter selection. A string type check before
set membership sends arrays/objects through the unsupported diagnostic. Selected values
are existing metrics; no new parsing, commands, paths or network calls are introduced.
The diagnostic contains fixed text, never arbitrary metadata or host paths.

## Validation and rollback

Focused selftests cover selector sources, historical numeric CIFS traffic for unsupported
filesystems, all five consumers, as-found repetition 1, missing counters and local/CIFS
controls. Run focused just recipes, assembled source checks and final `just ci`.
Documentation is reviewed against executable selection, without prose snapshot tests.
Rollback is a normal revert; no data migration or evidence-file rewrite is required.
