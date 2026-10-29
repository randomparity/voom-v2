"""Private deployment must work through the node agent's cleared environment."""
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import install
import runtime


class InstallTests(unittest.TestCase):
    def setUp(self):
        pinned = Path(__file__).resolve().parents[2] / ".test-tmp"
        pinned.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=pinned)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source, self.stage = self.root / "source", self.root / "stage"
        self.source.mkdir()
        self.stage.mkdir()
        self.config = self.root / "config.json"
        value = {"engine": "docker", "image": "sha256:" + "1" * 64,
                 "read_only_roots": [str(self.source)],
                 "read_write_roots": [str(self.stage)]}
        if platform.system() == "Darwin":
            value["desktop"] = "desktop-linux"
        self.config.write_text(json.dumps(value))
        self.original = self.root / "original runtime.py"
        self.original.write_text('''import json, os, sys
if sys.argv[1] == "check-config" or "-version" in sys.argv:
    sys.exit(0)
sys.stdout.buffer.write(sys.stdin.buffer.read())
sys.stderr.write(json.dumps({"parent": os.getppid(), "cwd": os.getcwd(),
                           "argv": sys.argv[2:], "env": dict(os.environ)}))
sys.exit(23)
''')
        self.engine = self.root / "engine tools" / "docker"
        self.engine.parent.mkdir()
        self.engine.write_text("#!/bin/sh\nexit 0\n")
        self.engine.chmod(0o700)
        self.destination = self.root / "installed ' tools"
        for context in [patch.object(install, "RUNTIME_PATH", self.original),
                        patch.dict(os.environ, {"HOME": str(self.root)}, clear=True),
                        patch("shutil.which", return_value=str(self.engine)),
                        patch("os.getcwd", return_value=str(self.stage))]:
            context.start()
            self.addCleanup(context.stop)

    def test_private_exec_survives_checkout_removal_and_empty_environment(self):
        tools = install.install(self.config, self.destination, [])
        self.original.unlink()
        self.config.unlink()
        self.assertEqual(self.destination.stat().st_mode & 0o777, 0o700)
        for name in ["config.json", "runtime.py"]:
            self.assertEqual((self.destination / name).stat().st_mode & 0o777, 0o600)
        for tool in tools.values():
            self.assertEqual(Path(tool).stat().st_mode & 0o777, 0o700)
            result = subprocess.run([tool, "literal ' argument", "$(must-not-execute)"],
                                    cwd=self.stage, input=b"\x00\xff\n", capture_output=True,
                                    env={"VOOM_MEDIA_CALLER_PID": str(os.getpid())}, check=False)
            self.assertEqual(result.returncode, 23)
            self.assertEqual(result.stdout, b"\x00\xff\n")
            observed = json.loads(result.stderr)
            self.assertEqual(observed["parent"], os.getpid())
            self.assertEqual(observed["cwd"], str(self.stage))
            self.assertEqual(observed["argv"], ["literal ' argument", "$(must-not-execute)"])
            self.assertEqual(observed["env"]["HOME"], str(self.root))
            self.assertEqual(observed["env"]["VOOM_MEDIA_CALLER_PID"], str(os.getpid()))
            self.assertEqual(observed["env"]["VOOM_MEDIA_CONTAINER_CONFIG"],
                             str(self.destination / "config.json"))
            self.assertNotIn("XDG_CONFIG_HOME", observed["env"])

    def test_existing_and_symlink_destinations_are_never_overwritten(self):
        for symlink in [False, True]:
            destination = self.root / str(symlink)
            if symlink:
                destination.symlink_to(self.source, target_is_directory=True)
            else:
                destination.mkdir()
            with self.subTest(symlink=symlink), self.assertRaises(runtime.SetupError):
                install.install(self.config, destination, [])
            self.assertTrue(destination.exists())

    def test_media_overlap_is_rejected_before_engine_access(self):
        for root in [self.source, self.stage]:
            with self.subTest(root=root), patch.object(install, "validate_install") as validate:
                with self.assertRaisesRegex(runtime.SetupError, "media grants"):
                    install.install(self.config, root / "installed", [])
                validate.assert_not_called()
                self.assertFalse((root / "installed").exists())
        with self.assertRaisesRegex(runtime.SetupError, "media grants"):
            install.validate_separation(self.root, runtime.load_config(str(self.config)))

    def test_mounted_destination_rejected_before_copied_config_can_change(self):
        replacement = self.root / "replacement-stage"
        replacement.mkdir()
        original = install.write_private

        def decoder_write(path, data, mode):
            if path.name == "config.json":
                value = json.loads(data)
                value["read_write_roots"] = [str(replacement)]
                data = json.dumps(value).encode()
            original(path, data, mode)

        destination = self.stage / "installed"
        with patch("os.getcwd", return_value=str(self.source)), \
                patch.object(install, "write_private", side_effect=decoder_write) as writes:
            with self.assertRaisesRegex(runtime.SetupError, "media grants"):
                install.install(self.config, destination, [])
            writes.assert_not_called()
        self.assertFalse(destination.exists())

    def test_private_copy_is_revalidated_before_setup(self):
        original = install.write_private

        def changed_copy(path, data, mode):
            if path.name == "config.json":
                value = json.loads(data)
                value["read_write_roots"] = [str(self.destination)]
                data = json.dumps(value).encode()
            original(path, data, mode)

        with patch("os.getcwd", return_value=str(self.source)), \
                patch.object(install, "write_private", side_effect=changed_copy), \
                patch.object(install, "validate_install") as validate:
            with self.assertRaisesRegex(runtime.SetupError, "media grants"):
                install.install(self.config, self.destination, [])
            validate.assert_not_called()
        self.assertFalse(self.destination.exists())

    def test_failed_setup_removes_only_new_installation(self):
        with patch.object(install, "validate_install", side_effect=runtime.SetupError("setup")):
            with self.assertRaisesRegex(runtime.SetupError, "setup"):
                install.install(self.config, self.destination, [])
        self.assertFalse(self.destination.exists())
        self.assertTrue(self.original.exists())
        self.assertTrue(self.config.exists())

    def test_supported_context_is_explicit_and_other_steering_is_rejected(self):
        for name in install.OPTIONAL_CONTEXT:
            with self.subTest(name=name), patch.dict(os.environ, {name: str(self.root)}):
                context = install.capture_context("docker")
                self.assertEqual(context[name], str(self.root))
        for name in ["DOCKER_HOST", "DOCKER_API_VERSION", "CONTAINER_HOST",
                     "CONTAINERS_STORAGE_CONF", "PODMAN_CONNECTIONS_CONF", "STORAGE_OPTS"]:
            with self.subTest(name=name), patch.dict(os.environ, {name: "override"}):
                with self.assertRaisesRegex(runtime.SetupError, "unsupported"):
                    install.capture_context("docker")
        self.assertEqual(install.capture_context("docker")["PATH"],
                         str(self.engine.parent) + os.pathsep + os.defpath)

    def test_untrusted_parent_and_source_symlink_fail_without_installing(self):
        self.root.chmod(0o777)
        with self.assertRaisesRegex(runtime.SetupError, "private parent"):
            install.install(self.config, self.destination, [])
        self.root.chmod(0o700)
        alias = self.root / "alias.json"
        alias.symlink_to(self.config)
        with self.assertRaises(runtime.SetupError):
            install.install(alias, self.destination, [])
        self.assertFalse(self.destination.exists())
