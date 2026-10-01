#!/usr/bin/env python3
"""One bounded Cargo/libtest transfer experiment; no production runner policy."""
import hashlib
import shutil
import json
import os
from pathlib import Path
import platform
import re
import stat
import subprocess
import sys
import tarfile
import time

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / '.tmp/641'


def digest(path):
    with Path(path).open('rb') as stream:
        value = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
        return value.hexdigest()


def checked_relative(value):
    path = Path(value)
    if path.is_absolute() or '..' in path.parts or not path.parts:
        raise ValueError('unsafe archive path')
    if path.parts[:2] != ('target', 'debug'):
        raise ValueError('archive member outside debug artifacts')
    return path


def write(name, value):
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / name).write_text(json.dumps(value, sort_keys=True, indent=2) + '\n')


def tokens():
    sysroot = subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip()
    return {'@ROOT@': str(ROOT), '@SYSROOT@': sysroot, '@HOME@': str(Path.home())}


def encode(value, paths):
    for token, path in sorted(paths.items(), key=lambda item: -len(item[1])):
        value = value.replace(path, token)
    return value


def decode(value, paths):
    for token, path in paths.items():
        value = value.replace(token, path)
    return value


def call(args, env=None, cwd=ROOT):
    started = time.monotonic()
    result = subprocess.run(args, cwd=cwd, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, errors='replace')
    text = encode(result.stdout, tokens())
    print(text, end='', flush=True)
    if result.returncode:
        raise RuntimeError(f'child command failed with exit {result.returncode}')
    return result.stdout, time.monotonic() - started


