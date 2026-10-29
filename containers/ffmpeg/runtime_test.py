"""Contract tests for the opt-in container boundary; no engine is required."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import runtime


class ConfigTests(unittest.TestCase):
    def setUp(self):
        pinned = Path(__file__).resolve().parents[2] / ".test-tmp"
        pinned.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=pinned)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.source = self.root / "source with spaces"
        self.stage = self.root / "stage"
        self.source.mkdir()
        self.stage.mkdir()
        self.file = self.root / "config.json"
        self.values = {"engine": "docker", "image": "sha256:" + "1" * 64,
                       "read_only_roots": [str(self.source)],
                       "read_write_roots": [str(self.stage)]}

    def load(self, system="Linux"):
        self.file.write_text(json.dumps(self.values))
        return runtime.load_config(str(self.file), system=system, cwd=str(self.stage))

    def test_explicit_grants_and_immutable_image(self):
        config = self.load()
        self.assertEqual(config.read_only_roots, (str(self.source),))
        self.assertEqual(config.read_write_roots, (str(self.stage),))
        self.assertEqual(config.image, self.values["image"])

    def test_rejects_invalid_configuration_before_engine_access(self):
        cases = [{"extra": True}, {"engine": "shell"}, {"image": "latest"},
                 {"read_only_roots": "not a list"}, {"read_write_roots": ["/"]},
                 {"read_write_roots": [str(self.root)]},
                 {"read_only_roots": [str(self.stage)]},
                 {"read_only_roots": [str(self.root / "missing")]},
                 {"desktop": "desktop-linux"}]
        for changes in cases:
            with self.subTest(changes=changes):
                previous = self.values.copy()
                self.values.update(changes)
                with self.assertRaises(runtime.SetupError):
                    self.load()
                self.values = previous

    def test_rejects_symlink_mount_and_undeclared_cwd(self):
        alias = self.root / "alias"
        alias.symlink_to(self.source, target_is_directory=True)
        self.values["read_only_roots"] = [str(alias)]
        with self.assertRaises(runtime.SetupError):
            self.load()
        self.values["read_only_roots"] = [str(self.source)]
        self.file.write_text(json.dumps(self.values))
        with self.assertRaises(runtime.SetupError):
            runtime.load_config(str(self.file), system="Linux", cwd=str(self.root))

    def test_desktop_selector_is_explicit_and_platform_specific(self):
        with self.assertRaises(runtime.SetupError):
            self.load("Darwin")
        self.values["desktop"] = "desktop-linux"
        self.assertEqual(self.load("Darwin").desktop, "desktop-linux")
        with self.assertRaises(runtime.SetupError):
            self.load("Windows")

    def test_mount_grammar_is_rejected_without_shell_interpolation(self):
        directory = self.root / "comma,readonly"
        directory.mkdir()
        self.values["read_only_roots"] = [str(directory)]
        with self.assertRaises(runtime.SetupError):
            self.load()

    def test_missing_empty_or_noncanonical_source_witness_fails_before_engine(self):
        config = self.load()
        empty = self.source / "empty"
        empty.touch()
        witness = self.source / "witness"
        witness.write_bytes(b"content")
        alias = self.source / "alias"
        alias.symlink_to(witness)
        for witnesses in ([], [str(empty)], [str(alias)], [str(self.source / "missing")]):
            with self.subTest(witnesses=witnesses), patch.object(runtime, "setup_capture") as run:
                with self.assertRaises(runtime.SetupError):
                    runtime.check_config(config, witnesses)
                run.assert_not_called()


    def test_copied_audio_proof_passes_platform_configuration_to_installer(self):
        repository = Path(__file__).resolve().parents[2]
        spec = importlib.util.spec_from_file_location(
            "copied_audio_regression", repository / "scripts/issue-786-copied-audio-regression.py")
        proof = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(proof)

        class InstallationObserved(Exception):
            pass

        for engine, system, desktop in [("docker", "Linux", ""),
                                        ("docker", "Darwin", "desktop-linux"),
                                        ("podman", "Darwin", "owned-machine")]:
            with self.subTest(engine=engine, system=system):
                root = self.root / f"proof-{engine}-{system}"
                root.mkdir()
                args = SimpleNamespace(engine=engine, image="sha256:" + "1" * 64,
                                       desktop=desktop, bin_dir=root / "unused")

                def external_command(arguments, cwd=None):
                    if Path(arguments[1]).name == "install.py":
                        config = runtime.parse_config((root / "container.json").read_bytes(),
                                                      system=system, cwd=str(root / "staging"))
                        self.assertEqual(config.desktop, desktop)
                        raise InstallationObserved
                    return b""

                with patch.object(proof.shutil, "which", return_value="/native/tool"), \
                        patch.object(proof, "run", side_effect=external_command), \
                        self.assertRaises(InstallationObserved):
                    proof.regression(args, root)


class EngineTests(unittest.TestCase):
    def test_desktop_inspection_uses_native_temp_without_changing_worker_tmpdir(self):
        config = runtime.Config("podman", "sha256:" + "1" * 64, (), (), "owned-machine")
        machine = [{"Name": config.desktop, "State": "running", "Rootful": False,
                    "ConnectionInfo": {"PodmanSocket": {"Path": "/native/engine.sock"}}}]
        info = {"host": {"security": {"rootless": True}, "arch": "arm64"}}
        image = [{"Os": "linux", "Architecture": "arm64", "Id": config.image}]
        pinned = Path(__file__).resolve().parents[2] / ".test-tmp"
        with tempfile.TemporaryDirectory(dir=pinned) as native, \
                patch.dict(os.environ, {"TMPDIR": "/worker/staging"}, clear=True), \
                patch.object(runtime, "capture", return_value=native.encode()) as native_temp, \
                patch.object(runtime, "unix_endpoint", return_value="unix:///native/engine.sock"), \
                patch.object(runtime, "json_capture", side_effect=[machine, info, image]) as inspect:
            engine = runtime.Engine(config)
            self.assertEqual(inspect.call_args_list[0].kwargs.get("env", {}).get("TMPDIR"), native)
            native_temp.assert_called_once_with(["getconf", "DARWIN_USER_TEMP_DIR"])
            self.assertEqual(os.environ["TMPDIR"], "/worker/staging")
            self.assertEqual(engine.prefix, ["podman", "--url", "unix:///native/engine.sock"])

    def test_steering_override_and_remote_endpoint_fail_before_engine_access(self):
        config = runtime.Config("docker", "sha256:" + "1" * 64, (), ())
        for name in ("DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_TLS_VERIFY",
                     "DOCKER_CERT_PATH", "CONTAINER_HOST", "CONTAINER_CONNECTION"):
            with self.subTest(name=name), patch.dict(os.environ, {name: "untrusted"}, clear=True), \
                    patch.object(runtime, "json_capture") as run:
                with self.assertRaises(runtime.SetupError):
                    runtime.Engine(config)
                run.assert_not_called()
        for endpoint in ("tcp://localhost:2375", "ssh://remote", "/tmp/socket"):
            with self.assertRaises(runtime.SetupError):
                runtime.unix_endpoint(endpoint)

    def test_image_platform_and_docker_user_namespace_fail_closed(self):
        config = runtime.Config("docker", "sha256:" + "1" * 64, (), ())
        context = [{"Endpoints": {"docker": {"Host": "unix:///engine"}}}]
        info = {"OSType": "linux", "Architecture": "amd64", "SecurityOptions": []}
        image = {"Os": "linux", "Architecture": "amd64", "Id": config.image}
        cases = [(dict(info, SecurityOptions=["name=rootless"]), image),
                 (dict(info, SecurityOptions=["name=userns"]), image),
                 (info, dict(image, Architecture="arm64")),
                 (info, dict(image, Os="windows"))]
        for engine_info, image_info in cases:
            with self.subTest(info=engine_info, image=image_info), \
                    patch.dict(os.environ, {}, clear=True), \
                    patch.object(runtime, "unix_endpoint", return_value="unix:///engine"), \
                    patch.object(runtime, "json_capture", side_effect=[context, engine_info, [image_info]]):
                with self.assertRaises(runtime.SetupError):
                    runtime.Engine(config)


class CallerTests(unittest.TestCase):
    def test_missing_wrong_and_malformed_identity_fail_closed(self):
        for identity in [None, "0", "1x", str(os.getpid())]:
            with self.subTest(identity=identity), patch.dict(os.environ, {}, clear=True):
                if identity is not None:
                    os.environ["VOOM_MEDIA_CALLER_PID"] = identity
                with self.assertRaises(runtime.SetupError):
                    runtime.CallerWatch()

    def test_caller_exit_before_launcher_initialization_cannot_bind_adopter(self):
        release_read, release_write = os.pipe()
        result_read, result_write = os.pipe()
        parent = os.fork()
        if parent == 0:
            expected = os.getpid()
            if os.fork() == 0:
                os.close(release_write)
                os.close(result_read)
                os.read(release_read, 1)
                os.environ["VOOM_MEDIA_CALLER_PID"] = str(expected)
                try:
                    with runtime.CallerWatch():
                        result = b"accepted"
                except runtime.SetupError:
                    result = b"rejected"
                os.write(result_write, result)
            os._exit(0)
        os.close(release_read)
        os.close(result_write)
        try:
            os.waitpid(parent, 0)
            os.write(release_write, b"x")
            self.assertTrue(runtime.select.select([result_read], [], [], 5)[0])
            self.assertEqual(os.read(result_read, 32), b"rejected")
        finally:
            os.close(release_write)
            os.close(result_read)

    def test_live_actual_parent_watch_is_closeable(self):
        with patch.dict(os.environ, {"VOOM_MEDIA_CALLER_PID": str(os.getppid())}):
            with runtime.CallerWatch() as watch:
                self.assertFalse(watch.dead())

class CleanupTests(unittest.TestCase):
    def engine(self):
        engine = object.__new__(runtime.Engine)
        engine.prefix = ["engine"]
        engine.config = runtime.Config("docker", "unused", (), ())
        return engine

    def test_failed_remove_requires_successful_exact_absence_inventory(self):
        engine = self.engine()
        identity = "a" * 64
        with patch.object(runtime, "capture", side_effect=[runtime.SetupError("gone"), b""]) as run:
            engine.remove(identity, runtime.time.monotonic() + 5)
        self.assertIn("id=" + identity, run.call_args.args[0])
        for result in [runtime.SetupError("engine unavailable"), identity.encode()]:
            with patch.object(runtime, "capture", side_effect=[runtime.SetupError("rm failed"), result]):
                with self.assertRaises(runtime.SetupError):
                    engine.remove(identity, runtime.time.monotonic() + 5)

    def test_interrupted_create_never_removes_a_foreign_name_or_label(self):
        engine = self.engine()
        for name, label in [("foreign", "owned"), ("owned", "foreign")]:
            with patch.object(runtime, "capture", return_value=b"a" * 64), \
                    patch.object(runtime, "json_capture", return_value=[
                        {"Name": name, "Config": {"Labels": {runtime.LABEL: label}}}]), \
                    patch.object(engine, "remove") as remove:
                engine.reconcile("owned", runtime.time.monotonic() + 5)
                remove.assert_not_called()

    def test_interrupted_create_removes_only_verified_exact_identity(self):
        engine = self.engine()
        identity = "a" * 64
        with patch.object(runtime, "capture", return_value=identity.encode()), \
                patch.object(runtime, "json_capture", return_value=[
                    {"Name": "/owned", "Config": {"Labels": {runtime.LABEL: "owned"}}}]), \
                patch.object(engine, "remove") as remove:
            engine.reconcile("owned", runtime.time.monotonic() + 5)
            self.assertEqual(remove.call_args.args[0], identity)

    def test_podman_maps_server_identity_to_nonroot_image_user_without_host_uid_guess(self):
        engine = self.engine()
        engine.config = runtime.Config("podman", "unused", (), ())
        engine.image = "immutable"
        args = engine.create_args("owned", "/usr/local/bin/ffmpeg", ["-version"])
        self.assertIn("--userns=keep-id:uid=65532,gid=65532", args)
        self.assertEqual(args[args.index("--user") + 1], "65532:65532")
        self.assertNotIn("label=disable", " ".join(args))

    def test_watch_registration_failure_fails_closed(self):
        target, method = (os, "pidfd_open") if runtime.platform.system() == "Linux" else (runtime.select, "kqueue")
        with patch.dict(os.environ, {"VOOM_MEDIA_CALLER_PID": str(os.getppid())}), \
                patch.object(target, method, side_effect=ProcessLookupError("caller exited")):
            with self.assertRaises(runtime.SetupError):
                runtime.CallerWatch()


if __name__ == "__main__":
    unittest.main()
