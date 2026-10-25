# Ambiguous tracks and ASS production regressions (#743)

## Problem and authority

#743 and #740 decomposition D require authored track-flag cells and ASS grouping through production scans, persisted evidence and existing policy semantics. The human explicitly approved “Approve ASS support within #743” after the test-only design exposed missing ASS classification. The cycle-2 frozen charter records that authority. ADR 0022 already anticipates localized subtitle extensions using its existing role and stem classifier; no new architecture decision or ADR is needed.

## Scope and architecture

Extend `voom-scan-worker::discover::classify_sidecar` to recognize `.ass` case-insensitively beside `.srt`. Retain the existing `external_subtitle` role and longest-stem association. No schema, renderer, role, ownership or policy changes; `.ssa`, `.sub` and `.vtt` stay unsupported.

The CLI integration uses narrow `support/track_cases.py` fixture authoring against the pinned Chaos Librarian. Select its four authored flag recipes (multiple defaults, none, default+forced, forced-only; respectively 2/2/2/8 audio and embedded SRT tracks), preserving commentary on the first audio. Add an independently authored healthy MKV control (one ordinary default audio and one non-default/non-forced embedded subtitle) with one ASS sidecar. No metadata mutation or timeline. Labels describe authored tags, not spoken language.

Materialize once, then reuse landed `real_owner_scan.py --existing-fixtures`, authenticated production workers, settlement polling, probe receipt and observed exporter unchanged. Persisted observations/snapshots and exporter output are the observed side of assertions; fixture intent supplies expected counts/flags. No oracle supplies normalized evidence to VOOM. There is no ownership transition or obsolete path.

## Success

C1: Classifier recognizes lowercase/mixed-case ASS, with existing role; the walker groups a language-suffixed ASS onto the longest matching media stem. An orphan is skipped and unrelated subtitle formats remain unclassified.

C2: The four named flag cells and healthy control each produce one successful real probe, durable snapshot and primary observation, with zero failed/degraded content. Assert case-specific audio/subtitle counts and default/forced booleans in persisted snapshots and exported state, and first-audio commentary in persisted snapshots. Assert the control ASS's persisted sidecar role, locator, digest/size and exported association. ASS is not a primary or fabricated embedded stream.

C3: Create offline durable policy inputs from queried real snapshots through the existing input API after Bootstrap has shut down the owner; do not invent root availability to use scan import. The existing policy removes commentary audio and forced subtitles and uses audio first/subtitle none defaults. Assert exact retained snapshot IDs using public production `stream_facts`/`resolve_track_keep_ids`, default strategies without fabricated selected IDs, payload snapshot linkage, expected planned/no-op status and repeated plan determinism. Healthy control stays unchanged; multiple/none retain main audio and non-forced subtitles; default+forced/forced-only remove subtitles. No new ambiguity/tie-breaking semantics or output-byte/decode claim.

## Failure model

- Actors and deployments: local equipped developer and existing optional heavy CI runner, production owner-node worker stack over temporary SQLite and generated media.
- Invariants and assets: truthful durable observations and policy selection; only ASS extends the published discovery capability; existing grouping/roles and tool/process supervision remain intact.
- Accepted failure classes: missing/unsupported media tools fail readiness explicitly; no rendering, spoken-language, decodability or remux-execution guarantee is made by these probe/planning tests; optional heavy tests remain ignored in default CI.
- Covered elsewhere: scan/probe/export infrastructure #742; misleading metadata #744; corruption #745; combined corpus/timeline/replay #746; soak/resource/workflow controls #747; selector product changes separately scoped. Existing bootstrap owns credentials, process deadlines and child cleanup.

## Threat model

- Boundary inventory: no new boundary; library filename-to-sidecar association widens to ASS.
- Actors: library-content writers are untrusted; configured owner-node execution and the test fixture author are trusted for their existing roles.
- Controls: existing walker rejects symlinks/non-files, validates locators and root containment before stem association; existing owner hash/probe validation applies. Unclassifiable/orphan files are counted skips. No new credential or raw-log output is introduced.
- Out of scope: ASS parsing/rendering is not performed; infrastructure credential/process threats retain #742/#747 ownership. Other subtitle extensions remain skipped.

## Validation

Focused red/green classifier and filesystem-walk tests prove C1. The real integration proves C2/C3, with a deliberate missing/wrong flag and selection mismatch check proving assertions reject omissions. Run affected scan-worker tests, the named ignored production case, formatting/lint, equipped `just chaos-e2e-ci`, required commit hooks and final explicit `just ci` before push. Record failures, warnings, ignored/default gating and actual durations. Prose has no executable consumer; independent design review and scope audit validate the design rather than a wording test.