def list_names(binary, ignored=False, env=None, cwd=ROOT):
    args = [str(binary), '--list', '--format', 'terse']
    if ignored:
        args.append('--ignored')
    result = subprocess.run(args, cwd=cwd, env=env, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError('libtest inventory command failed')
    names = [line[:-6] for line in result.stdout.splitlines() if line.endswith(': test')]
    if len(names) != len(set(names)):
        raise ValueError('duplicate inventory name')
    return sorted(names)


def reconcile(names, ignored, output):
    found = re.findall(r'^test (.*?) \.\.\. (ok|FAILED|ignored)(?:,.*)?$', output, re.M)
    actual = {name: status for name, status in found}
    if len(actual) != len(found) or set(actual) != set(names):
        raise ValueError('test inventory omitted, duplicated or unexpected')
    if any(actual[name] != ('ignored' if name in ignored else 'ok') for name in names):
        raise ValueError('test outcome or ignored policy differs')
    return actual


def identity():
    def output(*args):
        return subprocess.check_output(args, cwd=ROOT, text=True).strip()
    return {'commit': output('git', 'rev-parse', 'HEAD'),
            'rust': output('rustc', '-Vv'), 'cargo': output('cargo', '-V'),
            'os': platform.system(), 'arch': platform.machine(),
            'image': os.environ.get('ImageVersion', 'unknown'),
            'layout': {key: hashlib.sha256(value.encode()).hexdigest()
                       for key, value in tokens().items()},
            'lock': digest(ROOT / 'Cargo.lock'), 'collector': digest(__file__),
            'profile': 'debug/full-default', 'features': 'workspace/all-features',
            'ffmpeg': output('ffmpeg', '-version').splitlines()[0],
            'mkvmerge': output('mkvmerge', '--version')}


def env_record(env, paths):
    allowed = {'CARGO', 'CARGO_MANIFEST_DIR', 'CARGO_MANIFEST_PATH',
               'CARGO_TARGET_TMPDIR', 'LD_LIBRARY_PATH', 'DYLD_FALLBACK_LIBRARY_PATH'}
    allowed.update('CARGO_PKG_' + key for key in (
        'VERSION', 'VERSION_MAJOR', 'VERSION_MINOR', 'VERSION_PATCH', 'VERSION_PRE',
        'AUTHORS', 'NAME', 'DESCRIPTION', 'HOMEPAGE', 'REPOSITORY', 'LICENSE',
        'LICENSE_FILE', 'RUST_VERSION', 'README'))
    selected = {key: encode(value, paths) for key, value in env.items()
                if key in allowed or key.startswith('CARGO_BIN_EXE_')}
    for key, value in selected.items():
        if key in {'CARGO', 'CARGO_MANIFEST_DIR', 'CARGO_MANIFEST_PATH',
                   'CARGO_TARGET_TMPDIR', 'LD_LIBRARY_PATH', 'DYLD_FALLBACK_LIBRARY_PATH'} or key.startswith('CARGO_BIN_EXE_'):
            system_paths = {'/usr/local/lib', '/usr/lib'} if key in (
                'LD_LIBRARY_PATH', 'DYLD_FALLBACK_LIBRARY_PATH') else set()
            if any(part and part not in system_paths and
                   not part.startswith(('@ROOT@', '@SYSROOT@', '@HOME@'))
                   for part in value.split(os.pathsep)):
                raise ValueError('unsupported runtime path')
            if any('..' in Path(part).parts for part in value.split(os.pathsep)):
                raise ValueError('runtime path traversal')
    selected.update(TMPDIR='@ROOT@/.test-tmp', VOOM_TEST_PREBUILT_WORKERS='1',
                    CARGO_TARGET_DIR='@ROOT@/target')
    return selected


def file_record(path):
    relative = str(path.relative_to(ROOT))
    checked_relative(relative)
    if path.is_symlink() or not path.is_file():
        raise ValueError('artifact must be a regular file')
    return {'path': relative, 'sha256': digest(path), 'mode': stat.S_IMODE(path.stat().st_mode)}


def verify_files(records):
    for item in records:
        path = ROOT / checked_relative(item['path'])
        if file_record(path) != item:
            raise ValueError('artifact identity or mode changed')


def cargo_artifacts(args):
    output, duration = call(['cargo', *args, '--locked', '--message-format=json'])
    records = []
    for line in output.splitlines():
        if line.startswith('{'):
            item = json.loads(line)
            if item.get('reason') == 'compiler-artifact' and item.get('executable'):
                records.append(item)
    return records, duration


def record(arm, executable, args):
    paths = tokens()
    binary = Path(executable).resolve()
    names = list_names(binary, env=os.environ, cwd=Path.cwd())
    ignored = list_names(binary, True, os.environ, Path.cwd())
    item = {'arm': arm, 'file': file_record(binary), 'cwd': encode(str(Path.cwd()), paths),
            'env': env_record(os.environ, paths), 'args': args,
            'names': names, 'ignored': ignored}
    key = hashlib.sha256((arm + str(binary)).encode()).hexdigest()
    path = OUT / ('run-' + key + '.json')
    if path.exists():
        raise ValueError('duplicate binary invocation')
    result, item['seconds'] = call([str(binary), *args, '--format', 'pretty', '--color', 'never'],
                                   env=os.environ, cwd=Path.cwd())
    item['outcomes'] = reconcile(names, ignored, result)
    write(path.name, item)


def tar_metadata(item):
    item.uid = item.gid = item.mtime = 0
    item.uname = item.gname = ''
    return item


def resources():
    disk = subprocess.check_output(['df', '-P', str(ROOT / '.test-tmp')], text=True).splitlines()[-1].split()
    if not disk[0].startswith('/dev/'):
        raise ValueError('temp root is not on a verified disk device')
    if platform.system() == 'Darwin':
        memory = int(subprocess.check_output(['sysctl', '-n', 'hw.memsize'], text=True))
    else:
        memory = int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines()
                          if line.startswith('MemTotal:'))) * 1024
    return {'cpus': os.cpu_count(), 'memory_bytes': memory, 'disk_backed': True,
            'disk_blocks': int(disk[1]), 'available_blocks': int(disk[3]),
            'cache': 'fresh target; no Rust cache action; hosted tools may be preinstalled'}


