# Issue #339 Production Acceptance

Issue #339 closes only after the production-safe policy has been exercised on
real operator media and the evidence is recorded. Generated-media acceptance
already proves deterministic semantics; this document covers heterogeneous
library behavior, capacity, resumability, and failure classification.

## Scope

- Use `reference-user.voom` as the flagship policy:
  `crates/voom-control-plane/tests/fixtures/policies/reference-user.voom`.
- Run add-only into a dedicated output root. Never point `--output-dir` at an
  input library.
- Treat `/mnt/cifs/cineplex/media` as read-only during canary selection. Copy
  selected files to `/mnt/pool0/test-video` or `/mnt/raid0/media` before running
  mutating worker output.
- Do not run published grammar coverage policies on production media.

## Directory Tiers

| Tier | Path | Use |
| --- | --- | --- |
| Curated canary | `/mnt/pool0/test-video` | First representative real-media run. |
| Large rehearsal | `/mnt/raid0/media` | Capacity and longer-duration rehearsal. |
| Production source | `/mnt/cifs/cineplex/media` | Read-only inventory; later controlled full run. |

## Acceptance Checklist

- [ ] Confirm current `main` commit and `just ci` status before the run.
- [ ] Record host OS, filesystem mounts, free space, and tool versions.
- [ ] Build or select a dedicated SQLite database for this acceptance run.
- [ ] Confirm `ffmpeg`, `ffprobe`, and `mkvmerge` are on `PATH`.
- [ ] Inventory the canary source with `scripts/issue-339-media-inventory.py`.
- [ ] Preserve the inventory JSON as evidence.
- [ ] Review the recommended canary set for coverage across:
  containers, video codecs, audio codecs, channel counts, audio languages,
  subtitles, attachments, large files, and probe failures.
- [ ] Copy production-selected canary files only into a local test directory.
- [ ] Scan only the intended canary root.
- [ ] Create or reuse the accepted `reference-user.voom` policy version.
- [ ] Create the policy input set from the scan.
- [ ] Run `compliance execute` with dedicated staging and output roots.
- [ ] Preserve the execute envelope and final `compliance report`.
- [ ] Classify every failed or skipped file.
- [ ] Resume at least once after a completed or partially completed canary run.
- [ ] Verify resume does not redo completed file phases or overwrite outputs.
- [ ] Inspect final artifacts with `ffprobe` or Voom verification evidence.
- [ ] Record retention and cleanup decisions for staging, DB, outputs, and copied
  canary inputs.

## Evidence Template

Fill this section in a closeout note before closing #339.

### Run Identity

- Git commit:
- Branch:
- Date:
- Operator:
- Host:
- Database URL/path:
- Staging root:
- Output root:
- Input root:
- Policy file:
- Policy version id:
- Input set id:
- Job id(s):

### Versions

```text
voom:
ffmpeg:
ffprobe:
mkvmerge:
rustc:
cargo:
```

### Capacity

```text
df -h <input-root> <staging-root> <output-root>
```

- Input file count:
- Input total bytes:
- Inventory file:
- Recommended canary count:
- Copied canary bytes:
- Output file count:
- Output total bytes:

### Commands

```sh
export VOOM_DATABASE_URL=sqlite://<absolute-db-path>
voom init
voom scan --path <input-root>
voom policy create --slug reference-user-library-normalize --file \
  crates/voom-control-plane/tests/fixtures/policies/reference-user.voom
voom policy input create-from-scan --all --slug issue-339-canary
voom worker run-local --kind ffmpeg
voom worker run-local --kind mkvtoolnix
voom compliance execute \
  --policy-version-id <version-id> \
  --input-set-id <input-set-id> \
  --staging-root <staging-root> \
  --output-dir <output-root>
voom compliance report --job-id <job-id>
```

### Outcome Matrix

| Bucket | Count | Evidence | Follow-up |
| --- | ---: | --- | --- |
| Completed |  |  |  |
| Expected skip |  |  |  |
| Unsupported input |  |  |  |
| Corrupt or unprobeable input |  |  |  |
| Policy-language blocked |  |  |  |
| Tool failure |  |  |  |
| Voom bug |  |  |  |

### Closeout Decision

- Canary accepted:
- Larger rehearsal required before full run:
- Full-library run accepted:
- Cleanup retained:
- Cleanup deleted:
- Issues opened:
