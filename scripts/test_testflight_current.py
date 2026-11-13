#!/usr/bin/env python3
"""Real IPA selection and idempotent upload controls; no credentials/network."""
import contextlib
import io
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile
from urllib.parse import parse_qs,urlsplit

import prepare_testflight_current as current
import check_testflight_upload as upload

VERSION='0.15.2'
IPA=f'CrispSorter_{VERSION}_ios-arm64.ipa'

class Selection(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name); self.project=self.root/'project'; (self.project/'src-tauri').mkdir(parents=True)
        (self.project/'src-tauri/tauri.conf.json').write_text(json.dumps({'version':VERSION}))
        self.assets=self.root/'assets';self.assets.mkdir();self.output=self.root/'selected IPA'
        self.release={'assets':[{'name':IPA}]};self.calls=[];self.write_ipa()

    def write_ipa(self,version=VERSION,bundle=upload.BUNDLE,profile=True,build='42'):
        with zipfile.ZipFile(self.assets/IPA,'w') as archive:
            archive.writestr('Payload/App.app/Info.plist',plistlib.dumps({'CFBundleIdentifier':bundle,
                'CFBundleShortVersionString':version,'CFBundleVersion':build,'CFBundleExecutable':'App'}))
            archive.writestr('Payload/App.app/App',b'fixture executable; not a signature assertion')
            if profile:archive.writestr('Payload/App.app/embedded.mobileprovision',b'fixture profile')

    def gh(self,*args):
        self.calls.append(args)
        if args[:2]==('release','view'): return json.dumps(self.release)
        self.assertEqual(args[:2],('release','download'));self.assertEqual(args[2],'v'+VERSION)
        self.assertEqual(args[args.index('--pattern')+1],IPA)
        shutil.copyfile(self.assets/IPA,Path(args[args.index('--dir')+1])/IPA)
        return ''

    def prepare(self,**kwargs):
        with patch.object(current,'gh',side_effect=self.gh):
            return current.prepare('Example/Project',self.project,self.output,**kwargs)

    def test_selects_actual_version_build_and_bundle_without_receipt_or_git(self):
        selected=self.prepare();self.assertEqual(selected['version'],VERSION);self.assertEqual(selected['build_number'],'42')
        self.assertEqual(Path(selected['ipa']).read_bytes(),(self.assets/IPA).read_bytes())
        self.assertNotIn('source',selected);self.assertNotIn('receipt',selected)
        self.assertEqual(len(self.calls),2)
        self.assertEqual(list(self.output.iterdir()),[self.output/IPA])

    def test_normal_retry_reuses_existing_output(self):
        self.prepare();self.write_ipa(build='43');self.assertEqual(self.prepare()['build_number'],'43')

    def test_source_receipt_and_draft_target_do_not_gate_selection(self):
        self.release.update(isDraft=False,targetCommitish='unrelated')
        self.prepare()

    def test_missing_or_duplicate_exact_ipa_never_selects_an_older_package(self):
        for assets in [[],[{'name':'CrispSorter_0.15.1_ios-arm64.ipa'}],[{'name':IPA},{'name':IPA}]]:
            self.release['assets']=assets
            with self.subTest(assets=assets),self.assertRaises(ValueError):self.prepare()
            self.assertFalse(self.output.exists())

    def test_actual_bundle_version_profile_and_build_are_required(self):
        for kwargs in [dict(version='0.15.1'),dict(bundle='wrong.app'),dict(profile=False),dict(build='wrong')]:
            self.write_ipa(**kwargs)
            with self.subTest(kwargs=kwargs),self.assertRaises((ValueError,KeyError)):self.prepare()
            self.assertFalse(self.output.exists())

    def test_flags_reject_before_github_access(self):
        for kwargs in [dict(publish=True),dict(mac_appstore=True),dict(mode='bad')]:
            with self.subTest(kwargs=kwargs),self.assertRaises(ValueError):self.prepare(**kwargs)
        self.assertEqual(self.calls,[])

    def test_resume_needs_no_original_cutoff_and_clears_old_environment(self):
        result=self.prepare(mode='resume');self.assertEqual(result['uploaded_after'],'')
        env=self.root/'env';current.write_environment(result,env)
        self.assertIn('UPLOADED_AFTER=\n',env.read_text());self.assertNotIn('BUILD_NUMBER=',env.read_text())

    def test_optional_resume_filter_is_validated(self):
        self.assertEqual(self.prepare(mode='resume',uploaded_after='2026-11-12T12:00:00Z')['uploaded_after'],'2026-11-12T12:00:00Z')
        for mode,value in [('upload','2026-11-12T12:00:00Z'),('resume','invalid')]:
            with self.subTest(mode=mode),self.assertRaises(ValueError):self.prepare(mode=mode,uploaded_after=value)

    def test_environment_newline_injection_rejected(self):
        value=self.prepare();value['ipa']='bad\nKEY=value'
        with self.assertRaises(ValueError):current.write_environment(value,self.root/'env')

    def test_existing_upload_or_resume_reaches_exact_assignment_with_empty_cutoff(self):
        import testflight_distribute as distribution
        workflow=(Path(__file__).resolve().parents[1]/'.github/workflows/release.yml').read_text()
        job=workflow.split('  testflight-existing:',1)[1].split('  release-draft-preflight:',1)[0]
        command=next(line.strip() for line in job.splitlines() if 'python scripts/testflight_distribute.py --platform IOS' in line)
        for mode in ['upload','resume']:
            selected=self.prepare(mode=mode)
            env_file=self.root/'github-env';env_file.write_text('')
            current.write_environment(selected,env_file)
            environment=dict(line.split('=',1) for line in env_file.read_text().splitlines())
            environment.update(UPLOADED_AFTER=environment.get('UPLOADED_AFTER',''),BUILD_NUMBER=selected['build_number'])
            if mode=='upload':
                environment.update(ASC_KEY_ID='fixture',ASC_ISSUER_ID='fixture',ASC_KEY_P8_BASE64='Zml4dHVyZQ==',
                    ASC_APP_ID='123',GITHUB_ENV=str(env_file))
                with patch.dict(upload.os.environ,environment),patch.object(upload,'make_token',return_value='fixture'), \
                        patch.object(upload,'needs_upload',return_value=False),contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(upload.main(),0)
                environment.update(dict(line.split('=',1) for line in env_file.read_text().splitlines()))
                self.assertEqual(environment['TESTFLIGHT_UPLOAD_REQUIRED'],'false')
            self.assertEqual(environment['UPLOADED_AFTER'],'')
            # Expand the workflow's real assignment command through a shell;
            # replace only its executable with an argv printer, never call Apple.
            printer='"'+sys.executable+'" -c \'import sys,json;print(json.dumps(sys.argv[1:]))\''
            probe=command.replace('python scripts/testflight_distribute.py',printer,1)
            args=json.loads(subprocess.check_output(['bash','-c',probe],env={**upload.os.environ,**environment},text=True))
            kwargs={flag:args[args.index(flag)+1] for flag in ['--version','--marketing-version','--uploaded-after','--mode']}
            self.assertEqual(kwargs,{'--version':'42','--marketing-version':VERSION,'--uploaded-after':'','--mode':'internal'})
            payload={'data':[{'id':'exact','attributes':{'version':'42','processingState':'VALID'},
                'relationships':{'preReleaseVersion':{'data':{'id':'pv'}}}}],
                'included':[{'type':'preReleaseVersions','id':'pv','attributes':{'version':VERSION,'platform':'IOS'}}]}
            asc=type('ASC',(),{'get':lambda _,path:(payload,200)})()
            with contextlib.redirect_stdout(io.StringIO()):
                build=distribution.latest_build(asc,'123','IOS',kwargs['--version'],0,
                    kwargs['--uploaded-after'],kwargs['--marketing-version'])
            self.assertEqual(build['id'],'exact')


