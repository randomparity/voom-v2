# Real chaos encode (#566)

## Problem
The chaos transcode case currently commits synthetic bytes. #566 and its duplicate-folded comment require one real authenticated API/node-agent encode and independent output probing, while retaining emulation elsewhere.

## Scope and architecture
Reuse `scripts/acceptance-bootstrap.py` without changing its lifecycle or production contracts. A narrow Python test-support entrypoint imports its `Bootstrap`, copies the scenario’s single MP4 candidate into a private one-source input root, starts a private real stack, runs production scan and root-scoped policy input creation over the materialized h264 candidate, creates the existing HEVC fixture policy, and executes compliance. The Rust chaos case materializes the same scenario and invokes this entrypoint with explicit binary and private work directories. The entrypoint returns plan, execution and independent ffprobe evidence; Rust retains the durable-success assertions and asserts actual HEVC Matroska media. Its context manager shuts down the real stack before returning.

Production `voom-node-agent` remains the sole envelope-to-worker translator. No ownership transition or compatibility shim is needed. The other chaos cases keep their existing seeding and emulator paths. `just chaos-e2e-ci` builds the API, agent and six worker binaries used by the bootstrap.

A custom Rust API/credential supervisor would duplicate the existing bootstrap's lifecycle. Calling ffmpeg from the emulator would duplicate production translation and fabricate reprobe facts, contrary to C4. Reusing the bootstrap is the smallest coherent extension; no new ADR decision is introduced.

## Success
C1: the named transcode case runs the real API, authenticated owner node and real ffmpeg worker against Chaos Librarian's h264 candidate, reaches one successful transcode and committed file phase with durable output/reprobe IDs.
C2: an independent ffprobe process succeeds on the committed output, reports Matroska and an HEVC video stream with positive dimensions and duration. A negative oracle check rejects synthetic bytes and h264 media.
C3: other chaos cases retain their existing fixture execution.
C4: no test support translates a dispatch envelope into a worker request.

## Failure model
- Actors and deployments: equipped Linux/macOS local operators and weekly/manual chaos CI running trusted repository tests and generated media in private temporary roots.
- Invariants and assets: no leaked bearer in returned JSON/logs; the bootstrap owns credential files and authenticated activation; test children must stop on ordinary success, exception and bootstrap timeout; output assertions must fail for synthetic bytes or wrong codec/container.
- Accepted failure classes: host SIGKILL/power loss may prevent graceful cleanup; media-tool absence fails readiness explicitly; disk exhaustion and arbitrary hostile edits to test scripts are outside this bounded case. No aggregate resource/retention claim is added.
- Covered elsewhere: aggregate resource caps, interrupt bundles and soak lifecycle #747; production adversarial scan/probe/export #742; other media cases remain emulator-based per #566; distribution lifecycle #500.

## Threat model
Inputs are trusted generated scenario paths and private work directories. Commands use argument arrays and fixed CLI operations. Bearers remain owned by Bootstrap's private credential directory; registration envelopes are not returned. The helper returns only plan, execution and independent probe evidence. Reusing existing loopback opt-in avoids a new transport/auth implementation. Failures must not dump configuration or credentials.

## Validation
Run a controlled red on the current synthetic output oracle, then the real case with equipped tools. A pure output classifier regression must reject synthetic/unparseable evidence, h264 and a non-Matroska format, and accept the intended probe. Run the whole equipped `just chaos-e2e-ci` to prove preserved emulator coverage, and `just ci` before push with mandatory commit hooks. Prose is reviewed rather than string-tested.
