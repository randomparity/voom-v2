# Production chaos probe plan (#742)

Base main; branch feat/production-chaos-probe-742. Frozen scope #742 q742-16c8054b; full-spec L fixed denominator1000. No deferrals. Expected implementation350–650 changed lines across the existing chaos E2E, observed exporter and narrow scan runner. No production or numbered files.

## One coupled Cast task
1. Add red projection and omission tests in chaos_librarian_e2e.rs; extend observed_state.rs with persisted-tag projection and explicit coverage checks. Verification focused-test: current null exporter fails expected persisted tags; remove required metadata/flags to prove coverage rejects omissions. Green cargo test -p voom-cli --test chaos_librarian_e2e observed_state.
2. Add the bounded real scan runner importing Bootstrap; reuse the #566 Rust supervisor via a script selector, preserving encoded-case behavior. Create one tagged/flagged small MKV and invalid MP4 in a private library. Verification focused-test: real production scan must persist healthy facts and the bad-file observation, then export from that DB; removing scan/persisted raw/flags must fail. Green cargo test -p voom-cli --test chaos_librarian_e2e production_scan -- --ignored --nocapture.
3. Verify all heavy cases with just chaos-e2e-ci and lint; run mandatory commit hooks, branch adversarial/security review, simplification and final worker-owned just ci before push. Existing566 identical-tree baseline is applicable only to initial code, never candidate acceptance. No prepush hook; full final run remains explicit.

C1–C4 require the same produced SQLite and exporter path, so independent implementation workers would duplicate context; execute directly. Design prose has task-test-not-applicable because no executable consumes its wording. Existing Bootstrap and production node agent retain their ownership and all public/persisted contracts. No obsolete production path or caller migration.
