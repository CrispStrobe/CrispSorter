#!/usr/bin/env python3
"""CPU-only adoption evaluation, separate from native app/binary qualification.

References and expected answers are frozen before inference. The model runtimes
remain opt-in regardless of these bounded results. Run on the admitted Kaggle
CPU worker; do not copy model payloads back to the development host.
"""
import argparse
import contextlib
import gc
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.request


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(4 * 1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def write(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + '\n')
    temporary.replace(path)


def download(url, path, digest=None, size=None):
    path = Path(path)
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(path.suffix + '.partial')
        with urllib.request.urlopen(url, timeout=120) as response, temporary.open('wb') as stream:
            shutil.copyfileobj(response, stream, 4 * 1024 * 1024)
        temporary.replace(path)
    if size is not None and path.stat().st_size != size:
        raise ValueError(f'Unexpected model size: {path.name}')
    if digest is not None and sha(path) != digest:
        raise ValueError(f'Model integrity failed: {path.name}')
    return path


def request(base, route, value=None, timeout=120):
    data = None if value is None else json.dumps(value, ensure_ascii=False).encode()
    req = urllib.request.Request(base + route, data=data, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=timeout) as response:
        payload = response.read(4 * 1024 * 1024 + 1)
    if len(payload) > 4 * 1024 * 1024:
        raise ValueError('Server response exceeds consumer bound')
    return json.loads(payload)


@contextlib.contextmanager
def server(binary, flag, model, output):
    env = os.environ.copy()
    gates = ['MOEDERN_PUBLIC_TEXT', 'MOEDERN_TEXT_FRONTEND', 'MOEDERN_TEXT_COMPOSED', 'MOEDERN_COLBERT', 'MOEDERN_PACKED', 'MOEDERN_FAMILY',
             'JOSEV_PACKED', 'JOSEV_COMPOSED', 'JOSEV_BACKBONE', 'QWEN35_OCR', 'JOSEV_HEAD',
             'JOSEV_TEXT_FRONTEND', 'JOSEV_ANSWER_SERIALIZER']
    for gate in gates:
        env['CRISPEMBED_' + gate] = '1'
    env.update(CRISPEMBED_MOEDERN_FAST='0', CRISPEMBED_FORCE_CPU='1', CRISPEMBED_OFFLINE='1')
    with Path(output).open('w') as log:
        process = subprocess.Popen([str(binary), flag, str(model), '--port', '18080', '-t', '2', '--gpu-backend', 'cpu'],
                                   env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            for _ in range(180):
                if process.poll() is not None:
                    raise RuntimeError(f'Native server exited during startup; see {output}')
                try:
                    report = request('http://127.0.0.1:18080', '/capabilities', timeout=1)
                    break
                except Exception:
                    time.sleep(1)
            else:
                raise TimeoutError('Native server startup exceeded 180 seconds')
            yield 'http://127.0.0.1:18080', report
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def metrics(rankings, queries):
    import math
    rows = []
    for query in queries:
        ranking = rankings[query['id']]
        relevant = set(query['relevant'])
        hits = [1 if name in relevant else 0 for name in ranking]
        ideal = sum(1 / math.log2(i + 2) for i in range(min(10, len(relevant))))
        dcg = sum(hit / math.log2(i + 2) for i, hit in enumerate(hits[:10]))
        rank = next((i + 1 for i, hit in enumerate(hits) if hit), None)
        rows.append({'query': query['id'], 'recall_at_1': sum(hits[:1]) / len(relevant),
                     'recall_at_5': sum(hits[:5]) / len(relevant), 'ndcg_at_10': dcg / ideal,
                     'mrr': 0 if rank is None else 1 / rank, 'ranking': ranking})
    return {'mean': {key: sum(r[key] for r in rows) / len(rows)
                     for key in ['recall_at_1', 'recall_at_5', 'ndcg_at_10', 'mrr']}, 'queries': rows}


def baseline(corpus, lock):
    import numpy as np
    import torch
    import transformers
    from transformers import AutoModel, AutoTokenizer
    torch.set_num_threads(2)
    config = lock['baseline']
    tokenizer = AutoTokenizer.from_pretrained(config['id'], revision=config['revision'])
    model = AutoModel.from_pretrained(config['id'], revision=config['revision']).float().eval()
    durations = []
    def encode(text):
        inputs = tokenizer(text, return_tensors='pt', truncation=True, max_length=512)
        started = time.perf_counter()
        with torch.inference_mode():
            vector = model(**inputs).last_hidden_state[:, 0]
            vector = torch.nn.functional.normalize(vector, dim=1)[0].cpu().numpy()
        durations.append(time.perf_counter() - started)
        return vector
    documents = np.array([encode(doc['text']) for doc in corpus['documents']])
    rankings = {}
    for query in corpus['queries']:
        scores = documents @ encode(query['text'])
        ranking = np.argsort(-scores, kind='stable')
        rankings[query['id']] = [corpus['documents'][int(i)]['id'] for i in ranking]
    result = metrics(rankings, corpus['queries'])
    result.update(model=config, device='CPU', dtype='float32', transformers=transformers.__version__,
                  torch=torch.__version__, encode_seconds=durations,
                  vector_bytes=int(documents.nbytes), parameters=sum(p.numel() for p in model.parameters()))
    del model, tokenizer, documents
    gc.collect()
    return result, rankings


def multivectors(response, role):
    import numpy as np
    if response['role'] != role:
        raise ValueError('Native role differs from requested role')
    count = len(response['ids'])
    if not 1 <= count <= 512:
        raise ValueError('Invalid token count')
    for key in ['supplied_mask', 'effective_mask', 'scoring_mask']:
        if len(response[key]) != count or any(value not in [0, 1] for value in response[key]):
            raise ValueError('Invalid role mask: ' + key)
    selected = sum(response['scoring_mask'])
    vectors = np.asarray(response['selected_vectors'], dtype=np.float32)
    if not selected or vectors.size != selected * 128 or not np.isfinite(vectors).all():
        raise ValueError('Selected vectors do not match scoring mask')
    return vectors.reshape(selected, 128)


def evaluate_moedern(base, corpus, baseline_rankings):
    import numpy as np
    durations = []
    documents = {}
    for doc in corpus['documents']:
        started = time.perf_counter()
        documents[doc['id']] = multivectors(request(base, '/moedern/encode', {'text': doc['text'], 'role': 'document'}), 'document')
        durations.append(time.perf_counter() - started)
    rankings = {}
    parity = []
    for query in corpus['queries']:
        started = time.perf_counter()
        vectors = multivectors(request(base, '/moedern/encode', {'text': query['text'], 'role': 'query'}), 'query')
        # A separate reranker operates on the baseline's candidate window.
        candidates = baseline_rankings[query['id']][:20]
        scores = {name: float((vectors @ documents[name].T).max(axis=1).sum(dtype=np.float32)) for name in candidates}
        rankings[query['id']] = sorted(candidates, key=lambda name: -scores[name])
        durations.append(time.perf_counter() - started)
        first = next(doc for doc in corpus['documents'] if doc['id'] == rankings[query['id']][0])
        native = request(base, '/moedern/score', {'query': query['text'], 'document': first['text']})['score']
        error = abs(float(native) - scores[first['id']])
        if error > 0.002:
            raise ValueError('Consumer MaxSim differs from native score')
        parity.append({'query': query['id'], 'maxsim_absolute_error': error})
    result = metrics(rankings, corpus['queries'])
    result.update(encode_seconds=durations, document_vector_bytes=sum(v.nbytes for v in documents.values()),
                  role_mask_checks=True, native_score_checks=parity, candidate_window=20)
    return result


def evaluate_systemone(base, corpus):
    choices = corpus['decision_choices']
    criteria = {f'c{i:02}': choice for i, choice in enumerate(choices)}
    rows = []
    for doc in corpus['documents']:
        if doc['id'] not in corpus['decision_ids']:
            continue
        started = time.perf_counter()
        response = request(base, '/systemone', {'state': doc['text'][:1600], 'questions': {'suggestion': {
            'type': 'choice', 'instructions': 'Choose the document type best supported by this evidence.', 'criteria': criteria}}})
        answer = response['answers']['suggestion']
        if answer['type'] != 'choice' or answer['choice'] not in criteria:
            raise ValueError('Typed decision violated the consumer schema')
        actual = criteria[answer['choice']]
        rows.append({'document': doc['id'], 'expected': doc['type'], 'actual': actual,
                     'correct': actual == doc['type'], 'model_score': answer['confidence'],
                     'elapsed_seconds': time.perf_counter() - started, 'response': response})
    return {'accuracy': sum(row['correct'] for row in rows) / len(rows), 'cases': rows,
            'default_promotion': False, 'quality_gate': sum(row['correct'] for row in rows) / len(rows) >= corpus['acceptance']['systemone_accuracy_min'],
            'scope': '12 author-written examples; independent of publisher parity fixtures'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--corpus', type=Path, required=True)
    parser.add_argument('--lock', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    output = args.output
    output.mkdir(parents=True, exist_ok=True)
    report = {'schema': 'crispsorter-library-model-evaluation-v1', 'completed': False, 'default_promotion': False}
    write(output / 'report.json', report)
    try:
        lock = json.loads(args.lock.read_text())
        if sha(args.corpus) != lock['corpus_sha256']:
            raise ValueError('Frozen corpus changed')
        corpus = json.loads(args.corpus.read_text())
        report.update(corpus_sha256=lock['corpus_sha256'], vendor_source=lock['vendor_source'], ggml_source=lock['ggml_source'])
        scratch = Path('/kaggle/temp/crispsorter-library-eval')
        scratch.mkdir(parents=True, exist_ok=True)
        source = scratch / 'CrispEmbed'
        if not source.exists():
            subprocess.run(['git', 'clone', '--filter=blob:none', '--no-checkout', 'https://github.com/CrispStrobe/CrispEmbed.git', str(source)], check=True)
        subprocess.run(['git', '-C', str(source), 'checkout', '--detach', lock['vendor_source']], check=True)
        subprocess.run(['git', '-C', str(source), 'submodule', 'update', '--init', '--recursive', '--depth', '1'], check=True)
        if subprocess.check_output(['git', '-C', str(source / 'ggml'), 'rev-parse', 'HEAD'], text=True).strip() != lock['ggml_source']:
            raise ValueError('ggml source differs')
        harness_path = download('https://raw.githubusercontent.com/CrispStrobe/CrispASR/' + lock['harness_source'] + '/tools/kaggle/kaggle_harness.py', scratch / 'kaggle_harness.py')
        spec = importlib.util.spec_from_file_location('kaggle_harness', harness_path)
        kh = importlib.util.module_from_spec(spec)
        sys.modules['kaggle_harness'] = kh
        spec.loader.exec_module(kh)
        kh.install_build_toolchain()
        os.environ['CCACHE_MAXSIZE'] = '1G'
        if shutil.which('ccache'):
            subprocess.run(['ccache', '--max-size=1G'], check=True)
        build = scratch / 'build'
        flags = ['-DCMAKE_BUILD_TYPE=Release', '-DGGML_NATIVE=OFF', '-DGGML_LLAMAFILE=ON', '-DGGML_CUDA=OFF', '-DCRISPEMBED_BUILD_SHARED=ON']
        flags += kh.cache_and_link_flags()
        # CPU worker: CUDA flags intentionally unused; no GPU quota requested.
        jobs = kh.safe_build_jobs(gpu=False)
        jobs = str(os.cpu_count() or 2) if jobs == '$(nproc)' else jobs
        subprocess.run(['cmake', '-S', str(source), '-B', str(build), *flags], check=True)
        with kh.build_heartbeat('CrispSorter model evaluation runtime'):
            subprocess.run(['cmake', '--build', str(build), '--parallel', jobs, '--target', 'crispembed-server'], check=True)
        binary = build / 'crispembed-server'
        report['runtime_binary_sha256'] = sha(binary)
        write(output / 'report.json', report)
        baseline_result, baseline_rankings = baseline(corpus, lock)
        report['baseline'] = baseline_result
        write(output / 'report.json', report)
        report['moedern'] = {}
        for model in lock['models']:
            path = download(model['url'], scratch / 'models' / (model['id'] + '.gguf'), model['sha256'], model['bytes'])
            flag = '--systemone-model' if model['id'] == 'systemone' else '--moedern-model'
            with server(binary, flag, path, output / (model['id'] + '.log')) as (base, capabilities):
                if model['id'] == 'systemone':
                    if not capabilities['routes'].get('systemone'):
                        raise ValueError('Actual SystemOne route unavailable')
                    report['systemone'] = evaluate_systemone(base, corpus)
                    report['systemone']['model'] = model
                else:
                    if not capabilities['routes'].get('moedern'):
                        raise ValueError('Actual MoEdern route unavailable')
                    result = evaluate_moedern(base, corpus, baseline_rankings)
                    result.update(model=model, capabilities=capabilities)
                    result['baseline_delta'] = {key: result['mean'][key] - baseline_result['mean'][key] for key in result['mean']}
                    result['quality_gate'] = (result['baseline_delta']['recall_at_5'] >= corpus['acceptance']['minimum_recall_at_5_delta']
                                              and result['baseline_delta']['ndcg_at_10'] >= corpus['acceptance']['minimum_ndcg_at_10_delta'])
                    report['moedern'][model['id']] = result
            write(output / 'report.json', report)
        report['completed'] = True
        write(output / 'report.json', report)
    except Exception as error:
        report['error'] = {'type': type(error).__name__, 'message': str(error)}
        write(output / 'report.json', report)
        raise


if __name__ == '__main__':
    main()
