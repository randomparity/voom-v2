#!/usr/bin/env python3
"""Focused failure controls for the opt-in artifact transfer experiment."""
import importlib.util
import json
import re
import sys
import tarfile
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'experiment', Path(__file__).with_name('ci-sharding-feasibility.py'))
experiment = importlib.util.module_from_spec(spec)
spec.loader.exec_module(experiment)


class InventoryControls(unittest.TestCase):
    def test_omission_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'inventory'):
            experiment.reconcile(['required_test'], [], '')

    def test_duplicate_is_rejected(self):
        with self.assertRaises(ValueError):
            experiment.reconcile(['case'], [], 'test case ... ok\ntest case ... ok\n')

    def test_ignored_policy_is_preserved(self):
        with self.assertRaises(ValueError):
            experiment.reconcile(['case'], ['case'], 'test case ... ok\n')
        self.assertEqual(experiment.reconcile(['case'], ['case'],
                                              'test case ... ignored, opt-in\n'),
                         {'case': 'ignored'})

    def test_failure_is_rejected(self):
        with self.assertRaises(ValueError):
            experiment.reconcile(['case'], [], 'test case ... FAILED\n')

    def test_empty_inventory_is_valid(self):
        self.assertEqual(experiment.reconcile([], [], ''), {})


class ArtifactControls(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.output = self.root / '.tmp/641'
        self.output.mkdir(parents=True)
        for name, value in [('ROOT', self.root), ('OUT', self.output)]:
            patcher = patch.object(experiment, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.binary = self.root / 'target/debug/worker'
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b'fixture executable bytes')
        self.binary.chmod(0o755)
        self.record = experiment.file_record(self.binary)

    def archive(self):
        with tarfile.open(self.output / 'executables.tar', 'w') as archive:
            archive.add(self.binary, arcname=self.record['path'], filter=experiment.tar_metadata)
        self.binary.unlink()
        return {'files': [self.record], 'archive_sha256': experiment.digest(self.output / 'executables.tar')}

    def test_worker_change_and_mode_change_are_rejected(self):
        experiment.verify_files([self.record])
        self.binary.chmod(0o644)
        with self.assertRaises(ValueError):
            experiment.verify_files([self.record])
        self.binary.chmod(0o755)
        self.binary.write_bytes(b'changed')
        with self.assertRaises(ValueError):
            experiment.verify_files([self.record])

    def test_safe_archive_preserves_hash_and_executable_mode(self):
        experiment.extract(self.archive())
        self.assertEqual(experiment.file_record(self.binary), self.record)

    def test_traversal_and_duplicate_members_are_rejected(self):
        for name in ('../outside', '/absolute', 'target/debug/../outside'):
            with self.subTest(name=name), self.assertRaises(ValueError):
                experiment.checked_relative(name)
        manifest = self.archive()
        manifest['files'].append(self.record)
        with self.assertRaises(ValueError):
            experiment.extract(manifest)

    def test_symlink_ancestor_is_rejected_before_write(self):
        manifest = self.archive()
        self.binary.parent.rmdir()
        outside = self.root / 'outside'
        outside.mkdir()
        self.binary.parent.symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'symlink'):
            experiment.extract(manifest)
        self.assertEqual(list(outside.iterdir()), [])

    def test_wrong_provenance_stops_before_extract(self):
        (self.output / 'manifest.json').write_text(json.dumps({'identity': {}}))
        with patch.object(experiment, 'identity', return_value={'commit': 'different'}), \
             patch.object(experiment, 'extract') as extract:
            with self.assertRaisesRegex(ValueError, 'provenance'):
                experiment.consumer(0)
            extract.assert_not_called()

    def test_environment_does_not_capture_secrets_or_unknown_package_keys(self):
        env = {'AWS_SECRET_ACCESS_KEY': 'secret', 'CARGO_PKG_PASSWORD': 'secret',
               'CARGO_MANIFEST_DIR': '/workspace/crates/example'}
        selected = experiment.env_record(env, {'@ROOT@': '/workspace'})
        self.assertNotIn('secret', json.dumps(selected))
        self.assertEqual(selected['CARGO_MANIFEST_DIR'], '@ROOT@/crates/example')
        with self.assertRaises(ValueError):
            experiment.env_record({'CARGO': '/unbound/cargo'}, {})

    def test_native_macos_loader_defaults_are_preserved(self):
        value = '@HOME@/lib:/usr/local/lib:/usr/lib'
        self.assertEqual(experiment.env_record({'DYLD_FALLBACK_LIBRARY_PATH': value},
                         {'@HOME@': '/home/fixture'})['DYLD_FALLBACK_LIBRARY_PATH'], value)
        with self.assertRaises(ValueError):
            experiment.env_record({'DYLD_FALLBACK_LIBRARY_PATH': '/untrusted/lib'}, {})

    def test_nonzero_child_exit_is_not_success(self):
        with patch.object(experiment, 'tokens', return_value={}):
            with self.assertRaisesRegex(RuntimeError, 'exit 7'):
                experiment.call([sys.executable, '-c', 'raise SystemExit(7)'], cwd=self.root)


class WorkflowControls(unittest.TestCase):
    def test_exact_six_job_publication(self):
        text = (Path(__file__).resolve().parents[1] / '.github/workflows/ci.yml').read_text()
        chunks = re.split(r'^  ([a-z-]+):\n', text.split('jobs:\n', 1)[1], flags=re.M)
        jobs = dict(zip(chunks[1::2], chunks[2::2]))
        self.assertEqual(set(jobs), {'source-guards', 'test', 'coverage', 'shard-producer', 'shard-consumer'})
        normal = "github.event_name != 'push' || github.ref != 'refs/heads/feat/ci-test-sharding-641'"
        selected = "github.event_name == 'push' && github.ref == 'refs/heads/feat/ci-test-sharding-641'"
        for name in ('source-guards', 'test', 'coverage'):
            self.assertIn(normal, jobs[name].split('steps:', 1)[0])
        for name, timeout, axes in [('shard-producer', 45, ['os']), ('shard-consumer', 15, ['os', 'shard'])]:
            job = jobs[name]
            self.assertIn('if: ' + selected, job)
            self.assertIn(f'timeout-minutes: {timeout}', job)
            matrix = job.split('matrix:\n', 1)[1].split('    steps:', 1)[0]
            self.assertEqual(re.findall(r'^        ([a-z]+):', matrix, re.M), axes)
            self.assertIn('os: [ubuntu-latest, macos-latest]', matrix)
        self.assertIn('shard: [0, 1]', jobs['shard-consumer'])
        self.assertIn('needs: shard-producer', jobs['shard-consumer'])
        self.assertIn('cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info -- --test-threads=1', jobs['coverage'])
        self.assertIn('branches: [main, feat/ci-test-sharding-641]', text)


if __name__ == '__main__':
    unittest.main()