class DuplicateUpload(unittest.TestCase):
    def setUp(self):
        self.pages=[];self.calls=[];self.bundle=upload.BUNDLE
        self.asc=type('ASC',(),{'get':lambda _,path:self.get(path)})()

    def get(self,path):
        self.calls.append(path)
        if path.startswith('/apps/'):
            return {'data':{'attributes':{'bundleId':self.bundle}}},200
        return self.pages.pop(0)

    def payload(self,version=VERSION,build='42',platform='IOS',state='PROCESSING'):
        return {'data':[{'id':'fixture','attributes':{'version':build,'processingState':state},
            'relationships':{'preReleaseVersion':{'data':{'id':'pv'}}}}],
            'included':[{'id':'pv','type':'preReleaseVersions','attributes':{'version':version,'platform':platform}}]}

    def required(self):return upload.needs_upload(self.asc,'123',VERSION,'42')

    def test_any_existing_exact_build_skips_reupload(self):
        for state in ['PROCESSING','VALID','INVALID']:
            self.pages=[(self.payload(state=state),200)]
            self.assertFalse(self.required())
        query=parse_qs(urlsplit(self.calls[-1]).query)
        self.assertEqual(query['filter[version]'],['42']);self.assertEqual(query['filter[app]'],['123'])
        self.assertEqual(query['filter[preReleaseVersion.platform]'],['IOS'])

    def test_absent_build_allows_upload_and_other_marketing_version_does_not_match(self):
        for payload in [{'data':[]},self.payload(version='0.15.1')]:
            self.pages=[(payload,200)];self.assertTrue(self.required())

    def test_failed_lookup_never_means_absent(self):
        for payload,status in [({},401),({},429),({},500),({},200),({'data':[],'errors':[{}]},200)]:
            self.pages=[(payload,status)]
            with self.subTest(status=status),self.assertRaises(ValueError):self.required()
        self.pages=[(self.payload(),200)];self.bundle='another.app'
        with self.assertRaises(ValueError):self.required()

    def test_missing_marketing_identity_is_not_an_absent_build(self):
        payload=self.payload();payload.pop('included');self.pages=[(payload,200)]
        with self.assertRaises(ValueError):self.required()

    def test_all_pages_checked_before_upload(self):
        next_page='https://api.appstoreconnect.apple.com/v1/builds?cursor=next'
        self.pages=[({'data':[],'links':{'next':next_page}},200),(self.payload(),200)]
        self.assertFalse(self.required());self.assertEqual(self.calls[-1],next_page)

    def test_redirected_or_repeated_pagination_refused(self):
        for link in ['https://wrong.invalid/v1/builds','http://api.appstoreconnect.apple.com/v1/builds',
                     'https://api.appstoreconnect.apple.com/v1/apps','https://api.appstoreconnect.apple.com/v1/builds#bad']:
            self.pages=[({'data':[],'links':{'next':link}},200)]
            with self.subTest(link=link),self.assertRaises(ValueError):self.required()
        link='https://api.appstoreconnect.apple.com/v1/builds?cursor=again'
        self.pages=[({'data':[],'links':{'next':link}},200)]*2
        with self.assertRaises(ValueError):self.required()

    def test_resume_checks_actual_app_and_preserves_optional_cutoff(self):
        for cutoff in ['', '2026-11-12T12:00:00Z']:
            with tempfile.TemporaryDirectory() as temp:
                path=Path(temp)/'env';path.write_text('UPLOADED_AFTER='+cutoff+'\n')
                env={'ASC_KEY_ID':'fixture','ASC_ISSUER_ID':'fixture','ASC_KEY_P8_BASE64':'Zml4dHVyZQ==',
                    'ASC_APP_ID':'123','IPA_VERSION':VERSION,'BUILD_NUMBER':'42','GITHUB_ENV':str(path),
                    'TESTFLIGHT_SELECTION_MODE':'resume','UPLOADED_AFTER':cutoff}
                self.calls=[]
                with patch.dict(upload.os.environ,env),patch.object(upload,'make_token',return_value='fixture'), \
                        patch.object(upload,'Asc',return_value=self.asc),contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(upload.main(),0)
                values=dict(line.split('=',1) for line in path.read_text().splitlines())
                self.assertEqual(values['TESTFLIGHT_UPLOAD_REQUIRED'],'false')
                self.assertEqual(values['UPLOADED_AFTER'],cutoff)
                self.assertEqual(len(self.calls),1);self.assertTrue(self.calls[0].startswith('/apps/123?'))

    def test_wrong_app_or_failed_app_lookup_blocks_resume_without_environment_changes(self):
        for mode in ['upload','resume']:
            for response in [({'data':{'attributes':{'bundleId':'wrong.app'}}},200),({},401),({},500)]:
                with tempfile.TemporaryDirectory() as temp:
                    path=Path(temp)/'env';path.write_text('KEEP=1\n')
                    env={'ASC_KEY_ID':'fixture','ASC_ISSUER_ID':'fixture','ASC_KEY_P8_BASE64':'Zml4dHVyZQ==',
                        'ASC_APP_ID':'123','IPA_VERSION':VERSION,'BUILD_NUMBER':'42','GITHUB_ENV':str(path),
                        'TESTFLIGHT_SELECTION_MODE':mode}
                    asc=type('ASC',(),{'get':lambda _,p:response})()
                    with patch.dict(upload.os.environ,env),patch.object(upload,'make_token',return_value='fixture'), \
                            patch.object(upload,'Asc',return_value=asc),contextlib.redirect_stderr(io.StringIO()):
                        self.assertEqual(upload.main(),1)
                    self.assertEqual(path.read_text(),'KEEP=1\n')

    def test_cli_lookup_failure_cannot_enable_upload(self):
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/'env';path.write_text('KEEP=1\n')
            env={'ASC_KEY_ID':'fixture','ASC_ISSUER_ID':'fixture','ASC_KEY_P8_BASE64':'Zml4dHVyZQ==',
                'ASC_APP_ID':'123','IPA_VERSION':VERSION,'BUILD_NUMBER':'42','GITHUB_ENV':str(path)}
            with patch.dict(upload.os.environ,env),patch.object(upload,'make_token',return_value='fixture'), \
                    patch.object(upload,'needs_upload',side_effect=ValueError('API failed')),contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(upload.main(),1)
            self.assertEqual(path.read_text(),'KEEP=1\n')

