"""Source-pinned frontend/browser qualification on a CPU worker."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import urllib.request

APP_SOURCE = '@APP_SOURCE@'
root = Path('/kaggle/temp/crispsorter-frontend')
output = Path('/kaggle/working/frontend-qualification')
output.mkdir(parents=True, exist_ok=True)
root.mkdir(parents=True, exist_ok=True)
report = {'app_source': APP_SOURCE, 'completed': False, 'checks': []}


def run(name, command, cwd=None, env=None):
    with (output / (name + '.log')).open('w') as log:
        process = subprocess.Popen(command, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT)
        while process.poll() is None:
            time.sleep(10)
            print('Frontend qualification:', name, flush=True)
    report['checks'].append({'name': name, 'command': command, 'exit_code': process.returncode})
    if process.returncode:
        raise RuntimeError(f'{name} failed; see {name}.log')


try:
    if len(APP_SOURCE) != 40:
        raise ValueError('Immutable source required')
    archive = root / 'node-v24.22.1-linux-x64.tar.xz'
    base = 'https://nodejs.org/dist/v24.22.1/'
    with urllib.request.urlopen(base + 'SHASUMS256.txt', timeout=60) as response:
        checksums = response.read().decode()
    expected = next(line.split()[0] for line in checksums.splitlines() if line.endswith('  ' + archive.name))
    with urllib.request.urlopen(base + archive.name, timeout=120) as response, archive.open('wb') as stream:
        shutil.copyfileobj(response, stream)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
        raise ValueError('Node integrity failed')
    run('node-extract', ['tar', '-xJf', str(archive), '-C', str(root)])
    env = os.environ.copy()
    env['PATH'] = str(root / 'node-v24.22.1-linux-x64/bin') + ':' + env['PATH']
    env['PLAYWRIGHT_BROWSERS_PATH'] = str(root / 'browsers')
    app = root / 'CrispSorter'
    run('clone', ['git', 'clone', '--filter=blob:none', '--no-checkout', 'https://github.com/CrispStrobe/CrispSorter.git', str(app)], env=env)
    run('checkout', ['git', 'checkout', '--detach', APP_SOURCE], cwd=app, env=env)
    run('install', ['npm', 'ci', '--no-audit', '--no-fund'], cwd=app, env=env)
    # These checks are independent; preserve each failure while completing the others.
    for name, command in [('svelte', ['npm', 'run', 'check']),
                          ('unit', ['npm', 'test', '--', '--maxWorkers=1', '--minWorkers=1']),
                          ('build', ['npm', 'run', 'build']),
                          ('browser-install', ['npx', 'playwright', 'install', '--with-deps', 'chromium']),
                          ('browser', ['npx', 'playwright', 'test'])]:
        try:
            run(name, command, cwd=app, env=env)
        except RuntimeError:
            pass
    if (app / 'test-results').exists():
        shutil.copytree(app / 'test-results', output / 'browser-results', dirs_exist_ok=True)
    report['completed'] = all(check['exit_code'] == 0 for check in report['checks'])
except Exception as error:
    report['error'] = {'type': type(error).__name__, 'message': str(error)}
finally:
    (output / 'report.json').write_text(json.dumps(report, indent=2))
if not report['completed']:
    raise SystemExit(1)
