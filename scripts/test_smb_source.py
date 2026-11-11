"""Run the independent Rust consumer against the shared authenticated fixture."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

cloud = Path(sys.argv[1]).resolve()
expected = 'ffeae11d53054190cf6f3a6fbcec2eb44b32dfb2'
actual = subprocess.check_output(['git', '-C', str(cloud), 'rev-parse', 'HEAD'], text=True).strip()
if actual != expected:
    raise SystemExit('Use the pinned CrispCloud commit: ' + expected)
root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='sorter-smb-') as directory:
    size = 3 * 1024 * 1024 + 17
    pattern = bytes(range(251))
    (Path(directory) / 'fixture.bin').write_bytes((pattern * (size // 251 + 1))[:size])
    fixture = subprocess.Popen([sys.executable, str(cloud / 'packages/crisp_storage/tool/smb_fixture.py'), directory], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    try:
        ready = json.loads(fixture.stdout.readline())
        env = dict(os.environ, CRISP_SMB_TEST_ADDRESS=f"127.0.0.1:{ready['port']}")
        subprocess.run(['cargo', 'test', '--locked', '--manifest-path', str(root / 'crates/crisp-smb-source/Cargo.toml'), '--', '--ignored'], env=env, check=True, timeout=300)
    finally:
        fixture.terminate()
        try: fixture.wait(timeout=10)
        except subprocess.TimeoutExpired: fixture.kill(); fixture.wait()
