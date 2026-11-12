"""Exercise hosted template routing without downloads or remote jobs."""
import json
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
from unittest.mock import patch

TEMPLATE = Path(__file__).resolve().parents[2] / 'tools/remote-ci/provider_conformance.py'
PIN = '33bdc33f64d0a2f1144dceec8ed372d18c12b551'


class ProviderConformanceTemplateTests(unittest.TestCase):
    def exercise(self, pin_valid=True):
        with tempfile.TemporaryDirectory(prefix='provider-template-test-') as directory:
            root = Path(directory)
            (root / 'out').mkdir()
            concrete_path = type(root)
            calls = []

            def paths(value):
                return concrete_path({
                    '/kaggle/temp/provider-conformance': root / 'work',
                    '/kaggle/working': root / 'out',
                }.get(str(value), value))

            def command(args, **kwargs):
                calls.append((args, kwargs))
                resolving = any(str(arg).endswith('crisp_cloud_rev.py') for arg in args)
                return subprocess.CompletedProcess(args, 1 if resolving and not pin_valid else 0,
                    PIN + '\n' if resolving else '', '')

            def revision(args, **_kwargs):
                return ('__SORTER_SHA__' if str(args[2]).endswith('CrispSorter') else PIN) + '\n'

            with patch('pathlib.Path', side_effect=paths), \
                    patch('subprocess.run', side_effect=command), \
                    patch('subprocess.check_output', side_effect=revision):
                if pin_valid:
                    runpy.run_path(str(TEMPLATE))
                else:
                    with self.assertRaisesRegex(RuntimeError, 'resolve-cloud-revision failed'):
                        runpy.run_path(str(TEMPLATE))
            return calls, json.loads((root / 'out/provider-conformance-report.json').read_text())

    def test_checks_out_and_tests_the_app_pin_without_vendored_overrides(self):
        calls, report = self.exercise()
        self.assertTrue(report['passed'])
        self.assertEqual(report['cloud_source'], PIN)
        checkout = next(args for args, _ in calls if args[:2] == ['git', '-C'] and str(args[2]).endswith('crisp-cloud-rs'))
        self.assertEqual(checkout[-1], PIN)
        args, options = next((args, options) for args, options in calls if any(str(arg).endswith('conformance/run.py') for arg in args))
        self.assertNotIn('--rust-filen', args)
        self.assertNotIn('--rust-internxt', args)
        self.assertEqual(options['cwd'].name, 'crisp-cloud-rs')

    def test_pin_validation_failure_stops_before_cloning_cloud_or_running_conformance(self):
        calls, report = self.exercise(pin_valid=False)
        self.assertFalse(report['passed'])
        self.assertEqual(report['error'], 'resolve-cloud-revision failed')
        self.assertFalse(any('https://github.com/CrispStrobe/crisp-cloud-rs.git' in args for args, _ in calls))


if __name__ == '__main__':
    unittest.main()
