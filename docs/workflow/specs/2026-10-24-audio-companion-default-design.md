# Preserve companion audio default selection (#737)

## Problem and authority
Issue #737 and the operator-approved bounty require synthesis to keep original audio defaults and make appended companions non-default. Shared metadata copying currently duplicates a default source's flag. Ordinary in-place transcoding must retain existing metadata semantics. Frozen scope: q737-0108ebe4, campaign fc95ce176603-ecf41a3d-b957-4cc3-b2ce-b1fd9d581778.

## Design
[ADR 0110](../../adr/0110-synthesized-audio-companions-are-non-default.md) records the approved default-only exception to ADR 0043 and its historical design; their other decisions remain unchanged.
Keep ownership in the FFmpeg worker. After copying metadata onto each appended companion, apply a synthesis-only relative `-default` disposition adjustment. Explicitly preserve original audio disposition options to prevent FFmpeg automatic default selection when no original is default. Relative adjustment preserves other applicable source flags; the shared metadata helper and its ordinary transcode callers remain unchanged. Keep snapshot identity assignment and actual output probing unchanged.

Reject a synthesized selected output whose probed default is not explicitly false, using the existing output-facts mismatch error. Returned facts continue to describe observed media. No schema or protocol changes.

## Success
Original tracks retain their probed default states, including a non-default source; appended companions are non-default for one or multiple selected sources. Existing language, title, identity, forced/commentary metadata behavior remains intact. Ordinary transcode metadata preservation remains protected by existing tests.

## Failure model
- Actors and deployments: local operators and CI invoke the FFmpeg worker on Matroska media through authenticated worker HTTP.
- Invariants and assets: copied original defaults remain unchanged; companions are non-default; observed facts must reject unexpected or unobserved companion defaults.
- Accepted failure classes: none within this bounded disposition contract.
- Covered elsewhere: track ordering and broader final-policy verification belong to #339; source/selection/retention/full run belong to the operator; parent owns canary rerun after merge.

## Validation
Run sibling command/validation regressions red before implementation, then green. Extend the existing direct-handler conformance case to launch the worker binary and dispatch through HTTP/progress, with generated EAC3 media and direct ffprobe assertions of original and companion flags plus truthful returned facts. Run the worker suite and final `just ci`. ADR 0110 records the operator-selected companion default semantics; existing worker ownership is retained.