def producer():
    if OUT.exists():
        raise ValueError('producer output already exists; retry forbidden')
    OUT.mkdir(parents=True)
    provenance = identity()
    write('conditions.json', resources())
    build, build_seconds = cargo_artifacts(['build', '--workspace', '--all-features', '--all-targets'])
    workers = [file_record(Path(item['executable'])) for item in build if not item['profile']['test']]
    arms = {'default': ['-p', 'voom-control-plane'], 'all': ['--workspace', '--all-features']}
    expected, prep = {}, {}
    for arm, selection in arms.items():
        artifacts, prep[arm] = cargo_artifacts(['test', *selection, '--lib', '--bins', '--tests', '--no-run'])
        intended = [item for item in artifacts if item['profile']['test']]
        expected[arm] = {str(Path(item['executable']).relative_to(ROOT)):
                         file_record(Path(item['executable'])) for item in intended}
        if len(expected[arm]) != len(intended) or not intended:
            raise ValueError('empty or duplicated Cargo target inventory')
        write(arm + '-build.json', [dict(file=expected[arm][str(Path(item['executable']).relative_to(ROOT))],
              target=item['target']['name'], kind=item['target']['kind'], features=item['features'],
              profile=item['profile']) for item in intended])
        verify_files(workers)
    verify_files(workers)
    all_files = list(expected['all'].values())
    verify_files(all_files)
    # Freeze the immutable transfer candidates before default-feature execution.
    packing_started = time.monotonic()
    with tarfile.open(OUT / 'executables.tar', 'w') as archive:
        for item in {item['path']: item for item in workers + all_files}.values():
            archive.add(ROOT / item['path'], arcname=item['path'], recursive=False, filter=tar_metadata)
    packing_seconds = time.monotonic() - packing_started
    host = next(line.split(': ', 1)[1] for line in provenance['rust'].splitlines() if line.startswith('host: '))
    runner_key = 'CARGO_TARGET_' + host.upper().replace('-', '_') + '_RUNNER'
    timings = {'build': build_seconds, 'preparation': prep, 'packing': packing_seconds}
    for arm, selection in arms.items():
        env = dict(os.environ, VOOM_TEST_PREBUILT_WORKERS='1', CARGO_TARGET_DIR=str(ROOT / 'target'))
        env[runner_key] = f'python3 {ROOT}/scripts/ci-sharding-feasibility.py record {arm}'
        _, timings[arm] = call(['cargo', 'test', *selection, '--locked', '--lib', '--bins', '--tests'], env)
        verify_files(workers)
        verify_files(all_files)
        runs = [json.loads(path.read_text()) for path in OUT.glob('run-*.json')]
        actual = [run['file']['path'] for run in runs if run['arm'] == arm]
        if len(actual) != len(set(actual)) or set(actual) != set(expected[arm]):
            raise ValueError('Cargo binary inventory mismatch')
        _, timings[arm + '_doc'] = call(['cargo', 'test', *selection, '--locked', '--doc'], env)
        verify_files(workers)
        verify_files(all_files)
    runs = [json.loads(path.read_text()) for path in OUT.glob('run-*.json')]
    selected = [run for run in runs if run['arm'] == 'all']
    totals = [0.0, 0.0]
    for run in sorted(selected, key=lambda run: (-run['seconds'], run['file']['path'])):
        shard = min(range(2), key=lambda index: (totals[index], index))
        run['shard'] = shard
        totals[shard] += run['seconds']
    if len({run['file']['path'] for run in selected}) != len(selected):
        raise ValueError('duplicated partition target')
    # Native build-output library paths supplement executable files.
    runtime_files = {}
    for run in selected:
        for key in ('LD_LIBRARY_PATH', 'DYLD_FALLBACK_LIBRARY_PATH'):
            for value in run['env'].get(key, '').split(os.pathsep):
                if value.startswith('@ROOT@/target/debug/'):
                    directory = Path(decode(value, tokens()))
                    if directory.is_dir():
                        for path in directory.iterdir():
                            if path.is_file() and ('.so' in path.name or path.suffix == '.dylib'):
                                runtime_files[str(path)] = file_record(path)
    files = {item['path']: item for item in workers + all_files + list(runtime_files.values())}
    packing_started = time.monotonic()
    with tarfile.open(OUT / 'executables.tar', 'a') as archive:
        for item in runtime_files.values():
            if item['path'] not in {entry['path'] for entry in workers + all_files}:
                archive.add(ROOT / item['path'], arcname=item['path'], recursive=False, filter=tar_metadata)
    timings['packing'] += time.monotonic() - packing_started
    manifest = {'identity': provenance, 'workers': workers, 'files': list(files.values()),
                'runs': selected, 'timings': timings, 'default_runs': [r for r in runs if r['arm'] == 'default'],
                'builds': {arm: json.loads((OUT / (arm + '-build.json')).read_text()) for arm in arms},
                'archive_sha256': digest(OUT / 'executables.tar'),
                'archive_bytes': (OUT / 'executables.tar').stat().st_size, 'shard_seconds': totals}
    write('manifest.json', manifest)


