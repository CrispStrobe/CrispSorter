#!/usr/bin/env python3
"""Run one CPU Paddle fixture with bounded, sanitized test diagnostics."""
import argparse
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import sys
import time

from paddle_live_diagnostics import sanitize_output

TEST = 'extractors::ocr_paddle::tests::live_paddle_recognises_the_fixture_sentence'
FEATURES = 'desktop,sidecars,audio-glint,aitoolkit,cloud-backup,pdf-zpdf,drive-filen-native,drive-internxt-native,crispembed,ocr-reviewed-pdf,crispembed-license-api,ocr-photo-tools,crispasr,translate-align,translate-nmt,pdf-render,anydoc,fuse,paddle-ocr'
COMMAND = ['cargo', 'test', '-p', 'crispsorter', '--lib', '--features', FEATURES,
           '--', '--ignored', '--exact', TEST, '--nocapture', '--test-threads=1']
MAX_OUTPUT = 8 * 1024**2
MAX_SECONDS = 90 * 60


class TestFailure(Exception):
    """A real Cargo test failure, without arbitrary subprocess error text."""
    def __init__(self, exit_code):
        self.exit_code = exit_code
        super().__init__('Cargo test failed')


class CaptureFailure(Exception):
    def __init__(self, code, output):
        self.code, self.output = code, output
        super().__init__('Scoped capture stopped')


def capture(project, env, command=None):
    process, selector, failure, code = None, None, None, None
    output = bytearray()
    try:
        process = subprocess.Popen(COMMAND if command is None else command, cwd=project, env=env, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        start = heartbeat = time.monotonic()
        while selector.get_map():
            now = time.monotonic()
            if now - heartbeat >= 30:
                print(f'Scoped Paddle: {int(now-start)} seconds, {len(output)} diagnostic bytes captured', flush=True)
                heartbeat = now
            if now - start > MAX_SECONDS:
                raise CaptureFailure('timeout', b'')
            for key, _ in selector.select(timeout=1):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                if len(output) + len(chunk) > MAX_OUTPUT:
                    output.extend(chunk[:MAX_OUTPUT-len(output)])
                    raise CaptureFailure('output-limit', b'')
                output.extend(chunk)
        code = process.wait(timeout=10)
    except Exception as error:
        failure = error.code if isinstance(error, CaptureFailure) else 'capture-error'
    finally:
        if process is not None:
            try:
                if failure is not None or process.poll() is None:
                    # A descendant may hold the pipe after its parent exits.
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    process.wait(timeout=10)
            except Exception:
                failure = failure or 'capture-error'
            try:
                process.stdout.close()
            except Exception:
                failure = failure or 'capture-error'
        if selector is not None:
            try:
                selector.close()
            except Exception:
                failure = failure or 'capture-error'
    if failure is not None:
        raise CaptureFailure(failure, bytes(output))
    return code, bytes(output)


def prepare_output(output):
    """Reuse only known diagnostic filenames; output problems never gate tests."""
    try:
        if output.is_symlink() or output.resolve() != output.absolute():
            return
        output.mkdir(parents=True, exist_ok=True)
        for name in ['receipt.json', 'diagnostics.txt', 'native-smoke.json']:
            path = output / name
            if path.is_file() or path.is_symlink():
                path.unlink()
    except OSError:
        pass


def write_diagnostic(path, text):
    try:
        if path.is_symlink() or path.parent.resolve() != path.parent.absolute():
            return False
        path.write_text(text)
        return True
    except OSError:
        return False


def retain_diagnostics(output, report, raw, project):
    roots = [project, project.parent, os.environ.get('RUNNER_TEMP'), os.environ.get('HOME')]
    text, status = sanitize_output(raw.decode('utf-8', errors='replace'), roots)
    retained = write_diagnostic(output / 'diagnostics.txt', text)
    report['diagnostics_status'] = status if retained else 'not-written'


def run(project, output):
    project, output = project.resolve(), output.absolute()
    prepare_output(output)
    report = {'schema': 'crispsorter-scoped-paddle-live-v1', 'status': 'failed', 'test': TEST,
              'features': FEATURES, 'default_features': True, 'cargo_locked': False,
              'limits': {'seconds': MAX_SECONDS, 'output_bytes': MAX_OUTPUT, 'build_jobs': 2, 'test_threads': 1},
              'scope': 'One CPU Paddle fixture, cache reuse and table session; not a full suite, release, GUI/device/corpus qualification'}
    try:
        env = os.environ.copy()
        env.update(CARGO_BUILD_JOBS='2', CUDA_VISIBLE_DEVICES='', OMP_NUM_THREADS='2')
        try:
            code, raw = capture(project, env)
        except CaptureFailure as error:
            raw = error.output
            retain_diagnostics(output, report, raw, project)
            raise
        retain_diagnostics(output, report, raw, project)
        report['exit_code'] = code
        text = raw.decode('utf-8', errors='replace')
        if code:
            raise TestFailure(code)
        if not re.search(r'(?m)^running 1 test\s*$', text) or not re.search(
                r'(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored;', text):
            raise ValueError('Exactly one live Paddle test must execute and pass')
        report.update(status='passed', executed_tests=1)
        return report
    except Exception as error:
        report['failure_code'] = error.code if isinstance(error, CaptureFailure) else 'validation-failed'
        raise
    finally:
        write_diagnostic(output / 'receipt.json', json.dumps(report, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        run(Path.cwd(), args.output)
    except TestFailure as error:
        print(f'Paddle live fixture failed: Cargo exited {error.exit_code}', file=sys.stderr)
        return error.exit_code if 0 < error.exit_code < 256 else 1
    except Exception:
        print('Scoped Paddle live fixture failed; inspect its bounded test diagnostics', file=sys.stderr)
        return 1
    print('Exactly one live Paddle fixture passed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
