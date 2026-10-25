# Chaos Librarian consumer contracts (#741)

## Problem and authority

Issue #741 and epic #740 slice A require a reviewed exact upstream pin,
compatible consumers, and existing heavy cases remaining green. The frozen
WORK:SCOPE token is q741-2b684648. Approval of exclusions is inherited unchanged
from the human campaign decision of 2026-10-25.

## Scope and design

Keep the existing test-only ownership and entry points; no caller migration or
production architecture decision is needed. Pin the upstream gitlink and the
three Rust/shell guards to 3b26eb9625babca61feaf1f56cb7ed8d36f1bd1a.
The checked-in models/schema-reference govern observed-state 6, scenario 37,
replay 15, materialization 17, asset reports 11, and divergence 1.
Use those upstream models for integration contract validation rather than a
second Rust schema implementation or a new dependency. A helper invokes the
locked upstream Python environment with explicit model selection and validates
JSON without rewriting it. Materialization validates the scenario and emitted
materialization/replay/asset reports. Comparison validates observed input and
its divergence output. Unknown versions fail loudly before a result is trusted.

Emit observed-state 6. Export explicitly recorded default/forced booleans for
both audio and subtitle streams, retaining false and omitting unavailable
values; video dispositions remain outside that projection. Report unavailable
format-tag evidence as null, never an invented empty map. Synthetic contract
cases prove null/unreported, empty map, absent key and empty string remain
distinct. Missing stream evidence must cause an oracle mismatch. The upstream
metadata comparator deliberately permits null evidence: tests must not mistake
that permissiveness for coverage; production evidence/coverage guards belong
to #742. Do not infer facts from oracle expectations.

Alternative: merely bumping the integer loses evidence semantics and contract
validation. Reimplementing schemas duplicates upstream ownership and grows the
seam unnecessarily. These are implementation choices within existing ownership,
not a new architectural decision requiring an ADR.

## Success

The existing heavy suite passes at the new pin. Focused regressions show
recorded true/false audio/subtitle flags survive export, missing flags remain
missing, and incompatible observed/scenario/replay/report versions are rejected.
A v6 synthetic model round-trip preserves tag evidence distinctions. Existing
shell harness tests use the same full SHA as the gitlink and Rust guard.

## Failure model

1. Actors and deployments: equipped local developers and weekly/manual CI,
   using the initialized pinned checkout and locked uv environment.
2. Invariants/assets: truthful test evidence, schema incompatibility detection,
   stable default CI scope, storage-owner execution boundaries, existing cases.
3. Accepted failure classes: absent media tools/submodule fail readiness rather
   than masquerading as passing coverage; this is an equipped optional suite.
4. Covered elsewhere: real encoding #566; production discovery/lossless persisted
   extraction #742; tracks/ASS #743; metadata matrix #744; corruption #745;
   seeded corpus/replay #746; aggregate bounds/failure bundles #747;
   production-library acceptance #339; ffmpeg distribution lifecycle #500.

## Validation and rollback

Run focused exporter intent tests, shell harness selftest, upstream schema-export
check and equipped just chaos-e2e-ci, then just ci before push. Record durations
and real skips. Pre-commit hooks remain enabled. Revert the bounded consumer
change and gitlink together for rollback; no database migration is involved.
