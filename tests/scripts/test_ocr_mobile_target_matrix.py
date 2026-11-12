import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('targets', Path(__file__).parents[2] / 'scripts/ocr_mobile_target_matrix.py')
targets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(targets)

class TargetMatrixTest(unittest.TestCase):
    def test_all_preserves_targets(self):
        self.assertEqual([t['runtime'] for t in targets.matrix('all')['include']],
                         ['android-arm64', 'android-x86_64', 'ios-sim-arm64'])
    def test_individual_selection(self):
        for target in targets.TARGETS:
            self.assertEqual(targets.matrix(target['runtime']), {'include': [target]})
    def test_unknown_rejected(self):
        with self.assertRaises(ValueError):
            targets.matrix('unknown')
