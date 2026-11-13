#!/usr/bin/env python3
"""Model-free checks for the one-job Paddle selector and evidence contracts."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace
from paddle_live_diagnostics import sanitize_output, OMITTED

import run_paddle_live_fixture as runner

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / '.github/workflows/ci.yml'


def conditions(expression, event='workflow_dispatch', scope='paddle-live', cloud_live=False):
    expression = expression.replace('github.event_name', 'event').replace('inputs.scope', 'scope').replace('inputs.cloud_live', 'cloud_live')
    expression = expression.replace('always()', 'True').replace('&&', ' and ').replace('||', ' or ')
    return bool(eval(expression, {'__builtins__': {}}, {'event': event, 'scope': scope, 'cloud_live': cloud_live}))


def jobs():
    value = WORKFLOW.read_text().split('\njobs:\n', 1)[1]
    matches = list(re.finditer(r'^  ([A-Za-z][\w-]*):\s*$', value, re.M))
    return {m[1]: value[m.end():matches[i+1].start() if i+1<len(matches) else len(value)] for i,m in enumerate(matches)}


def job_active(block, **context):
    match = re.search(r'^    if: (.+)$', block, re.M)
    return conditions(match[1], **context) if match else True


class WorkflowScope(unittest.TestCase):
    def test_paddle_dispatch_admits_exactly_one_linux_job_even_with_cloud_live(self):
        for cloud_live in [False, True]:
            active = [name for name, body in jobs().items() if job_active(body, cloud_live=cloud_live)]
            self.assertEqual(active, ['rust'])
        self.assertIn('runs-on: ubuntu-24.04', jobs()['rust'])
        self.assertIn('timeout-minutes: 120', jobs()['rust'])

    def test_ordinary_job_scope_behavior_is_preserved(self):
        expected = {'native-linux': {'rust'}, 'platforms': {'rust','windows','macos'},
                    'all': {'frontend','rust','windows','macos','cloud-backends','cloud-facade'}}
        for scope, names in expected.items():
            self.assertEqual({n for n,b in jobs().items() if job_active(b,scope=scope)}, names)
        for event in ['push','pull_request']:
            self.assertEqual({n for n,b in jobs().items() if job_active(b,event=event)}, expected['all'])

    def test_only_setup_and_exact_fixture_steps_run_in_linux_job(self):
        active=[]
        for step in re.split(r'\n      - ', jobs()['rust'])[1:]:
            name = re.search(r'^name: (.+)$',step,re.M)
            name = name[1] if name else re.search(r'^uses: (.+)$',step,re.M)[1]
            condition = re.search(r'^        if: (.+)$',step,re.M)
            if not condition or conditions(condition[1]):active.append(name)
        self.assertEqual(active,[
            'Check out CrispSorter','Check out sibling CrispEmbed (cargo path dep)',
            'Check out sibling CrispASR (cargo path dep)','Check out sibling crisp-docx (cargo path dep)',
            'Install system deps','dtolnay/rust-toolchain@stable','Download libcrispembed bundle',
            'Download libcrispasr bundle','Rust dependency cache','actions/setup-node@v4','Build the frontend',
            'Create bin placeholder for the resources glob','Run only the source-bound live Paddle fixture',
            'Retain scoped Paddle source receipt and bounded diagnostics'])
        rust=jobs()['rust']
        self.assertIn('npm ci\n          npm run build',rust)
        self.assertIn('mkdir -p src-tauri/bin && touch src-tauri/bin/.gitkeep',rust)
        self.assertIn('scripts/verify_crispembed_bundle.py',rust)

    def test_native_archives_stay_outside_checked_out_source(self):
        rust=jobs()['rust']
        for archive in ['crispembed-libs.tar.gz','crispasr-libs.tar.gz']:
            self.assertIn('ARCHIVE="$RUNNER_TEMP/'+archive+'"',rust)
            self.assertNotIn('-o '+archive,rust)
            self.assertNotIn('tar -xzf '+archive,rust)
        self.assertIn('python scripts/verify_crispembed_bundle.py "$ARCHIVE"',rust)
        self.assertEqual(rust.count('curl -fL --retry 8 --retry-all-errors -o "$ARCHIVE" "$URL"'),2)

    def test_original_paddle_command_and_feature_profile_remain_intact(self):
        step=jobs()['rust'].split('      - name: Live PaddleOCR recognition',1)[1].split('\n      - ',1)[0]
        self.assertIn('-- --ignored live_paddle_recognises_the_fixture_sentence',step)
        self.assertEqual(re.search(r'--features ([^\s]+)',step)[1],runner.FEATURES)
        self.assertEqual(runner.COMMAND[-5:],['--ignored','--exact',runner.TEST,'--nocapture','--test-threads=1'])
        self.assertIn('--locked',runner.COMMAND)
        self.assertIn('options: [native-linux, platforms, all, paddle-live, qwen-live]',WORKFLOW.read_text())

    def test_concurrency_blocks_unchanged(self):
        lines=WORKFLOW.read_text().splitlines(keepends=True);blocks=[]
        for i,line in enumerate(lines):
            if re.match(r'^\s*concurrency:',line):
                indent=len(line)-len(line.lstrip());end=i+1
                while end<len(lines) and (not lines[end].strip() or len(lines[end])-len(lines[end].lstrip())>indent):end+=1
                blocks.append(''.join(lines[i:end]))
        # Baseline includes the workflow block and every existing job block.
        self.assertEqual(len(blocks),8)
        self.assertEqual(hashlib.sha256(''.join(blocks).encode()).hexdigest(), CONCURRENCY_SHA256)


class SourceSnapshot(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name).resolve();self.project=self.root/'app';self.project.mkdir()
        self.environment={'GITHUB_EVENT_NAME':'workflow_dispatch','PADDLE_LIVE_SCOPE':'paddle-live'}
        for name, variable in [('CrispEmbed','CRISPEMBED_COMMIT'),('CrispASR','CRISPASR_REF'),('crisp-docx','CRISPDOCX_REF')]:
            sibling=self.root/name;sibling.mkdir();(sibling/'fixture').write_text(name)
            self.environment[variable]=self.commit(sibling)
        for name in runner.INPUTS:
            path=self.project/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text('source fixture '+name)
        lock=self.project/'scripts/crispembed-runtime-lock.json'
        lock.write_text(json.dumps({'source_commit':self.environment['CRISPEMBED_COMMIT']}))
        self.environment['GITHUB_SHA']=self.commit(self.project)
        self.context=patch.dict(os.environ,self.environment);self.context.start();self.addCleanup(self.context.stop)

    def commit(self,root):
        for args in [('init','-q'),('add','.'),('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture')]:
            subprocess.run(['git','-C',str(root),*args],check=True,capture_output=True)
        return subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip()

    def test_real_clean_source_and_sibling_layout_snapshot(self):
        value=runner.snapshot(self.project)
        self.assertEqual(value['source'],self.environment['GITHUB_SHA'])
        self.assertEqual(set(value['source_files_sha256']),set(runner.INPUTS))
        self.assertEqual(value['dependency_sources']['CrispEmbed'],self.environment['CRISPEMBED_COMMIT'])

    def test_changed_source_lock_rejected(self):
        (self.project/'Cargo.lock').write_text('changed source')
        with self.assertRaises(ValueError):runner.snapshot(self.project)

    def test_source_file_symlink_rejected(self):
        path=self.project/'src-tauri/src/extractors/paddle_resize.rs'
        path.unlink();path.symlink_to(self.project/'Cargo.lock')
        with self.assertRaises(ValueError):runner.snapshot(self.project)

    def test_setup_archive_in_source_is_rejected_without_new_exceptions(self):
        for name in ['crispembed-libs.tar.gz','crispasr-libs.tar.gz']:
            path=self.project/name;path.write_bytes(b'archive fixture')
            with self.assertRaises(ValueError):runner.snapshot(self.project)
            path.unlink()
        self.assertEqual(runner.snapshot(self.project)['source'],self.environment['GITHUB_SHA'])

    def test_wrong_sibling_pin_rejected(self):
        with patch.dict(os.environ,{'CRISPEMBED_COMMIT':self.environment['CRISPASR_REF']}):
            with self.assertRaises((ValueError,subprocess.CalledProcessError)):runner.snapshot(self.project)


class Evidence(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name);self.project=self.root/'app';self.project.mkdir()
        self.output=self.root/'evidence'
        self.identity={'source':'a'*40,'tree':'b'*40,'source_identity':{'application_source_clean':True},'source_files_sha256':{},'dependency_sources':{}}

    def execute(self, code=0, text='running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 20 filtered out\n'):
        with patch.object(runner,'snapshot',return_value=self.identity),patch.object(runner,'capture',return_value=(code,text.encode())):
            return runner.run(self.project,self.output)

    def test_success_means_exactly_one_test_and_source_recheck(self):
        receipt=self.execute();self.assertEqual(receipt['status'],'passed');self.assertEqual(receipt['executed_tests'],1)
        self.assertEqual(json.loads((self.output/'receipt.json').read_text())['status'],'passed')

    def test_zero_matching_tests_cannot_claim_pass(self):
        with self.assertRaises(ValueError):self.execute(text='running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;\n')
        self.assertEqual(json.loads((self.output/'receipt.json').read_text())['status'],'failed')

    def test_test_failure_retains_sanitized_diagnostics(self):
        with self.assertRaises(ValueError):self.execute(code=101,text=str(self.project)+'\npassword=SECRET-SENTINEL\nrunning 1 test\nFAILED\n')
        text=(self.output/'diagnostics.txt').read_text()
        self.assertNotIn('SECRET-SENTINEL',text);self.assertNotIn(str(self.project),text)
        self.assertEqual(json.loads((self.output/'receipt.json').read_text())['exit_code'],101)

    def test_source_change_during_test_cannot_claim_pass(self):
        with patch.object(runner,'snapshot',side_effect=[self.identity,{**self.identity,'source':'c'*40}]),patch.object(runner,'capture',return_value=(0,b'running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n')):
            with self.assertRaises(ValueError):runner.run(self.project,self.output)
        self.assertEqual(json.loads((self.output/'receipt.json').read_text())['status'],'failed')

    def test_partial_capture_failure_is_explicit_and_retained(self):
        with patch.object(runner,'snapshot',return_value=self.identity),patch.object(runner,'capture',side_effect=runner.CaptureFailure('output-limit',b'partial fixture output')):
            with self.assertRaises(runner.CaptureFailure):runner.run(self.project,self.output)
        value=json.loads((self.output/'receipt.json').read_text())
        self.assertEqual(value['status'],'failed');self.assertEqual(value['failure_code'],'output-limit')
        self.assertIn('partial fixture output',(self.output/'diagnostics.txt').read_text())

    def test_capture_output_bound_kills_command_and_preserves_partial_bytes(self):
        command=[sys.executable,'-c','print("X"*1000)']
        with patch.object(runner,'COMMAND',command),patch.object(runner,'MAX_OUTPUT',32):
            with self.assertRaises(runner.CaptureFailure) as error:runner.capture(self.project,os.environ.copy())
        self.assertEqual(error.exception.code,'output-limit');self.assertEqual(len(error.exception.output),32)

    def test_capture_deadline_is_bounded(self):
        command=[sys.executable,'-c','import time; print("started",flush=True); time.sleep(3)']
        with patch.object(runner,'COMMAND',command),patch.object(runner,'MAX_SECONDS',0.01):
            with self.assertRaises(runner.CaptureFailure) as error:runner.capture(self.project,os.environ.copy())
        self.assertEqual(error.exception.code,'timeout')

    def process_proxy(self):
        process=subprocess.Popen([sys.executable,'-c','print("retained partial output",flush=True)'],
            stdout=subprocess.PIPE,stderr=subprocess.STDOUT,start_new_session=True)
        self.addCleanup(lambda: process.wait(timeout=5))
        return process, SimpleNamespace(stdout=process.stdout,pid=process.pid,poll=process.poll,wait=process.wait)

    def test_wait_exception_after_output_preserves_buffer(self):
        process,proxy=self.process_proxy()
        def broken_wait(**kwargs):raise subprocess.TimeoutExpired('SENTINEL-VALUE-7816',10)
        proxy.wait=broken_wait
        with patch.object(runner.subprocess,'Popen',return_value=proxy):
            with self.assertRaises(runner.CaptureFailure) as error:runner.capture(self.project,os.environ.copy())
        self.assertEqual(error.exception.code,'capture-error')
        self.assertIn(b'retained partial output',error.exception.output)
        self.assertNotIn(b'SENTINEL-VALUE-7816',error.exception.output)

    def test_read_exception_after_output_preserves_buffer(self):
        process,proxy=self.process_proxy();original=os.read;calls=0
        def broken_read(fd,size):
            nonlocal calls
            calls+=1
            if calls>1:raise OSError('SENTINEL-VALUE-7816')
            return original(fd,size)
        with patch.object(runner.subprocess,'Popen',return_value=proxy),patch.object(runner.os,'read',side_effect=broken_read):
            with self.assertRaises(runner.CaptureFailure) as error:runner.capture(self.project,os.environ.copy())
        self.assertEqual(error.exception.code,'capture-error')
        self.assertIn(b'retained partial output',error.exception.output)
        self.assertNotIn(b'SENTINEL-VALUE-7816',error.exception.output)

    def test_diagnostic_withholding_is_explicit_and_never_changes_raw_test_result(self):
        value='running 1 test\n'+json.dumps({'token':'SENTINEL-VALUE-7816'})+'\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n'
        report=self.execute(text=value)
        self.assertEqual(report['status'],'passed');self.assertEqual(report['executed_tests'],1)
        self.assertEqual(report['diagnostics_status'],'withheld-sensitive-or-ambiguous')
        self.assertEqual((self.output/'diagnostics.txt').read_text(),OMITTED)
        self.assertNotIn('SENTINEL-VALUE-7816',(self.output/'receipt.json').read_text())

    def test_non_manual_or_different_source_fails_before_test(self):
        for event,scope,sha in [('push','paddle-live','a'*40),('workflow_dispatch','all','a'*40),('workflow_dispatch','paddle-live','c'*40)]:
            with patch.dict(os.environ,{'GITHUB_EVENT_NAME':event,'PADDLE_LIVE_SCOPE':scope,'GITHUB_SHA':sha}),patch.object(runner,'git',return_value='a'*40):
                with self.assertRaises(ValueError):runner.snapshot(self.project)


class DiagnosticPrivacy(unittest.TestCase):
    def assert_withheld(self,value):
        text,status=sanitize_output(value)
        self.assertNotIn('SENTINEL-VALUE-7816',text)
        self.assertEqual(text,OMITTED);self.assertEqual(status,'withheld-sensitive-or-ambiguous')

    def test_quoted_nested_and_escaped_json_credentials_are_withheld(self):
        for value in [
            '{"token":"SENTINEL-VALUE-7816"}',
            json.dumps({'nested':[{'password':'SENTINEL-VALUE-7816'}]}),
            json.dumps({r'access_\u0074oken':'SENTINEL-VALUE-7816'}),
            json.dumps(json.dumps({'api_key':'SENTINEL-VALUE-7816'})),
            json.dumps({'x'*300+'_token':'SENTINEL-VALUE-7816'}),
            '[info] {"password":"SENTINEL-VALUE-7816',
        ]:
            with self.subTest(value=value):self.assert_withheld(value)

    def test_header_pairs_objects_and_escaped_labels_are_withheld(self):
        for value in [
            {'headers':[['Authorization','SENTINEL-VALUE-7816']]},
            {'headers':[{'name':'Authorization','value':'SENTINEL-VALUE-7816'}]},
            {'headers':[{r'na\u006de':r'Author\u0069zation','values':['SENTINEL-VALUE-7816']}]},
            {'pairs':[['Cookie',{'nested':'SENTINEL-VALUE-7816'}]]},
        ]:
            with self.subTest(value=value):self.assert_withheld(json.dumps(value))

    def test_uri_userinfo_queries_fragments_and_encoded_forms_are_withheld(self):
        for uri in [
            'https://user:SENTINEL-VALUE-7816@example.test/path',
            'https://SENTINEL-VALUE-7816@example.test/path',
            'https://example.test/path?code=SENTINEL-VALUE-7816',
            'https://example.test/path#SENTINEL-VALUE-7816',
            'https://user%3ASENTINEL-VALUE-7816%40example.test/path',
            'https://[SENTINEL-VALUE-7816]/path',
        ]:
            for value in [uri,json.dumps({'message':uri}).replace('/',r'\/')]:
                with self.subTest(value=value):self.assert_withheld(value)

    def test_standalone_bearer_and_encoded_bearer_are_withheld(self):
        for value in ['Bearer SENTINEL-VALUE-7816','bearer\nSENTINEL-VALUE-7816',
                      'Bearer "SENTINEL-VALUE-7816"',json.dumps({'message':r'Bea\u0072er SENTINEL-VALUE-7816'})]:
            with self.subTest(value=value):self.assert_withheld(value)

    def test_private_key_body_and_multiline_plaintext_assignments_are_withheld(self):
        for value in ['-----BEGIN PRIVATE KEY-----\nSENTINEL-VALUE-7816\n-----END PRIVATE KEY-----',
                      'password=\nSENTINEL-VALUE-7816']:
            self.assert_withheld(value)

    def test_native_markers_vectors_and_progress_remain_readable(self):
        value='[info] Paddle loading\n[graph] tensor [1, 2, 3] [:5] [0..4] [0 1 2]\n[1/2] detector ready\n[2/2] recognizer ready\n'
        text,status=sanitize_output(value)
        self.assertEqual(status,'sanitized');self.assertEqual(text,value)
        text,status=sanitize_output('download https://example.test/model.onnx\n')
        self.assertEqual(status,'sanitized');self.assertIn('https://example.test/model.onnx',text)


CONCURRENCY_SHA256 = 'b0d5cf0477b3f97d6b50b13ac9354e637940ddd34d31781314c64b4b29b4040e'
if __name__=='__main__':unittest.main()
