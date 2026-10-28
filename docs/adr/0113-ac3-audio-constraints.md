# 0113: Bound AC-3 settings and check encoder availability per operation

## Status

Accepted

## Context

Issues #759/#758 require AC-3 mono/stereo/5.1 transcodes and explicit surround
downmix synthesis without silently downmixing ordinary transcodes or requiring
unrelated audio encoders. The current audio profile resolves per-channel bitrate.

This supersedes ADR0020's assumption that native audio encoders require no
availability check; its existing AAC/Opus/E-AC-3 bitrate decision is unchanged.

## Decision

Add ac3 to that existing contract at 96 kbps/channel (96/192/576 kbps for
1/2/6 channels). Ordinary AC-3 transcodes accept known mono, stereo, 5.1 and
5.1(side) layouts matching their channel counts. Unsupported or unknown facts
fail before output creation. Six-channel output is canonical 5.1(side).
Explicit strict-downmix synthesis accepts 1/2/6 target channels and preserves
original streams and non-default companion semantics.
Record optional audio encoders during existing preflight; reject only the
encoder needed by an operation before output work. Do not add a startup
requirement for AC-3 or retain unrelated AAC/Opus startup requirements.

## Consequences

The default profile and durable payload shape stay unchanged. AC-3 is lossy.
Deploy new binaries before policies naming ac3. No runtime fallback/installation.
Video/muxer prerequisites and compatibility-track behavior remain unchanged.

## Considered & rejected

- Add an AC-3 settings object: judgment: unnecessary public/persisted surface
  for a fixed lossy profile with three supported channel counts.
- Allow FFmpeg to choose channels: judgment: implicit downmix violates #758.
- Require audio encoders at startup: judgment: prevents unrelated operations
  and violates the per-operation availability requirement.
- Treat 5.1 and 5.1(side) as distinct output choices: verified:
  FFmpeg 8.1.3 generated 48kHz MKV AC-3 encodes from both layouts at 576k,
  probed with `ffprobe -show_entries stream=channel_layout`, return 5.1(side).
- Leave AC-3 unsupported: judgment: leaves #759's requested experiment blocked.
