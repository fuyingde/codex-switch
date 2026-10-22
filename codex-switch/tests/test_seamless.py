"""Offline end-to-end tests. Only synthetic credentials in a temporary directory.
Run cargo build --offline --bin codex-switch --example fake_codex first.
"""
import base64
import json
import os
from pathlib import Path
import queue
import socket
import stat
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
EXE = Path(os.environ.get("CODEX_SWITCH_TEST_EXE", ROOT / "target/debug/codex-switch.exe"))
PROXY = Path(os.environ.get("CODEX_SWITCH_TEST_PROXY", EXE))
FAKE = ROOT / "target/debug/examples/fake_codex.exe"


def credential(account):
    claims = {"sub": "user-" + account, "email": account + "@example.com", "exp": 4102444800,
              "https://api.openai.com/auth": {"chatgpt_account_id": account,
                  "chatgpt_user_id": "user-" + account, "chatgpt_plan_type": "plus"}}
    encoded = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=")
    token = "e30." + encoded + ".sig"
    return {"auth_mode": "chatgpt", "tokens": {"account_id": account, "id_token": token,
        "access_token": token, "refresh_token": "synthetic-refresh"}, "last_refresh": "2026-10-20T00:00:00Z"}


class Fixture:
    def __init__(self, path, real=FAKE, config=None):
        self.path = Path(path)
        self.home = self.path / "active"
        self.data = self.path / "data"
        self.home.mkdir(parents=True)
        self.data.mkdir()
        accounts = ["a", "b", "reject", "bad-read"]
        for account in accounts:
            profile = self.data / "profiles" / account
            profile.mkdir(parents=True)
            (profile / "auth.json").write_text(json.dumps(credential(account)), encoding="utf-8")
        (self.home / "auth.json").write_text(json.dumps(credential("a")), encoding="utf-8")
        index = {"version": 1, "active": "a", "accounts": [
            {"id": a, "email": a + "@example.com", "added_at": 1} for a in accounts]}
        (self.data / "index.json").write_text(json.dumps(index), encoding="utf-8")
        if config:
            (self.home / "config.toml").write_text(config, encoding="utf-8")
        self.env = dict(os.environ, CODEX_HOME=str(self.home), CODEX_SWITCH_DATA_HOME=str(self.data),
                        CODEX_SWITCH_REAL_CLI=str(real),
                        CODEX_SWITCH_SETTINGS_FILE=str(self.path / "user-setting.json"))
        self.processes = []
        self.start()

    def start(self):
        proxy_args = [] if PROXY.stem == "codex-switch-bridge" else ["bridge-run"]
        p = subprocess.Popen([str(PROXY), *proxy_args, "app-server", "--stdio"], env=self.env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", bufsize=1)
        self.processes.append(p)
        messages, errors = queue.Queue(), []
        def read():
            for line in p.stdout:
                try:
                    v = json.loads(line)
                    assert not str(v.get("id", "")).startswith("codex-switch:"), "internal RPC leaked"
                    messages.put(v)
                except Exception as e:
                    errors.append(str(e))
        threading.Thread(target=read, daemon=True).start()
        def read_errors():
            for line in p.stderr: errors.append(line.rstrip())
        threading.Thread(target=read_errors, daemon=True).start()
        p.stdin.write(json.dumps({"id": "init", "method": "initialize", "params": {
            "clientInfo": {"name": "codex_switch_integration", "version": "1.0"}}}) + "\n")
        p.stdin.flush()
        try:
            first = messages.get(timeout=20)
        except queue.Empty:
            code = p.poll()
            p.stdin.close()
            try: p.wait(timeout=3)
            except subprocess.TimeoutExpired: p.kill(); p.wait()
            raise AssertionError(("initialization timed out", str(PROXY), code, errors))
        assert first.get("id") == "init" and "result" in first, (first, errors)
        # Actual Desktop omits `initialized`. A successful initialize response is
        # sufficient, otherwise the old bridge remained ready=false forever.
        self.p, self.messages, self.errors = p, messages, errors
        for _ in range(100):
            for f in (self.data / "bridge/sessions").glob("*.json"):
                session = json.loads(f.read_text())
                if session["proxy_pid"] == p.pid:
                    self.session = session
                    if self.control("status")["result"]["ready"]: return
            time.sleep(.02)
        raise AssertionError(("proxy not ready", errors))

    def request(self, method, params=None):
        rid = "desktop:" + str(time.time_ns())
        self.p.stdin.write(json.dumps({"id": rid, "method": method, "params": params or {}}) + "\n")
        self.p.stdin.flush()
        while True:
            v = self.messages.get(timeout=25)
            if v.get("id") == rid:
                assert "error" not in v, v
                return v.get("result")

    def control(self, method, account=None, capability=None):
        with socket.create_connection(("127.0.0.1", self.session["port"]), timeout=30) as s:
            s.sendall((json.dumps({"capability": capability or self.session["capability"],
                "method": method, "account": account}) + "\n").encode())
            raw = s.makefile("rb").readline()
            return json.loads(raw) if raw else None

    def cli(self, *args, ok=True):
        p = subprocess.run([str(EXE), *args], env=self.env, capture_output=True,
                           text=True, encoding="utf-8", timeout=40)
        assert (p.returncode == 0) == ok, (args, p.returncode, p.stdout, p.stderr)
        return p.stdout

    def active(self):
        disk = json.loads((self.home / "auth.json").read_text())["tokens"]["account_id"]
        index = json.loads((self.data / "index.json").read_text())["active"]
        assert disk == index, (disk, index)
        return disk

    def idle(self):
        for _ in range(250):
            status = self.control("status")["result"]
            if not status["activeTurns"] and not status["pending"]: return status
            time.sleep(.02)
        raise AssertionError("did not become idle")

    def turn(self, thread="thread-test", duration=10):
        self.request("turn/start", {"threadId": thread, "durationMs": duration})

    def close(self):
        for p in self.processes:
            if p.poll() is None:
                p.stdin.close()
                try: p.wait(timeout=5)
                except subprocess.TimeoutExpired: p.kill(); p.wait()


def run():
    with tempfile.TemporaryDirectory(prefix="codex-seamless-test-") as directory:
        f = Fixture(directory)
        try:
            passthrough = subprocess.run([str(PROXY), *([] if PROXY.stem == "codex-switch-bridge" else ["bridge-run"]), "--version"],
                env=f.env, capture_output=True, text=True, timeout=10)
            assert passthrough.returncode == 0 and "fake-codex" in passthrough.stdout
            backend = f.control("status")["result"]["backendPid"]
            f.turn(); f.idle()
            f.cli("switch", "b")
            assert f.active() == "b"
            f.turn(); f.idle()
            records = [json.loads(line) for line in (f.home / "requests.jsonl").read_text().splitlines()]
            assert [r["account"] for r in records] == ["a", "b"]
            assert {r["pid"] for r in records} == {backend}
            print("PASS: next turn changes account; same thread and same backend PID")
            f.turn(duration=1200)
            f.cli("switch", "a")
            assert f.active() == "b"
            assert f.control("status")["result"]["pending"] == "a"
            assert f.request("test/ping")["pong"]
            # A new chat starts while another chat is still on the previous account.
            f.turn(thread="thread-next")
            f.idle(); assert f.active() == "a"
            assert json.loads((f.home / "requests.jsonl").read_text().splitlines()[-1])["account"] == "a"
            print("PASS: current turn and unrelated RPCs preserved; queued switch applies at idle")
            f.turn(duration=700)
            assert f.control("switch", "b")["result"]["state"] == "queued"
            assert f.control("cancel")["ok"]
            f.idle(); assert f.active() == "a"
            print("PASS: cancelling queue leaves credentials and backend unchanged")
            for target in ["reject", "bad-read"]:
                assert not f.control("switch", target)["ok"]
                assert f.active() == "a"
                f.turn(); f.idle()
                assert json.loads((f.home / "requests.jsonl").read_text().splitlines()[-1])["account"] == "a"
            print("PASS: login rejection and backend verification failure roll back")
            index = f.data / "index.json"
            index.chmod(stat.S_IREAD)
            try:
                assert not f.control("switch", "b")["ok"]
                assert f.active() == "a"
            finally: index.chmod(stat.S_IREAD | stat.S_IWRITE)
            f.turn(); f.idle()
            assert json.loads((f.home / "requests.jsonl").read_text().splitlines()[-1])["account"] == "a"
            print("PASS: failed index commit restores both disk and live backend")
            assert f.control("switch", "b")["ok"]
            assert f.request("test/refresh")["refreshed"]
            active = json.loads((f.home / "auth.json").read_text())
            profile = json.loads((f.data / "profiles/b/auth.json").read_text())
            assert active["tokens"]["access_token"] == profile["tokens"]["access_token"]
            assert f.active() == "b"
            assert f.control("status", capability="invalid") is None
            print("PASS: refresh stays on selected account; unauthenticated control rejected")
            f.start()
            f.cli("switch", "a", ok=False)
            assert f.active() == "b"
            print("PASS: multiple backends fail closed")
        finally: f.close()
        assert not list((f.data / "bridge/sessions").glob("*.json")), "descriptors not cleaned"
        print("PASS: session descriptors cleaned on shutdown")


def run_native(real):
    """Use the actual installed app-server against a loopback-only synthetic inference server."""
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    seen = []
    class Endpoint(BaseHTTPRequestHandler):
        def log_message(self, *args): pass
        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))) or b"{}")
            if self.path != "/v1/responses":
                wire = b'{}'
                self.send_response(200); self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(wire))); self.end_headers(); self.wfile.write(wire)
                return
            auth = self.headers.get("Authorization", "")
            token = auth.removeprefix("Bearer ")
            claims = json.loads(base64.urlsafe_b64decode(token.split(".")[1] + "=="))
            seen.append((claims["https://api.openai.com/auth"]["chatgpt_account_id"], self.headers.get("ChatGPT-Account-ID"), self.path))
            item = {"type": "message", "id": "msg-fixture", "role": "assistant",
                    "content": [{"type": "output_text", "text": "fixture complete"}]}
            response = {"id": "resp-fixture-" + str(len(seen)), "object": "response", "status": "completed",
                        "output": [item], "usage": {"input_tokens": 4, "output_tokens": 3, "total_tokens": 7}}
            events = [{"type": "response.created", "response": dict(response, status="in_progress", output=[])},
                      {"type": "response.output_item.added", "output_index": 0, "item": item},
                      {"type": "response.output_item.done", "output_index": 0, "item": item},
                      {"type": "response.completed", "response": response}]
            wire = "".join("event: " + e["type"] + "\ndata: " + json.dumps(e) + "\n\n" for e in events).encode()
            self.send_response(200); self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(wire))); self.end_headers(); self.wfile.write(wire)
        def do_GET(self):
            wire = b'{"data":[]}'
            self.send_response(200); self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(wire))); self.end_headers(); self.wfile.write(wire)
    http = ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
    threading.Thread(target=http.serve_forever, daemon=True).start()
    config = '''model_provider = "fixture"
model = "fixture-model"
chatgpt_base_url = "http://127.0.0.1:%d"
approval_policy = "never"
sandbox_mode = "read-only"
[analytics]
enabled = false
[features]
apps = false
[model_providers.fixture]
name = "Loopback fixture"
base_url = "http://127.0.0.1:%d/v1"
wire_api = "responses"
requires_openai_auth = true
supports_websockets = false
''' % (http.server_port, http.server_port)
    try:
        with tempfile.TemporaryDirectory(prefix="codex-native-seamless-test-") as directory:
            f = Fixture(directory, real, config)
            try:
                pid = f.control("status")["result"]["backendPid"]
                thread = f.request("thread/start", {"cwd": str(f.home), "model": "fixture-model",
                                                   "approvalPolicy": "never", "sandbox": "read-only"})["thread"]["id"]
                for account in ["a", "b", "a"]:
                    if f.active() != account:
                        switch_result = f.control("switch", account)
                        if not switch_result["ok"]:
                            auth = f.request("getAuthStatus", {"includeToken": True, "refreshToken": False})
                            raise AssertionError((switch_result, {k: (bool(v) if k == "authToken" else v) for k, v in auth.items()}, f.errors[-10:]))
                    f.request("turn/start", {"threadId": thread,
                        "input": [{"type": "text", "text": "say fixture complete"}]})
                    status = f.idle()
                    assert status["backendPid"] == pid
                    assert seen[-1][0] == account, seen
                assert [row[0] for row in seen] == ["a", "b", "a"], seen
                print("PASS: installed native app-server sends A -> B -> A inference Authorization on same thread and PID")
                print("Native endpoints:", [(a, path) for a, _, path in seen])
            finally:
                f.close()
    finally:
        http.shutdown(); http.server_close()


if __name__ == "__main__":
    import sys
    if len(sys.argv) == 3 and sys.argv[1] == "--real-cli": run_native(Path(sys.argv[2]))
    else: run()
