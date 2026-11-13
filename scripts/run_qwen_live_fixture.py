#!/usr/bin/env python3
"""One explicitly consented internal non-commercial CPU Qwen preset smoke."""
import json
import math
import os
from pathlib import Path
import re
import sys
import tempfile
import shutil

import run_paddle_live_fixture as shared
from mobile_draft_safe_cli import SafeArgumentParser
from paddle_live_diagnostics import sanitize_output

SCOPE = 'qwen-live'
TEST = 'extractors::ocr_crispembed::tests::qwen_internal_noncommercial_smoke_live'
PROFILE = 'scripts/qwen-internal-smoke-profile.json'
COMMAND = [*shared.COMMAND]
COMMAND[COMMAND.index(shared.TEST)] = TEST


def authorize():
    if (os.environ.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'
            or os.environ.get('QWEN_LIVE_SCOPE') != SCOPE
            or os.environ.get('QWEN_NONCOMMERCIAL_EVALUATION') != 'true'):
        raise ValueError('Explicit scoped Qwen Research non-commercial evaluation required')


def keys(value, expected):
    if not isinstance(value, dict) or set(value) != set(expected):
        raise ValueError('Unexpected Qwen evaluation record fields')


def load_profile(project):
    value = json.loads((project / PROFILE).read_text())
    keys(value, ['schema','native_source','preset','model','fixture','expected_words','license','use_scope','satisfies_publication_gate'])
    keys(value['model'], ['registry_name','filename','bytes','sha256','companion_count','native_license'])
    keys(value['fixture'], ['filename','sha256'])
    if type(value['model']['bytes']) is not int or type(value['model']['companion_count']) is not int:
        raise ValueError('Qwen model byte/count fields require integers')
    if (value['schema'] != 'crispsorter-qwen-internal-smoke-profile-v1'
            or value['preset'] != 'multilingual' or value['license'] != 'Qwen Research License'
            or value['use_scope'] != 'Internal non-commercial evaluation only'
            or value['satisfies_publication_gate'] is not False
            or value['expected_words'] != ['cobalt','warranty','november','2031']
            or value['model'] != {'registry_name':'qwen2vl-3b','filename':'qwen2.5-vl-3b-q4_k.gguf',
                'bytes':2742343200,'sha256':'4cc16e226005a2d7ecce7749d5a876704a499ca301428167f5ceb3b75b967312',
                'companion_count':0,'native_license':'qwen-research'}
            or value['fixture'] != {'filename':'ocr-task.png','sha256':'25f0e5ee8d518403fc97a94435f88d525526649adbc08c2b937c3469ff4fb4ef'}):
        raise ValueError('Qwen smoke profile differs from its reviewed internal evaluation')
    return value


def validate_native(path, profile):
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= 2*1024**2:
        raise ValueError('Missing or oversized native Qwen smoke record')
    value = json.loads(path.read_text())
    keys(value, ['schema','fixture','preset','model','license','use_scope','satisfies_publication_gate',
                 'config','text','matched','elapsed_seconds'])
    if (value['schema'] != 'crispsorter-internal-qwen-smoke-v1'
            or any(value[name] != profile[name] for name in ['fixture','preset','model','license','use_scope'])
            or value['satisfies_publication_gate'] is not False
            or not isinstance(value['text'],str) or len(value['text']) > 131072
            or type(value['matched']) is not bool
            or type(value['elapsed_seconds']) not in (int,float) or not math.isfinite(value['elapsed_seconds'])
            or not 0 <= value['elapsed_seconds'] <= shared.MAX_SECONDS):
        raise ValueError('Native Qwen smoke identity or bounds differ')
    if type(value['model'].get('bytes')) is not int or type(value['model'].get('companion_count')) is not int:
        raise ValueError('Qwen native model byte/count fields require integers')
    config = value['config']
    if not isinstance(config,dict) or not isinstance(config.get('stages'),list) or len(config['stages']) != 1:
        raise ValueError('Qwen smoke requires one actual production preset stage')
    stage = config['stages'][0]
    if (not isinstance(stage,dict) or stage.get('engine') != 'qwen2vl'
            or stage.get('det_model') != 'qwen2vl-3b' or stage.get('vlm_max_tokens') != 4096
            or stage.get('vlm_prompt') != '' or config.get('enabled') is not True
            or config.get('cpu_pixel_budget') is not False or config.get('router') is not False
            or config.get('drop_headers_footers') is not True):
        raise ValueError('Qwen smoke changed the production multilingual preset')
    matched = all(word in value['text'].lower() for word in profile['expected_words'])
    if value['matched'] != matched:
        raise ValueError('Qwen fixture match declaration differs from actual text')
    text, status = sanitize_output(value['text'])
    # Minimize arbitrary configuration metadata; retain only checked preset facts.
    return {'schema':value['schema'], **{k:profile[k] for k in ['fixture','preset','model','license','use_scope']},
            'satisfies_publication_gate':False,'config':{'engine':'qwen2vl','model':'qwen2vl-3b','max_tokens':4096,
                'cpu_pixel_budget':False,'router':False,'drop_headers_footers':True},
            'matched':matched,'text':text,'text_retention_status':status,'elapsed_seconds':value['elapsed_seconds']}


