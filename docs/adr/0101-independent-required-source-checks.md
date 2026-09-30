# ADR 0101: Schedule source checks independently and require their results

## Status

Accepted — 2026-09-30, issue #640; operator approved the two-platform design
and separate guard-only protection administration.

## Context

Source guards currently precede tests in both CI platform jobs. They inspect source
and run shell fixtures, independently of Cargo output. Local developers still need one
complete CI recipe. Main has no required checks at the recorded preflight.

## Decision

Partition the existing `ci` leaf recipes into `source-checks` and `platform-checks`.
Run the source group in independent Ubuntu and macOS jobs; require both guard check
contexts through separately verified main protection. Preserve both platform suites
and leave local `ci` complete. Keep the guard implementations and hooks unchanged.

## Consequences

Compilation can overlap source checks. Additional runner setup may increase total runner
time; repeated measurements must report both cost and latency. GNU/BSD fixture coverage
is retained. Workflow configuration alone does not enforce merging: verify required-check
administration and an intentionally failing source guard before claiming completion.

## Considered & rejected

- Keep guards inside test jobs. judgment: retains the scheduling dependency #640 removes.
- One Linux guard job. judgment: dropping BSD execution is unnecessary coverage loss.
- Make test jobs depend on guards. judgment: retains the critical-path delay and relies
  on required platform contexts that the operator has not chosen to introduce.
