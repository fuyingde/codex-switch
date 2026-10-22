"""Fully isolated automatic setup/restart tests; never touches real Desktop or user settings."""
import json
import os
from pathlib import Path
import subprocess
import socket
import threading
import tempfile
import time

from test_seamless import EXE, ROOT, FAKE, credential

DESKTOP = ROOT / "target/debug/examples/fake_desktop.exe"


def run():
    with tempfile.TemporaryDirectory(prefix="codex-connect-test-") as directory:
        base = Path(directory)
        home, data = base / "active", base / "data"
        home.mkdir(); data.mkdir()
        (home / "auth.json").write_text(json.dumps(credential("a")))
        for account in ["a", "b", "reject"]:
            profile = data / "profiles" / account
            profile.mkdir(parents=True)
            (profile / "auth.json").write_text(json.dumps(credential(account)))
        (data / "index.json").write_text(json.dumps({"version":1,"active":"a","accounts":[
            {"id":a,"email":a+"@example.com","added_at":1} for a in ["a","b","reject"]]}))
        settings = base / "settings.json"
        env = dict(os.environ, CODEX_HOME=str(home), CODEX_SWITCH_DATA_HOME=str(data),
            CODEX_SWITCH_REAL_CLI=str(FAKE), CODEX_SWITCH_DESKTOP_EXE=str(DESKTOP),
            CODEX_SWITCH_SETTINGS_FILE=str(settings))
        env.pop("CODEX_CLI_PATH", None)
        parents = []

        def cli(*args, ok=True):
            p = subprocess.run([str(EXE), *args], env=env, capture_output=True, text=True,
                encoding="utf-8", timeout=55)
            assert (p.returncode == 0) == ok, (args, p.stdout, p.stderr)
            return p.stdout

        def launches():
            p = home / "desktop-launches.jsonl"
            return [json.loads(s) for s in p.read_text(encoding="utf-8").splitlines()] if p.exists() else []

        def status():
            return json.loads(cli("seamless-status"))

        def cleanup():
            for entry in launches():
                subprocess.run(["taskkill", "/PID", str(entry["pid"]), "/T", "/F"], capture_output=True)
            for p in parents:
                try: p.wait(timeout=5)
                except subprocess.TimeoutExpired: p.kill(); p.wait()
            # taskkill returns before Windows finishes unloading descendant images.
            time.sleep(.4)

        try:
            # First-run automatic setup also works when Desktop was not open.
            cli("connect-seamless")
            assert status()["installed"] and status()["sessions"][0]["ready"]
            assert launches()[0]["account"] == "a"
            first = status()["sessions"][0]
            cli("switch", "b")
            assert status()["sessions"][0]["backendPid"] == first["backendPid"]
            assert len(launches()) == 1
            print("PASS: first run installs/launches automatically; subsequent switch preserves Desktop/backend")
            # Authentication failure must never trigger a restart fallback.
            cli("switch", "reject", ok=False)
            assert len(launches()) == 1
            assert json.loads((home / "auth.json").read_text())["tokens"]["account_id"] == "b"
            cli("switch", "a", "--restart", ok=False)
            assert len(launches()) == 1
            print("PASS: authentication failure and removed --restart never restart or change credentials")

            # Simulate an existing Desktop launched by an environment with no override.
            cleanup()
            time.sleep(.3)
            parents.append(subprocess.Popen([str(DESKTOP)], env=env))
            for _ in range(100):
                if len(launches()) >= 2: break
                time.sleep(.05)
            cli("switch", "a")
            assert len(launches()) == 3
            assert launches()[-1]["account"] == "b", "reconnect must open OLD account before switching"
            assert status()["sessions"][0]["active"] == "a"
            cli("connect-seamless")
            assert len(launches()) == 3, "healthy setup must be idempotent"
            print("PASS: disconnected Desktop restarts once on OLD account, then seamless switch; repeated setup does not restart")

            if old := os.environ.get("CODEX_SWITCH_TEST_OLD_PROXY"):
                cleanup()
                before = len(launches())
                old_env = dict(env, CODEX_CLI_PATH=old, CODEX_SWITCH_FIXTURE_TURN_MS="1600")
                parents.append(subprocess.Popen([str(DESKTOP)], env=old_env))
                for _ in range(100):
                    current = status()["sessions"]
                    if current and current[0]["activeTurns"]:
                        assert not current[0]["ready"], "fixture must reproduce old Desktop initialization bug"
                        break
                    time.sleep(.025)
                else:
                    raise AssertionError("old bridge fixture did not start a turn")
                started = time.monotonic()
                cli("connect-seamless")
                assert time.monotonic() - started >= .5, "upgrade interrupted an active turn"
                assert len(launches()) == before + 2
                assert status()["sessions"][0]["ready"]
                assert launches()[-1]["account"] == "a"
                print("PASS: old bridge with ready=false upgrades while running, waits for current turn, reconnects automatically")

            # Disabling remains a maintenance CLI operation, not a second switching mode.
            cli("disable-seamless")
            assert json.loads(settings.read_text()) is None
            count = len(launches())
            cli("connect-seamless")
            assert len(launches()) == count
            print("PASS: saved launch setting restores and re-enables without another restart")

            # A cosmetic rebuild must not restart a healthy backend from a different build.
            cleanup()
            older = data / "bridge" / "compatible-build" / "codex-switch-bridge.exe"
            older.parent.mkdir(parents=True, exist_ok=True)
            older.write_bytes(EXE.read_bytes() + b"\ncompatible-ui-build-fixture\n")
            before = len(launches())
            parents.append(subprocess.Popen([str(DESKTOP)], env=dict(env, CODEX_CLI_PATH=str(older))))
            for _ in range(100):
                current = status()["sessions"]
                if current and current[0]["ready"]: break
                time.sleep(.05)
            else: raise AssertionError("compatible different-build fixture not ready")
            backend = current[0]["backendPid"]
            cli("connect-seamless")
            cli("connect-seamless")
            assert len(launches()) == before + 1, "UI rebuild/reopen restarted healthy Codex"
            assert status()["sessions"][0]["backendPid"] == backend
            print("PASS: reopening/updating UI reuses a compatible different-build backend without restart")

            # A status timeout is not permission to close a live Desktop.
            current=status()["sessions"][0]
            descriptor=data / "bridge" / "sessions" / (str(current["proxyPid"])+".json")
            original=descriptor.read_text(encoding="utf-8")
            listener=socket.socket();listener.bind(("127.0.0.1",0));listener.listen();listener.settimeout(.2)
            stop=threading.Event()
            def hold(client):
                with client:
                    client.settimeout(1)
                    try: client.recv(65536)
                    except OSError: pass
                    stop.wait(3)
            def accept():
                while not stop.is_set():
                    try: client,_=listener.accept()
                    except socket.timeout: continue
                    except OSError: break
                    threading.Thread(target=hold,args=(client,),daemon=True).start()
            threading.Thread(target=accept,daemon=True).start()
            altered=json.loads(original);altered["port"]=listener.getsockname()[1]
            descriptor.write_text(json.dumps(altered),encoding="utf-8")
            count=len(launches())
            try:
                cli("connect-seamless",ok=False)
                assert len(launches())==count,"status timeout restarted Desktop"
            finally:
                descriptor.write_text(original,encoding="utf-8");stop.set();listener.close()
            assert status()["sessions"][0]["backendPid"]==backend
            print("PASS: unresponsive status preserves the live Desktop and backend")
        finally:
            cleanup()


if __name__ == "__main__":
    run()
