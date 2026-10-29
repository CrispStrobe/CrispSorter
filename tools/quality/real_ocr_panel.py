"""Evaluate shipped OCR presets on a preregistered, hash-verified real panel.

Images and transcripts remain on the worker. Committed summaries contain only
metrics, hashes, attribution and source references. Uses the upstream scorer
unchanged so retained newer-engine measurements can be compared by page.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import time
import urllib.request


def sha(data):
    return hashlib.sha256(data).hexdigest()


def fetch(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'CrispEmbed-OCR-eval/1.0 (https://github.com/CrispStrobe/CrispEmbed)'})
    for attempt in range(5):
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                return response.read()
        except Exception:
            if attempt == 4: raise
            time.sleep(2 ** attempt)


def child(command, log, timeout):
    started = time.monotonic()
    timed_out = False
    with log.open('w') as stream:
        process = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
        while True:
            pid, status, usage = os.wait4(process.pid, os.WNOHANG)
            if pid:
                break
            if time.monotonic() - started > timeout:
                os.killpg(process.pid, signal.SIGKILL)
                _, status, usage = os.wait4(process.pid, 0)
                timed_out = True
                break
            time.sleep(1)
        process.returncode = os.waitstatus_to_exitcode(status)
    return {'exit_code': process.returncode, 'timed_out': timed_out,
            'wall_seconds': time.monotonic() - started, 'peak_rss_mb': usage.ru_maxrss / 1024}


def main(app, executable, output):
    app, executable, output = app.resolve(), executable.resolve(), output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    scratch = app.parent / 'real-ocr-inputs'
    scratch.mkdir(exist_ok=True)
    plan = json.loads((app / 'tests/quality/ocr-real-panel.json').read_text())
    base = 'https://raw.githubusercontent.com/CrispStrobe/CrispEmbed/' + plan['upstream_source'] + '/'
    manifest_bytes = fetch(base + plan['manifest_path'])
    scorer_bytes = fetch(base + plan['scorer_path'])
    if sha(manifest_bytes) != plan['manifest_sha256'] or sha(scorer_bytes) != plan['scorer_sha256']:
        raise ValueError('Frozen upstream manifest/scorer mismatch; no inference will run')
    manifest = {page['id']: page for page in json.loads(manifest_bytes)['pages']}
    scorer = scratch / 'score.py'
    scorer.write_bytes(scorer_bytes)
    spec = importlib.util.spec_from_file_location('frozen_ocr_score', scorer)
    scoring = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(scoring)
    report = {'scope': plan['scope'], 'plan_sha256': sha((app / 'tests/quality/ocr-real-panel.json').read_bytes()),
              'upstream_source': plan['upstream_source'], 'manifest_sha256': sha(manifest_bytes),
              'scorer_sha256': sha(scorer_bytes), 'images': [], 'runs': [], 'completed': False,
              'protocol': 'Fresh process/context per case; initial model downloads excluded. Original preset recipe except the preregistered per-page VLM output cap. No model sees the reference text.',
              'decision_rule': plan['decision_rule']}
    report['cpu_count'] = os.cpu_count()
    report['cpu_model'] = next(line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name'))
    report['mem_total_gb'] = int(next(line.split()[1] for line in Path('/proc/meminfo').read_text().splitlines() if line.startswith('MemTotal'))) / 2**20
    def save():
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    save()
    inputs = {}
    parquet = None
    # Verify EVERY selected image/reference before any inference, so a changed
    # CDN object cannot be silently substituted after seeing quality results.
    for case in plan['cases']:
        page = manifest[case['id']]
        if sha(page['ground_truth'].encode()) != case['ground_truth_sha256']:
            raise ValueError('Frozen reference mismatch for ' + case['id'])
        image = page['image']
        if 'hf_parquet' in image:
            info = image['hf_parquet']
            if parquet is None:
                data = fetch(info['url'])
                if sha(data) != info['sha256']:
                    raise ValueError('Frozen receipt parquet mismatch')
                parquet = scratch / 'cord.parquet'
                parquet.write_bytes(data)
            import pyarrow.parquet as pq
            data = pq.read_table(parquet, columns=['image']).slice(info['row'], 1).to_pylist()[0]['image']['bytes']
        else:
            data = fetch(image['url'])
        report['images'].append({'id':case['id'], 'sha256':sha(data), 'ground_truth_sha256':case['ground_truth_sha256'],
            'expected_sha256':case['image_sha256'], 'sha256_ok':sha(data) == case['image_sha256'],
            'image_license':page['image_license'], 'ground_truth_license':page['ground_truth_license'],
            'ground_truth_provenance':page['ground_truth_provenance'], 'tags':page['tags'], 'primary_metric':case['primary_metric']})
        save()
        if sha(data) != case['image_sha256'] or image['sha256'] != case['image_sha256']:
            raise ValueError('Frozen image mismatch for ' + case['id'])
        path = scratch / (case['id'] + '.jpg')
        path.write_bytes(data)
        inputs[case['id']] = path
        print('PANEL input verified:', case['id'], flush=True)
    save()
    models_path = output / 'models.json'
    warmup = child([str(executable), 'models', str(models_path)], output / 'models.log', 900)
    if warmup['exit_code']:
        raise RuntimeError('Model resolution failed before the comparison')
    report['models'] = json.loads(models_path.read_text())
    if report['models'] != plan['models']:
        save()
        raise ValueError('Frozen model bytes differ from the qualified preset weights; no comparison will run')
    report['warmup'] = warmup
    started = time.monotonic()
    for preset in ['quick', 'multilingual', 'tables', 'difficult']:
        for case in plan['cases']:
            if preset not in case['presets']: continue
            if time.monotonic() - started > plan['total_budget_seconds']:
                save()
                raise TimeoutError('Preregistered panel time budget exhausted')
            page = manifest[case['id']]
            name = case['id'] + '-' + preset
            payload_path = output / (name + '.json')
            print('PANEL start:', name, flush=True)
            measurement = child([str(executable), preset, str(inputs[case['id']]), str(page['max_tokens']), str(payload_path)],
                                output / (name + '.log'), plan['case_timeout_seconds'])
            row = {'page':case['id'], 'preset':preset, 'max_tokens':page['max_tokens'], **measurement}
            if measurement['exit_code'] == 0:
                payload = json.loads(payload_path.read_text())
                row.update(scoring.score(payload['text'], page['ground_truth']))
                row.update(prediction_sha256=sha(payload['text'].encode()), config=payload['config'],
                           generation_cap=payload['generation_cap'], confidence_count=payload['confidence_count'])
            report['runs'].append(row)
            print('PANEL result:', json.dumps(row), flush=True)
            save()
    report['completed'] = True
    report['all_cases_executed_successfully'] = all(row['exit_code'] == 0 for row in report['runs'])
    save()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--executable', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    main(args.app, args.executable, args.output)
