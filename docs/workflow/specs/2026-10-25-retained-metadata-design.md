# Retained misleading metadata regressions (#744)

## Problem
Seven pinned upstream metadata cases lack production scan/probe coverage (#744,
parent #740). Reuse the owner stack and persisted exporter from #742.

## Scope
Generate seven bounded scenarios from pinned `edit-metadata.yaml`, mirroring
`test_misleading_metadata_real.py`: embedded title, year 9999, 4096-byte Unicode
title, tab/newline title, deletion after prior title/year, combined title/year,
and absent tags with eng/fra audio labels. Materialize with the pinned generator.
Use the existing owner scan process with an explicit existing-fixture selector;
retain its default healthy/invalid fixture, Bootstrap, settlement and probe shim.
Export actual SQLite snapshots, assert authored expected tags and audio labels,
and compare persisted probe evidence with the pinned oracle. Mutate only a copy
of the exported evidence to prove title, year and language differences are caught.
The NFO title is fixture disagreement evidence only; no NFO ingestion or spoken
language claim is made. No ownership transition or production contract changes.
Exclusions/owners approved by the human through campaign dispatch: exporter and
harness #742; track/ASS #743; corruption #745; corpus/replay #746; workflow/soak
#747; NFO ingestion/spoken-language inference ingestion/domain proposal; product
normalization separately scoped voom-ffprobe-worker issue.

### Failure model
- Actors/deployments: equipped local/weekly heavy-suite operators, temporary
  libraries and existing authenticated production owner stack; no live libraries.
- Invariants and assets: dropped/changed tags, absent evidence, wrong language labels,
  unexecuted cases, unsuccessful probes, or a comparator accepting injected drift.
- Accepted failures: tool absence makes the explicitly ignored heavy suite
  unavailable, reported as such; cross-version encoded-byte equality is excluded
  because assertions compare retained semantic evidence.
- Covered elsewhere: existing bounded process supervision and Bootstrap cleanup
  own child lifetime. Assert seven cases, durable success and evidence before comparison.

## Success
Seven cases retain exact expected supported tags (including observed empty maps)
and audio labels after real production scan/probe. Each has durable success and
actual worker probe receipts. Pinned comparison accepts actual supported evidence
and rejects deliberate title/year/language drift. No NFO/spoken-language inference.

## Validation
- Case generation and persisted assertions: focused-test: ignored production
  metadata regression in `chaos_librarian_e2e`; assert seven named cells, fixture
  mutation history, exact persisted export tags/labels, successful durable outcomes.
- Comparator: focused-test: same regression exercises pinned oracle and deliberate
  mismatch negatives; reject null/missing tags and absent stream-label evidence.
- Fixture selector/default preservation: focused-test: existing production scan
  regression plus new metadata regression through the same process helper.
- Design prose: task-test-not-applicable: no executable contract; enforce four
  sections, failure model, success inventory, 500 words and 60 lines.
- Equipped `just chaos-e2e-ci`; repository-required final `just ci` before push.
