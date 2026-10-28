"""CPU-only native qualification when the shared host cannot admit compilation.

Render @APP_SOURCE@ to the immutable commit before dispatch. Only reports and
logs are retained; dependencies and compiler outputs stay in worker scratch.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import urllib.request

APP_SOURCE = '@APP_SOURCE@'
root = Path('/kaggle/temp/crispsorter-native')
output = Path('/kaggle/working/native-qualification')
output.mkdir(parents=True, exist_ok=True)
report = {'schema': 'crispsorter-native-qualification-v1', 'app_source': APP_SOURCE, 'completed': False}


def save():
    (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')


def run(command, cwd=None, env=None):
    print('Running:', command, flush=True)
    with (output / 'native.log').open('a') as log:
        process = subprocess.Popen(command, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT)
        while process.poll() is None:
            time.sleep(20)
            print('Native qualification heartbeat:', command[0], 'free disk GiB', round(shutil.disk_usage(root).free / 2**30, 1), flush=True)
        if process.returncode:
            raise RuntimeError(f'{command[0]} exited {process.returncode}; see native.log')


save()
try:
    if len(APP_SOURCE) != 40:
        raise ValueError('Immutable app source required')
    root.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.update(CARGO_BUILD_JOBS='2', CARGO_INCREMENTAL='0', CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0',
               CARGO_TARGET_DIR=str(root / 'target'), CARGO_HOME=str(root / 'cargo'),
               CARGO_HTTP_MULTIPLEXING='false', CARGO_NET_RETRY='5', DEBIAN_FRONTEND='noninteractive')
    run(['apt-get', 'update', '-qq'], env=env)
    run(['apt-get', 'install', '-y', '-qq', 'libwebkit2gtk-4.1-dev', 'libgtk-3-dev',
         'libayatana-appindicator3-dev', 'librsvg2-dev', 'protobuf-compiler', 'libssl-dev',
         'libopenblas0', 'clang', 'lld', 'cmake', 'pkg-config', 'curl', 'git'], env=env)
    rustup = root / 'rustup-init.sh'
    with urllib.request.urlopen('https://sh.rustup.rs', timeout=60) as response:
        rustup.write_bytes(response.read())
    env['RUSTUP_HOME'] = str(root / 'rustup')
    run(['sh', str(rustup), '-y', '--profile', 'minimal', '--default-toolchain', '1.98.1'], env=env)
    env['PATH'] = str(root / 'cargo/bin') + ':' + env['PATH']
    report['rustc'] = subprocess.check_output(['rustc', '--version'], env=env, text=True).strip()
    for name, revision in [('CrispSorter', APP_SOURCE), ('CrispEmbed', '78493a31fc3043f3af4ab6fd2f197a32da6f07f0'),
                           ('CrispASR', 'v0.8.32'), ('crisp-docx', 'ddf7830d8d92e6b59b10874d144a59f163a256e1')]:
        destination = root / name
        run(['git', 'clone', '--filter=blob:none', '--no-checkout', f'https://github.com/CrispStrobe/{name}.git', str(destination)], env=env)
        run(['git', 'checkout', '--detach', revision], cwd=destination, env=env)
    app = root / 'CrispSorter'
    archive = root / 'crispembed-linux-x86_64.tar.gz'
    with urllib.request.urlopen('https://github.com/CrispStrobe/CrispEmbed/releases/download/v0.17.12/' + archive.name, timeout=120) as response, archive.open('wb') as stream:
        shutil.copyfileobj(response, stream)
    run(['python', 'scripts/verify_crispembed_bundle.py', str(archive), '--source', str(root / 'CrispEmbed')], cwd=app, env=env)
    libs = root / 'CrispEmbed/build'
    libs.mkdir()
    run(['tar', '-xzf', str(archive), '-C', str(libs)], env=env)
    env['CRISPEMBED_SYS_LIB_DIR'] = str(libs)
    env['LD_LIBRARY_PATH'] = str(libs) + ':' + env.get('LD_LIBRARY_PATH', '')
    # No bundling in this job; do not pretend placeholder resources are a release.
    (app / 'src-tauri/bin').mkdir(exist_ok=True)
    (app / 'src-tauri/bin/.gitkeep').touch()
    config = app / 'src-tauri/tauri.conf.json'
    value = json.loads(config.read_text())
    value['bundle']['externalBin'] = []
    config.write_text(json.dumps(value))
    features = 'desktop,crispembed,pdf-zpdf'
    report['features'] = features
    save()
    run(['cargo', 'test', '--locked', '-p', 'crispsorter', '--lib', '--features', features, '--', '--test-threads=2'], cwd=app, env=env)
    report['completed'] = True
except Exception as error:
    report['error'] = {'type': type(error).__name__, 'message': str(error)}
    raise
finally:
    save()
