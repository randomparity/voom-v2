# Structural corruption production regressions (#745)

## Problem

Request acceptance and successful ffprobe output alone cannot prove per-file corruption handling or decodability (#745, parent #740).

## Scope

One test unit extends the existing production scan harness with four pinned upstream cells: MP4 moov at start/end, damaged MKV Cues, and MP3 truncated after sixteen complete frames. Independently materialized healthy MP4/MKV/MP3 controls accompany them in one private root. Reuse pinned scenario templates and structural events, asserting corruption hashes and exact byte spans against healthy originals before scanning. Configure the private root through the existing public extension allowlist to include MP3; preserve default helper fixtures and production defaults. No ownership transition is needed.

Assertions read VOOM persisted observations/snapshots and actual worker invocation receipts independently from upstream fixture facts. Both MP4 failures must settle within the existing scan deadline while the five probeable files retain evidence. Successful MKV/MP3 probes do not imply intact content: bounded ffmpeg decoding compares MKV frame hashes and verifies a shorter nonempty MP3 PCM output; healthy controls decode successfully. Fixture evidence never enters observed export.

### Failure model

- Actors and deployments: equipped local/weekly/manual heavy suite; ffmpeg/ffprobe >=7 and MKVToolNix >=80; ordinary CI preserves opt-in gating.
- Invariants and assets: seven-file discovery, actual mutation preimages/byte spans/hashes, worker probe classifications and durable evidence; healthy-file progress and bounded decode claims.
- Accepted failure classes: encoded bytes may differ across independent muxes/tool versions; compare mutation bytes against their actual retained preimages, not a separately muxed control.
- Covered elsewhere: broad codec matrix → epic #740; product failure/retry fixes → separate control-plane/worker issue; aggregate retention/time limits → #747.

## Success

Seven files reach durable observations through real owner scan/hash/probe. Two MP4 failures publish no snapshots; healthy MP4/MKV/MP3 and damaged MKV/MP3 publish snapshots with real probe evidence. Exact upstream mutation receipts establish four cells. Bounded decode evidence proves the stated limited MKV equivalence and MP3 partial-decode claims. Existing heavy cases remain green.

## Validation

`focused-test`: new structural fixture authoring/verification helper; first run the structural case against its missing implementation, expecting helper absence. Then execute its fixture command under pinned `uv run python`, checking four corruption actions, bytes, probes and decodes.
`focused-test`: production matrix and optional helper extension selection; run `cargo test -p voom-cli --test chaos_librarian_e2e production_scan_structural_corruption -- --ignored --exact --nocapture` before allowlist configuration, expecting missing MP3 observations, then after implementation expecting seven observations, two classified failures and five snapshots.
`task-test-not-applicable`: this design prose has no executable consumer contract.
Run the pinned temp-root guard early, final-source `just lint`, equipped `just chaos-e2e-ci`, mandatory commit hooks and final candidate `just ci` before push. Record ignored/skipped tests and actual timings explicitly.
