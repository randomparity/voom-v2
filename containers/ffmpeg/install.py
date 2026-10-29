#!/usr/bin/env python3
"""Install private, checkout-independent tools for typed worker dependency paths."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys

import runtime

RUNTIME_PATH = Path(__file__).with_name("runtime.py")
OPTIONAL_CONTEXT = ("DOCKER_CONFIG", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR")


def absolute(path):
    path = Path(path)
    if not path.is_absolute() or any(character in str(path) for character in "\n\r\x00"):
        raise runtime.SetupError("installation paths must be absolute without newlines or NUL")
    if path.resolve() != path:
        raise runtime.SetupError("installation paths must be canonical, without symlinks")
    return path


def capture_context(engine):
    for name, value in os.environ.items():
        if value and name not in OPTIONAL_CONTEXT and (
                name.startswith(("DOCKER_", "CONTAINER_", "CONTAINERS_", "PODMAN_")) or
                name in ("STORAGE_DRIVER", "STORAGE_OPTS")):
            raise runtime.SetupError(f"unsupported engine override {name}; unset it or use "
                                     "the supported HOME/Docker/XDG configuration files")
    home = absolute(os.environ.get("HOME", ""))
    if not home.is_dir():
        raise runtime.SetupError("set HOME to the worker account's existing home directory")
    executable = shutil.which(engine)
    if not executable:
        raise runtime.SetupError(f"install {engine} and put its executable on the installer's PATH")
    # Retain the selected executable's entry name, including normal package-manager symlinks.
    engine_directory = Path(executable).absolute().parent.resolve()
    context = {"HOME": str(home), "PATH": str(engine_directory) + os.pathsep + os.defpath}
    for name in OPTIONAL_CONTEXT:
        if os.environ.get(name):
            directory = absolute(os.environ[name])
            if not directory.is_dir():
                raise runtime.SetupError(f"{name} must name an existing configuration directory")
            context[name] = str(directory)
    return context


def validate_separation(destination, config):
    for root in config.read_only_roots + config.read_write_roots:
        if runtime.within(str(destination), root) or runtime.within(root, str(destination)):
            raise runtime.SetupError("installation must be disjoint from all media grants; "
                                     "choose a private directory outside source/staging roots")


def write_private(path, data, mode):
    with path.open("xb") as target:
        os.fchmod(target.fileno(), mode)
        target.write(data)


def validate_install(destination, context, witnesses, tools):
    env = {**context, "VOOM_MEDIA_CONTAINER_CONFIG": str(destination / "config.json")}
    args = [sys.executable, "-I", str(destination / "runtime.py"), "check-config"]
    for witness in witnesses:
        args += ["--source-witness", str(witness)]
    calls = [(args, env)] + [([tool, "-version"],
                             {"VOOM_MEDIA_CALLER_PID": str(os.getpid())})
                            for tool in tools.values()]
    for command, environment in calls:
        result = subprocess.run(command, env=environment, capture_output=True, timeout=60,
                                check=False)
        if result.returncode:
            raise runtime.SetupError("installed tool setup failed; verify engine, account, "
                                     "configuration and shares: " +
                                     result.stderr.decode(errors="replace"))


def install(config_path, destination, witnesses):
    source = absolute(config_path)
    destination = absolute(destination)
    parent = destination.parent
    if not source.is_file():
        raise runtime.SetupError("configuration must be an existing regular file")
    if not parent.is_dir() or parent.stat().st_uid != os.getuid() or \
            parent.stat().st_mode & 0o022:
        raise runtime.SetupError("choose an existing account-owned private parent without "
                                 "group/other write permission")
    try:
        destination.mkdir(mode=0o700)
    except FileExistsError as error:
        raise runtime.SetupError("installation already exists; choose a new destination") from error
    try:
        destination.chmod(0o700)
        write_private(destination / "config.json", source.read_bytes(), 0o600)
        config = runtime.load_config(str(destination / "config.json"))
        validate_separation(destination, config)
        context = capture_context(config.engine)
        write_private(destination / "runtime.py", RUNTIME_PATH.read_bytes(), 0o600)
        python = str(Path(sys.executable).resolve())
        exports = {**context, "VOOM_MEDIA_CONTAINER_CONFIG": str(destination / "config.json")}
        prefix = "#!/bin/sh\n" + "".join(
            f"export {name}={shlex.quote(value)}\n" for name, value in exports.items())
        prefix += "".join(f"unset {name}\n" for name in OPTIONAL_CONTEXT if name not in context)
        tools = {}
        for tool in ("ffmpeg", "ffprobe"):
            path = destination / tool
            command = [python, "-I", str(destination / "runtime.py"), tool]
            text = prefix + "exec " + shlex.join(command) + ' "$@"\n'
            write_private(path, text.encode(), 0o700)
            tools[tool + "_bin"] = str(path)
        validate_install(destination, context, witnesses, tools)
        return tools
    except BaseException:
        shutil.rmtree(destination)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--source-witness", action="append", default=[])
    args = parser.parse_args()
    try:
        print(json.dumps(install(args.config, args.destination, args.source_witness)))
        return 0
    except (runtime.SetupError, OSError, subprocess.TimeoutExpired) as error:
        print(f"voom container install: {error}", file=sys.stderr)
        return 125


if __name__ == "__main__":
    sys.exit(main())
