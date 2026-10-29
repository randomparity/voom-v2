# Copied audio packet preservation (#786)

## Problem and authority

A 700-sample 48 kHz FLAC packet muxed by MKVToolNix has observed duration15ms.
Native FFmpeg8.1.3 and the project9.0.2 build remux it as14ms when a non-PCM
track is encoded. Payload/PTS/DTS are unchanged. The current project duration
patch is gated by pcm_wavex, which only PCM requests enable.
Authority: [frozen scope](https://github.com/randomparity/voom-v2/issues/786#issuecomment-6496872502).
The approved preserve-or-reject policy retains ADR0119 native optionality and
PCM strict facts/samples/timing. No public or persisted schema changes.
FFprobe8.0.1 observes14ms for the same explicit15ms source bytes. Preservation
therefore proves the configured probe's observations, not unobserved container fields.
Use the current paired project image/probe for strict BlockDuration evidence.

## Scope and ownership

Extend the FFmpeg worker: the media owner compares copied-audio observations
before returning a successful result. Handler finalize/publication ordering
stays intact. The control plane neither reads bytes nor acquires new duties.
Native startup and encoder inventory remain unchanged. A native operation can
succeed when its output proves preservation; unsafe output fails before publication.
No copied audio means no packet scan. Synthesis copies every original audio track;
ordinary transcode copies audio ordinals outside selected provider stream indices.
Source/output audio ordinals map originals independently of attachment positions.

The project mux patch gains default-off preserve_audio_duration, independently
probed in existing Matroska help. It emits positive incoming audio BlockDuration
without selecting ACM/WAVEX; existing pcm_wavex retains its accepted behavior.
Non-PCM worker MKV output enables only the duration option when available.
Both modes preserve input timestamp origin and disable negative timestamp shifting.
PCM keeps its explicit layout guessing and existing capability prerequisite.
No source mutation, automatic installation or fallback encoder is introduced.

## Failure model

- Actors and deployments: local operators and authenticated owner workers;
  native Linux/macOS and the existing project Docker/Podman deployments, plus CI.
- Invariants and assets at stake: copied packet payload/order/PTS/DTS/duration,
  original media bytes, user metadata/dispositions, lineage and publication safety.
  Reject missing streams, invalid fields/time bases, probe errors and timeouts.
- Accepted failure classes: absent source timing is an explicit observation,
  equal only to absent output timing, never inferred. Additional packet scans
  cost two sequential reads of copied audio; per-stream digests bound memory.
  Existing source hashes/commit boundaries own source races; no snapshot claim.
- Covered elsewhere: aggregate matrix/all-target acceptance (#764), real-library
  runs (#339), generic test framework (excluded/no owner), additional features
  (#758), ranking (#765/PR766), and ADR0050 migration (#416–425).

## Packet proof and process bounds

A worker-private module invokes configured ffprobe with select_streams=a,
show_packets, show_data_hash=sha256 and compact key/value output. Explicit
packet_side_data= suppresses nested side-data fields; accept its trailing empty
section delimiter, while rejecting unknown fields and empty interior sections. Read only
stream_index,pts,dts,duration,data_hash. Map indices using observed source/output
stream metadata and audio ordinal. Stream discovery also rejects error-level
diagnostics before deciding no audio is
copied; the existing metadata helper retains its audio/video/extraction callers.
For each copied stream, maintain count and SHA256 digest over canonical records;
never retain all packets. Time base is a
checked positive rational. Canonical reduced i128 rational values preserve exact
integer equality at identical time bases and permit exactly equal time values
across containers. Rounding or rescaling drift fails. Absent timing is distinct
from zero. Reject duplicate/unknown fields, malformed values and records for an
unknown audio stream. Preserve packet order within each stream.

Bound individual lines to1024bytes and stderr to16384bytes. Drain both pipes
concurrently with waiting. The existing process_timeout bounds the entire probe,
including readers and wait.
The probe uses error verbosity; any diagnostic fails even with a zero exit status,
using a static sanitized error rather than forwarding tool diagnostics.
Kill and reap the direct child on error/timeout; kill_on_drop retains cancellation
cleanup consistent with current worker commands
and installed launcher guardian. No unbounded stdout capture or detached reader.
Errors name copied-audio preservation and actionable project-build capability;
paths/media payloads do not enter diagnostics. No partial digest authorizes success.

## Security boundaries

Media and paired-tool output are untrusted; validate compact fields and positive
time bases, cap buffers, check arithmetic, and fail closed before publication.
The local operator selects trusted executable paths using existing configuration;
commands use argv, no shell. Existing installer grants/env_clear/guardian remain
unchanged. No network, credentials, protocol or authorization surface is added.

## Success and validation

- A generated MKVToolNix short FLAC sentinel reproduces15-to14 with a native
  encoder/current project probe and fails before publication. A native-pair WAV
  synthesis control rejects exact700/48000-to14/1000 duration drift. Safe native
  fixtures use complete frames and retain samples/layouts/metadata assertions.
- The project build preserves exact copied MKV tuples for six non-PCM targets
  in ordinary and strict-downmix synthesis, with metadata/dispositions, source
  SHA256 and lineage intact. Dedicated actual authenticated CLI proof checks
  committed output plus a native negative path with no published artifact.
- Existing PCM16/24 strict layout/sample/timing tests remain green. A copied
  known-layout PCM track in a non-PCM request retains its native representation.
- Parser/process tests cover malformed/missing/duplicate fields, absent values,
  negative/offset timing, rational equality/drift, copied Opus skip-sample output,
  huge output, timeout, reader
  hang and nonzero exit. Each new executable contract has red/green evidence.
- Run focused worker and CLI proofs, source guards, lint, and final justci.
  Keep native Linux/macOSCI and required project-container proof. Report actual
  local arms and the suite's existing ignored platform cases truthfully.

## Alternatives

A blanket project-build requirement changes accepted native optionality.
Application EBML rewriting duplicates FFmpeg's container ownership. Reusing
pcm_wavex for non-PCM changes copied PCM headers unnecessarily. Duration-only
mux output plus operation verification extends existing owners without those costs.

## Global constraints

Rust1.95; existing dependencies only; native Linux/macOSCI retained; pinned
FFmpeg9.0.2 project images retain native amd64/arm64 build support. No dependency,
MSRV, persisted setting, protocol version, or byte-ownership change.
