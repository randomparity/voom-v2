# Empty scan-derived policy input implementation plan

## Success criteria

Whole-scan and root-scoped builders durably create an empty input set when no
video is eligible. Planning, execution, and reporting return zero-work success,
while generic targetless drafts remain invalid and failures leave no partial
durable state.

## Steps

1. Add policy-domain tests for the explicit empty-scan validator and continued
   generic rejection, then implement shared validation without weakening the
   default constructor.
2. Add control-plane tests for empty whole-scan, all-non-video, empty-root,
   concurrent creation, and transaction rollback. Route both builders through
   a shared store-owned persistence helper that chooses the narrow validator.
3. Add planning, reporting, and execution tests for an empty stored input.
   Resolve stored inputs before runtime preflight and bypass worker-only checks
   only when the resolved file set is empty.
4. Add process-boundary CLI tests for exact empty creation, execution, and
   report envelopes, including durable aggregate and workflow row assertions.
5. Run focused tests, formatting, linting, and `just ci`; review the diff for
   architecture, failure-state evidence, and unnecessary complexity.
