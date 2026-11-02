# 0127 — Content normalization failures affect one file

## Status

Accepted (2026-11-01). Implements #818.

## Context

The ffprobe normalizer currently labels rejected file metadata `MalformedWorkerResult`.
The strict scan pump correctly treats that as a fatal provider failure, so one malformed
compatibility tag, disposition, or numeric fact prevents unrelated files from publishing.
ADR 0122 already uses `MalformedMedia` for a snapshot rejected by the content-size bound.

## Decision

The normalizer emits `MalformedMedia` for invalid content values in correctly represented
provider fields: empty/conflicting compatibility tags, invalid disposition values, numeric
parse/range/finite failures, and inconsistent or out-of-range audio facts. It retains
`MalformedWorkerResult` for provider JSON structure/type violations and internal encoding
errors. Existing checked provider structure/types are validated across the document before
content interpretation, so a provider defect also wins on mixed invalid input. Audio field
types are included so shared-parser semantic errors need no message-string matching. Existing handler translation and the
strict scan-pump classification remain unchanged. No message shape or version changes.

## Consequences

Rejected files are observed without identity evidence while other files can publish at
session completion. Provider defects still fail the session. Classification tests pin both
sides of this distinction; no successful snapshot or media bytes change.

## Considered & rejected

- **Translate every normalizer error to MalformedMedia.** judgment: hides provider defects.
- **Forgive MalformedWorkerResult in the scan pump.** verified: `classify_single_file_error`
  at cbbdb185 applies to hash and probe workers; widening it would conceal other provider defects.
- **Keep the current classification.** verified: #818's diagnostic normalizer replay at
  cbbdb185 returns MalformedWorkerResult for a `default = "maybe"` disposition (cargo exit 101
  when the expected class is MalformedMedia), preserving the reported failure.
