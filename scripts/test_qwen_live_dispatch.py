#!/usr/bin/env python3
"""Model-free Qwen evaluation consent, identity, privacy and selector controls."""
import copy
import contextlib
import io
import sys
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import run_qwen_live_fixture as qwen
import test_paddle_live_dispatch as selectors

ROOT = Path(__file__).resolve().parents[1]


class QwenScope(unittest.TestCase):
    def test_only_existing_linux_job_and_qwen_steps_run(self):
        for cloud_live in [False,True]:
            active=[name for name,body in selectors.jobs().items() if selectors.job_active(body,scope='qwen-live',cloud_live=cloud_live)]
            self.assertEqual(active,['rust'])
        names=[]
        rust=selectors.jobs()['rust']
        for step in re.split(r'\n      - ',rust)[1:]:
            name=re.search(r'^name: (.+)$',step,re.M)
            condition=re.search(r'^        if: (.+)$',step,re.M)
            if not condition or selectors.conditions(condition[1],scope='qwen-live',cloud_live=True):
                names.append(name[1] if name else re.search(r'^uses: (.+)$',step,re.M)[1])
        self.assertEqual(names,[
            'Check out CrispSorter','Require explicit internal non-commercial Qwen evaluation',
            'Check out sibling CrispEmbed (cargo path dep)','Check out sibling CrispASR (cargo path dep)',
            'Check out sibling crisp-docx (cargo path dep)','Install system deps','dtolnay/rust-toolchain@stable',
            'Download libcrispembed bundle','Download libcrispasr bundle','Rust dependency cache',
            'actions/setup-node@v4','Build the frontend','Create bin placeholder for the resources glob',
            'Run only the consented internal Qwen smoke','Retain internal Qwen evaluation evidence only'])
        self.assertIn('qwen_noncommercial_evaluation:',selectors.WORKFLOW.read_text())
        self.assertIn('type: boolean\n        default: false',selectors.WORKFLOW.read_text().split('qwen_noncommercial_evaluation:',1)[1])
        self.assertLess(rust.index('Require explicit internal non-commercial Qwen evaluation'),rust.index('Install system deps'))

    def test_exact_one_smoke_not_full_preset_qualification(self):
        self.assertNotIn('--locked',qwen.COMMAND)
        self.assertEqual(qwen.COMMAND[-5:],['--ignored','--exact',qwen.TEST,'--nocapture','--test-threads=1'])
        self.assertNotIn('ocr_task_presets_live',qwen.COMMAND)
        source=(ROOT/'src-tauri/src/extractors/ocr_crispembed.rs').read_text()
        old=source.split('    fn ocr_task_presets_live() {',1)[1].split('    /// Live E2E for the per-stage builder',1)[0]
        self.assertEqual(hashlib.sha256(old.encode()).hexdigest(), OLD_QUALIFICATION_SHA256)
        self.assertNotIn('CRISPSORTER_ACCEPT_MODEL_LICENSE',selectors.jobs()['rust'].split('Run only the consented internal Qwen smoke',1)[0])

    def test_consent_is_specific_manual_and_noncommercial(self):
        valid={'GITHUB_EVENT_NAME':'workflow_dispatch','QWEN_LIVE_SCOPE':'qwen-live','QWEN_NONCOMMERCIAL_EVALUATION':'true'}
        with patch.dict(os.environ,valid):qwen.authorize()
        for variable,value in [('GITHUB_EVENT_NAME','push'),('QWEN_LIVE_SCOPE','all'),('QWEN_NONCOMMERCIAL_EVALUATION','false'),('QWEN_NONCOMMERCIAL_EVALUATION','1')]:
            with patch.dict(os.environ,{**valid,variable:value}):
                with self.assertRaises(ValueError):qwen.authorize()


