import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('waiter', Path(__file__).with_name('wait_required_checks.py'))
waiter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(waiter)


class CheckAggregation(unittest.TestCase):
    def states(self, checks=(), workflows=(), statuses=()):
        def api(path, paginate=False):
            if '/check-runs?' in path:
                return [{'check_runs': list(checks)}]
            if '/actions/runs?' in path:
                return [{'workflow_runs': list(workflows)}]
            return {'statuses': list(statuses)}
        with patch.object(waiter, 'gh_api', side_effect=api):
            return waiter.checks_for('owner/repo', 'a' * 40)

    def workflow(self, **extra):
        return dict(name='CI', workflow_id=7, event='push', id=10,
                    created_at='2026-11-14T10:00:00Z', run_attempt=1,
                    status='completed', conclusion='success') | extra

    def check(self, **extra):
        return dict(name='Test', id=1, check_suite={'id': 3},
                    started_at='2026-11-14T10:00:00Z', status='completed',
                    conclusion='success') | extra

    def test_queued_workflow_without_jobs_prevents_false_green(self):
        states = self.states(checks=[self.check()], workflows=[self.workflow(status='queued', conclusion=None)])
        self.assertIn('queued', states.values())
        self.assertFalse(all(value in waiter.GREEN for value in states.values()))

    def test_same_named_jobs_in_different_workflows_remain_separate(self):
        states = self.states(checks=[self.check(), self.check(check_suite={'id': 4}, conclusion='failure')])
        self.assertEqual(set(states.values()), {'success', 'failure'})
        self.assertEqual(len(states), 2)

    def test_latest_job_attempt_within_suite_wins(self):
        states = self.states(checks=[self.check(conclusion='failure'), self.check(id=2, started_at='2026-11-14T10:01:00Z')])
        self.assertEqual(list(states.values()), ['success'])

    def test_old_workflow_retry_cannot_hide_a_new_queued_run(self):
        states = self.states(workflows=[self.workflow(run_attempt=9), self.workflow(id=20, run_attempt=1, created_at='2026-11-14T11:00:00Z', status='queued', conclusion=None)])
        self.assertEqual(list(states.values()), ['queued'])

    def test_different_same_named_workflow_definitions_are_not_combined(self):
        states = self.states(workflows=[self.workflow(), self.workflow(workflow_id=8, status='queued', conclusion=None)])
        self.assertEqual(set(states.values()), {'success', 'queued'})
        self.assertEqual(len(states), 2)

    def test_events_on_the_same_workflow_are_separate(self):
        states = self.states(workflows=[self.workflow(), self.workflow(event='pull_request', status='queued', conclusion=None)])
        self.assertEqual(len(states), 2)

    def test_missing_workflow_identity_falls_back_without_dropping_run(self):
        states = self.states(workflows=[self.workflow(workflow_id=None, status='queued', conclusion=None)])
        self.assertEqual(list(states.values()), ['queued'])

    def test_failed_commit_status_still_blocks_successful_jobs(self):
        states = self.states(checks=[self.check()], statuses=[{'context': 'security', 'state': 'failure'}])
        self.assertEqual(states['security'], 'failure')


if __name__ == '__main__':
    unittest.main()
