"""Regression for lost system hooks; only a disposable root and DB are used.

Usage: python3 tests/tui_hooks.py target/debug/paru-tui
No sudo is needed. Intentionally unexecutable pre-hooks must stop commits;
a failing post-hook must report failure even after the fixture DB changes.
"""

import json
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile


BINARY = Path(sys.argv[1]).resolve()
REPO = Path(__file__).resolve().parent.parent
SYSTEM_HOOK = "10-system-guard.hook"
CUSTOM_HOOK = "20-custom-guard.hook"
CLI_HOOK = "30-cli-guard.hook"
POST_HOOK = "40-post-failure.hook"
GUARD = """[Trigger]
Operation = Remove
Type = Package
Target = polybar
[Action]
When = PreTransaction
Exec = /paru-test-intentionally-missing-executable
AbortOnFail
"""

with tempfile.TemporaryDirectory(prefix="paru-hooks-") as temporary:
    root = Path(temporary)
    db = root / "db"
    shutil.copytree(REPO / "testdata/db", db)
    for package in (db / "local").iterdir():
        if package.is_dir() and not (package / "files").exists():
            (package / "files").write_text("%FILES%\n\n")
    package = db / "local/polybar-1.0.0-1"
    original = {p.name: p.read_bytes() for p in package.iterdir() if p.is_file()}
    system = root / "usr/share/libalpm/hooks"
    custom = root / "custom-hooks"
    cli = root / "cli-hooks"
    for directory in (system, custom, cli):
        directory.mkdir(parents=True)
    (system / SYSTEM_HOOK).write_text(GUARD)
    configuration = f"""[options]
RootDir = {root}
DBPath = {db}
LogFile = {root}/pacman.log
HookDir = {custom}
Architecture = x86_64
"""
    endpoint = str(root / "frontend.sock")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(endpoint)
        listener.listen(1)
        listener.settimeout(20)
        for expected in (SYSTEM_HOOK, CUSTOM_HOOK, CLI_HOOK, POST_HOOK):
            options = [["noscriptlet", None]]
            if expected == CUSTOM_HOOK:
                (custom / SYSTEM_HOOK).symlink_to("/dev/null")
                (custom / CUSTOM_HOOK).write_text(GUARD)
            if expected == CLI_HOOK:
                (cli / CUSTOM_HOOK).symlink_to("/dev/null")
                (cli / CLI_HOOK).write_text(GUARD)
            if expected == POST_HOOK:
                (cli / CLI_HOOK).unlink()
                (cli / POST_HOOK).write_text(
                    GUARD.replace("PreTransaction", "PostTransaction").replace("AbortOnFail\n", "")
                )
            if expected in (CLI_HOOK, POST_HOOK):
                options.append(["hookdir", str(cli)])
            log = root / "pacman.log"
            log.write_text("")
            request = root / "request.json"
            request.write_text(json.dumps(dict(
                configuration=configuration, operation="remove", options=options,
                targets=["polybar"], assume_installed=[],
            )))
            worker = subprocess.Popen(
                [str(BINARY), "--alpm-worker", endpoint, str(request)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            try:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(20)
                    with connection.makefile("r") as reader:
                        plans = 0
                        notices = []
                        for line in reader:
                            message = json.loads(line)
                            if message.get("notice"):
                                notices.append(message["text"])
                                continue
                            assert [p["name"] for p in message["plan"]] == ["polybar"]
                            plans += 1
                            connection.sendall(b'{"value":"yes","cancel":false}\n')
                out, err = worker.communicate(timeout=20)
                assert plans == 1, (out, err)
                assert worker.returncode != 0, "Missing/failed hook was reported as success"
                entries = log.read_text()
                assert f"running '{expected}'" in entries, (entries, out, err)
                for other in (SYSTEM_HOOK, CUSTOM_HOOK, CLI_HOOK, POST_HOOK):
                    if other != expected:
                        assert f"running '{other}'" not in entries, entries
                if expected == POST_HOOK:
                    assert not package.exists(), "Fixture removal did not commit"
                    assert any("Post-transaction hooks failed; packages have already been changed:" in n for n in notices), (notices, err)
                    assert "Transaction completed" not in notices
                else:
                    assert package.exists(), "Pre-transaction hook failed to protect package"
                    assert original == {p.name: p.read_bytes() for p in package.iterdir() if p.is_file()}
                assert not (db / "db.lck").exists()
            finally:
                if worker.poll() is None:
                    worker.kill()
                    worker.wait()

print("PASS: system hooks discovered, custom/CLI overrides, AbortOnFail, post-hook failure propagation")
