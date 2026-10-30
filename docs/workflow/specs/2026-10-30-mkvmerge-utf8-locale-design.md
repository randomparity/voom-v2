# mkvmerge runs with an explicit UTF-8 locale (#819)

## Problem

The node agent starts workers with a cleared environment, so the bundled
mkvtoolnix worker runs mkvmerge with no locale. mkvmerge v102 then converts
through ASCII: `--identify --identification-format json` stops at the first
non-ASCII character yet exits 0, and a non-ASCII path cannot be opened. The
worker reports only the JSON parser error.

## Scope

Set `LC_ALL=C.UTF-8` on each mkvmerge child the worker starts: preflight
`--version`, `--identify`, and the remux run (the worker starts no other
MKVToolNix tool); one worker constant owns the pair. The value overrides any
inherited locale, so parsing does not depend on the host environment.

Identify output that does not parse as JSON after exit 0 fails with
`IdentifyFailed` naming the byte count, the parser error, the cause (mkvmerge
did not emit UTF-8 under `LC_ALL=C.UTF-8`) and the action (make a UTF-8 locale
usable by the mkvmerge at the configured path). The error class is unchanged.

Rejected: charset options. `--output-charset UTF-8` repairs identify JSON, but
`env -i mkvmerge --command-line-charset UTF-8 --output-charset UTF-8 -J dír/f.mkv`
exits 2, "could not be opened" (Fedora 44, v102). The locale fixes both; no ADR.

### Failure model

1. Actors and deployments: the node agent on a Linux or macOS host, and the
   worker started directly by an operator.
2. Invariants: identify facts drive stream selection and output verification;
   a truncated or misread identification must fail, never yield wrong facts.
3. Accepted: a host whose mkvmerge cannot use `C.UTF-8` gets the actionable
   identify error for non-ASCII metadata, and the existing open-failure error
   for non-ASCII paths; neither yields wrong facts. Measured on Fedora 44, v102:
   `env -i LC_ALL=xx_YY.UTF-8` still yields complete JSON.
4. Covered elsewhere: other workers' tools (sweep reported to the campaign),
   the cleared agent environment (#817), evidence rerun (#794). The value is
   a compile-time literal; no input reaches it.

## Success

- With a cleared environment, identify of a file whose track name contains
  non-ASCII characters returns that exact name, at ASCII and non-ASCII paths.
- A remux of that file to a non-ASCII output path succeeds and preserves it.
- Exit-0 truncated identify output yields the actionable error; preflight
  `--version` runs with the same locale.

## Validation

- Identify and remux under a cleared environment — Mode: focused-test.
  `mkvmerge_test` re-executes itself with `env_clear()` and real mkvmerge on a
  fixture named `Director’s cut`, built from a UTF-8 JSON option file, at an
  ASCII path (red: truncated JSON) and a non-ASCII path (red: open failure).
  It passed on the ubuntu-latest and macos-latest (Homebrew) CI test jobs with
  `C.UTF-8`; that is the criterion-5 evidence (PR #826).
- Truncated output error — Mode: focused-test. A stub prints truncated JSON and
  exits 0; the error names the cause, action, and byte count.
- Locale on preflight and remux — Mode: focused-test. Stubs succeed only when
  `LC_ALL=C.UTF-8`; red when the variable is absent.
