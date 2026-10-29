# Generated audio acceptance — #764

## Problem

The eight implemented audio targets need aggregate, truthful CLI acceptance and a
support matrix (#764/#758, frozen WORK:SCOPE q764-50b114a3).

## Scope

Extend existing authenticated acceptance-bootstrap/owner-encode entry points with
one focused generated-audio runner in crates/voom-cli/tests/support, committed
policy fixtures, just invocation and required Linux container CI execution.
Retain existing native Linux/macOS suites; label platform evidence by test level.
No ownership transition: CLI/control plane orchestrate; owner node workers read
and mutate bytes. Reuse installed private FFmpeg/FFprobe shims through existing
typed dependencies. No production, schema, dependency, ranking or default change.
The approved exclusions/owners remain in WORK:SCOPE; no new ADR or migration.

### Failure model

Actors/deployments: authenticated test operators on local Linux Docker and CI;
existing native Linux/macOS suites remain independent. Assets: source bytes,
selected audio fidelity, unaffected packets, user metadata/dispositions, lineage,
private credentials and honest test execution. Required cases fail on absent tools,
empty selection, unsupported facts or output mismatch. No accepted failure class
for these assertions. Known native copied-lossless duration rounding must be
reported with evidence, never hidden by fixture choice or weakened assertions;
production corrections require root scope decision. Real libraries (#339), new
runtime features (#759–#763), broader formats (#758), ranking (#765/PR766) and
ownership migration (ADR0050) retain their approved owners.
Threat controls: existing authenticated bootstrap owns/reaps children and secrets;
only new private fixture roots are granted, installation stays outside grants;
commands use separate argv, bounded subprocesses and immutable generated inputs.

## Success

Committed fixtures execute ordinary AAC/Opus/E-AC-3/AC-3/FLAC/ALAC/PCM16/PCM24 and
explicit strict downmix where supported through actual CLI/owner workers. Probe
codec/rate/layout/precision; compare signed integer archival samples canonically.
Compare downmix against an independent reference transform; retain originals and
non-default companions. Assert source hashes, unaffected packet tuples, metadata,
full dispositions and durable lineage. Repeated compliant planning is a no-op.
Exercise missing encoders, invalid facts/layout/format and precision reduction
before publication. Matrix separates product encoding, build availability,
decode/copy/no-op, constraints, exclusions, prerequisites and actual proof.

## Validation

- Mode: focused-test. New audio runner executes exact nonempty case inventory via
  `just audio-acceptance docker IMAGE`; initial absent runner/fixtures is red.
  Controlled sample, copied-packet, default and no-op faults must fail assertions;
  restore each. Negative cases require failure diagnostics and no output/source
  mutation. Actual CLI evidence covers all eight targets; existing deeper worker
  matrices supply orthogonal layout/error coverage, explicitly identified.
- Mode: focused-test. Required CI invokes the same runner; removing a required
  case fails its exact inventory check. Run `just source-checks`, `just fmt-check`,
  `just lint`, mandatory hooks, final `just ci` and hosted CI on the exact head.
- Mode: task-test-not-applicable. Human-readable support matrix claims are checked
  against code and actual receipts; no executable consumer validates prose.
