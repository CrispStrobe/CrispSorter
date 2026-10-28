"""Standalone Kaggle CPU entrypoint; render the exact app commit before push.

Only this code file is uploaded by Kaggle. Inputs are fetched from an immutable
public source after the admitted network preflight; a failed fetch stops the job.
"""
import json
from pathlib import Path
import subprocess
import sys
import urllib.request

APP_SOURCE = '@APP_SOURCE@'
root = Path('/kaggle/working/crispsorter-evaluation-inputs')
root.mkdir(parents=True, exist_ok=True)
try:
    if len(APP_SOURCE) != 40:
        raise ValueError('An immutable app source is required')
    for relative in ['tests/quality/library-corpus.json', 'tests/quality/model-lock.json', 'tools/quality/run_library_models.py']:
        url = f'https://raw.githubusercontent.com/CrispStrobe/CrispSorter/{APP_SOURCE}/{relative}'
        destination = root / Path(relative).name
        with urllib.request.urlopen(url, timeout=60) as response:
            destination.write_bytes(response.read())
    (root / 'app-source.txt').write_text(APP_SOURCE + '\n')
    subprocess.run([sys.executable, str(root / 'run_library_models.py'),
                    '--corpus', str(root / 'library-corpus.json'), '--lock', str(root / 'model-lock.json'),
                    '--output', '/kaggle/working/qualification'], check=True)
except Exception as error:
    Path('/kaggle/working/entrypoint-error.json').write_text(json.dumps({
        'completed': False, 'app_source': APP_SOURCE, 'type': type(error).__name__, 'message': str(error),
    }, indent=2))
    raise
