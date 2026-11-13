#!/usr/bin/env python3
"""Linux-only retries keep normal checks and never start other platform jobs."""
from pathlib import Path
import re
import unittest

ROOT=Path(__file__).resolve().parents[1]

class LinuxRelease(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow=(ROOT/'.github/workflows/release.yml').read_text()
        blocks=re.split(r'^  ([a-z][a-z0-9-]+):\n',cls.workflow.split('jobs:\n',1)[1],flags=re.M)
        cls.jobs=dict(zip(blocks[1::2],blocks[2::2]))

    def admitted(self,name,publish=False,mac_appstore=False,event='workflow_dispatch',platform='linux'):
        text=self.jobs[name]
        match=re.search(r'^    if: (.+?)(?=\n    [a-z]|\Z)',text,re.M|re.S)
        if not match:return True
        expression=match[1].strip().removeprefix('${{').removesuffix('}}').strip()
        expression=expression.replace('github.event_name',repr(event)).replace('github.ref',repr('refs/heads/main'))
        for key,value in dict(platform=platform,publish=publish,mac_appstore=mac_appstore).items():
            expression=expression.replace('inputs.'+key,repr(value))
        for dependency in ['release-required','release-android','release-ios']:
            expression=expression.replace('needs.'+dependency+'.result',repr('skipped'))
        expression=' '.join(expression.replace('&&',' and ').replace('||',' or ').split())
        expression=re.sub(r'!(?!=)',' not ',expression)
        return eval(expression,{'__builtins__':{},'always':lambda:True,'startsWith':lambda value,prefix:value.startswith(prefix)})

    def test_linux_selector_runs_only_normal_preflight_and_linux(self):
        for publish in [False,True]:
            selected={name for name in self.jobs if self.admitted(name,publish=publish)}
            self.assertEqual(selected,{'release-draft-preflight','release-linux'})

    def test_linux_selector_never_turns_into_app_store_or_public_release(self):
        for publish in [False,True]:
            self.assertFalse(self.admitted('publish',publish=publish))
            selected={name for name in self.jobs if self.admitted(name,publish=publish,mac_appstore=True)}
            self.assertEqual(selected,set())

    def test_linux_reuses_normal_recipe_steps_and_required_checks(self):
        job=self.jobs['release-linux']
        self.assertIn('needs: release-draft-preflight',job)
        self.assertIn('- *linux_release',job)
        self.assertIn('env: *desktop_release_env',job)
        self.assertIn('steps: *desktop_release_steps',job)
        self.assertIn('- &linux_release\n            platform: ubuntu-24.04',self.jobs['release-required'])
        steps=self.jobs['release-required']
        for check in ['Native regression tests for shipped features','Verify final packages',
                      'Qualify licensed OCR presets before publication (Linux)']:
            self.assertIn(check,steps)
        self.assertIn("runner.os == 'Linux' && (github.event_name != 'workflow_dispatch' || inputs.publish)",steps)

    def test_regular_all_and_other_desktop_routes_still_work(self):
        self.assertTrue(self.admitted('release-required',platform='all'))
        self.assertTrue(self.admitted('release-windows',platform='windows'))
        self.assertTrue(self.admitted('release-macos',platform='macos'))
        for platform in ['all','windows','macos','testflight_current','android_ocr_draft','ios_ocr_draft']:
            self.assertFalse(self.admitted('release-linux',platform=platform))
        self.assertFalse(self.admitted('release-linux',event='push'))

if __name__=='__main__':unittest.main()
