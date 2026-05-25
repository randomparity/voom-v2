# Chaos Librarian E2E

The Chaos Librarian deterministic E2E suite runs with:

```bash
just chaos-e2e-ci
```

It is intentionally outside default `just ci` because it requires `uv`, Python
3.13, ffmpeg/ffprobe 7.0+, MKVToolNix, and the pinned Chaos Librarian submodule.

Maintainers should run the manual `chaos-e2e` GitHub Actions workflow before or
after changes that affect:

- the Chaos Librarian integration;
- media scan or observed-state export behavior;
- ffprobe, ffmpeg, or artifact verification workers;
- policy report or execution paths exercised by the Chaos fixtures.

The workflow is not a required merge gate. It exists to make the heavy media
suite reproducible in a clean runner while runtime cost and tool availability
are still being characterized.

The Ubuntu runner's apt ffmpeg package may lag Chaos Librarian's minimum. The
workflow therefore installs ffmpeg/ffprobe from a pinned 7.x archive and
verifies its checksum before running the suite.
