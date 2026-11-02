#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
python3 - "$script_dir/check-host-paths.sh" <<'PY'
from pathlib import Path
import subprocess
import sys
import tempfile

check = sys.argv[1]
with tempfile.TemporaryDirectory(prefix="voom-host-paths-") as directory:
    root = Path(directory)
    subprocess.run(["git", "init", "--quiet", directory], check=True)

    def write(name, text, tracked=True):
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        if tracked:
            subprocess.run(["git", "add", "--", name], cwd=root, check=True)

    def run(expected, diagnostics=()):
        result = subprocess.run(["bash", check], cwd=root, capture_output=True, text=True)
        assert result.returncode == expected, result
        for diagnostic in diagnostics:
            assert diagnostic in result.stdout, result
        return result

    # Empty repositories, placeholders and relative paths must remain usable.
    run(0)
    write("README.md", "docs/spec.md <repo>/docs <media-root>/movie $HOME/media\n"
          "/home/<user>/media /Users/${USER}/media /mnt/<volume>/media\n")
    write("docs/nested/safe.md", "../other.md /usr/bin/tool\n")
    # Scope is the tracked root/docs Markdown, never deliberate fixture literals.
    for name in ("docs/untracked.md", "untracked.md"):
        write(name, "/home/fixture/private\n", tracked=False)
    for name in ("scripts/fixture.md", "crates/example/tests/fixture.md", "docs/data.txt"):
        write(name, "/home/fixture/private\n")
    run(0)

    for family in ("home", "Users", "mnt"):
        name = f"docs/nested/{family} path with spaces.md"
        write(name, f"Safe first line\n`/{family}/fixture/private`\n")
        result = run(1, [f"{name}:2:"])
        assert "fixture/private" not in result.stdout, result
        assert result.stdout.count(":2:") == 1, result
        write(name, "<repo>/safe\n")
        run(0)

    write("Root with spaces.md", "/mnt/fixture\n")
    run(1, ["Root with spaces.md:1:"])
    write("Root with spaces.md", "<media-root>\n")
    # An unstaged change to an already tracked document must be checked too.
    write("README.md", '"sqlite:///Users/fixture/db"\n', tracked=False)
    run(1, ["README.md:1:"])
    write("README.md", "$HOME/db\n")
    run(0)
print("check-host-paths-selftest: safe controls and all three path families passed")
PY
