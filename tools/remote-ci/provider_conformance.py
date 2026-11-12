"""Run the pinned provider conformance on a hosted CPU worker."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

SORTER = '__SORTER_SHA__'
CLOUD = '121bfa6682ab710cc19fbd7c7897e4aea0729920'
ROOT = Path('/kaggle/temp/provider-conformance')
OUT = Path('/kaggle/working')
ROOT.mkdir(parents=True, exist_ok=True)
start = time.monotonic()
report = {'schema': 'provider-conformance-hosted-v1', 'sorter_source': SORTER,
          'cloud_source': CLOUD, 'dart': '3.13.7', 'steps': [], 'passed': False}


def save():
    report['elapsed_s'] = round(time.monotonic() - start, 2)
    (OUT / 'provider-conformance-report.json').write_text(json.dumps(report, indent=2) + '\n')


def run(name, args, cwd=None, env=None, timeout=1800):
    t = time.monotonic()
    p = subprocess.run(list(map(str, args)), cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
    (OUT / (name + '.log')).write_text(p.stdout[-100000:] + '\n' + p.stderr[-100000:])
    report['steps'].append({'name': name, 'exit': p.returncode, 'elapsed_s': round(time.monotonic()-t, 2)})
    save()
    if p.returncode:
        raise RuntimeError(name + ' failed')


try:
    save()
    app, cloud = ROOT / 'CrispSorter', ROOT / 'crisp-cloud-rs'
    run('clone-sorter', ['git', 'clone', '-q', 'https://github.com/CrispStrobe/CrispSorter.git', app])
    run('checkout-sorter', ['git', '-C', app, 'checkout', '-q', SORTER])
    run('clone-cloud', ['git', 'clone', '-q', 'https://github.com/CrispStrobe/crisp-cloud-rs.git', cloud])
    run('checkout-cloud', ['git', '-C', cloud, 'checkout', '-q', CLOUD])
    for name, repo, expected in [('sorter', app, SORTER), ('cloud', cloud, CLOUD)]:
        actual = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        if actual != expected:
            raise RuntimeError(name + ' source mismatch')
    run('dart-download', ['curl', '-fsSL', '-o', ROOT / 'dart.zip',
        'https://storage.googleapis.com/dart-archive/channels/stable/release/3.13.7/sdk/dartsdk-linux-x64-release.zip'])
    run('dart-extract', ['unzip', '-q', ROOT / 'dart.zip', '-d', ROOT])
    run('rustup-download', ['curl', '-fsSL', '-o', ROOT / 'rustup.sh', 'https://sh.rustup.rs'])
    env = {k: v for k, v in os.environ.items() if not k.startswith(('KAGGLE_', 'HF_', 'HUGGING'))}
    env.update(CARGO_HOME=str(ROOT / 'cargo'), RUSTUP_HOME=str(ROOT / 'rustup'), CARGO_BUILD_JOBS='2')
    run('rustup-install', ['sh', ROOT / 'rustup.sh', '-y', '--profile', 'minimal', '--default-toolchain', 'stable', '--no-modify-path'], env=env)
    env['PATH'] = str(ROOT / 'dart-sdk/bin') + os.pathsep + str(ROOT / 'cargo/bin') + os.pathsep + env.get('PATH', '')
    run('python-setup', [sys.executable, '-m', 'pip', 'install', '-q', 'cryptography==50.0.0'], env=env)
    run('conformance', [sys.executable, cloud / 'conformance/run.py', '--pointycastle', '4.0.0', '--dart-pg', '2.1.0',
                        '--rust-filen', 'crates/crisp-filen-native', '--rust-internxt', 'crates/crisp-internxt-native'],
        cwd=app, env=env, timeout=5400)
    report['passed'] = True
except BaseException as e:
    report['error_type'] = type(e).__name__
    report['error'] = str(e)
    raise
finally:
    save()
