#!/usr/bin/env python3
"""Test draft target checks with simulated GitHub release and Git API responses."""
import json
import subprocess
import unittest
from unittest.mock import patch

from check_release_target import validate_release_target

SOURCE = 'a' * 40
TAG_OBJECT = 'b' * 40
OTHER = 'c' * 40
REPO = 'Example/Project'
TAG = 'v0.14.0'
REF = f'repos/{REPO}/git/ref/tags/{TAG}'
TAG_API = f'repos/{REPO}/git/tags/{TAG_OBJECT}'


class ReleaseTarget(unittest.TestCase):
    def setUp(self):
        self.release = {'isDraft': True, 'targetCommitish': SOURCE}
        self.responses = {REF: (1, '', 'gh: Not Found (HTTP 404)')}

    def fake_run(self, args, **kwargs):
        if args[1:3] == ['release', 'view']:
            return subprocess.CompletedProcess(args, 0, json.dumps(self.release), '')
        self.assertEqual(args[1], 'api')
        code, output, error = self.responses[args[2]]
        return subprocess.CompletedProcess(args, code, json.dumps(output) if isinstance(output, dict) else output, error)

    def check(self):
        with patch('check_release_target.subprocess.run', self.fake_run):
            return validate_release_target(REPO, TAG, SOURCE)

    def test_missing_ref_uses_exact_draft_target(self):
        self.assertEqual(self.check(), 'draft target')
        self.release['targetCommitish'] = OTHER
        with self.assertRaisesRegex(ValueError, 'Draft target'):
            self.check()

    def test_lightweight_tag_must_match_source(self):
        self.responses[REF] = (0, {'object': {'type': 'commit', 'sha': SOURCE}}, '')
        self.assertEqual(self.check(), 'existing tag')
        self.responses[REF] = (0, {'object': {'type': 'commit', 'sha': OTHER}}, '')
        with self.assertRaisesRegex(ValueError, 'different commit'):
            self.check()

    def test_annotated_tag_is_dereferenced(self):
        self.responses[REF] = (0, {'object': {'type': 'tag', 'sha': TAG_OBJECT}}, '')
        self.responses[TAG_API] = (0, {'object': {'type': 'commit', 'sha': SOURCE}}, '')
        self.assertEqual(self.check(), 'existing tag')
        self.responses[TAG_API] = (0, {'object': {'type': 'commit', 'sha': OTHER}}, '')
        with self.assertRaisesRegex(ValueError, 'different commit'):
            self.check()

    def test_api_errors_are_not_treated_as_missing_refs(self):
        for error in ['gh: authentication failed (HTTP 401)', 'gh: validation failed (HTTP 422)', 'network failure']:
            with self.subTest(error=error):
                self.responses[REF] = (1, '', error)
                with self.assertRaisesRegex(RuntimeError, 'GitHub lookup failed'):
                    self.check()

    def test_missing_annotated_tag_object_is_an_error(self):
        self.responses[REF] = (0, {'object': {'type': 'tag', 'sha': TAG_OBJECT}}, '')
        self.responses[TAG_API] = (1, '', 'gh: Not Found (HTTP 404)')
        with self.assertRaisesRegex(RuntimeError, 'GitHub lookup failed'):
            self.check()

    def test_ref_cycle_and_noncommit_objects_fail(self):
        self.responses[REF] = (0, {'object': {'type': 'tag', 'sha': TAG_OBJECT}}, '')
        self.responses[TAG_API] = (0, {'object': {'type': 'tag', 'sha': TAG_OBJECT}}, '')
        with self.assertRaisesRegex(ValueError, 'does not resolve'):
            self.check()
        self.responses[REF] = (0, {'object': {'type': 'tree', 'sha': SOURCE}}, '')
        with self.assertRaisesRegex(ValueError, 'does not resolve'):
            self.check()

    def test_public_release_rejected(self):
        self.release['isDraft'] = False
        with self.assertRaisesRegex(ValueError, 'not a draft'):
            self.check()


if __name__ == '__main__':
    unittest.main()
