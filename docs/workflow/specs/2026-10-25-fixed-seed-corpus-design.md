# Fixed-seed corpus regression (#746)

## Problem and scope
The pinned Chaos Librarian generator provides an opt-in adversarial lane whose complete
recipe and timeline coverage VOOM does not yet execute. This test-only extension implements
#746 under #740; excluded families remain owned by #741–745, #566, #747, #339 and #500.
Use upstream generation, run and replay APIs; retain production owner scan/hash/probe and
DB-only observed export. No ownership transition or public product contract changes.

## Design
A narrow Python corpus helper generates FUZZ_REGRESSION seed 248 with explicit
ADVERSARIAL_MEDIA, checks YAML bytes against the pinned fixture, enumerates the fifteen
required recipe cells from actual payload coverage and bounds 18 assets, two-second
media, eight audio/subtitle tracks. It runs the full eleven-event timeline and replays
its genuine bundle. Upstream replay integrity remains its own contract; our cross-run
comparison uses semantic evidence, never encoded-byte equality across tool versions.
The helper derives bounded seed-248 MALFORMED and NETWORK_FS_CHAOS lane slices
and a representative ASS fixture using the existing supported recipe. Preserve default
lane planning unchanged. Run original and replay through production scans in separate DBs.
Assertions join fixture asset paths to actual exported persisted observations. Require
metadata maps, stream language and disposition evidence explicitly before the upstream
permissive comparison; require expected damaged-media probe absence and healthy controls.
Corrupt-tag and network cases assert actual applied actions and production terminal results;
network-fs is simulation, not evidence about real NFS. ASS asserts durable grouping.
Corrupt-tag keeps generated fuzz_0005_corrupt_tags (null_bytes/64, asset_005 MKV)
and an unaffected control: expect one failed probe and one successful snapshot. Network
keeps generated first eight actions (permissions/quota/readonly/stale-handle/unmount/
remount/acquire/release) and eight assets: expect eight successful production probes
after paired restoration. Supplemental media durations are bounded to 0.2 seconds;
these are declared lane slices, not byte-identical generated golden scenarios.
The local runner reports applied versus requested timeline counts after a successful run,
marking a short prefix incomplete. It gains no aggregate lifecycle or resource guarantee.

## Success
Generated seed-248 YAML matches the pin; the fifteen recipe cells are explicitly checked,
eleven event IDs appear in order and both original/replay production semantics match.
Fixed-seed supplemental lanes and ASS have non-vacuous action/outcome evidence.
Missing/null evidence fails coverage. Prefix reporting and ordinary lane selection remain
truthful. Equipped heavy tests and final just ci pass with skips disclosed.

## Failure model
- Actors and invariants: test operator invokes pinned generator on temporary fixtures;
  fixture oracle never supplies observations. Preserve exact golden bytes, actual required
  recipe cells, complete event IDs, separate production DBs and explicit persisted evidence.
- Prevent/detect: explicit seed/lane, schema checks, checked event/count/path joins and
  probe/metadata/disposition assertions fail generation, replay or semantic omissions loudly.
- Accepted failures: different encoded bytes across tool versions; simulated network behavior
  does not establish real NFS correctness; arbitrary seeds/codecs outside the named cases.
- Covered elsewhere: aggregate elapsed/disk/process retention and soak hardening #747;
  production acceptance #339. This runs only in equipped local/weekly heavy tests.

## Threat model
- Boundaries: pinned generated YAML/replay JSON enters upstream parsers; fixture paths enter
  production scanner and exporter; CLI/process arguments use argument arrays, no shell evaluation.
- Actors: local test operator controls temporary paths; pinned upstream fixtures are test inputs,
  not trusted product evidence. Production SQLite observations remain typed exporter inputs.
- Invariants: no production library mutation or credential publication; no oracle-derived observed
  values; bounded subprocess supervision and tool output draining reuse existing harness seams.
- Accepted/covered elsewhere: hostile arbitrary corpus import and real network mounts are excluded;
  aggregate lifecycle and retained artifact controls remain #747, not a new guarantee here.

## Validation
Python contract negatives remove recipe parameters and metadata evidence and truncate counts.
Rust ignored E2E invokes helpers, real owner scans and typed DB export; compares semantic
facts for original and replay. Script selftests cover complete versus prefix reporting.
Run focused corpus test, existing equipped heavy suite, just lint and final just ci.
No ADR is warranted: this extends existing test ownership and accepted upstream APIs.
