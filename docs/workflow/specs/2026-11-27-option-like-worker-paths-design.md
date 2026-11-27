# Reject option-like worker paths (#260)

## Problem and scope

Issue #260 identifies missing parity with ffmpeg's existing first-byte-dash
rejection. The ffprobe probe command and mkvmerge identify/remux commands
currently accept an option-like path as a subprocess argument.

The frozen authority is issue #260, scope token `q260-9f97589b`, and the
operator-approved campaign scope. Exclusions are empty. The permitted surface
is the two affected worker crates, their sibling tests, and these workflow
artifacts. Complexity is M; the fixed design denominator is 250 changed lines,
from the live issue/adapter assessment frozen in WORK:SCOPE.

## Design

Mirror ffmpeg's `as_os_str().as_encoded_bytes().first() == Some(&b'-')`
predicate at the affected subprocess boundaries. Keep adapter-specific errors
local. No ownership transition, shared protocol policy export, dependency,
schema, migration, or ADR decision is needed: this extends the accepted ffmpeg
pattern to the two missing adapters.

`run_ffprobe_json` rejects before command construction. Add a `ConfigInvalid`
ffprobe error with `FailureClass::MalformedWorkerResult`,
`ErrorCode::ConfigInvalid`, and `input_path` as its payload stage, mirroring
ffmpeg. Override code derivation only for this variant.

`handle_remux` checks raw input then output immediately after request-contract
validation, before filesystem observation or identification.
`build_mkvmerge_args` also checks input then output before constructing arguments;
`identify_json` checks its path before command construction, covering both input
and output identification. Reuse mkvtoolnix's existing `ConfigInvalid` error.
Error messages name the field and leading-dash restriction. Ordinary paths,
including `./-clip.mkv` and an absolute path with an internal dash-prefixed
component, remain accepted by this check.

The smallest extension is adapter-local validation. Shared extraction was
explicitly optional in the operator-approved dispatch; a new public protocol
helper would widen ownership without improving the required rejection.

## Success

- ffprobe rejects a first-byte-dash probe argument before invoking its binary.
- mkvmerge identify and remux reject their first-byte-dash path arguments before
  invoking their binary; remux validates both input and output.
- Rejection uses `ConfigInvalid` through existing worker envelopes.
- Non-leading dash components are accepted by the new predicate.
- Focused regressions, repository hooks, `just ci`, and remote CI pass.

## Failure model

- Actors and deployments: authenticated local control-plane requests received by
  standalone workers; CI exercising worker unit tests.
- Invariants and assets: path data must not become subprocess options; rejected
  inputs must not launch a child; existing error envelopes retain their shape.
- Accepted failure classes: tool failures and unsupported media for ordinary
  paths retain existing handling. This change does not claim filesystem path
  confinement or reject internal dash-prefixed components.
- Covered elsewhere: worker authentication and request schemas remain owned by
  worker-protocol; file identity and staging checks remain owned by existing
  worker handlers and observers.

## Threat model

- Boundary inventory: request-controlled path enters an external media tool's
  argument parser. No boundary is added or widened.
- Actor model: local operator/control plane provides paths; defensive validation
  prevents malformed or crafted path data becoming tool options.
- Control per boundary: byte-preserving first-byte validation before command
  execution; rejection discloses only a field name and constant restriction.
- Explicitly out of scope threats: authentication bypass and arbitrary executable
  selection are covered by existing server/configuration controls. Filesystem
  traversal and media-parser vulnerabilities are not changed by this repair.

## Validation

Benign path strings exercise dash rejection and accepted prefixed/absolute paths.
A handler-level remux regression checks the output before any input observation.
A nonexistent executable proves invalid paths return configuration errors before
spawn rather than a spawn failure; no option effects or exploit payloads run.
Existing worker tests cover ordinary command behavior and error serialization.
