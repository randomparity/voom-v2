# Production chaos probe evidence (#742)

## Problem
Issue #742 requires a real production discovery/probe regression and lossless persisted-fact export before the matrices in #743–#745. The exporter already retains recorded audio/subtitle flags but emits null format tags; seeded fixtures cannot prove production persistence. Scope is frozen at #742 WORK:SCOPE q742-16c8054b.

## Scope and architecture
Reuse the existing acceptance Bootstrap and #566 Rust subprocess supervisor. A narrow Python support runner invokes the production scan command over a private fixture library and returns only the scan result after stack cleanup. Rust reads its persisted SQLite using the existing exporter. Do not duplicate activation, envelope translation, normalization or hashing. Generalize the Rust supervisor only enough to select the encode or scan support entrypoint and preserve its existing bounded lifetime and sanitized diagnostics.

Use one small deterministic MKV with video, one audio and one embedded subtitle, explicitly authored flags and title/year, plus one invalid MP4. This is infrastructure proof, not the track, metadata or corruption matrices. Fixture creation uses bounded argument-array ffmpeg with file-backed/drained output; no oracle report supplies observed input. Assert production discovery/hash/probe persistence directly: healthy media has a blake3 content identity and normalized snapshot, flags and retained raw tags; invalid media has a durable per-file scan observation without a fabricated successful snapshot. Require actual worker failure evidence and a completed session with failed_content_count=1 for this controlled one-bad-file fixture. Call the persisted result an evidence-less outcome and per-file isolation; a missing snapshot alone does not establish a failure class. Production stores no per-file failure reason; this limitation remains explicit. Authority: campaign operator confirmed this existing-contract interpretation on 2026-10-25.

Project title/year solely from snapshot.raw.ffprobe_json.format.tags. Match the pinned oracle: case-insensitive supported keys, unique string values, maximum 4096 UTF-8 bytes each, retaining empty strings and excluding unrelated keys. Missing raw format evidence stays null; a reported format with no tags yields an observed empty map. Invalid supported types, duplicates or overlength values fail loudly. Preserve product snapshot contracts and exporter callers.

Add an explicit evidence-coverage check for required tags and audio/subtitle default/forced presence. Oracle comparison remains permissive for null metadata, so coverage must independently reject null/missing evidence. The production fixture proves actual values; omission negatives remove each evidence family and must fail coverage.

The required full check exposed an existing store test whose 500 ms wall-clock proxy fails under parallel initialization stress even though its no-op report assertions pass. The operator explicitly approved C6: repair only `crates/voom-store/tests/init.rs`. Retain its report assertions and compare complete ordered `_sqlx_migrations` records, `schema_meta` rows and `schema.initialized` event rows before and after the second init. Require expected migration count, metadata presence and one initialization event before comparison. A local snapshot helper is called twice; existing repository helpers do not expose all these full row sets. Do not claim this observes internal polling, SQLx apply calls or wall-clock latency. Production code and other init/stress tests remain unchanged.

## Success
C1–C4 map directly to #742's frozen charter: real owner scan/hash/probe, persisted flags and bounded supported tags, explicit failed-file observation and healthy isolation, and coverage omission rejection. Export never reads oracle-derived media facts. C5 requires focused tests, equipped heavy suite and final just ci before push. C6 proves durable initialization idempotence without the timing proxy, as explicitly approved by the operator on 2026-10-25.

## Failure model
- Actors and deployment: trusted test authors and equipped local/weekly CI; private temporary roots and real out-of-process workers.
- Invariants/assets: observed facts come from SQLite, unknown evidence stays unknown, malformed fixture cannot acquire a fabricated probe, and children/credentials remain Bootstrap-owned. Serialized raw evidence is validated before projection. C6 preserves complete durable initialization records while retaining the no-op report assertions.
- Accepted failures: unavailable tools fail readiness, host kill/power loss can prevent graceful cleanup, exhaustion may fail this bounded test. No decode or aggregate-disk guarantee.
- Covered elsewhere: pin/schema #741, encode #566, track/ASS #743, metadata matrix #744, corruption/decode matrix #745, corpus/replay #746, aggregate resources/bundles #747. Production behavior defects require a separately scoped issue in the affected crate.

## Threat model
Only trusted generated fixture paths and private work roots reach subprocess argv. Existing Bootstrap owns credentials and sanitizes CLI receipts. Do not return configuration, registration tokens or full receipts; reuse sanitized error extraction. Raw JSON tags are bounded and type checked; no shell interpretation.

## Validation
Focused projection tests cover case, UTF-8 byte bound, unique/type checks, null/empty/absent/empty-value semantics. Coverage tests remove metadata and each stream flag and reject omission. One equipped real scan test checks durable healthy/bad outcomes and exported facts, followed by all equipped chaos tests and just ci. Prose is reviewed, not string-tested. No new architecture decision or numbered record is needed; the smaller seeded-only alternative fails C1/C3.

For C6, retain the reproduced timing failure as causal evidence. Controlled test-only mutations to migration execution metadata, schema metadata and initialization event rows after the second call must each fail the durable-state assertion while report assertions pass. Restore each mutation byte-for-byte, run the init file repeatedly at serial and parallel settings, preserve mandatory hooks, and run final just ci. No production instrumentation, threshold increase or broader timing cleanup is authorized.
