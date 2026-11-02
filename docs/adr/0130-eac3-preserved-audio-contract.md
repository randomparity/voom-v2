# 0130 — Preserve channels during E-AC-3 transcoding

## Status

Accepted (2026-11-01). Implements #832's operator-approved plan-time-block choice.

## Context

Ordinary transcode preserves selected channel count/title; explicit synthesis adds a downmixed
companion (ADR0026). FFmpeg's native EAC3 encoder does not accept every source layout.
At FFmpeg8.1.3/Linux, generated 7.1/8 becomes 5.1(side)/6 and 6.0/6 becomes 5.0(side)/5
when re-encoded without a channel override. A channel-count ceiling alone is insufficient.
The reported TrueHD title mismatch has separate, currently unestablished causality.

## Decision

Block ordinary EAC3 encodes at planning unless selected source channel/layout pairs belong to
FFmpeg's supported 16-pair layout set. Missing facts block. Preserve true no-ops and filtering.
This partially supersedes ADR0011 only for EAC3 encode channel/layout admission and ADR0020
only for its unsupported-layout failure assumption; optional title and other-codec rules remain.
The worker re-probes and applies the same admission before encoding, retaining the existing
post-encode title/channel checks. Use existing explicit `synthesize audio` for intentional downmix;
do not add or reinterpret ordinary transcode parameters. Keep the small admission table local
to each current owner rather than add a shared cross-crate surface for two occurrences.

## Consequences

Unsupported sources fail with an actionable planning reason before staging work. Older snapshots
without EAC3 layout facts need fresh facts before encoding. Supported three-through-six-channel
layouts remain accepted; this is not an AC3-style mono/stereo/5.1 restriction. New encoder
layouts require deliberate admission. Explicit synthesis retains the original multichannel
track; it does not authorize a subsequent unconditional EAC3 transcode of that original.
Title checks remain intact; bounded synthetic evidence cannot establish the original canary's
unavailable title failure, which requires an explicit operator completion disposition.

## Considered & rejected

- **Implicitly downmix ordinary transcode.** judgment: violates the operator's channel-preservation
  direction and existing ordinary-transcode semantics.
- **Only reject more than six channels.** verified: FFmpeg8.1.3/Linux command replay from generated
  FLAC 6.0, `ffmpeg -i source.mkv -map 0 -c copy -c:a:0 eac3 -b:a:0 576k out.mkv`,
  probes as five-channel 5.0(side), while source probes as six-channel 6.0.
- **Reuse AC3's mono/stereo/5.1 table.** verified: `ffmpeg -hide_banner -h encoder=eac3`
  on FFmpeg8.1.3/Linux advertises 16 layouts including 3.0, 4.0 and 5.0.
- **Drop channel/title postchecks.** judgment: hides data loss instead of preventing it.
- **Change title mapping speculatively.** judgment: current native title-preservation controls
  do not establish the original title failure's cause; retain the contract and report limits.