def run(project, output):
    project, output = project.resolve(), output.absolute()
    shared.prepare_output(output)
    temporary = Path(tempfile.mkdtemp(prefix='crispsorter-qwen-'))
    raw = temporary / 'native.json'
    cache = temporary / 'model-cache'
    report = {'schema':'crispsorter-scoped-qwen-internal-evaluation-v1','status':'failed','test':TEST,
              'satisfies_publication_gate':False,'consent_status':'not established',
              'scope':'One production multilingual Qwen preset on a synthetic fixture; internal non-commercial evaluation only; not full preset, release or corpus qualification',
              'limits':{'seconds':shared.MAX_SECONDS,'output_bytes':shared.MAX_OUTPUT,'build_jobs':2,'test_threads':1}}
    started = False
    try:
        authorize()
        profile = load_profile(project)
        report.update(model=profile['model'],fixture=profile['fixture'],preset=profile['preset'],
                      license=profile['license'],use_scope=profile['use_scope'],consent_status='Explicit operator acceptance for this evaluation',
                      features=shared.FEATURES,default_features=True,cargo_locked=False)
        cache.mkdir()
        env = os.environ.copy()
        env.update(CARGO_BUILD_JOBS='2',OMP_NUM_THREADS='2',CUDA_VISIBLE_DEVICES='',QWEN2VL_OCR_FORCE_CPU='1',
            QWEN_INTERNAL_EVALUATION='true',CRISPSORTER_ACCEPT_MODEL_LICENSE='qwen2vl-3b',
            CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE='1',CRISPEMBED_CACHE_DIR=str(cache),CRISPSORTER_QWEN_SMOKE_RECEIPT=str(raw))
        for name in ['CRISPSORTER_OCR_PACKAGE_RECEIPT','CRISPSORTER_OCR_RECEIPT','CRISPEMBED_MAX_PIXELS']:
            env.pop(name,None)
        started = True
        try:
            code, data = shared.capture(project,env,COMMAND)
        except shared.CaptureFailure as error:
            shared.retain_diagnostics(output,report,error.output,project)
            raise
        shared.retain_diagnostics(output,report,data,project)
        report['exit_code'] = code
        # The Rust test owns model, preset and recognition assertions. An
        # auxiliary JSON report is useful diagnostics, never an admission gate.
        try:
            native = validate_native(raw, profile)
            retained = shared.write_diagnostic(output / 'native-smoke.json', json.dumps(native, indent=2) + '\n')
            report['native_diagnostics_status'] = 'retained' if retained else 'not-written'
        except Exception:
            report['native_diagnostics_status'] = 'unavailable-or-invalid'
        text = data.decode('utf-8',errors='replace')
        if code:
            raise shared.TestFailure(code)
        if (not re.search(r'(?m)^running 1 test\s*$',text)
                or not re.search(r'(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored;',text)):
            raise ValueError('Exactly one actual Qwen smoke must execute and pass')
        report.update(status='passed',executed_tests=1)
        return report
    except Exception as error:
        report['failure_code'] = error.code if isinstance(error,shared.CaptureFailure) else 'validation-failed'
        raise
    finally:
        try:
            if started and (raw.is_file() or raw.is_symlink()):
                raw.unlink()
        except OSError:
            # Raw diagnostics live outside the uploaded output directory.
            report['raw_cleanup_status'] = 'failed-outside-artifact'
        shutil.rmtree(temporary, ignore_errors=True)
        shared.write_diagnostic(output/'receipt.json', json.dumps(report,indent=2)+'\n')


def main():
    parser = SafeArgumentParser(description=__doc__)
    parser.add_argument('--preflight',action='store_true')
    parser.add_argument('--output',type=Path)
    args = parser.parse_args()
    try:
        if args.preflight:
            authorize();load_profile(Path.cwd())
            print('Explicit internal non-commercial Qwen evaluation selected; no model acceptance or loading performed by preflight')
        else:
            if args.output is None:raise ValueError('Explicit output required')
            run(Path.cwd(),args.output)
            print('Exactly one internal non-commercial Qwen smoke passed; publication gate remains unsatisfied')
    except shared.TestFailure as error:
        print(f'Qwen internal evaluation failed: Cargo exited {error.exit_code}', file=sys.stderr)
        return error.exit_code if 0 < error.exit_code < 256 else 1
    except Exception:
        print('Scoped Qwen evaluation failed; inspect its bounded evidence',file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':sys.exit(main())
