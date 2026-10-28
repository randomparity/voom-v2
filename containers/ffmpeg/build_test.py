"""Prefetched inputs fail before engine access; in-image verification remains mandatory."""
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
