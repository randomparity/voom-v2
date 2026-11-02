# Owner-publication verification (#843)

The [approved expanded scope](https://github.com/randomparity/voom-v2/issues/843#issuecomment-6575989857)
repairs the ADR0109 replay/lease observation race and retains ADR0123 header checks.
Runtime build: `07b60b5cf969ee8b2180c5e2be2216fa02d7bce1`, including fix `4003c036`.
Observed host: Linux x86_64; no target architecture is declared by project policy.
Tools: Rust 1.95.0, Python 3.14.8, ffmpeg/ffprobe 8.1.3, mkvmerge 99.0.
Inputs were generated one-second 64×64 media; each mode used a fresh retained private directory.

| Mode | Exit | Whole command seconds | Checks | Failure array |
| --- | ---: | ---: | ---: | --- |
| extract | 0 | 35.636 | 56 | `[]` |
| abort | 0 | 4.423 | 1 | `[]` |
| reuse | 0 | 0.635 | 14 | `[]` |
| late-rollback | 0 | 0.629 | 2 | `[]` |

The actual files were `group-proof-failures.json` for owner modes and `failures.json` for clones.
Owner commands: `just check-owner-publication <fresh-extract> extract` and the analogous `abort`.
Clone commands used `python3 scripts/issue-733-reuse-clone-regression.py` with fresh `--workdir`
and `--mode reuse` or `--mode late-rollback`. Reuse read `<extract>/stack/voom.db`; late rollback read the
actual `<extract>/stack/prepublication-recovery.db`. Directory tokens above redact private paths.
Owner timings include their requested Cargo build; cache conditions were not controlled.

`cargo test -p voom-control-plane waiter_` first compiled but found invalid test-fixture
release-reason vocabulary (exit 101, 51.098 s); this was corrected before claiming causal RED.
The unchanged waiter then failed with lost-source-lease Conflict (101, 6.916 s).
After repair: GREEN (0, 12.097 s); reinserted old read: RED (101, 11.854 s);
restored repair: GREEN (0, 11.879 s). Each green ran three tests, zero ignored, including
the two new tests. Notify/pool synchronization forces publication between snapshots;
expired-held and future-expiry released sources independently remain errors without replay.

[Earlier failed runs and investigation](https://github.com/randomparity/voom-v2/issues/843#issuecomment-6573055935)
remain evidence. One temporary controlled schedule produced HTTP 409/200 after committed replay;
its hooks were removed and original source restored before this repair. The original failed run
retained no response pair, so that particular run's exact attribution remains limited.

Extract proved two physical Opus outputs, durable grouped publication, exact duplicate/replay
responses and unchanged explicit resume/reuse. Abort preserved truthful terminal failure.
Late rollback preserved 78 tables with only one allowed error replay, then published both members
in its positive control. Owner API/agent shutdowns exited 0 and removed credentials.
Relevant format/layout/paused-time/SQL-boundary/transaction guards, lint and mandatory fix-commit
hooks passed. This report records local correctness evidence; final full CI is tracked in delivery.
No cold-cache, performance-comparison or additional-platform result is claimed here.
