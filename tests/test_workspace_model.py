"""The pane-row model behind the pickers and the dashboard. No tmux, no curses."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import workspace_model as model


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


if __name__ == '__main__':
    unittest.main()
