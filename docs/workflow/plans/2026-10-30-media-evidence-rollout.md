# Plan: media evidence rollout gates (#794)

Spec: [2026-10-30-media-evidence-rollout-design.md](../specs/2026-10-30-media-evidence-rollout-design.md).
Measurements hold `flock -x` on the corpus lock. Builds and checks hold `flock -s`. Every
private manifest, stack work directory and copy stays outside the checkout or under an ignored
path.

1. **Instrument.** Add `scripts/media-evidence-rollout.py` (`scan`, `construct`) and its
   selftest, wired into `just source-checks`. Done when the selftest passes, a synthetic tmpfs
   corpus scans with the setting off and on, and a synthetic construct run detects (a)–(c).
2. **G3 stage-0.** `scan --reuse off --evict`, full local, ×3, before any setting-on scan
   cell. Commit the record whatever its outcome.
3. **G1.** Harness cells `buf8k` (change off) then `baseline` (1 MiB), full local, evicted,
   ×3. Judge them with `media-evidence-candidates.py compare`.
4. **Canary (6-file subset `f0004,f0025,f0022,f0005,f0009,f0015`).** Scan off, then on.
   Then `--kill hash` and `--kill probe` with the setting on. Then a setting-off scan
   checking for no reused rows. G4 `construct` on btrfs (reflink copy, evicted) and tmpfs.
   G5 `construct` on an ext4 image. G6 and G9 canary policy runs on reflink copies.
5. **Full local.** `scan --reuse on --evict`, ×3. Judge G2 and G3 against step 2. Run G9:
   a repeat scan compares the hash digest, then the complete-library application runs and is
   repeated.
6. **Network subset (≤ 3 files, as-found).** `scan` off then on. This is G3 reported and
   G5's network arm. Record the interference.
7. **G7 mixed versions.** Build protocol-5 binaries at the pre-#811 commit in a scratch
   worktree. Pair them with protocol-6 binaries in the stack and record where each pairing
   is refused.
8. **Report.** Write the rollout README, `docs/media-evidence-rollout-report.md` with
   per-gate verdicts, and the runbook enable/rollback section. Host-path-scan the records,
   then run `just ci`.
