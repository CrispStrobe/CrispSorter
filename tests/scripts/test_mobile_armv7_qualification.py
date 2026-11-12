import json
from pathlib import Path
import re
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / '.github/workflows/mobile-crispembed-qualification.yml').read_text()


class AndroidCandidateQualificationTests(unittest.TestCase):
    def test_matrix_selects_one_candidate_without_changing_original_default(self):
        match = re.search(r"fromJSON\(inputs.target == 'android-armv7'\s*&& '([^']+)'"
                          r"\s*\|\| inputs.target == 'android-x86_64'\s*&& '([^']+)'"
                          r"\s*\|\| '([^']+)'\)", WORKFLOW)
        self.assertIsNotNone(match)
        armv7, x86_64, default = map(json.loads, match.groups())
        self.assertEqual(armv7, [{'runner': 'ubuntu-24.04', 'target': 'android-armv7'}])
        self.assertEqual(x86_64, [{'runner': 'ubuntu-24.04', 'target': 'android-x86_64'}])
        self.assertEqual(default, [{'runner': 'ubuntu-24.04', 'target': 'android-arm64'},
                                   {'runner': 'macos-15', 'target': 'ios-arm64'},
                                   {'runner': 'macos-15', 'target': 'ios-sim-arm64'}])
        self.assertIn('options: [all, android-armv7, android-x86_64]\n        default: all', WORKFLOW)

    def test_actual_compiler_selection_uses_target_ndk_toolchain(self):
        case = re.search(r"            case '\$\{\{ matrix.target \}\}' in\n(.*?)            esac", WORKFLOW, re.S)[0]
        for target, expected in [('android-armv7', 'armv7a-linux-androideabi24-clang'),
                                 ('android-x86_64', 'x86_64-linux-android24-clang'),
                                 ('android-arm64', 'aarch64-linux-android24-clang')]:
            script = case.replace('${{ matrix.target }}', target) + '\nprintf "%s" "$compiler"'
            self.assertEqual(subprocess.check_output(['bash', '-c', script], text=True), expected)

    def test_unknown_android_compiler_is_rejected(self):
        case = re.search(r"            case '\$\{\{ matrix.target \}\}' in\n(.*?)            esac", WORKFLOW, re.S)[0]
        result = subprocess.run(['bash', '-c', case.replace('${{ matrix.target }}', 'android-unknown')],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Unsupported Android qualification compiler', result.stderr)

    def test_candidate_lock_and_provenance_remain_bound_to_build(self):
        self.assertIn('ref: ${{ steps.runtime.outputs.source }}', WORKFLOW)
        self.assertIn('--lock "$RUNTIME_LOCK"', WORKFLOW)
        self.assertIn('cp "$RUNTIME_LOCK" "$RUNNER_TEMP/crispembed-mobile/source-lock.json"', WORKFLOW)
        self.assertIn('name: crispembed-mobile-candidate-${{ matrix.target }}', WORKFLOW)
        for artifact in ('lib/', 'notices/', 'runtime-build.json', 'source-lock.json', 'link-probe-elf.txt'):
            self.assertIn('${{ runner.temp }}/crispembed-mobile/' + artifact, WORKFLOW)
        self.assertIn('${{ runner.temp }}/ocr-link-probe', WORKFLOW)
        self.assertIn('retention-days: 14', WORKFLOW)
        self.assertNotIn('concurrency:', WORKFLOW)


if __name__ == '__main__':
    unittest.main()
