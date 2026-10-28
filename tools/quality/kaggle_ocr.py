"""CPU-only execution of source-bound production OCR code.

Render both source placeholders before dispatch. This probe omits unrelated
application dependencies; final release jobs exercise actual packaged apps.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import urllib.request

APP_SOURCE = '@APP_SOURCE@'
BINARY_SOURCE = '@BINARY_SOURCE@'
root = Path('/kaggle/temp/crispsorter-ocr')
output = Path('/kaggle/working/ocr-qualification')
output.mkdir(parents=True, exist_ok=True)
root.mkdir(parents=True, exist_ok=True)
report = {'scope': 'Verbatim production OCR functions/configuration and native bindings, not a full application build',
          'harness_source': APP_SOURCE, 'binary_source': BINARY_SOURCE, 'completed': False, 'checks': []}


def save():
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')


def run(name, command, cwd=None, env=None, timeout=600):
    print('OCR qualification:', name, command, flush=True)
    started = time.monotonic()
    with (output / 'probe.log').open('a') as log:
        process = subprocess.Popen(command, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        while process.poll() is None:
            time.sleep(10)
            elapsed = time.monotonic() - started
            print('OCR heartbeat:', name, round(elapsed), 'seconds', flush=True)
            if elapsed > timeout:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                raise TimeoutError(name + ' exceeded its bounded worker timeout')
    report['checks'].append({'name': name, 'exit_code': process.returncode, 'elapsed_seconds': time.monotonic() - started})
    save()
    if process.returncode:
        raise RuntimeError(name + ' failed; see probe.log')


save()
try:
    if not all(len(revision) == 40 for revision in [APP_SOURCE, BINARY_SOURCE]):
        raise ValueError('Immutable source commits required')
    env = os.environ.copy()
    env.update(CARGO_BUILD_JOBS='2', CARGO_INCREMENTAL='0', CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0',
               CARGO_TARGET_DIR=str(root / 'target'), CARGO_HOME=str(root / 'cargo'), RUSTUP_HOME=str(root / 'rustup'),
               RUSTC_WRAPPER='', RUSTC_WORKSPACE_WRAPPER='', SCCACHE_DISABLE='1',
               CARGO_HTTP_MULTIPLEXING='false', CARGO_NET_RETRY='5', DEBIAN_FRONTEND='noninteractive')
    run('apt-update', ['apt-get', 'update', '-qq'], env=env)
    run('toolchain', ['apt-get', 'install', '-y', '-qq', 'build-essential', 'cmake', 'pkg-config', 'curl', 'git'], env=env)
    installer = root / 'rustup-init.sh'
    with urllib.request.urlopen('https://sh.rustup.rs', timeout=60) as response:
        installer.write_bytes(response.read())
    run('rust', ['sh', str(installer), '-y', '--profile', 'minimal', '--default-toolchain', '1.98.1'], env=env)
    env['PATH'] = str(root / 'cargo/bin') + ':' + env['PATH']
    report['rustc'] = subprocess.check_output(['rustc', '--version'], env=env, text=True).strip()
    for name, revision in [('CrispSorter', APP_SOURCE), ('CrispEmbed', '78493a31fc3043f3af4ab6fd2f197a32da6f07f0'),
                           ('CrispASR', '5e318f0c7ec93a1c7cf5851de4625f0fd74bd7c0')]:
        destination = root / name
        run('clone-' + name, ['git', 'clone', '--filter=blob:none', '--no-checkout', 'https://github.com/CrispStrobe/' + name + '.git', str(destination)], env=env)
        run('checkout-' + name, ['git', 'checkout', '--detach', revision], cwd=destination, env=env)
    app = root / 'CrispSorter'
    harness = root / 'CrispASR/tools/kaggle/kaggle_harness.py'
    spec = importlib.util.spec_from_file_location('kaggle_harness', harness)
    kh = importlib.util.module_from_spec(spec)
    sys.modules['kaggle_harness'] = kh
    spec.loader.exec_module(kh)
    kh.install_build_toolchain()
    admitted = kh.safe_build_jobs(gpu=False)
    env['CARGO_BUILD_JOBS'] = str(min(2, os.cpu_count() or 2)) if admitted == '$(nproc)' else str(min(2, int(admitted)))
    report['build_jobs'] = int(env['CARGO_BUILD_JOBS'])
    # No C++ source/CUDA build: the CPU archive is verified below. The shared
    # harness still supplies toolchain setup, admission and build heartbeats.
    # A tools/tests-only follow-up must not silently qualify different OCR code
    # from the already queued platform binaries.
    paths = ['src-tauri/src/extractors/mod.rs', 'src-tauri/src/extractors/ocr_crispembed.rs',
             'src-tauri/src/extractors/context_cache.rs', 'src-tauri/src/extractors/ocr_presets.rs',
             'tests/quality/ocr-task.png', 'tests/quality/ocr-task.json']
    run('source-equivalence', ['git', 'diff', '--exit-code', BINARY_SOURCE, APP_SOURCE, '--', *paths], cwd=app, env=env)
    archive = root / 'crispembed-linux-x86_64.tar.gz'
    with urllib.request.urlopen('https://github.com/CrispStrobe/CrispEmbed/releases/download/v0.17.12/' + archive.name, timeout=120) as response, archive.open('wb') as stream:
        shutil.copyfileobj(response, stream)
    run('verify-runtime', ['python', 'scripts/verify_crispembed_bundle.py', str(archive), '--asset', archive.name, '--source', str(root / 'CrispEmbed')], cwd=app, env=env)
    libs = root / 'CrispEmbed/build'
    libs.mkdir()
    run('extract-runtime', ['tar', '-xzf', str(archive), '-C', str(libs)], env=env)
    env.update(CRISPEMBED_SYS_LIB_DIR=str(libs), LD_LIBRARY_PATH=str(libs))
    probe = app / 'ocr-qualification'
    run('source-probe', ['python', 'tools/quality/isolated_ocr.py', '--app', str(app), '--embed', str(root / 'CrispEmbed'), '--destination', str(probe)], cwd=app, env=env)
    shutil.copyfile(probe / 'source-fragments.json', output / 'source-fragments.json')
    run('lock', ['cargo', 'generate-lockfile', '--manifest-path', str(probe / 'Cargo.toml')], cwd=app, env=env)
    shutil.copyfile(probe / 'Cargo.lock', output / 'probe.Cargo.lock')
    report['probe_lock_sha256'] = hashlib.sha256((probe / 'Cargo.lock').read_bytes()).hexdigest()
    with kh.build_heartbeat('CrispSorter source-bound OCR compilation'):
        run('model-free', ['cargo', 'test', '--locked', '--manifest-path', str(probe / 'Cargo.toml'), '--lib'], cwd=app, env=env)
    env.update(CRISPSORTER_OCR_RECEIPT=str(output / 'ocr.json'), QWEN2VL_OCR_FORCE_CPU='1', MATH_OCR_FORCE_CPU='1')
    with kh.build_heartbeat('CrispSorter source-bound live OCR'):
        run('live-ocr', ['cargo', 'test', '--locked', '--manifest-path', str(probe / 'Cargo.toml'), '--lib', 'ocr_task_presets_live', '--', '--ignored', '--nocapture', '--test-threads=1'], cwd=app, env=env, timeout=1800)
    report['completed'] = True
except Exception as error:
    report['error'] = {'type': type(error).__name__, 'message': str(error)}
    raise
finally:
    save()
