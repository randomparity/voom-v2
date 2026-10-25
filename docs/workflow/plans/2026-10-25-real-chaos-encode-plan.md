# Real chaos encode implementation plan

Expected implementation size: 150–300 changed lines, based on one Python test entrypoint (80–130), the Rust case/oracle (60–110), justfile prerequisites and design documents. Fixed design denominator: 1000 changed lines (L), live #566 cross-process acceptance assessment.

## Task 1 — real encode and output oracle (Cast, tightly coupled)
Implement `crates/voom-cli/tests/support/real_owner_encode.py` by importing the existing bootstrap with no production or bootstrap changes. Require one successful scan observation and one root-scoped policy input. Create the existing policy fixture through the CLI; return plan, compliance execution and ffprobe output for the committed artifact. Locate exactly one MKV under the isolated output root. Use subprocess deadlines for the helper's CLI (inherited 60 seconds) and independent ffprobe (30 seconds); Bootstrap retains startup and graceful shutdown bounds. The Rust runner uses a private temporary directory and explicit target/debug binaries; capture stdout in a file and poll child completion with a finite overall deadline so failure cannot hang the heavy suite. On deadline terminate the helper so its signal-aware context can close children before forced fallback. Preserve stderr as failure diagnostics without credential contents.

Replace only the required-transcode case's emulator/seeding setup with this runner. Retain its plan and durable committed-success assertions. Add a pure probe-evidence assertion and meaningful negative cases for synthetic output and wrong codec/container. Keep other emulator cases intact. Expand `just chaos-e2e-ci` build prerequisites to include voom-api, voom-node-agent, scan/hash/remux workers.

Verification inventory:
- focused-test: output oracle rejects text masquerading as MKV and genuine h264/non-Matroska probe facts; expected red is the prior existence-only assertion accepting synthetic bytes; green command `cargo test -p voom-cli --test chaos_librarian_e2e real_encode_probe`.
- focused-test: `transcode_required_executes_real_owner_and_commits_hevc_mkv`; expected red is old emulator output failing independent ffprobe; green command `cargo test -p voom-cli --test chaos_librarian_e2e transcode_required_executes_real_owner -- --ignored --nocapture` with all real stack binaries prebuilt.
- focused-test: preserved suite coverage and runtime prerequisites; green `just chaos-e2e-ci` on equipped tooling.
- task-test-not-applicable: design prose has no executable consumer; independent design review and scope audit validate it.

## Integration and handoff
Run the contract checks, equipped heavy suite, review/security/simplification and final `just ci`; commit through mandatory prek hooks, deliver the PR and wait for CI. Do not merge: finish with the exact-head author handshake and leave the worktree intact. Preserve findings, dispositions, artifact identities and consumed budgets in private workflow notes.
