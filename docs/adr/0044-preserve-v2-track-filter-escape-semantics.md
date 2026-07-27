---
status: accepted
date: 2026-07-27
deciders: [VOOM core]
---

# 0044 — Preserve V2 quoted track-filter escape semantics

## Context

Published `title contains <quoted-string>` filters already accept backslash
pairs. The source scanner treats a backslash as escaping the next character
while locating the closing delimiter, but compiled-policy schema V2 lowers the
complete lexeme with `trim_matches('"')`.

For most values this retains the source spelling between the delimiters. When
an escaped quote is immediately before the closing delimiter, however, the
transformation removes both quote characters and leaves the backslash. Existing
compiled policy versions record that result.

Policy version identity prevents an in-place semantic correction:

- `source_hash` is BLAKE3 over the exact accepted UTF-8 source bytes;
- `policy_versions` is unique by `(policy_document_id, source_hash)`; and
- uploading duplicate source returns the existing immutable version before
  recompiling it.

Changing the compiled value for identical source bytes would therefore let one
public source hash denote two meanings across databases or compiler versions.
It would also make upgrade and rollback behavior depend on which binary first
accepted the source.

Design:
[`docs/superpowers/specs/2026-07-27-issue-358-track-filter-escape-semantics-design.md`](../superpowers/specs/2026-07-27-issue-358-track-filter-escape-semantics-design.md).

## Decision

Compiled-policy schema V2 keeps its historical quoted-title transformation as
the normative contract:

1. A backslash escapes the next Unicode scalar value only for locating the
   closing quote.
2. Backslash pairs are not decoded.
3. Lowering removes the opening delimiter and the maximal trailing run of
   quote characters, including the closing delimiter.

Consequently:

- `"Director \"Cut\""` compiles to the source-spelled value
  `Director \"Cut\`;
- `"Path\\Name"` compiles to `Path\\Name`; and
- `"\"Quoted\" middle"` compiles to `\"Quoted\" middle`.

The implementation names this V2 transformation and expresses it directly
instead of relying on a generic quote-trimming helper. Historical, current,
upgrade, and rollback tests pin the exact source hash, compiled values, and
stored-version behavior.

No source spelling, compiled field, discriminator, schema version, provenance
format, hash algorithm, or hash preimage changes.

## Consequences

- Identical accepted source bytes retain identical compiled meaning under old
  and current binaries.
- Fresh databases, upgraded databases, duplicate uploads, and rolled-back
  binaries agree on the same immutable V2 policy version.
- Escapes remain source-spelled rather than decoded. A terminal escaped quote
  retains the historical, unintuitive trailing-backslash value.
- Operators can see the behavior in the published language contract and
  compatibility fixtures instead of depending on an incidental library call.
- A future corrected or decoded string model must introduce a distinct source
  identity domain before accepting source whose compiled value would change.

## Considered and rejected alternatives

### Decode backslash escapes in place

Rejected. Existing source hashes would acquire new compiled values and violate
immutable policy identity.

### Strip exactly one opening and closing delimiter

Rejected for schema V2. It fixes the terminal escaped-quote result but changes
the meaning of source bytes accepted by prior compilers.

### Include compiler or schema version in `source_hash`

Rejected. The public hash contract is exact source bytes, and the database
uniqueness model uses that hash to identify duplicate source. Changing the
preimage would be a broader registry migration and rollback contract, not an
escape-parser fix.

### Add a new DSL escape or source-version marker

Rejected. Issue #358 does not publish a new grammar production, and the
campaign explicitly excludes parser-only or unpublished forms.
