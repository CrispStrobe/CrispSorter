#!/usr/bin/env python3
"""Model-free original desktop retry admission, restoration and upload tests."""
import copy
import json
import re
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import prepare_desktops_original_retry as retry

ROOT = Path(__file__).resolve().parents[1]

class DesktopRetryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.app = Path(self.temp.name) / 'app'; self.tools = Path(self.temp.name) / 'tools'
        self.app.mkdir(); self.tools.mkdir()
        self.pin_data = (ROOT / retry.PIN).read_bytes(); self.pin = json.loads(self.pin_data)
        self.data = {name: subprocess.check_output(['git', '-C', str(ROOT), 'show', self.pin['source'] + ':' + name])
                     for name in [*retry.INPUTS, '.github/workflows/release.yml']}
        for name, data in self.data.items():
            path = self.app / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(data)
        path = self.tools / retry.PIN; path.parent.mkdir(parents=True); path.write_bytes(self.pin_data)
        self.workflow = (ROOT / '.github/workflows/release.yml').read_bytes()
        self.env = {'GITHUB_EVENT_NAME': 'workflow_dispatch', 'CRISPSORTER_RELEASE_SELECTOR': 'linux_original_d204',
                    'CRISPSORTER_RELEASE_PUBLISH': 'false', 'CRISPSORTER_RELEASE_MAC_APPSTORE': 'false',
                    'CRISPSORTER_MOBILE_RELEASE_PROFILE': 'standard', 'GITHUB_WORKFLOW_SHA': 'a'*40, 'RUNNER_OS': 'Linux'}
        self.modified = []; self.tool_modified = ''; self.dep_changed = False
        for name in self.pin['dependency_sources']:
            (self.app / '_sibling' / name).mkdir(parents=True)
        self.patches = [patch.object(retry, 'TOOLING', self.tools), patch.object(retry, 'git', side_effect=self.git),
                        patch.object(retry, 'committed', side_effect=self.committed),
                        patch.object(retry, 'draft', return_value=('c'*64, {}))]
        for p in self.patches: p.start(); self.addCleanup(p.stop)
        (self.tools / retry.OCR).parent.mkdir(parents=True, exist_ok=True)
        (self.tools / retry.OCR).write_bytes(retry.overlay(self.data[retry.OCR]))

    def committed(self, root, name):
        if root == self.tools:
            if name == retry.PIN: return self.pin_data
            if name == '.github/workflows/release.yml': return self.workflow
            if name == retry.OCR: return retry.overlay(self.data[name])
        return self.data[name]

    def git(self, root, *args):
        if args[0] == 'diff': return self.tool_modified if root == self.tools else '\n'.join(self.modified) if root == self.app else ''
        if root == self.app: return self.pin['tree'] if args[-1] == 'HEAD^{tree}' else self.pin['source']
        if root == self.tools: return 'b'*40 if args[-1] == 'HEAD^{tree}' else 'a'*40
        return 'd'*40 if self.dep_changed else self.pin['dependency_sources'][root.name]

    def verify(self, **kwargs): return retry.verify(self.app, 'CrispStrobe/CrispSorter', self.env, **kwargs)

    def test_distinct_source_workflow_and_exact_lock(self):
        proof=self.verify(runtime=True)
        self.assertEqual(proof['app_source'], self.pin['source']); self.assertEqual(proof['workflow_source'], 'a'*40)
        self.assertEqual(proof['source_adjustments'], [])
        self.assertNotIn('release_id', self.pin); self.assertNotIn('failed_run', self.pin)

    def test_reject_modes(self):
        for key,value in [('GITHUB_EVENT_NAME','push'),('CRISPSORTER_RELEASE_SELECTOR','all'),
                          ('CRISPSORTER_RELEASE_PUBLISH','true'),('CRISPSORTER_RELEASE_MAC_APPSTORE','true'),
                          ('CRISPSORTER_MOBILE_RELEASE_PROFILE','ocr-draft'),('GITHUB_WORKFLOW_SHA','e'*40)]:
            with self.subTest(key=key), patch.dict(self.env,{key:value}), self.assertRaises(ValueError): self.verify()

    def test_every_pinned_input_rejects_changed_bytes(self):
        for name in retry.INPUTS:
            path=self.app/name; old=path.read_bytes(); path.write_bytes(old+b'\n')
            with self.subTest(name=name), self.assertRaises(ValueError): self.verify()
            path.write_bytes(old)

    def test_relocated_manifest_only(self):
        path=self.app/'src-tauri/Cargo.toml'; path.write_bytes(retry.relocated_manifest(self.data['src-tauri/Cargo.toml']))
        self.modified=['src-tauri/Cargo.toml']; self.verify()
        path.write_bytes(path.read_bytes()+b'\n')
        with self.assertRaises(ValueError): self.verify()

    def test_changed_source_tree_or_tooling_rejected(self):
        self.modified=['src-tauri/src/main.rs']
        with self.assertRaises(ValueError): self.verify()
        self.modified=[]; self.tool_modified='scripts/prepare_desktops_original_retry.py'
        with self.assertRaises(ValueError): self.verify()

    def test_recipe_change_rejected(self):
        self.workflow=self.workflow.replace(b'--features desktop,sidecars',b'--features changed,sidecars')
        with self.assertRaises(ValueError): self.verify()

    def test_runtime_dependency_and_os_rejected(self):
        self.dep_changed=True
        with self.assertRaises(ValueError): self.verify(runtime=True)
        self.dep_changed=False; self.env['RUNNER_OS']='Windows'
        with self.assertRaises(ValueError): self.verify(runtime=True)

    def test_overlay_is_only_two_attributes(self):
        original=self.data[retry.OCR]; changed=retry.overlay(original)
        self.assertEqual(changed.count(b'#[cfg(feature = "sidecars")]'), original.count(b'#[cfg(feature = "sidecars")]')+2)
        self.assertEqual(changed.replace(b'    #[cfg(feature = "sidecars")]\n',b''), original.replace(b'    #[cfg(feature = "sidecars")]\n',b''))
        with self.assertRaises(ValueError): retry.overlay(changed)

    def test_overlay_cannot_be_production_input(self):
        (self.app/retry.OCR).write_bytes(retry.overlay(self.data[retry.OCR])); self.modified=[retry.OCR]
        with self.assertRaises(ValueError): self.verify()
        self.verify(overlay_active=True)

    def run_tests(self, code=0, error=None):
        output=self.app/'proof.json'
        def execute(*args,**kwargs):
            self.assertEqual((self.app/retry.OCR).read_bytes(), retry.overlay(self.data[retry.OCR]))
            if error: raise error
            return subprocess.CompletedProcess(args[0],code)
        with patch.object(retry.subprocess,'run',side_effect=execute):
            retry.run_tests(self.app,'CrispStrobe/CrispSorter',self.env,output)
        return output

    def test_passing_tests_restore_before_check(self):
        output=self.run_tests(); self.assertEqual((self.app/retry.OCR).read_bytes(),self.data[retry.OCR])
        proof=json.loads(output.read_text()); self.assertTrue(proof['production_ocr_restored']); self.assertEqual(proof['tests_status'],'passed')
        retry.checked_proof(self.app,'CrispStrobe/CrispSorter',self.env,output)

    def test_failing_and_interrupted_tests_restore(self):
        for code,error in [(1,None),(0,RuntimeError('interrupted')),(0,KeyboardInterrupt())]:
            output=self.app/'proof.json'; output.unlink(missing_ok=True)
            with self.subTest(code=code,error=type(error)), self.assertRaises((ValueError,RuntimeError,KeyboardInterrupt)):
                self.run_tests(code,error)
            self.assertEqual((self.app/retry.OCR).read_bytes(),self.data[retry.OCR])
            self.assertTrue(json.loads(output.read_text())['production_ocr_restored'])
            with self.assertRaises(ValueError): retry.checked_proof(self.app,'CrispStrobe/CrispSorter',self.env,output)

    def test_changed_proof_or_restored_source_rejected(self):
        output=self.run_tests(); baseline=output.read_bytes()
        for key,value in [('workflow_source','e'*40),('app_source','e'*40),('draft_identity_sha256','f'*64),
                          ('production_ocr_restored',False),('test_exit_code',1),('test_command',[])]:
            data=json.loads(baseline); data[key]=value; output.write_text(json.dumps(data))
            with self.subTest(key=key), self.assertRaises(ValueError): retry.checked_proof(self.app,'CrispStrobe/CrispSorter',self.env,output)
        output.write_bytes(baseline); (self.app/retry.OCR).write_bytes(retry.overlay(self.data[retry.OCR]))
        with self.assertRaises(ValueError): retry.checked_proof(self.app,'CrispStrobe/CrispSorter',self.env,output)

    def test_all_selectors_get_original_recipe(self):
        for selector,platform in retry.SELECTORS.items():
            self.env['CRISPSORTER_RELEASE_SELECTOR']=selector
            proof=self.verify(); self.assertEqual(proof['platform'],platform)
            self.assertIn('--locked',retry.test_commands(proof))
            self.assertEqual('--target' in retry.test_commands(proof),platform=='macos-arm64')

    def test_cross_platform_names_disjoint(self):
        names=[retry.allowed_names(p,'0.15.2') for p in retry.SELECTORS.values()]
        for i in range(3):
            for j in range(i): self.assertFalse(names[i]&names[j])

    def setup_upload(self, platform='linux-x86_64'):
        selector=next(s for s,p in retry.SELECTORS.items() if p==platform)
        self.env['CRISPSORTER_RELEASE_SELECTOR']=selector
        proof=self.verify()
        proof['assets']={'mobile.ipa': {'id': 1, 'size': 7, 'digest': 'sha256:protected'}}
        names={'linux-x86_64':['CrispSorter_0.15.2_amd64.deb'],
               'windows-x86_64':['CrispSorter_0.15.2_x64-windows-portable-unsigned.zip'],
               'macos-arm64':['CrispSorter_0.15.2_aarch64-unsigned.dmg','CrispSorter_aarch64-unsigned.app.tar.gz']}[platform]
        artifacts=[]; paths=[]
        for name in names:
            if platform=='windows-x86_64': path=self.app/name
            else:
                folder='deb' if platform=='linux-x86_64' else 'dmg' if name.endswith('.dmg') else 'macos'
                path=self.app/'target/release/bundle'/folder/name
            path.parent.mkdir(parents=True,exist_ok=True); path.write_bytes(b'package')
            paths.append(path); artifacts.append({'filename':name,'bytes':7,'sha256':retry.sha(b'package')})
        receipt={'schema':'crispsorter-release-receipt-v1','app_source':proof['app_source'],'version':'0.15.2',
                 'platform':platform,'dependency_sources':proof['dependency_sources'],
                 'runtime_lock':json.loads(self.data['scripts/crispembed-runtime-lock.json']),'artifacts':artifacts}
        receipt_path=self.app/f'CrispSorter_0.15.2_{platform}.receipt.json'; receipt_path.write_text(json.dumps(receipt))
        checksum=self.app/f'CrispSorter_0.15.2_{platform}.sha256'
        checksum.write_text(''.join(a['sha256']+'  '+a['filename']+'\n' for a in artifacts))
        output=self.app/'proof.json'; output.write_text('{}')
        tests={'test_overlay':{},'tests_status':'passed','test_command':[], 'test_exit_code':0}
        return proof,tests,output,receipt_path,checksum,paths

    def test_upload_each_platform_only_exact_assets_and_verify_remote_digests(self):
        for platform in retry.SELECTORS.values():
            proof,tests,output,receipt_path,checksum,paths=self.setup_upload(platform)
            calls=[]
            def remote_verify(*args,**kwargs):
                value=copy.deepcopy(proof)
                if calls:
                    for path in [*paths,receipt_path,checksum]:
                        value['assets'][path.name]={'size':path.stat().st_size,'digest':'sha256:'+retry.sha(path.read_bytes())}
                return value
            def execute(command,**kwargs): calls.append(command)
            with patch.object(retry,'checked_proof',return_value=(proof,tests)), \
                    patch.object(retry,'verify',side_effect=remote_verify),patch.object(retry.subprocess,'run',side_effect=execute):
                retry.upload(self.app,'CrispStrobe/CrispSorter',self.env,output,receipt_path)
            self.assertEqual(len(calls),1); self.assertEqual(calls[0][:4],['gh','release','upload','v0.15.2'])
            self.assertEqual(set(calls[0][7:]),set(map(str,[*paths,receipt_path,checksum])))
            self.assertNotIn('delete-asset',calls[0]); self.assertNotIn('mobile.ipa',calls[0])

    def test_upload_rejects_foreign_receipt_names_hashes_and_checksums(self):
        for case in ['source','platform','dependency','name','bytes','hash','checksum','receipt-path']:
            proof,tests,output,receipt_path,checksum,paths=self.setup_upload()
            receipt=json.loads(receipt_path.read_text())
            if case=='source': receipt['app_source']='f'*40
            elif case=='platform': receipt['platform']='ios-arm64'
            elif case=='dependency': receipt['dependency_sources']={}
            elif case=='name': receipt['artifacts'][0]['filename']='mobile.ipa'
            elif case=='bytes': receipt['artifacts'][0]['bytes']=2
            elif case=='hash': receipt['artifacts'][0]['sha256']='0'*64
            elif case=='checksum': checksum.write_text('wrong')
            receipt_path.write_text(json.dumps(receipt))
            if case=='receipt-path': receipt_path=self.app/'other.receipt.json'
            with self.subTest(case=case), patch.object(retry,'checked_proof',return_value=(proof,tests)), \
                    patch.object(retry.subprocess,'run') as execute, self.assertRaises(ValueError):
                retry.upload(self.app,'CrispStrobe/CrispSorter',self.env,output,receipt_path)
            execute.assert_not_called()

    def test_protected_assets_or_draft_identity_changes_block_upload(self):
        for case in ['protected','identity']:
            proof,tests,output,receipt_path,checksum,paths=self.setup_upload()
            changed=copy.deepcopy(proof)
            if case=='protected': changed['assets']['mobile.ipa']['size']=8
            else: changed['draft_identity_sha256']='f'*64
            with self.subTest(case=case), patch.object(retry,'checked_proof',return_value=(proof,tests)), \
                    patch.object(retry,'verify',return_value=changed),patch.object(retry.subprocess,'run') as execute, self.assertRaises(ValueError):
                retry.upload(self.app,'CrispStrobe/CrispSorter',self.env,output,receipt_path)
            execute.assert_not_called()

    def test_remote_uploaded_digest_mismatch_fails(self):
        proof,tests,output,receipt_path,checksum,paths=self.setup_upload()
        with patch.object(retry,'checked_proof',return_value=(proof,tests)), \
                patch.object(retry,'verify',return_value=proof),patch.object(retry.subprocess,'run'),self.assertRaises(ValueError):
            retry.upload(self.app,'CrispStrobe/CrispSorter',self.env,output,receipt_path)

    def test_selected_matrix_exactly_matches_original_recipes(self):
        recipes,_=retry.recipes(self.data['.github/workflows/release.yml'].decode())
        workflow=self.workflow.decode()
        job=workflow.split('  release-desktop-original-d204:\n',1)[1].split('  # Reuse only',1)[0]
        matrices=re.findall(r"'(\[\{.*?\}\])'",job)
        self.assertEqual(len(matrices),3)
        for matrix in matrices:
            values=json.loads(matrix); self.assertEqual(len(values),1)
            self.assertEqual(values[0],recipes[values[0]['qualification_platform']])
        self.assertIn("github.event_name == 'workflow_dispatch' && !inputs.mac_appstore",job)

    def test_workflow_routes_retries_through_preflight_restore_and_scoped_upload(self):
        workflow=self.workflow.decode()
        preflight=workflow.split('  release-draft-preflight:',1)[1].split('  release-required:',1)[0]
        self.assertIn('prepare_desktops_original_retry.py',preflight)
        self.assertIn("inputs.platform != 'windows_original_b739' && !contains",preflight)
        steps=workflow.split('    steps: &desktop_release_steps',1)[1].split('  release-windows:',1)[0]
        self.assertLess(steps.index('--phase test'),steps.index('--phase check --output'))
        self.assertLess(steps.index('--phase check --output'),steps.index('uses: tauri-apps/tauri-action'))
        self.assertIn("if ('${{ contains(",steps)
        self.assertIn("' -eq 'true') { exit 0 }",steps)
        upload=steps.split('Re-upload patched assets',1)[1].split('shell: bash',1)[0]
        self.assertIn('!contains',upload)
        seal=steps.split('Verify final packages and attach',1)[1]
        self.assertLess(seal.index('--phase upload'),seal.index('gh release upload'))
        self.assertNotIn('delete-asset',(ROOT/'scripts/prepare_desktops_original_retry.py').read_text())

    def test_pin_hashes_match_original_source(self):
        for name,digest in self.pin['files'].items(): self.assertEqual(retry.sha(self.data[name]),digest)

if __name__=='__main__': unittest.main()
