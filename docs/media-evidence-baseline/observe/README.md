# Observe-path hash buffer: 16 KiB against 1 MiB (#838)

Captured on 2026-11-17 under the frozen [protocol](../../media-evidence-protocol.md), with the
verdict rule fixed before any real-media cell in
[the #838 spec](../../workflow/specs/2026-11-17-observe-buffer-evidence-838-design.md). These are
measurements of one host, not a performance budget, and production source and defaults are
unchanged. The raw records are the sixteen JSON files beside this summary, schema
`media-evidence-observe-run/1`. The tables are output of
`scripts/media-evidence-observe.py compare`, rounded for display.

## What ran

Four full-hash reads use `observe_file_facts` with a 16 KiB buffer: transform input in the
ffmpeg and mkvtoolnix workers (before and after each operation), output verification in the
verify-artifact worker, and the ffprobe worker's re-hash when probe reuse is off. Each
`observe-<path>` example calls its crate's production function on the workers' runtime shape
(multi-thread, 2 worker threads), one process per repetition, 4 files in flight (the protocol's
stage-0 concurrency). It reads each of the 34 local-corpus files (935.3 GB, NVMe-backed btrfs)
once. No transform, ffprobe or verification runs.

Both buffers were built by `media-evidence-candidates.py prepare observe16k|observe1m` in
detached worktrees of `e2c29167`. `observe1m` changes only the buffer line in the four
`observe.rs` files to `vec![0_u8; 1024 * 1024]`; each 1 MiB record's `git_changed_paths` names
those four files, and `observe_buffer_bytes` is read from the edited source. Harness SHA-256
`7c4457e4…` in every record; later commits change only the harness's record validation and
selftests, not capture. Host: Linux x86_64, 32 CPUs, 124 GiB RAM.

Cells ran 18:52–21:51 UTC, 670–683 s each (budget 60 minutes), in this order, so neither
buffer always ran second: evicted ffmpeg 16k, 1m; mkvtoolnix 1m, 16k; verify-artifact 16k, 1m;
ffprobe 1m, 16k; then as-found ffmpeg 1m, 16k; mkvtoolnix 16k, 1m; verify-artifact 1m, 16k;
ffprobe 16k, 1m. Every row of every repetition ended `observed`; no file was skipped for
budget; every repetition of all sixteen records has the same `hash_set_sha256` (`c4ceb9b9…`),
so both buffers produced identical content hashes for every file. Every evicted repetition
started with 0 resident bytes for every file.

A first attempt's 16 KiB ffmpeg evicted cell (same binaries, earlier harness) was discarded,
not committed: its 1 MiB successor refused to start on a harness defect, fixed in `e2c29167`,
and all sixteen cells were then captured on one harness. Its values matched the committed cell
(wall 224–226 s, CPU 334–359 s).

## Results

Medians over 3 repetitions (as-found: repetitions 2–3; repetition 1 is in the records and
within 0.6% of them for wall). Logical reads are 935.3 GB per repetition in every cell.

| Path | Cache | Wall 16k → 1m (s) | Summed observe 16k → 1m (s) | CPU 16k → 1m (s) | CPU change | Physical 16k / 1m (GB) | Peak RSS 16k → 1m (MiB) | Verdict 1 MiB |
|---|---|---:|---:|---:|---:|---:|---:|---|
| ffmpeg | evicted | 224.4 → 223.4 | 888.2 → 884.7 | 360.6 → 150.4 | −58.3% | 935.2 / 935.2 | 4.0 → 20.5 | **go** |
| mkvtoolnix | evicted | 224.2 → 224.9 | 887.5 → 891.4 | 355.2 → 150.7 | −57.6% | 935.2 / 935.2 | 4.0 → 21.4 | **go** |
| verify-artifact | evicted | 225.1 → 223.6 | 891.3 → 886.0 | 333.4 → 150.8 | −54.8% | 935.2 / 935.2 | 4.0 → 21.3 | **go** |
| ffprobe | evicted | 224.8 → 224.3 | 889.6 → 888.6 | 335.3 → 151.0 | −55.0% | 935.2 / 935.2 | 4.0 → 21.3 | **go** |
| ffmpeg | as-found | 223.7 → 223.5 | 886.1 → 885.7 | 340.1 → 152.6 | −55.1% | 929.1 / 929.3 | 3.9 → 21.7 | **go** |
| mkvtoolnix | as-found | 223.9 → 224.1 | 886.8 → 887.5 | 337.9 → 154.1 | −54.4% | 928.8 / 928.5 | 4.0 → 19.3 | **go** |
| verify-artifact | as-found | 223.4 → 224.0 | 884.8 → 887.1 | 354.1 → 154.5 | −56.4% | 927.7 / 928.2 | 4.0 → 21.4 | **go** |
| ffprobe | as-found | 223.5 → 223.7 | 884.8 → 886.2 | 343.2 → 155.3 | −54.8% | 927.1 / 926.5 | 4.2 → 20.7 | **go** |

Per metric, by the protocol thresholds: CPU is an **improvement** in all eight comparisons
(baseline CV 0.2–6.5%). Wall, summed observe seconds and physical bytes are inconclusive in all
eight: the changes are under 1%, and both buffers read every byte once. Peak RSS meets the 5%
regression threshold in all eight but stays far inside the resource guardrail (baseline +
256 MiB); as in #789 and the spec's rule, memory is judged by the guardrail. No guardrail was
breached and no row failed.

**Verdicts.** 16 KiB is the baseline in every cell. **1 MiB is go** for all four paths in both
cache states: CPU improves by 54–58%, wall and summed observe seconds do not regress, memory
stays within the guardrail, and the hashes are equal. The ffmpeg, mkvtoolnix and
verify-artifact hash loops are byte-identical and ffprobe's differs only by a stack buffer, so
the four agreeing verdicts are one result measured four times, not four independent ones.

## Observations

- **The read is disk-bound; the buffer only changes CPU.** At 4 in flight both buffers read the
  corpus at about 4.2 GB/s. The 16 KiB loop hands about 57 million reads per pass to tokio's
  blocking pool against about 0.9 million at 1 MiB; that handoff is the likely source of the
  CPU difference (not profiled).
- **Per transform.** CPU per byte falls from about 0.36–0.39 to about 0.16 CPU-seconds per GB.
  A transform reads its input twice and its output once through these paths, so on a 27.5 GB
  (corpus-mean) input the 16 KiB loop spends about 20 CPU-seconds on input hashing where 1 MiB
  spends about 9. The time a transform waits on these reads does not change on this storage.
- **As-found is mostly cold.** The corpus is about 7× RAM. Residency before each as-found
  repetition was 28–32 GB, but physical reads fell only 6–9 GB below the corpus size: most
  resident pages belonged to files read late in the previous pass and were evicted before
  their turn.

## Disclosures

- The examples time the function, not a whole transform. A real output is observed while its
  write may still be cached, and real input observation competes with the transform's CPU.
  Neither was measured (`not_measured` in each record).
- Drive and controller caches, btrfs metadata after per-file eviction, and other host
  activity were not controlled. The host had no other measurement or build running.
- The network corpus is excluded from #838 and owned by the maintainer.
- A production buffer change is not made here; it belongs to a separate implementation issue.

## Reproduce

With a private manifest from the #788 `manifest` command:

```sh
python3 scripts/media-evidence-candidates.py prepare observe16k <scratch>/observe16k
python3 scripts/media-evidence-candidates.py prepare observe1m <scratch>/observe1m
python3 <scratch>/observe1m/scripts/media-evidence-observe.py run <private-dir>/local-manifest.json \
  --path ffmpeg --example-dir <scratch>/observe1m/target/release/examples \
  --max-bytes 3000000000000 --repetitions 3 --evict \
  --output docs/media-evidence-baseline/observe/<date>-ffmpeg-1m-local-corpus-evicted.json
python3 scripts/media-evidence-observe.py compare <16k-record.json> <1m-record.json>
git worktree remove --force <scratch>/observe1m
```

As-found cells drop `--evict`; `--path` is `ffmpeg`, `mkvtoolnix`, `verify-artifact` or
`ffprobe`.
