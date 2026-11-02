# Bounded external worker diagnostics

Source: #834; WORK:SCOPE q834-7d1858bb. Full-spec, complexity M, fixed denominator 250.

## Problem

A failed ffprobe copies unbounded stderr into both error-frame message fields.
The default NdjsonReader rejects lines over 65536 bytes, losing the domain classification.
The sibling ffmpeg and mkvmerge command_error functions concatenate unbounded external
stdout/stderr and their handlers duplicate the resulting message in the same way.

## Scope and architecture

Keep subprocess execution, full-output collection and classification with each worker.
Add bounded_error_diagnostic(&str) -> String beside the protocol NDJSON codec and export it.
It retains strings at most 4096 UTF-8 bytes unchanged; for larger strings it retains a
suffix of at most 4096 bytes, advancing the cut to a character boundary, and prepends
`[truncated N UTF-8 bytes] ` where N is the exact byte length omitted from that input.
Lossy subprocess decoding precedes bounding; accounting names decoded UTF-8 bytes,
including replacement characters, rather than claiming original subprocess-byte counts.

Ffprobe classifies using its complete decoded stderr before bounding stderr.trim().
Its exit status and fixed context remain outside the bounded portion. Ffmpeg retains
its existing full-stderr classifier and bounds only the concatenated stdout/stderr in
command_error. Mkvmerge bounds the same concatenated diagnostic, retaining status.
No dependency edges, frame fields, payload shape, class/code mappings or protocol version change.
The existing duplicate messages remain for compatibility. Three callers share one transport
budget; no classification logic moves and no obsolete compatibility path is introduced.
ADR [0128](../../adr/0128-bounded-worker-error-diagnostics.md) records this choice.

## Success

For the three subprocess-exit error paths named above, terminal frames containing hostile
control characters, invalid UTF-8 or multibyte text decode through the default NdjsonReader
with their original class/code. Exact retained diagnostic suffix and dropped-byte marker
are observable. The classifier must recognize a malformed-media marker before a long suffix
which removes that marker from the displayed text. Ordinary short diagnostics remain unchanged.

The complete serialized frame is bounded: JSON escapes one input byte to at most six bytes;
two 4096-byte diagnostic copies consume at most 49152 bytes. The short truncation marker,
status, worker error prefixes, fixed stage, u64 lease/sequence, RFC3339 timestamp and enum
names fit in the remaining 16384 bytes. Tests serialize full error frames rather than
assuming the raw-string bound proves this. These exit paths contain no request path/context
outside the bounded subprocess diagnostic; future additional context must retain this bound.

## Global Constraints

Use the pinned Rust 1.95.0 toolchain, existing dependencies, sibling unit-test files,
mandatory hooks and just guardrail recipes. Host Linux x86_64; no target architecture declared.
Use synthetic temporary media only. Do not use the shared build cache before coordinator grant.
Do not modify #818 normalization semantics, source media, persisted schema or error codes.

## Failure model

1. Actors and deployments: local worker executables and HTTP clients on supported project
   hosts; CI using executable subprocess stubs; media bytes/metadata can be hostile.
2. Invariants and assets: deliver classified terminal errors below the default frame cap;
   retain stage/status and class/code; valid UTF-8 diagnostics; source files unchanged.
3. Accepted failure classes: subprocess output still collects in memory, as required for
   existing complete-stderr classification; memory exhaustion before classification is
   unchanged by this transport fix. Diagnostics intentionally lose a reported prefix.
4. Covered elsewhere: timeout/process lifetime and startup-version output handling remain
   existing worker ownership; successful snapshot limits are ADR 0122; normalization is ADR 0127.

## Threat model

- Existing boundary: external tools supply untrusted bytes; no new or widened boundary.
- Actor: local media producer can induce arbitrary tool diagnostics; tool execution retains
  existing configured-binary trust and argv handling.
- Control: lossy decoding, UTF-8-safe suffix bound, explicit loss accounting, serde JSON
  escaping, and default NdjsonReader cap. No authorization or destination changes.
- Out of scope threats: prior collection-memory exhaustion and diagnostic privacy filtering;
  this fix neither changes the collection strategy nor adds new exposed diagnostic sources.

## Validation and audit

Add a stub ffprobe producing over 64 KiB stderr and exercise actual HTTP dispatch/default
reader before the fix; require FrameTooLarge as the observed red cause. Cover malformed
and transient classes, marker outside tail, controls, multibyte cut and invalid bytes.
Protocol unit tests pin empty/exact/over-bound behavior, accounting and doubled worst-case
serialized frames with u64-max identifiers. Sibling tests feed oversized stdout/stderr
through their actual error conversion/frame constructors and default reader, preserving classes.
Audit FfprobeConfigError::Exit: it is startup stderr rather than a terminal frame; its pipe
lifecycle is independent, so record any candidate without absorbing it. Check remaining worker
external-output paths for the same error-frame root cause; do not rewrite unrelated errors.
Run focused tests/red proof, just fmt-check, just lint, mandatory hooks and final just ci;
report explicit ignored test counts. Revert the logical commit for rollback; no data migration.
