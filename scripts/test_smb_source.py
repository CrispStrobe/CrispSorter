"""Run the independent Rust consumer against the shared authenticated fixture."""
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading

EXPECTED_REVISION = 'ffeae11d53054190cf6f3a6fbcec2eb44b32dfb2'


def read_fixture_ready(fixture, timeout=30):
    """A crashed or silent fixture must fail instead of hanging the CI job."""
    lines = queue.Queue(maxsize=1)

    def read_line():
        try:
            lines.put(fixture.stdout.readline())
        except Exception:
            lines.put('')

    threading.Thread(target=read_line, daemon=True).start()
    try:
        line = lines.get(timeout=timeout)
    except queue.Empty as error:
        raise RuntimeError('SMB fixture readiness timed out') from error
    try:
        ready = json.loads(line)
    except (TypeError, ValueError) as error:
        raise RuntimeError('SMB fixture exited or returned invalid readiness data') from error
    port = ready.get('port') if isinstance(ready, dict) else None
    if type(port) is not int or not 1 <= port <= 65535:
        raise RuntimeError('SMB fixture returned an invalid loopback port')
    return port


def stop_fixture(fixture):
    if fixture.poll() is None:
        fixture.terminate()
    try:
        fixture.wait(timeout=10)
    except subprocess.TimeoutExpired:
        fixture.kill()
        fixture.wait(timeout=10)
    if fixture.stdout is not None:
        fixture.stdout.close()


def main(cloud_path):
    cloud = Path(cloud_path).resolve()
    actual = subprocess.check_output(['git', '-C', str(cloud), 'rev-parse', 'HEAD'], text=True).strip()
    if actual != EXPECTED_REVISION:
        raise SystemExit('Use the pinned CrispCloud commit: ' + EXPECTED_REVISION)
    root = Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix='sorter-smb-') as directory:
        size = 3 * 1024 * 1024 + 17
        pattern = bytes(range(251))
        (Path(directory) / 'fixture.bin').write_bytes((pattern * (size // 251 + 1))[:size])
        fixture = subprocess.Popen([sys.executable, str(cloud / 'packages/crisp_storage/tool/smb_fixture.py'), directory], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        try:
            port = read_fixture_ready(fixture)
            env = dict(os.environ, CRISP_SMB_TEST_ADDRESS=f'127.0.0.1:{port}')
            subprocess.run(['cargo', 'test', '--locked', '--manifest-path', str(root / 'crates/crisp-smb-source/Cargo.toml'), '--', '--ignored'], env=env, check=True, timeout=300)
        finally:
            stop_fixture(fixture)


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('Usage: test_smb_source.py /path/to/pinned/CrispCloud')
    main(sys.argv[1])
