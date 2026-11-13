"""Retrospective observations never replace original release qualification."""
import copy
import contextlib
import hashlib
import io
import json
from pathlib import Path
import tempfile
import sys
import unittest
from unittest.mock import patch

import record_existing_linux_package as recorder
import release_ocr_qualification as qualification


class ExistingPackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'scripts').mkdir()
        (self.root / 'scripts/record_existing_linux_package.py').write_bytes(b'committed recorder fixture')
        self.package = self.root / recorder.PACKAGE
        self.package.write_bytes(b'nonexecutable DEB fixture')
        self.hash = hashlib.sha256(self.package.read_bytes()).hexdigest()
        self.fixture = b'PDF source fixture, never executed'
        (self.root / 'ocr-task.pdf').write_bytes(self.fixture)
        partial = {'schema': 'crispsorter-local-linux-partial-verification-v1',
                   'app_source_from_completed_build_and_draft_target': recorder.SOURCE,
                   'version': recorder.VERSION, 'sha256': self.hash, 'bytes': self.package.stat().st_size,
                   'matches_release_asset_digest': True,
                   'cold_loader': {'cache_inhibited': True, 'library_search_environment_removed': True, 'missing_libraries': []},
                   'package_checks': {'version': 'passed', 'doctor': 'passed', 'ocr_worker_protocol': 'passed'}}
        control = {'schema': 'crispsorter-offline-package-ocr-control-v1',
                   'app_source_from_completed_build': recorder.SOURCE, 'package_sha256': self.hash,
                   'fixture_sha256': hashlib.sha256(self.fixture).hexdigest(),
                   'status': 'passed', 'exit_code': 0, 'stopped': None,
                   'engine': 'dbnet_trocr', 'offline': True, 'license_acceptance_added': False,
                   'gpu_device_nodes_present': False, 'model_bytes_unchanged_after_control': True,
                   'qwen_preset_smoke': 'blocked_license_consent_required; not attempted',
                   'models': [{'filename': name, 'bytes': size, 'sha256': sha} for name, (size, sha) in recorder.MODELS.items()],
                   'sanitized_output_sha256': {}}
        for name, data in [('stdout.hocr', b'<div class="ocr_page">Cobalt warranty 2031</div>'), ('stderr.txt', b'CPU fixture')]:
            (self.root / ('offline-control.' + name)).write_bytes(data)
            control['sanitized_output_sha256'][name] = hashlib.sha256(data).hexdigest()
        self.data = {'partial-verification.json': partial, 'offline-control.json': control,
                     'version.json': {'name': 'crispsorter', 'version': recorder.VERSION, 'target': 'linux', 'arch': 'x86_64'},
                     'doctor.json': {'pdf_extract_compiled_in': True},
                     'worker-protocol.json': {'arguments': ['--crispsorter-ocr-worker'], 'exit_codes': [1, 1]}}
        for name, data in self.data.items(): (self.root / name).write_text(json.dumps(data))

    def fake_git(self, args, **kwargs):
        binary = not kwargs.get('text')
        if args[-1].startswith(recorder.SOURCE + ':'): return self.fixture
        if 'show' in args: return b'committed recorder fixture'
        result = '' if 'status' in args else ('3' * 40 if args[-1] == 'HEAD^{tree}' else '2' * 40)
        return result.encode() if binary else result

    def record(self):
        with patch.object(recorder, 'PACKAGE_BYTES', len(b'nonexecutable DEB fixture')), \
                patch.object(recorder, 'PACKAGE_SHA256', self.hash), \
                patch.object(recorder.subprocess, 'check_output', side_effect=self.fake_git):
            return recorder.record(self.root, self.root, {'target': recorder.SOURCE})

    def test_distinct_observation_binds_actual_retained_bytes_and_never_qualifies_publication(self):
        result = self.record()
        self.assertNotEqual(result['verification_tooling_source'], result['original_app_source_association'])
        self.assertEqual(result['package']['sha256'], self.hash)
        self.assertFalse(result['grants_license_consent'])
        self.assertFalse(result['satisfies_publication_gate'])
        self.assertIn('unqualified', result['licensed_qwen_presets'])
        with self.assertRaises(ValueError):
            qualification.package_entry(result, recorder.SOURCE, recorder.VERSION)
        for name, digest in result['retained_evidence_sha256'].items():
            self.assertEqual(digest, hashlib.sha256((self.root / name).read_bytes()).hexdigest())

    def test_changed_package_source_model_output_or_control_refuses_observation(self):
        for case in ['package', 'source', 'model', 'output', 'fixture', 'failure', 'license', 'worker']:
            with self.subTest(case=case):
                data = copy.deepcopy(self.data)
                if case == 'package': self.package.write_bytes(b'changed')
                elif case == 'source': data['partial-verification.json']['app_source_from_completed_build_and_draft_target'] = '4' * 40
                elif case == 'model': data['offline-control.json']['models'][0]['sha256'] = '4' * 64
                elif case == 'output': (self.root / 'offline-control.stdout.hocr').write_bytes(b'changed')
                elif case == 'fixture': (self.root / 'ocr-task.pdf').write_bytes(b'changed')
                elif case == 'failure': data['offline-control.json']['status'] = 'failed'
                elif case == 'license': data['offline-control.json']['license_acceptance_added'] = True
                else: data['worker-protocol.json']['exit_codes'] = [0, 0]
                for name, value in data.items(): (self.root / name).write_text(json.dumps(value))
                with self.assertRaises(ValueError): self.record()
                self.package.write_bytes(b'nonexecutable DEB fixture')
                (self.root / 'ocr-task.pdf').write_bytes(self.fixture)
                (self.root / 'offline-control.stdout.hocr').write_bytes(b'<div class="ocr_page">Cobalt warranty 2031</div>')

    def test_symlinked_and_oversized_evidence_is_rejected(self):
        target = self.root / 'doctor.json'
        outside = self.root / 'other.json'
        outside.write_bytes(target.read_bytes()); target.unlink(); target.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, 'symlinked'): self.record()
        with self.assertRaisesRegex(ValueError, 'oversized'): recorder.evidence_file(self.root, 'other.json', limit=1)

    def test_modified_tooling_cannot_claim_a_committed_verifier_identity(self):
        path = self.root / 'scripts/record_existing_linux_package.py'
        path.write_bytes(b'modified recorder')
        with self.assertRaisesRegex(ValueError, 'committed source'):
            self.record()

    def test_draft_tag_and_asset_binding_cannot_be_waived(self):
        asset = {'name': recorder.PACKAGE, 'size': recorder.PACKAGE_BYTES, 'digest': 'sha256:' + recorder.PACKAGE_SHA256}
        good = {'tag_name': 'v' + recorder.VERSION, 'draft': True, 'target_commitish': recorder.SOURCE, 'assets': [asset]}
        for case in ['valid', 'missing', 'duplicate', 'public', 'target', 'size', 'hash', 'asset-missing']:
            draft = copy.deepcopy(good)
            if case == 'public': draft['draft'] = False
            elif case == 'target': draft['target_commitish'] = 'main'
            elif case == 'size': draft['assets'][0]['size'] += 1
            elif case == 'hash': draft['assets'][0]['digest'] = 'sha256:' + '4' * 64
            elif case == 'asset-missing': draft['assets'] = []
            pages = [[draft]] if case != 'missing' else [[]]
            if case == 'duplicate': pages[0].append(draft)
            with self.subTest(case=case), patch.object(recorder, 'validate_release_target') as tag, \
                    patch.object(recorder.subprocess, 'check_output', return_value=json.dumps(pages)):
                if case == 'valid':
                    result = recorder.draft_identity('Example/Project')
                    self.assertEqual(result['asset_digest'], asset['digest'])
                else:
                    with self.assertRaises(ValueError): recorder.draft_identity('Example/Project')
                tag.assert_called_once_with('Example/Project', 'v' + recorder.VERSION, recorder.SOURCE)
        with patch.object(recorder, 'validate_release_target', side_effect=ValueError('tag mismatch')):
            with self.assertRaisesRegex(ValueError, 'tag mismatch'): recorder.draft_identity('Example/Project')

    def test_recorder_cannot_use_an_original_release_receipt_filename(self):
        with patch.object(sys, 'argv', ['record', '--repo', 'Example/Project', '--evidence-dir', str(self.root),
                                      '--output', str(self.root / 'CrispSorter_0.15.1_linux-x86_64.receipt.json')]), \
                patch.object(recorder, 'draft_identity') as lookup, contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit): recorder.main()
            lookup.assert_not_called()


if __name__ == '__main__':
    unittest.main()
