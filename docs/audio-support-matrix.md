# Audio encoding support and availability

VOOM's closed encoding target set is AAC, Opus, E-AC-3, AC-3, FLAC, ALAC,
`pcm_s16le` and `pcm_s24le`. Ordinary audio transcode and explicit strictly-downmix
synthesis produce MKV using the `default` audio profile. Synthesis retains the
original streams and adds non-default companions; it does not create a track
without an explicit policy operation or change track ranking/default policy.

The table separates the product target from its FFmpeg encoder. An installed
FFmpeg build must provide the requested encoder. Recognition, decoding, stream
copying and an already-compliant no-op do not establish encoding availability.

| Policy target | Required encoder | Ordinary conversion | Explicit downmix | Settings / bounds |
| --- | --- | --- | --- | --- |
| `aac` | `aac` | Preserves channel count; generated proof uses 48 kHz / 5.1 | Generated 5.1 → stereo | 64 kbps/channel; FFmpeg encoder/layout limits still apply |
| `opus` | `libopus` | Preserves channel count; generated proof uses 48 kHz / 5.1 | Generated 5.1 → stereo | 48 kbps/channel; Opus output is 48 kHz |
| `eac3` | `eac3` | Preserves channel count; generated proof uses 48 kHz / 5.1 | Generated 5.1 → stereo | 96 kbps/channel; broader encoder layouts are not promised by this evidence |
| `ac3` | `ac3` | Known mono/1, stereo/2, 5.1/6 or 5.1(side)/6; ordinary 7.1 rejected | Strict targets 1/2/6; worker proof includes explicit 7.1 → 5.1 | 96/192/576 kbps for mono/stereo/5.1; surround output is 5.1(side) |
| `flac` | `flac` | Signed integer 16/24-bit; mono, stereo, back 5.1, side 5.1, 7.1 | Strict targets mono/stereo/back 5.1 | Compression level 5; positive source rate ≤655350 Hz; no lossy bitrate |
| `alac` | `alac` | Signed integer 16/24-bit; mono, stereo, **back** 5.1 | Strict targets mono/stereo/back 5.1; known side 5.1/7.1 sources may explicitly downmix | Ordinary side 5.1/7.1 rejected; no lossy bitrate |
| `pcm_s16le` | `pcm_s16le` + project `pcm_wavex` mux capability | Signed integer 16-bit; mono, stereo, back/side 5.1, 7.1 | Strict targets mono/stereo/back 5.1 | Rejects 24 → 16 precision reduction; paired project container tools required |
| `pcm_s24le` | `pcm_s24le` + project `pcm_wavex` mux capability | Signed integer 16/24-bit source; mono, stereo, back/side 5.1, 7.1 | Strict targets mono/stereo/back 5.1 | Emits 24-bit; 16 → 24 padding preserves samples; paired project container tools required |

The lossless rows require observed positive sample rate, matching channel count
and exact layout, signed s16/s16p or s32/s32p representation, and known 16/24-bit
significant precision. Decoder storage width is not precision. Missing,
contradictory, floating or unsupported facts fail before encoding. Ordinary
archival conversion preserves source rate, exact layout and representable
integer precision. Increasing width cannot restore fidelity lost upstream.
A deliberate downmix is a transform, not bit-exact archival equivalence.

The implementation bounds are defined by
[the shared facts resolver](../crates/voom-worker-protocol/src/audio_facts.rs),
[the target/profile contract](../crates/voom-worker-protocol/src/operations/audio.rs)
and [worker encoding/verification](../crates/voom-ffmpeg-worker/src/ffmpeg.rs).
Additional FLAC, ALAC and PCM details are in
[FLAC](specs/flac-archival-audio.md), [ALAC](specs/alac-archival-audio.md) and
[PCM/container tools](specs/pcm-container-audio.md).

## Availability, copying and no-op behavior

The worker discovers audio encoders at startup and checks the requested one
before accessing input or creating output. Missing encoders produce an actionable
worker-build failure. It does not install a codec or substitute another encoder.
If the CLI envelope reports only a terminal failure class, inspect the durable
reason with `voom event list --kind ticket.failed_terminal --limit 20` using the
same database configuration.
Audio encoders are optional individually: a FLAC job does not require ALAC,
for example. Existing general worker startup requirements still include the
video encoder and Matroska/MP4/Ogg muxer preflight; an audio-only FFmpeg build is
not automatically a usable VOOM worker build.

Native executable selection remains supported for non-PCM targets. Merely finding
native PCM encoder names is insufficient: ordinary stock Matroska PCM omits the
required observed layout. PCM therefore checks the opt-in project mux capability
and requires the paired installed project FFmpeg/FFprobe launchers. Configure
those executable paths through the node agent's existing typed dependency fields;
plain parent environment exports do not cross the owner's cleared environment.

Unselected streams are copied, subject to the installed decoder/demuxer/muxer
capabilities and the verified publication constraints below. Already matching selected audio in
MKV plans as a no-op without demanding an otherwise-unused encoder. A codec
recognized in an input or copied by a muxer is not an additional supported target.
Neither decoding a lossy source nor copying an encoded stream proves archival
sample fidelity.

