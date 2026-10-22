"""Run actual helper/parent-exit/replace/relaunch only with a no-UI fixture in temp paths.
Build: cargo build --offline --example update_harness
Never launches the actual switcher or Codex and never requests the update domain.
"""
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import time
ROOT=Path(__file__).resolve().parents[1]
HARNESS=ROOT/'target/debug/examples/update_harness.exe'

def run():
    with tempfile.TemporaryDirectory(prefix='codex-update-fixture-') as directory:
        base=Path(directory)
        target=base/'software with spaces.exe'
        staged=base/'.codex-switch-update-fixture.exe'
        target.write_bytes(b'synthetic old executable, not runnable')
        shutil.copyfile(HARNESS,staged)
        # Helpers use a unique nested TEMP owned by this test, not a live installation.
        helpers=base/'temp'
        helpers.mkdir()
        env=dict(os.environ,TEMP=str(helpers),TMP=str(helpers))
        p=subprocess.run([str(HARNESS),'fixture',str(target),str(staged)],env=env,capture_output=True,text=True,encoding='utf-8',timeout=10)
        assert p.returncode==0,(p.stdout,p.stderr)
        marker=target.with_suffix('.launched.json')
        deadline=time.monotonic()+10
        while not marker.exists() and time.monotonic()<deadline: time.sleep(.05)
        assert marker.exists(),'new fixture did not relaunch'
        details=json.loads(marker.read_text())
        assert Path(details['target'])==target
        assert hashlib.sha256(target.read_bytes()).digest()==hashlib.sha256(HARNESS.read_bytes()).digest()
        assert not staged.exists()
        while list(helpers.iterdir()) and time.monotonic()<deadline: time.sleep(.05)
        assert not list(helpers.iterdir()),'helper temp files did not clean up'
        assert not list(base.glob('*.backup'))
        print('PASS: real helper handshake waits for fixture parent exit; replaces original path, relaunches no-UI fixture, cleans backup/temp; no real app or network')

if __name__=='__main__': run()
