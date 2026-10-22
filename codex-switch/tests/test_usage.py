"""Quota regression: actual reader/pool/RPC with fake processes only. No real credentials/network.
Build first: cargo build --offline --examples
Run: python -B tests/test_usage.py
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
ROOT = Path(__file__).resolve().parents[1]
FAKE = ROOT / "target/debug/examples/fake_usage.exe"
HARNESS = ROOT / "target/debug/examples/usage_transport.exe"

def run():
    with tempfile.TemporaryDirectory(prefix="codex-usage-test-") as temporary:
        base = Path(temporary)
        data = base / "data"
        data.mkdir()
        homes = []
        scenarios = ["slow-init"] + ["healthy"] * 15 + ["transient", "legacy", "auth", "stale-catalog"]
        for i, scenario in enumerate(scenarios):
            home = data / "profiles" / f"profile-{i}"
            home.mkdir(parents=True)
            (home / "fixture.json").write_text(json.dumps({"scenario":scenario,"used":i}))
            # Harmless model metadata; fixture never reads credentials or network.
            (home / "models_cache.json").write_text(json.dumps({"models":[{"slug":"synthetic"}]}))
            homes.append(home)
        for limit in [1, 5, 16]:
            before = time.monotonic()
            p = subprocess.run([str(HARNESS), str(FAKE), str(limit), *map(str,homes)],
                capture_output=True, text=True, encoding="utf-8", timeout=30)
            assert p.returncode == 0, (p.stdout,p.stderr)
            output = list(map(json.loads,p.stdout.splitlines()))
            if limit > 1:
                assert output[0]["id"] != "0", "slow initialize blocked unrelated profiles"
            results = {int(v["id"]):v for v in output}
            assert len(results) == len(homes), p.stdout
            for i, result in results.items():
                if scenarios[i] == "auth":
                    assert "401 Unauthorized" in result["error"], result
                else:
                    assert result == {"id":str(i),"five":100-i,"weekly":70,"resets":2}, result
            events=[]
            for i,home in enumerate(homes):
                calls=[]
                logs=list(home.glob("transport-*.jsonl"))
                for log in logs:
                    records=[json.loads(s) for s in log.read_text().splitlines()]
                    calls += [(log,r) for r in records if r['event']=='start' and r['method']=='account/rateLimits/read']
                    events += [(r['time'],1 if r['event']=='start' else -1) for r in records if r['method']=='account/rateLimits/read']
                    init=[r for r in records if r['method']=='initialize' and r['event']=='start']
                    assert len(init)==1, records
                    assert init[0]['catalog']==(scenarios[i]!='stale-catalog')
                expected=2 if scenarios[i] in ('transient','legacy') else 1
                assert len(calls)==expected,(i,calls)
                if expected==2:
                    assert calls[0][0]==calls[1][0], 'retry should reuse the initialized child'
                for log in logs: log.unlink()
            active=peak=0
            for _,delta in sorted(events):
                active+=delta; peak=max(peak,active)
            assert active==0 and peak<=limit, (limit,peak)
            if limit>1: assert peak>1, 'quota requests were accidentally serialized'
            print(f'PASS: {limit} slots, {len(homes)} actual child/RPC reads, peak {peak}; {time.monotonic()-before:.2f}s')
        executable = os.environ.get("CODEX_SWITCH_TEST_EXE")
        if executable:
            (data / "index.json").write_text(json.dumps({"version":1,"accounts":[
                {"id":f"profile-{i}","email":f"synthetic-{i}@example.com","added_at":1}
                for i in range(len(homes))]}))
            active = base / "active"
            active.mkdir()
            env = dict(os.environ, CODEX_SWITCH_DATA_HOME=str(data), CODEX_HOME=str(active),
                       CODEX_CLI_PATH=str(FAKE))
            p = subprocess.run([executable,"usage"],env=env,capture_output=True,
                               text=True,encoding="utf-8",timeout=30)
            assert p.returncode==0,(p.stdout,p.stderr)
            assert p.stdout.count("额度读取失败")==1,p.stdout
            assert "401 Unauthorized" in p.stdout,p.stdout
            assert p.stdout.count("可重置次数: 2")==len(homes)-1,p.stdout
            print('PASS: packaged EXE reads the same synthetic profiles through fake RPC only')
        print('PASS: no reset detail lookup; cached catalog; one transient retry; legacy and stale catalog fallback; no auth retry')

if __name__ == '__main__': run()
