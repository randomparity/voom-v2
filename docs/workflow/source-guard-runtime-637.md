# Source-guard runtime evidence — #637

Issue #637 requested measured runtime reductions with AST coverage, diagnostics,
failure behavior and configured hooks preserved. The approved implementation keeps
SQL rules and file-local alias processing, selects candidate files structurally,
and replaces payload per-line subprocesses with one POSIX awk source pass.

Measurements below use the unchanged production Rust tree at the issue base and
ast-grep 0.45.3 / just 1.58.0. Baseline values are medians of two sequential paired
runs. Candidate values are a final sequential run after the last source-operand
regression fix. Every recipe exited 0 in both baseline samples and the final run.

| Recipe | macOS baseline (s) | macOS candidate (s) | Linux baseline (s) | Linux candidate (s) |
| --- | ---: | ---: | ---: | ---: |
| check-control-plane-sql-boundary | 15.025 | 7.015 | 3.340 | 1.665 |
| check-control-plane-sql-boundary-selftest | 8.210 | 8.964 | 4.963 | 5.712 |
| check-payload-deny-unknown | 10.743 | 0.127 | 3.356 | 0.066 |
| check-payload-deny-unknown-selftest | 0.841 | 0.610 | 0.481 | 0.260 |
| Sum | 34.819 | 16.716 | 12.140 | 7.703 |

macOS ran natively on arm64 with Bash 3.2 and BSD tools. Linux ran in a native
arm64 Debian trixie Docker container with Bash 5.2 and GNU tools, reading the same
checkout through a read-only bind mount. Linux timings measure `just` inside the
container and exclude startup. They are local measurements, not hosted CI claims.
Earlier overlapping host/container samples were discarded. Timing variation and
storage differences prevent a guaranteed speedup on every machine.

Two earlier paired candidate runs corroborated the guard improvement: SQL ranges
were 6.917–7.056s on macOS and 1.675–1.705s on Linux; payload ranges were
0.124–0.263s and 0.060–0.063s respectively. SQL selftests take longer because the
candidate scan and additional coverage/tool-error fixtures also run there. The
four-recipe total still falls by about 52% on macOS and 37% on Linux.

All existing negative fixtures remain. Additional regressions exercise raw SQL
identifiers, macro token trees, comment/string-only files, empty candidates,
scanner failure/malformed output, payload processor failure, numeric coordinates,
assignment-shaped relative source operands, and exact diagnostic locations/counts
and ordering. Tool-error stubs require exit 2 without a clean result. Controlled
SQL candidate-selection and payload deny-check faults each made the real selftest
fail; both faults were restored before green runs. The source-operand regression
was observed false-clean before its fix, then rejected correctly afterward.

The configured `prek` hooks, justfile and CI workflow remain unchanged. ShellCheck,
shfmt, Bash syntax and focused guard recipes cover the changed shell surfaces;
final whole-repository and hosted CI results are recorded on the pull request.

Excluded work remains assigned to #259 (multiline serde), #485 (broader hooks),
#638/#639 (build/profile and runner optimization), #640/#641 (guard scheduling and
sharding), and separately authorized future work for unmeasured guards.
