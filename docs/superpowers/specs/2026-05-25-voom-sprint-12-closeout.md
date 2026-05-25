# VOOM Sprint 12 Video Transcode Closeout

| Requirement | Evidence |
|---|---|
| DSL compiles `transcode video to hevc {}` | `cargo test -p voom-policy` |
| Planner planned/no-op/blocked behavior | `cargo test -p voom-plan planner_test::transcode_video` |
| Worker preflight and transcode failures | `cargo test -p voom-ffmpeg-worker` |
| Verify and commit integration | `cargo test -p voom-control-plane --test video_transcode_flow` |
| CLI envelope | `cargo test -p voom-cli --test compliance_envelope` |
| Full suite | `just ci` |
