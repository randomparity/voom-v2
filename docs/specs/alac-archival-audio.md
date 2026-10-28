# ALAC archival audio — issue #762

## Scope and authority

Implements #762 under #758 using ADR 0114 settings/facts and #761's worker seams.
Frozen scope is `q762-b188b107`. No ownership transition, serialized field, profile
knob, dependency or protocol version change. Protocol 5 upgrade ordering remains
ADR 0114's. Exclusions and owners remain in the frozen WORK:SCOPE.

## Global Constraints

Rust 1.95.0; existing dependencies only; MKV only; protocol version 5 unchanged.
Preserve crate ownership, source bytes, transaction/publication boundaries,
existing lossy codecs, FLAC and compatibility-companion behavior.

## Behavior

Accept canonical `alac` in existing transcode and strict-downmix synthesis policy,
planning, protocol and worker paths. The default profile selects FFmpeg `alac`;
there is no lossy bitrate or new compression control. Reuse existing readiness
inventory with an optional ALAC encoder token. A missing encoder fails before
output creation with the existing actionable worker-build diagnostic. It is
not a startup dependency and never falls back.

Resolve each selected source with `LosslessAudioSettings::resolve` before
launch. Ordinary archival ALAC accepts known signed 16/24-bit integer facts,
positive source rate, and matching mono/1, stereo/2 or back 5.1/6 layout.
Preserve rate, exact layout and precision by default; existing explicit 16-to-24
widening is allowed and reduction rejected. Packed/planar decoder storage is
not precision. Emit s16p for 16-bit or s32p for 24-bit, explicit raw precision,
source rate and exact layout. Missing, float, unsupported or contradictory
facts fail before output creation, including a later invalid selected stream.

FFmpeg 8.1.3 `-h encoder=alac` lists s16p/s32p and layouts mono, stereo, 3.0,
4.0, 5.0, 5.1, 6.1(back), 7.1(wide). Real generated MKV probes confirm 16/24-bit
mono/stereo/5.1 facts. Explicit 5.1(side) and ordinary 7.1 fail with unsupported
layout, so ordinary ALAC rejects them instead of silently remixing. Additional
encoder layouts are outside ADR 0114's current bounded source contract.

Synthesis accepts existing known mono/stereo/5.1/5.1(side)/7.1 source facts and
strictly fewer target channels 1/2/6 (mono/stereo/back 5.1). Resolve source
precision first, then target layout; source 7.1 and side 5.1 may therefore
explicitly downmix. Retain originals and non-default companion identities,
metadata and full inherited accessibility dispositions. A downmix is a
requested transform, not archival sample equality.

After encoding, actual MKV codec/rate/layout/precision facts must match resolved
per-stream expectations before success. Existing metadata, selected identity,
publication and source-hash guards remain. Canonical decoded sample equality is
required generated-test proof through #760's helper, not new production reads.

## Implementation and validation

Extend closed policy/protocol target sets and optional readiness inventory.
Rename the worker's internal FLAC resolver/verification functions to lossless
names and reuse them for both codecs. Keep FLAC-specific rate/compression rules;
ALAC uses planar sample formats and a post-target-resolution exact layout check.
No generic codec registry, new persistence, or control-plane production change.
Existing plan/dispatch paths carry the token unchanged; focused tests verify it.

Required tests cover policy acceptance, stable planning/no-op/dispatch, bitrate
absence, exact argv, unknown target rejection, missing encoder pre-mutation,
invalid source facts/layouts/precision and output mismatches. Real-worker tests
exercise mono/stereo/5.1 at 16/24 bits and 44.1/96 kHz, independently resolved
multi-selection and explicit widening. Actual probe facts and canonical samples
must agree, source bytes stay identical, copied packet hashes and metadata,
full dispositions and lineage stay correct. Separate real downmix fixtures
7.1→5.1, side 5.1→stereo and stereo→mono compare with an independently generated
reference transform and assert retained originals/non-default companions.
Required tools fail loudly when absent; no new ignored or opt-in tests.

Run focused tests and controlled faults, then source checks, fmt-check, lint,
mandatory pre-commit hooks, final `just ci`, and remote CI at the exact head.

## Failure model

- Actors and deployments: authenticated local/owner-node workers, operators,
  and Linux/macOS CI generated-media jobs with their installed FFmpeg builds.
- Invariants and assets: source bytes; selected sample/rate/layout/precision;
  copied streams/metadata/dispositions/lineage; strict non-default downmix;
  complete selection validated before output creation, actual output verified.
- Accepted failure classes: none for these bounded ALAC contracts. Existing
  staging, no-overwrite, hash and deadline guards retain their current guarantees;
  encoded byte identity across encoder builds is not promised.
- Covered elsewhere: foundational facts/helper (#760), other targets (#759/#761/
  #763), aggregate CLI matrix (#764), library execution (#339), deferred formats
  and upstream fidelity loss (#758), ownership migration (ADR 0050/#416–#425).

## Threat model

- Boundary inventory: no new transport/authority boundary. Existing authenticated
  requests accept ALAC; untrusted media probe facts feed settings and argv;
  output facts cross back to existing publication checks.
- Actor model: authenticated operators supply policies/media; media facts are
  untrusted; configured FFmpeg/FFprobe and authenticated worker deployment trusted.
- Controls per boundary: existing auth/lease checks, closed token/default profile,
  checked per-stream resolution and bounded layouts, separate argv arguments,
  existing confined paths/no-overwrite/hash/deadlines, verified output facts.
- Explicitly out of scope: transport replacement, process containment/provisioning,
  library execution and byte-ownership migration under their existing owners.
