# 0128 — Bound external worker error diagnostics

## Status

Accepted (2026-11-01). Implements #834.

## Context

External ffprobe stderr and sibling ffmpeg/mkvmerge stdout/stderr enter terminal messages.
Handlers duplicate the message in payload.message. The default reader caps JSON frames
at 64 KiB, so unbounded diagnostics can replace a classified failure with FrameTooLarge.

## Decision

Classify complete subprocess stderr first, then use one protocol-owned helper to retain
at most the last 4096 UTF-8 bytes of diagnostic text. Advance the cut to a character boundary
and prefix the exact omitted decoded-byte count. Preserve status, class/code, frame shape
and both existing message copies. Decode invalid subprocess UTF-8 lossily before accounting.
Two copies at JSON's worst six-byte escaping expansion cost at most 49152 bytes before
the short marker and fixed context; full-frame tests verify remaining transport headroom.
The shared bound is used by the three verified subprocess-exit error paths; successful
payloads and startup-version errors keep their existing contracts.

## Consequences

Large diagnostics lose a reported prefix; full classification remains unchanged.
The transport fix does not bound subprocess collection memory. Future error context added
to these paths must preserve the serialized full-frame cap. No schema/version migration.

## Considered & rejected

- **Remove duplicate message fields.** judgment: bounding both copies preserves reader compatibility.
- **Raise MAX_FRAME_BYTES.** judgment: larger allocations leave the external-input defect unbounded.
- **Truncate before classification.** verified: ffprobe.rs at fec074c0 checks malformed-media
  signatures in complete stderr; dropping the prefix can discard the only matching signature.
- **Separate copies of the same bound in each worker.** judgment: three verified callers
  justify one protocol-owned implementation of this transport budget.
- **Leave diagnostics unbounded.** verified: ndjson.rs at fec074c0 rejects JSON lines over 65536
  bytes, while these three external command-error paths currently impose no message bound.
