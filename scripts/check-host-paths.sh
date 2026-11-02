#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
from pathlib import Path
import re
import subprocess
import sys

# NUL-delimited tracked names preserve spaces and exclude local scratch files.
root = subprocess.check_output(["git", "rev-parse", "--show-toplevel"]).decode().strip()
names = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).split(b"\0")
# Placeholder segments (<user>, $USER, ${USER}) are deliberately portable.
host_path = re.compile(r'''/(?:home|Users|mnt)/[^\s/<>"'`$]+''')
violations = 0
for raw_name in names:
    if not raw_name:
        continue
    name = raw_name.decode("utf-8", errors="surrogateescape")
    if not name.endswith(".md") or ("/" in name and not name.startswith("docs/")):
        continue
    path = Path(root) / name
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if host_path.search(line):
            # Do not repeat the private path in a CI log.
            print(f"{name}:{number}: replace host path with a relative path or placeholder")
            violations += 1
if violations:
    print(f"check-host-paths: {violations} violation(s)")
    sys.exit(1)
print("check-host-paths: OK")
PY