class QwenEvidence(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name).resolve();self.project=self.root/'app';self.project.mkdir()
        (self.project/'scripts').mkdir()
        (self.project/qwen.PROFILE).write_bytes((ROOT/qwen.PROFILE).read_bytes())
        self.profile=qwen.load_profile(self.project)
        self.output=self.root/'evidence'
        self.environment={'GITHUB_EVENT_NAME':'workflow_dispatch','QWEN_LIVE_SCOPE':'qwen-live',
            'QWEN_NONCOMMERCIAL_EVALUATION':'true','CRISPSORTER_ACCEPT_MODEL_LICENSE':'all',
            'CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE':'0','CRISPEMBED_MAX_PIXELS':'1'}
        self.context=patch.dict(os.environ,self.environment);self.context.start();self.addCleanup(self.context.stop)
        self.raw={ 'schema':'crispsorter-internal-qwen-smoke-v1',
            **{name:self.profile[name] for name in ['fixture','preset','model','license','use_scope']},
            'satisfies_publication_gate':False,'text':'Cobalt warranty November 2031','matched':True,'elapsed_seconds':1.5,
            'config':{'enabled':True,'cpu_pixel_budget':False,'router':False,'drop_headers_footers':True,
                'stages':[{'engine':'qwen2vl','det_model':'qwen2vl-3b','vlm_max_tokens':4096,'vlm_prompt':''}]}}
        self.child=None

    def capture(self,project,env,command):
        self.child=env
        Path(env['CRISPSORTER_QWEN_SMOKE_RECEIPT']).write_text(json.dumps(self.raw))
        return 0,b'running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 50 filtered out\n'

    def execute(self,capture=None):
        with patch.object(qwen.shared,'capture',side_effect=capture or self.capture):
            return qwen.run(self.project,self.output)

    def test_success_uses_exact_model_child_only_acceptance_and_no_publication(self):
        value=self.execute()
        self.assertEqual(value['status'],'passed');self.assertFalse(value['satisfies_publication_gate'])
        self.assertEqual(value['license'],'Qwen Research License')
        self.assertEqual(self.child['CRISPSORTER_ACCEPT_MODEL_LICENSE'],'qwen2vl-3b')
        self.assertEqual(self.child['CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE'],'1')
        self.assertEqual(self.child['QWEN2VL_OCR_FORCE_CPU'],'1');self.assertEqual(self.child['CUDA_VISIBLE_DEVICES'],'')
        self.assertNotIn('CRISPEMBED_MAX_PIXELS',self.child)
        self.assertEqual(os.environ['CRISPSORTER_ACCEPT_MODEL_LICENSE'],'all')
        self.assertEqual(os.environ['CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE'],'0')
        self.assertFalse(Path(self.child['CRISPSORTER_QWEN_SMOKE_RECEIPT']).exists())
        self.assertFalse(Path(self.child['CRISPEMBED_CACHE_DIR']).is_relative_to(self.output))
        self.assertEqual({p.name for p in self.output.iterdir()},{'receipt.json','diagnostics.txt','native-smoke.json'})

    def test_same_output_can_be_reused_without_stale_native_report(self):
        self.assertEqual(self.execute()['status'],'passed')
        value=self.execute(lambda *args:(0,b'running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n'))
        self.assertEqual(value['status'],'passed')
        self.assertFalse((self.output/'native-smoke.json').exists())

    def test_report_write_failure_does_not_override_test_result(self):
        def passing(*args):return 0,b'running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n'
        with patch.object(Path,'write_text',side_effect=OSError('private write error')):
            self.assertEqual(self.execute(passing)['status'],'passed')
            with self.assertRaises(qwen.shared.TestFailure):self.execute(lambda *args:(101,b'test failed'))

    def test_cli_preserves_real_cargo_failure_exit(self):
        with patch.object(sys,'argv',['runner','--output',str(self.output)]), \
                patch.object(qwen,'run',side_effect=qwen.shared.TestFailure(101)):
            self.assertEqual(qwen.main(),101)

    def test_native_test_keeps_real_assertions_without_git_or_report_gate(self):
        body=(ROOT/'src-tauri/src/extractors/ocr_crispembed.rs').read_text().split('fn qwen_internal_noncommercial_smoke_live() {',1)[1].split('    /// Actual released-model',1)[0]
        self.assertNotIn('Command::new("git")',body)
        self.assertNotIn('expect("explicit smoke output required")',body)
        for assertion in ['assert_eq!(bytes,','assert_eq!(model_sha256,','assert_eq!(fixture_sha256,','assert!(matched,']:
            self.assertIn(assertion,body)

    def test_missing_authorization_never_starts_child(self):
        with patch.dict(os.environ,{'QWEN_NONCOMMERCIAL_EVALUATION':'false'}),patch.object(qwen.shared,'capture') as child:
            with self.assertRaises(ValueError):qwen.run(self.project,self.output)
        child.assert_not_called()
        value=json.loads((self.output/'receipt.json').read_text());self.assertEqual(value['status'],'failed')
        self.assertEqual(value['consent_status'],'not established')

    def test_invalid_auxiliary_report_does_not_override_passing_rust_test(self):
        self.raw['fixture']={**self.raw['fixture'],'sha256':'c'*64}
        value=self.execute()
        self.assertEqual(value['status'],'passed')
        self.assertEqual(value['native_diagnostics_status'],'unavailable-or-invalid')
        self.assertFalse((self.output/'native-smoke.json').exists())

    def test_missing_auxiliary_report_is_not_a_test_prerequisite(self):
        value=self.execute(lambda *args: (0,b'running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n'))
        self.assertEqual(value['status'],'passed')
        self.assertEqual(value['native_diagnostics_status'],'unavailable-or-invalid')

    def test_zero_matching_tests_cannot_qualify(self):
        def zero(*args):
            self.capture(*args)
            return 0,b'running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n'
        with self.assertRaises(ValueError):self.execute(zero)
        self.assertEqual(json.loads((self.output/'receipt.json').read_text())['status'],'failed')

    def test_normal_retry_needs_no_source_admission(self):
        (self.project/'Cargo.lock').write_text('ordinary local edit')
        self.assertEqual(self.execute()['status'],'passed')

    def test_wrong_model_hash_or_companion_profile_is_refused(self):
        value=copy.deepcopy(self.profile);value['model']['companion_count']=1
        (self.project/qwen.PROFILE).write_text(json.dumps(value))
        with patch.object(qwen.shared,'capture') as child:
            with self.assertRaises(ValueError):qwen.run(self.project,self.output)
        child.assert_not_called()

    def test_failing_recognition_keeps_failed_scope_not_qualification(self):
        self.raw['text']='unrecognized';self.raw['matched']=False
        def failed(*args):
            self.capture(*args)
            return 101,b'running 1 test\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;\n'
        with self.assertRaises(qwen.shared.TestFailure):self.execute(failed)
        value=json.loads((self.output/'receipt.json').read_text());self.assertEqual(value['status'],'failed')
        native=json.loads((self.output/'native-smoke.json').read_text());self.assertFalse(native['matched'])
        self.assertFalse(native['satisfies_publication_gate'])

    def test_native_extra_secret_field_stays_outside_artifact(self):
        self.raw['unexpected_secret']='SENTINEL-VALUE-7816'
        self.assertEqual(self.execute()['status'],'passed')
        for path in self.output.iterdir():self.assertNotIn('SENTINEL-VALUE-7816',path.read_text())
        self.assertFalse((self.root/'evidence-native-raw.json').exists())

    def test_arbitrary_config_metadata_is_minimized(self):
        self.raw['config']['unexpected_secret']='SENTINEL-VALUE-7816'
        self.execute()
        self.assertNotIn('SENTINEL-VALUE-7816',(self.output/'native-smoke.json').read_text())

    def test_capture_failure_retains_safe_partial_evidence(self):
        def failure(*args):raise qwen.shared.CaptureFailure('capture-error',b'{"token":"SENTINEL-VALUE-7816"}')
        with self.assertRaises(qwen.shared.CaptureFailure):self.execute(failure)
        value=json.loads((self.output/'receipt.json').read_text());self.assertEqual(value['status'],'failed')
        self.assertEqual(value['failure_code'],'capture-error')
        self.assertEqual(value['diagnostics_status'],'withheld-sensitive-or-ambiguous')
        self.assertNotIn('SENTINEL-VALUE-7816',(self.output/'diagnostics.txt').read_text())

    def test_raw_cleanup_error_stays_outside_artifact_without_overriding_test(self):
        original=Path.unlink
        def unlink(path,*args,**kwargs):
            if path.name=='native.json':raise OSError('SENTINEL-VALUE-7816')
            return original(path,*args,**kwargs)
        with patch.object(Path,'unlink',unlink):
            self.assertEqual(self.execute()['status'],'passed')
        value=json.loads((self.output/'receipt.json').read_text())
        self.assertEqual(value['status'],'passed')
        self.assertEqual(value['raw_cleanup_status'],'failed-outside-artifact')
        self.assertNotIn('SENTINEL-VALUE-7816',(self.output/'receipt.json').read_text())
        self.assertTrue((self.output/'native-smoke.json').is_file())
        if self.child:
            Path(self.child['CRISPSORTER_QWEN_SMOKE_RECEIPT']).unlink(missing_ok=True)

    def test_cleanup_error_preserves_primary_validation_failure_code(self):
        self.raw['unexpected_secret']='SENTINEL-VALUE-7816'
        original=Path.unlink
        def unlink(path,*args,**kwargs):
            if path.name=='native.json':raise OSError('private cleanup error')
            return original(path,*args,**kwargs)
        def failed(*args):
            self.capture(*args)
            return 101,b'running 1 test\ntest result: FAILED.\n'
        with patch.object(Path,'unlink',unlink):
            with self.assertRaises(qwen.shared.TestFailure):self.execute(failed)
        value=json.loads((self.output/'receipt.json').read_text())
        self.assertEqual(value['failure_code'],'validation-failed')
        self.assertEqual(value['raw_cleanup_status'],'failed-outside-artifact')
        self.assertEqual(value['status'],'failed')
        self.assertNotIn('SENTINEL-VALUE-7816',(self.output/'receipt.json').read_text())
        if self.child:
            Path(self.child['CRISPSORTER_QWEN_SMOKE_RECEIPT']).unlink(missing_ok=True)

    def test_cli_never_prints_success_when_cleanup_failure_propagates(self):
        stdout,stderr=io.StringIO(),io.StringIO()
        with patch.object(sys,'argv',['run_qwen_live_fixture.py','--output',str(self.output)]), \
                patch.object(qwen,'run',side_effect=ValueError('SENTINEL-VALUE-7816')), \
                contextlib.redirect_stdout(stdout),contextlib.redirect_stderr(stderr):
            code=qwen.main()
        self.assertEqual(code,1);self.assertEqual(stdout.getvalue(),'')
        self.assertNotIn('SENTINEL-VALUE-7816',stderr.getvalue())
        self.assertIn('failed',stderr.getvalue())

    def test_reduced_tokens_or_pixel_budget_cannot_claim_actual_preset(self):
        self.raw['config']['stages'][0]['vlm_max_tokens']=128
        value=self.execute()
        self.assertEqual(value['native_diagnostics_status'],'unavailable-or-invalid')
        self.assertEqual(value['status'],'passed')


OLD_QUALIFICATION_SHA256='c3b68c0c3b9fe9d23e2320fc25674d57f94977191567b2fbb245fd0e48ac4056'
if __name__=='__main__':unittest.main()