## Preservation evidence and publication constraints

Generated acceptance uses immutable MKV sources with video, a selected integer
track, a short unselected FLAC track, a subtitle, user metadata, and accessibility,
forced, commentary and default dispositions. Every ordinary target and every
5.1 → stereo synthesis traverses the actual CLI, authenticated API/control plane,
owner node agent, real workers, verification and committed publication. The
checks cover source hash/mtime, copied packet payloads, user tags, full
dispositions, durable synthesis lineage and ordinary no-op planning. Archival
outputs compare canonical decoded signed integers; downmix references explicitly
represent the requested precision rather than an ideal wider intermediate.
Generated `ENCODER`/`DURATION` bookkeeping and reserved stream-identity markers
are distinguished from user metadata.

The current project muxer advertises optional `preserve_audio_duration` separately
from the PCM layout capability. The worker enables it for audio transforms and
verifies copied audio packet payloads and rational PTS/DTS/duration before
publication. Source representation and installed muxer/probe behavior therefore
constrain successful conversion as well as encoder availability. A preservation
mismatch rejects the result before publication; existing owned staging data may
remain. Native startup is supported, but success on arbitrary copied audio is
not promised: stock FFmpeg 9 can omit WavPack packet durations or round AAC timing.

The acceptance fixture pins FLAC block size and asserts that the installed
project probe observes one short copied packet with duration 15 in time base
1/1000. Before [the prerequisite fix](https://github.com/randomparity/voom-v2/pull/795),
all six non-PCM targets changed that duration to 14 in both ordinary and synthesis
operations (12 failures); payload, PTS and DTS remained unchanged. The strict
assertion remains in the aggregate runner. This is actual packet timing,
independent of regenerated textual `DURATION` tags.

Use a fresh installation of the current paired project image and probe for this
strict evidence. FFprobe 8.0.1 can report 14 ms for a container BlockDuration that
newer probes observe as 15 ms; matching old-probe observations do not establish
raw container preservation. An existing installation retains its immutable old
image until explicitly replaced. See [copied-audio timing](../containers/ffmpeg/README.md#copied-audio-timing)
for the capability, native rejection and separately executed prerequisite proof.

## Prerequisites and test levels

For generated acceptance, provision the Rust toolchain and repository tools with
`just setup`, native FFmpeg/FFprobe for generated inputs, MKVToolNix including
`mkvmerge`, host Python 3.13 or newer for the existing acceptance bootstrap,
and a working supported local container engine. Build
the pinned image with `just container-build docker` (or `podman`). The
[container specification](specs/pcm-container-audio.md) defines supported engine
profiles, mount/account restrictions, required native architecture and installer
prerequisites. Use a full immutable image ID or repository digest, never a mutable
tag or an abbreviated image ID. The acceptance runner installs private shims
outside all media grants and runs owner children from the granted staging root.
For existing macOS Desktop selections, use
`just audio-acceptance ENGINE IMAGE WORKDIR DESKTOP`; this passes the explicit
Docker context or Podman machine to the existing installer. It does not configure
Desktop or establish aggregate macOS acceptance evidence.

| Evidence | Actual scope |
| --- | --- |
| Focused committed policies in `crates/voom-control-plane/tests/fixtures/policies/audio-acceptance/` | Eight ordinary and eight explicit-downmix policies; compilation is separate from execution |
| `real_owner_audio.py` / `just audio-acceptance ENGINE IMAGE` | Linux amd64 Docker actual CLI/control-plane/owner-node pipeline; sixteen positive and six negative cases, including strict copied timing |
| `voom-ffmpeg-worker/tests/transcode_conformance.rs` | Required native Linux/macOS real-worker codec, layout, precision, downmix and rejection coverage; this is worker-level evidence |
| `just container-worker-check ENGINE IMAGE` | Required PCM worker matrix plus real owner-child cleared-environment proof; not aggregate CLI proof |
| Existing native Linux/macOS `just ci` arms | Repository guardrails and native suites; opt-in container/hardware/network/scale tests are separately identified and must not be described as executed by the native suite |

The earlier PCM runtime proofs on Linux Docker/Podman and macOS Docker/Podman
Desktop establish those runtime/worker deployment arms. They do not establish
new aggregate CLI acceptance on macOS or Podman. Representative real-library
runs and reports remain [#339](https://github.com/randomparity/voom-v2/issues/339).

## Exclusions

There is no new container/export format, profile CRUD, arbitrary FFmpeg argument
surface, automatic compatibility-track creation, same-channel companion, upmix,
loudness processing, object-audio authoring, or ranking/default change here.
MP3, Vorbis, MP2, WavPack, TTA, WMA, broader PCM, alternate AAC/Opus encoders,
DTS-core/TrueHD/MLP authoring and telephony/Bluetooth/game codecs remain outside
this bounded target set. Byte execution remains with the storage owner; this
acceptance does not extend control-plane filesystem ownership.
