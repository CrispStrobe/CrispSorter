#!/usr/bin/env python3
"""Run one source-bound CPU Paddle fixture, with explicit bounded evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import sys
import time

from ocr_mobile_build_receipt import source_tree_identity
from paddle_live_diagnostics import sanitize_output

TEST = 'extractors::ocr_paddle::tests::live_paddle_recognises_the_fixture_sentence'
FEATURES = 'desktop,sidecars,audio-glint,aitoolkit,cloud-backup,pdf-zpdf,drive-filen-native,drive-internxt-native,crispembed,ocr-reviewed-pdf,crispembed-license-api,ocr-photo-tools,crispasr,translate-align,translate-nmt,pdf-render,anydoc,fuse,paddle-ocr'
COMMAND = ['cargo', 'test', '--locked', '-p', 'crispsorter', '--lib', '--features', FEATURES,
           '--', '--ignored', '--exact', TEST, '--nocapture', '--test-threads=1']
INPUTS = ['Cargo.lock', 'Cargo.toml', 'src-tauri/Cargo.toml', 'src-tauri/src/extractors/ocr_paddle.rs',
          'src-tauri/src/extractors/paddle_resize.rs', 'src-tauri/src/extractors/context_cache.rs',
          'src-tauri/src/ocr_builtin_models.rs', 'src-tauri/src/ocr_models.rs',
          'tests/quality/ocr-task.png', 'scripts/run_paddle_live_fixture.py', 'scripts/paddle_live_diagnostics.py']
MAX_OUTPUT = 8 * 1024**2
MAX_SECONDS = 90 * 60


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], timeout=30, stderr=subprocess.PIPE).decode().strip()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def snapshot(project):
    source = git(project, 'rev-parse', 'HEAD')
    if (os.environ.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'
            or os.environ.get('PADDLE_LIVE_SCOPE') != 'paddle-live'
            or not re.fullmatch('[0-9a-f]{40}', os.environ.get('GITHUB_SHA', ''))
            or source != os.environ['GITHUB_SHA']):
        raise ValueError('Explicit scoped dispatch and exact checked-out source required')
    identity = source_tree_identity(project)
    hashes = {}
    for name in INPUTS:
        path = project / name
        if path.is_symlink() or path.resolve() != path or not path.is_file():
            raise ValueError('Source input must be a regular committed file')
        data = path.read_bytes()
        if data != subprocess.check_output(['git', '-C', str(project), 'show', 'HEAD:' + name], timeout=30, stderr=subprocess.PIPE):
            raise ValueError('Source input changed before the live fixture')
        hashes[name] = sha(data)
    siblings = {}
    for name, expected in [('CrispEmbed', os.environ['CRISPEMBED_COMMIT']),
                           ('CrispASR', os.environ['CRISPASR_REF']), ('crisp-docx', os.environ['CRISPDOCX_REF'])]:
        root = project.parent / name
        head = git(root, 'rev-parse', 'HEAD')
        if head != git(root, 'rev-parse', expected + '^{commit}'):
            raise ValueError('Sibling checkout differs from its workflow pin')
        siblings[name] = head
    lock = json.loads((project / 'scripts/crispembed-runtime-lock.json').read_text())
    if siblings['CrispEmbed'] != lock['source_commit']:
        raise ValueError('Native checkout and production runtime lock differ')
    return {'source': source, 'tree': git(project, 'rev-parse', 'HEAD^{tree}'),
            'source_identity': identity, 'source_files_sha256': hashes, 'dependency_sources': siblings}


class CaptureFailure(Exception):
    def __init__(self, code, output):
        self.code, self.output = code, output
        super().__init__('Scoped capture stopped')


def capture(project, env):
    process, selector, failure, code = None, None, None, None
    output = bytearray()
    try:
        process = subprocess.Popen(COMMAND, cwd=project, env=env, stdout=subprocess.PIPE,
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


def retain_diagnostics(output, report, raw, project):
    roots = [project, project.parent, os.environ.get('RUNNER_TEMP'), os.environ.get('HOME')]
    text, status = sanitize_output(raw.decode('utf-8', errors='replace'), roots)
    (output / 'diagnostics.txt').write_text(text)
    report.update(diagnostics_status=status, captured_output_sha256=sha(raw))


def run(project, output):
    project, output = project.resolve(), output.absolute()
    if output.resolve() != output or output.is_relative_to(project) or output.exists():
        raise ValueError('Use fresh external evidence output')
    output.mkdir(parents=True)
    report = {'schema': 'crispsorter-scoped-paddle-live-v1', 'status': 'failed', 'test': TEST,
              'features': FEATURES, 'default_features': True, 'cargo_locked': True,
              'limits': {'seconds': MAX_SECONDS, 'output_bytes': MAX_OUTPUT, 'build_jobs': 2, 'test_threads': 1},
              'scope': 'One CPU Paddle fixture, cache reuse and table session; not a full suite, release, GUI/device/corpus qualification'}
    try:
        before = snapshot(project)
        report.update(before)
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
        if code or not re.search(r'(?m)^running 1 test\s*$', text) or not re.search(
                r'(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored;', text):
            raise ValueError('Exactly one live Paddle test must execute and pass')
        if snapshot(project) != before:
            raise ValueError('Source/build inputs changed during live Paddle validation')
        report.update(status='passed', executed_tests=1)
        return report
    except Exception as error:
        report['failure_code'] = error.code if isinstance(error, CaptureFailure) else 'validation-failed'
        raise
    finally:
        (output / 'receipt.json').write_text(json.dumps(report, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        run(Path.cwd(), args.output)
    except Exception:
        print('Scoped Paddle live fixture failed; inspect its bounded source receipt/diagnostics', file=sys.stderr)
        return 1
    print('Exactly one source-bound live Paddle fixture passed')
    return 0


if __name__ == '__main__':
    sys.exit(main())
