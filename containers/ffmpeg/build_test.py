"""Source inputs are verified on the host before engine access; the builder verifies again."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class PrefetchedInputTests(unittest.TestCase):
    def test_missing_extra_symlinked_and_tampered_inputs_are_rejected(self):
        recipe = Path(__file__).resolve().parent
        pinned = recipe.parents[1] / ".test-tmp"
        pinned.mkdir(exist_ok=True)
        names = [line.split()[1] for line in (recipe / "source-sha256").read_text().splitlines()]
        with tempfile.TemporaryDirectory(dir=pinned) as temporary:
            root = Path(temporary)
            for kind in ("missing", "extra", "symlink", "tampered"):
                with self.subTest(kind=kind):
                    source = root / kind
                    source.mkdir()
                    for name in names:
                        (source / name).write_bytes(b"untrusted replacement")
                    if kind == "missing":
                        (source / names[0]).unlink()
                    elif kind == "extra":
                        (source / "unexpected").write_bytes(b"extra")
                    elif kind == "symlink":
                        (source / names[0]).unlink()
                        (source / names[0]).symlink_to(source / names[1])
                    result = subprocess.run(["bash", str(recipe / "build.sh"), "docker", str(source)],
                                            capture_output=True, timeout=5)
                    self.assertEqual(result.returncode, 2, result.stderr.decode())
                    self.assertNotIn(b"docker", result.stderr.lower())


STUB_CURL = """#!/usr/bin/env bash
output=
while (($#)); do
    case $1 in --output) output=$2; shift ;; *) url=$1 ;; esac
    shift
done
printf '%s\\n' "$url" >> "$STUB_LOG"
served="$STUB_SERVED/$(printf %s "$url" | tr '/:' '__')"
[[ -f $served ]] || exit 22
cp "$served" "$output"
"""


class FetchSourcesTests(unittest.TestCase):
    """Mirrors are untrusted transport: only bytes matching source-sha256 may reach DEST."""

    GOOD = {"a": b"pinned a", "b": b"pinned b"}
    URLS = {"a": ["https://first.test/a", "https://second.test/a"], "b": ["https://first.test/b"]}

    def setUp(self):
        pinned = Path(__file__).resolve().parents[2] / ".test-tmp"
        pinned.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=pinned)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.recipe = self.root / "recipe"
        self.recipe.mkdir()
        script = Path(__file__).resolve().parent / "fetch-sources.sh"
        (self.recipe / "fetch-sources.sh").write_bytes(script.read_bytes())
        (self.recipe / "source-sha256").write_text("".join(
            f"{hashlib.sha256(data).hexdigest()}  {name}\n" for name, data in self.GOOD.items()))
        (self.recipe / "source-urls").write_text("".join(
            f"{name} {url}\n" for name, urls in self.URLS.items() for url in urls))
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        (bin_dir / "curl").write_text(STUB_CURL)
        (bin_dir / "curl").chmod(0o755)
        self.served = self.root / "served"
        self.served.mkdir()
        self.log = self.root / "curl.log"
        self.log.touch()
        self.dest = self.root / "dest"
        self.dest.mkdir()
        self.env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                        STUB_LOG=str(self.log), STUB_SERVED=str(self.served))

    def serve(self, url, data):
        (self.served / url.replace("/", "_").replace(":", "_")).write_bytes(data)

    def fetch(self):
        return subprocess.run(["bash", str(self.recipe / "fetch-sources.sh"), str(self.dest)],
                              capture_output=True, env=self.env, timeout=10)

    def requested(self):
        return self.log.read_text().split()

    def test_unreachable_first_mirror_falls_through_to_next_verified_one(self):
        self.serve("https://second.test/a", self.GOOD["a"])
        self.serve("https://first.test/b", self.GOOD["b"])
        result = self.fetch()
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual({p.name: p.read_bytes() for p in self.dest.iterdir()}, self.GOOD)
        self.assertEqual(self.requested(), [*self.URLS["a"], *self.URLS["b"]])

    def test_wrong_bytes_from_every_mirror_fail_closed_without_installing(self):
        for url in self.URLS["a"]:
            self.serve(url, b"tampered")
        self.serve("https://first.test/b", self.GOOD["b"])
        result = self.fetch()
        self.assertEqual(result.returncode, 1)
        self.assertIn(b"pinned a;", result.stderr)
        self.assertEqual(list(self.dest.iterdir()), [])

    def test_verified_existing_inputs_need_no_network(self):
        for name, data in self.GOOD.items():
            (self.dest / name).write_bytes(data)
        result = self.fetch()
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(self.requested(), [])

    def test_tampered_and_symlinked_existing_inputs_are_replaced(self):
        (self.dest / "a").write_bytes(b"poisoned cache")
        (self.root / "elsewhere").write_bytes(self.GOOD["b"])
        (self.dest / "b").symlink_to(self.root / "elsewhere")
        self.serve("https://first.test/a", self.GOOD["a"])
        self.serve("https://first.test/b", self.GOOD["b"])
        result = self.fetch()
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertFalse((self.dest / "b").is_symlink())
        self.assertEqual(self.requested(), [self.URLS["a"][0], self.URLS["b"][0]])
        self.assertEqual({p.name: p.read_bytes() for p in self.dest.iterdir()}, self.GOOD)
