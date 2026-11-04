"""The pane-row model behind the pickers and the dashboard. No tmux, no curses."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from fixtures import FIXTURE, pane as fixture_pane  # noqa: E402
import workspace_model as model  # noqa: E402


def pane(pane_id, status, window='@1', session='alpha', command='omp', home=''):
    return {'session_name': session, 'window_id': window, 'pane_id': pane_id, 'pane_current_command': command,
            '@workspace_agent': status, '@workspace_home': home}


class Status(unittest.TestCase):
    def test_every_supported_shell_hides_the_status(self):
        for shell in ('zsh', 'bash', 'fish', 'sh', 'dash', 'ksh', 'tcsh', '-zsh', '/bin/bash'):
            with self.subTest(shell=shell):
                self.assertEqual(model.pane_status({'pane_current_command': shell, '@workspace_agent': 'working'}), '')

    def test_agent_keeps_its_status(self):
        self.assertEqual(model.pane_status({'pane_current_command': 'claude', '@workspace_agent': 'blocked'}), 'blocked')

    def test_unknown_status_is_ignored(self):
        self.assertEqual(model.pane_status({'pane_current_command': 'omp', '@workspace_agent': 'busy'}), '')

    def test_programs_named_like_shells_are_not_shells(self):
        for command in ('bashtop', 'fisher', 'zshdb', 'shellcheck'):
            with self.subTest(command=command):
                self.assertFalse(model.is_shell(command))


class RollUp(unittest.TestCase):
    def test_bash_pane_does_not_count(self):
        rows = [pane('%1', 'blocked', command='bash'), pane('%2', 'idle')]
        self.assertEqual(model.roll_up(rows), ({'%2': 'idle'}, {'@1': 'idle'}, {'alpha': 'idle'}))

    def test_order_is_blocked_done_working_idle(self):
        self.assertEqual(model.AGENT_ORDER, ('blocked', 'done', 'working', 'idle'))

    def test_parked_pane_counts_toward_its_home(self):
        rows = [pane('%2', 'done', '@9', session=model.PARKING, home='beta')]
        self.assertEqual(model.roll_up(rows)[2], {'beta': 'done'})


class Rows(unittest.TestCase):
    def setUp(self):
        self.rows = model.build_rows(FIXTURE)

    def test_rows_sort_by_workspace_urgency_then_position(self):
        self.assertEqual([r.workspace for r in self.rows], ['api', 'api', 'api', 'frontend', 'infra', 'docs'])
        self.assertEqual([r.pane_id for r in self.rows[:3]], ['%1', '%2', '%6'])

    def test_parked_pane_sits_last_under_its_home(self):
        parked = self.rows[2]
        self.assertTrue(parked.parked)
        self.assertEqual((parked.workspace, parked.tab), ('api', ''))

    def test_agent_comes_from_workspace_app_then_command(self):
        agents = {r.pane_id: r.agent for r in self.rows}
        self.assertEqual(agents['%1'], 'claude')   # @workspace_app, though the command is node
        self.assertEqual(agents['%3'], 'codex')    # command
        self.assertEqual(agents['%2'], '')         # a shell

    def test_shell_pane_has_no_status(self):
        self.assertEqual({r.pane_id: r.status for r in self.rows}['%2'], '')

    def test_label_falls_back_to_the_command(self):
        rows = model.build_rows([fixture_pane('%9', 'w', 9, 1, 't', 1, '', 'htop')])
        self.assertEqual(rows[0].label, 'htop')

    def test_broken_app_json_is_ignored(self):
        row = dict(FIXTURE[0], **{'@workspace_app': '{not json'})
        self.assertEqual(model.build_rows([row])[0].agent, 'omp')

    def test_bootstrap_session_is_hidden(self):
        rows = model.build_rows([fixture_pane('%9', '__workspace_bootstrap', 9, 1, 't', 1, '', 'sleep')])
        self.assertEqual(rows, [])

    def test_filter_matches_label_case_insensitively(self):
        self.assertEqual([r.pane_id for r in model.filter_rows(self.rows, 'TERRA')], ['%5'])

    def test_filter_matches_workspace_tab_and_agent(self):
        self.assertEqual({r.pane_id for r in model.filter_rows(self.rows, 'front')}, {'%3'})
        self.assertEqual({r.pane_id for r in model.filter_rows(self.rows, 'servers')}, {'%2'})
        self.assertEqual({r.pane_id for r in model.filter_rows(self.rows, 'claude')}, {'%1'})

    def test_agents_only_drops_rows_without_agent(self):
        self.assertEqual([r.pane_id for r in model.filter_rows(self.rows, agents_only=True)], ['%1', '%3', '%5', '%4'])

    def test_summary_counts(self):
        self.assertEqual(model.summary(self.rows),
                         {'blocked': 1, 'done': 1, 'working': 1, 'idle': 1, 'panes': 6, 'workspaces': 4})


if __name__ == '__main__':
    unittest.main()