def extract(manifest):
    if digest(OUT / 'executables.tar') != manifest['archive_sha256']:
        raise ValueError('archive digest mismatch')
    wanted = {item['path']: item for item in manifest['files']}
    if len(wanted) != len(manifest['files']):
        raise ValueError('duplicate manifest member')
    with tarfile.open(OUT / 'executables.tar') as archive:
        members = archive.getmembers()
        if len(members) != len(wanted) or {item.name for item in members} != set(wanted):
            raise ValueError('archive inventory mismatch')
        for item in members:
            checked_relative(item.name)
            if not item.isfile() or item.mode != wanted[item.name]['mode']:
                raise ValueError('unsafe archive member or mode mismatch')
        for item in members:
            path = ROOT / item.name
            if path.exists() or path.is_symlink():
                raise ValueError('refusing existing extraction destination')
            if any(parent.is_symlink() for parent in path.parents):
                raise ValueError('symlink extraction ancestor')
            path.parent.mkdir(parents=True, exist_ok=True)
            with archive.extractfile(item) as source, path.open('xb') as target:
                shutil.copyfileobj(source, target)
            path.chmod(item.mode)
    verify_files(manifest['files'])


def consumer(shard):
    manifest = json.loads((OUT / 'manifest.json').read_text())
    if identity() != manifest['identity']:
        raise ValueError('producer/consumer provenance mismatch')
    for run in manifest['runs']:
        if run['shard'] not in (0, 1) or not run['cwd'].startswith('@ROOT@/crates/'):
            raise ValueError('invalid shard or package cwd')
        if env_record(dict(run['env']), tokens()) != run['env']:
            raise ValueError('unexpected runtime environment field')
    if len({run['file']['path'] for run in manifest['runs']}) != len(manifest['runs']):
        raise ValueError('duplicated shard target')
    write(f'conditions-{shard}.json', resources())
    extract(manifest)
    paths, results = tokens(), []
    for run in manifest['runs']:
        if run['shard'] != shard:
            continue
        env = dict(os.environ, **{key: decode(value, paths) for key, value in run['env'].items()})
        binary = ROOT / checked_relative(run['file']['path'])
        cwd = Path(decode(run['cwd'], paths))
        if list_names(binary, env=env, cwd=cwd) != run['names'] or list_names(binary, True, env, cwd) != run['ignored']:
            raise ValueError('consumer inventory differs')
        output, seconds = call([str(binary), *run['args'], '--format', 'pretty', '--color', 'never'], env, cwd)
        outcomes = reconcile(run['names'], run['ignored'], output)
        verify_files(manifest['workers'])
        results.append({'path': run['file']['path'], 'seconds': seconds, 'outcomes': outcomes})
        write(f'results-{shard}.json', results)
    if {item['path'] for item in results} != {run['file']['path'] for run in manifest['runs'] if run['shard'] == shard}:
        raise ValueError('shard result inventory differs')


def main():
    if sys.argv[1:] == ['producer']:
        producer()
    elif sys.argv[1:2] == ['record']:
        record(sys.argv[2], sys.argv[3], sys.argv[4:])
    elif len(sys.argv) == 3 and sys.argv[1] == 'consumer' and sys.argv[2] in ('0', '1'):
        consumer(int(sys.argv[2]))
    else:
        raise SystemExit('usage: producer | record ARM BINARY [ARGS] | consumer 0|1')

if __name__ == '__main__':
    try:
        main()
    except (ValueError, RuntimeError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(encode(str(error), tokens()), file=sys.stderr)
        raise SystemExit(1)
