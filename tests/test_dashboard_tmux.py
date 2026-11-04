"""The dashboard against a real scratch tmux server: it lists panes, draws, jumps and parks."""
from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))  # tests/support.py
import support  # noqa: E402

SELF = str(support.SELF)


class Dashboard(support.ScratchTmux):
    def controller(self, *args):
        return subprocess.run([sys.executable, SELF, *args], env=self.controller_env(), capture_output=True,
                              text=True, timeout=20)

    def start_dashboard(self):
        """Run the curses UI in its own tab, the way the popup would run it."""
        command = f'{sys.executable} {SELF} dash'
        pane = self.tmux('new-window', 'alpha', '-d', '-P', '-F', '#{pane_id}', '-n', 'dash', command)
        self.wait_for(lambda: 'WORKSPACE' in self.tmux('capture-pane', pane, '-p'), True, timeout=10)
        return pane

    def test_once_lists_every_pane(self):
        self.pane('new-window', 'alpha', '-n', 'two')
        result = self.controller('dash', '--once', '--width', '100', '--height', '30')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('0:one', result.stdout)  # no config, so base-index is 0
        self.assertIn('1:two', result.stdout)

    def test_curses_ui_draws_and_enter_selects(self):
        self.pane('new-window', 'alpha', '-n', 'two')
        dash = self.start_dashboard()
        self.run_tmux('send-keys', '-t', dash, 'j')
        self.run_tmux('send-keys', '-t', dash, 'Enter')
        self.wait_for(lambda: self.expand('alpha', '#{window_name}'), 'two', timeout=10)

    def test_p_parks_the_selected_pane(self):
        first = self.pane('new-window', 'alpha', '-n', 'two')
        self.pane('split-window', first)
        dash = self.start_dashboard()
        self.run_tmux('send-keys', '-t', dash, 'j')
        self.run_tmux('send-keys', '-t', dash, 'p')
        self.wait_for(lambda: self.expand(first, '#{session_name}'), '__parked', timeout=10)


if __name__ == '__main__':
    unittest.main()
