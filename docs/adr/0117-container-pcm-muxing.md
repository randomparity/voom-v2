# 0117 — Preserve PCM layout with an opt-in FFmpeg container

## Status

Accepted (2026-10-28)

## Context

Issue #763 requires integer PCM16/PCM24 in MKV with observed channel layout and
sample fidelity. ADR 0114 rejects missing layout. The operator explicitly chose
a custom FFmpeg build in Docker/Podman and execution inside the container,
including macOS Docker/Podman Desktop.
Stock FFmpeg's native PCM Matroska representation does not provide the required
observed layout. Byte ownership remains with the owner worker under ADR 0050.

## Decision

Maintain a small FFmpeg 9.0.2 mux patch and a pinned, verifiable local container
build. An opt-in Matroska option emits standard A_MS/ACM with forced
WAVEFORMATEXTENSIBLE for checked PCM16/24 layouts, including mono/stereo.
Reuse FFmpeg's WAV writer; do not add an application EBML rewriter or infer
layout. Request-specific capability checks precede staging, and strict observed
output verification remains mandatory. Default upstream mux behavior is retained.

Two executable launchers use the existing FFmpeg/FFprobe path overrides. A
strict operator configuration grants explicit same-path RO/RW roots and selects
a local immutable image. Support local Linux Docker rootful without userns-remap
and rootless Podman keep-id, plus macOS Docker/Podman Desktop local VMs;
retain native execution and build native amd64/arm64 images. Container execution is CPU
only and source-label preserving. Require operator-prepared accessible roots;
never relabel existing media or disable SELinux. No image publication is included.

An operation guardian owns create/start/remove, watches a dedicated launcher
lease while the launcher watches the pre-spawn caller identity with Linux pidfd
or macOS kqueue NOTE_EXIT, and force-removes only its owned container on exit
or cancellation. Preserve stdin/stdout/stderr and exit codes. Bound engine and
cleanup commands; report removal failure rather than manufacturing success.
The detailed contracts and validation live in the PCM container specification.

## Consequences

PCM requires the custom mux capability; existing native codecs do not. The opt-in
runtime adds platform-specific Python process-watch capabilities, a local engine
or Desktop VM and prepared mount roots as deployment prerequisites. Image source/signature/base/package pins and build
provenance become project maintenance responsibilities. No serialized setting,
protocol version or control-plane byte-ownership change is needed.

The opt-in mux mode also emits positive observed audio BlockDuration values.
Stock copy-only muxing can discard an explicit final packet duration when it
lacks a default frame duration, changing a 15 ms packet to a codec-derived 14 ms
observation. Preserve those incoming values for copied audio; do not invent
missing durations. Default-off muxing stays unchanged. The generated DURATION
tag may be regenerated, as explicitly authorized, while user metadata and actual
packet bytes/pts/dts/duration remain exact.

The guardian handles launcher SIGKILL and worker death while the engine and
guardian remain available. Engine failure, late daemon creation, or killing the
whole guardian cgroup can prevent removal; no unconditional infrastructure-failure
cleanup guarantee is claimed. Desktop mount sharing and host output ownership require actual platform proof;
GPU passthrough is excluded.
Pinned inputs do not imply bit-for-bit reproducible outputs. Local rollback uses
the previous native executable paths and rejects PCM without its capability.

## Considered & rejected

- **Keep native PCM and infer layout.** verified: FFmpeg 8.1.3 generated ten
  PCM16/24 MKV depth/layout cells; `ffprobe -show_streams -of json` omitted layout
  in all ten while decoded samples matched. The 9.0.2 matroskaenc.c native codec
  dispatch retains the same representation. Inference violates ADR 0114.
- **Application EBML rewriting.** judgment: introduces a second media container
  parser/writer and offset/timestamp correctness surface where FFmpeg already owns
  muxing. The operator selected the FFmpeg build/runtime boundary instead.
- **Bare container CLI with --rm or signal traps.** verified: Docker 29.9.0 and
  Podman 5.8.7 on Linux left an Alpine sleep container running after SIGKILL of
  `run --rm`; `inspect` reported Running=true. Traps cannot handle SIGKILL.
- **In-container framed-stdin lease supervisor.** judgment: stronger daemon-independent
  cancellation adds a helper and stdin framing/buffering beyond the smallest
  guardian. Keep its infrastructure-failure advantage explicit in the spec.
- **Relabel or disable SELinux automatically.** judgment: changes source security
  state or isolation contrary to the operator's source-preservation requirement.
- **Leave PCM rejected.** judgment: no longer meets the explicitly authorized
  container implementation request; retain rejection only for absent capability.
- **Observe parent identity only inside the launcher.** verified: a Linux
  fork/pipe experiment with a live subreaper delayed launcher initialization until
  after worker exit; getppid/pidfd bound the live adopter. Pass expected caller
  identity from the spawner and reject mismatches before creating a container.