class Workflow(unittest.TestCase):
    def test_signature_and_server_lookup_precede_conditional_upload(self):
        workflow=(Path(__file__).resolve().parents[1]/'.github/workflows/release.yml').read_text()
        job=workflow.split('  testflight-existing:',1)[1].split('  release-draft-preflight:',1)[0]
        self.assertLess(job.index('codesign --verify --deep --strict'),job.index('check_testflight_upload.py'))
        self.assertLess(job.index('check_testflight_upload.py'),job.index('xcrun altool --upload-package'))
        lookup=job.split('      - name: Check App Store Connect app and whether upload is needed',1)[1].split('      - name:',1)[0]
        self.assertNotIn('        if:',lookup)
        self.assertIn("inputs.platform == 'testflight_resume' || inputs.platform == 'testflight_current_resume'",lookup)
        upload_step=job.split('      - name: Upload to App Store Connect',1)[1].split('      - name:',1)[0]
        self.assertIn("env.TESTFLIGHT_UPLOAD_REQUIRED == 'true'",upload_step)
        self.assertIn("inputs.platform == 'testflight_current'",upload_step)
        self.assertNotIn('testflight_current_resume',upload_step)
        self.assertNotIn('--source ',job);self.assertIn('--marketing-version "$IPA_VERSION"',job)
        self.assertIn('--version "$BUILD_NUMBER"',job);self.assertIn('--mode internal',job)

if __name__=='__main__':unittest.main()
