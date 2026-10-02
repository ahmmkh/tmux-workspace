"""Check agent-status.conf on a scratch tmux server that never loads the real config."""
import os
from pathlib import Path
import pty
import subprocess
import tempfile
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
CONF = ROOT / 'agent-status.conf'
BLOCKED, DONE, WORKING, IDLE = '#[fg=#f7768e]●', '#[fg=#7dcfff]●', '#[fg=#e0af68]●', '#[fg=#9ece6a]○'
ORDER = [('blocked', BLOCKED), ('done', DONE), ('working', WORKING), ('idle', IDLE)]
SLEEP = 'sleep 300'  # Panes end by themselves, so a leftover server does too.
# The one pane that must look like a shell. The zsh builtin read keeps zsh in the foreground.
SHELL = "zsh -fc 'read -t 300'"


class ScratchTmux(unittest.TestCase):
    def setUp(self):
        self.name = f'agent-status-test-{os.getpid()}-{uuid.uuid4().hex[:8]}'
        state = self.enterContext(tempfile.TemporaryDirectory())
        self.env = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE')}
        self.env['TMUX_WORKSPACE_STATE_DIR'] = state
        self.env['TERM'] = 'xterm-256color'
        self.run_tmux('new-session', '-d', '-s', 'alpha', '-n', 'one', '-x', '120', '-y', '40', SLEEP)
        self.addCleanup(self.stop_server, self.tmux('display-message', 'alpha', '-p', '#{socket_path}'))
        self.tmux('source-file', 'alpha', str(CONF))

    def argv(self, *args):
        return ['tmux', '-L', self.name, '-f', '/dev/null', *args]

    def run_tmux(self, *args, check=True):
        """Every tmux call goes through here. Only a call that may fail or that has no target calls it directly."""
        result = subprocess.run(self.argv(*args), env=self.env, capture_output=True, text=True)
        if check and result.returncode:
            self.fail(f'tmux {args}: {result.stderr.strip()}')
        return result

    def tmux(self, command, target, *args):
        return self.run_tmux(command, '-t', target, *args).stdout.rstrip('\n')

    def stop_server(self, socket):
        self.run_tmux('kill-server', check=False)
        deadline = time.monotonic() + 3
        while self.run_tmux('list-sessions', check=False).returncode == 0 and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertNotEqual(self.run_tmux('list-sessions', check=False).returncode, 0, 'scratch server still runs')
        Path(socket).unlink(missing_ok=True)  # tmux leaves the socket file of a dead server.

    def pane(self, command, target, *args, shell=SLEEP):
        return self.tmux(command, target, '-d', '-P', '-F', '#{pane_id}', *args, shell)

    def expand(self, target, text):
        return self.tmux('display-message', target, '-p', text)

    def status(self, pane, value=None):
        if value is None:
            return self.tmux('show-options', pane, '-pqv', '@workspace_agent')
        self.tmux('set-option', pane, *(('-p', '@workspace_agent', value) if value else ('-pu', '@workspace_agent')))

    def turn_over(self, pane):
        self.tmux('set-option', pane, '-pF', '@workspace_agent', '#{E:@agent_turn_over}')
        return self.status(pane)

    def wait_for(self, read, expected):
        deadline = time.monotonic() + 3
        while read() != expected and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(read(), expected)

    def control_client(self, session):
        """Attach a control-mode client. tmux counts it as a terminal with focus."""
        client = subprocess.Popen(self.argv('-C', 'attach-session', '-t', session), env=self.env,
                                  stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        assert client.stdin is not None
        self.addCleanup(client.wait)
        self.addCleanup(client.stdin.close)
        self.wait_for(lambda: self.tmux('list-clients', session, '-F', '#{client_flags}').count('focused'), 1)
        return self.tmux('list-clients', session, '-F', '#{client_name}')

    def terminal_client(self, session):
        """Attach a client on a pty and return a function that sends it focus in or focus out."""
        master, slave = pty.openpty()
        client = subprocess.Popen(self.argv('attach-session', '-t', session), env=self.env,
                                  stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        self.addCleanup(os.close, master)
        self.addCleanup(client.wait)
        self.addCleanup(client.terminate)
        focused = lambda: 'focused' in self.tmux('list-clients', session, '-F', '#{client_flags}')
        self.wait_for(focused, True)

        def focus(on):
            os.write(master, b'\x1b[I' if on else b'\x1b[O')
            self.wait_for(focused, on)
        return focus


class Parse(ScratchTmux):
    def test_parse_accepts_both_config_files(self):
        for path in (ROOT / 'tmux.conf', CONF):
            self.tmux('source-file', 'alpha', '-n', str(path))

    def test_parse_rejects_an_unclosed_brace(self):
        with tempfile.NamedTemporaryFile('w', suffix='.conf') as bad:
            bad.write("set-hook -g client-focus-in { if -F 1 {\n")
            bad.flush()
            self.assertNotEqual(self.run_tmux('source-file', '-t', 'alpha', '-n', bad.name, check=False).returncode, 0)


class Marks(ScratchTmux):
    def assert_rolls_up(self, panes, target, option):
        for pane, (status, _) in zip(panes, ORDER):
            self.status(pane, status)
        for pane, (_, mark) in zip(panes, ORDER):
            self.assertEqual(self.expand(target, f'#{{E:{option}}}'), mark)
            self.status(pane, '')
        self.assertEqual(self.expand(target, f'#{{E:{option}}}'), ' ')

    def test_tab_shows_the_most_urgent_status_of_its_panes(self):
        panes = ['alpha:one.0'] + [self.pane('split-window', 'alpha:one') for _ in range(3)]
        self.tmux('select-layout', 'alpha:one', 'tiled')
        self.assert_rolls_up(panes, 'alpha:one', '@agent_tab')

    def test_workspace_shows_the_most_urgent_status_of_its_tabs(self):
        panes = ['alpha:one.0'] + [self.pane('new-window', 'alpha') for _ in range(3)]
        self.assert_rolls_up(panes, 'alpha', '@agent_workspace')

    def test_pane_border_shows_one_mark_per_status(self):
        for status, mark in ORDER:
            self.status('alpha:one.0', status)
            self.assertEqual(self.expand('alpha:one.0', '#{E:@agent_pane}'), mark + ' ')
        self.status('alpha:one.0', '')
        self.assertEqual(self.expand('alpha:one.0', '#{E:@agent_pane}'), '')

    def test_parked_pane_counts_toward_its_home_workspace(self):
        self.run_tmux('new-session', '-d', '-s', 'beta', SLEEP)
        self.run_tmux('new-session', '-d', '-s', '__parked', SLEEP)
        self.tmux('set-option', '__parked', '-p', '@workspace_home', 'beta')
        self.status('__parked', 'blocked')
        self.status('beta', 'working')
        self.assertEqual(self.expand('beta', '#{E:@agent_workspace}'), BLOCKED)
        self.assertEqual(self.expand('alpha', '#{E:@agent_workspace}'), ' ')

    def test_pane_at_the_shell_shows_no_mark(self):
        shell = self.pane('split-window', 'alpha:one', shell=SHELL)
        self.wait_for(lambda: self.expand(shell, '#{pane_current_command}'), 'zsh')
        self.status(shell, 'working')
        self.assertEqual(self.expand(shell, '#{E:@agent_pane}'), '')
        self.assertEqual(self.expand('alpha:one', '#{E:@agent_tab}'), ' ')
        self.assertEqual(self.expand('alpha', '#{E:@agent_workspace}'), ' ')

    def test_tab_keeps_its_width_with_and_without_a_mark(self):
        widths = '#{w:#{E:window-status-format}} #{w:#{E:window-status-current-format}}'
        expected = str(len(' 0:one '))  # The width of a tab before this file replaced the formats.
        self.assertEqual(self.expand('alpha:one', widths), f'{expected} {expected}')
        for status, _ in ORDER:
            self.status('alpha:one.0', status)
            self.assertEqual(self.expand('alpha:one', widths), f'{expected} {expected}')


class TurnOverAndSeen(ScratchTmux):
    def setUp(self):
        super().setUp()
        self.one = 'alpha:one.0'
        self.two = self.pane('new-window', 'alpha', '-n', 'two')

    def test_visible_pane_becomes_idle(self):
        self.control_client('alpha')
        self.status(self.one, 'working')
        self.assertEqual(self.turn_over(self.one), 'idle')

    def test_pane_that_is_not_visible_becomes_done(self):
        self.control_client('alpha')
        for before, after in (('working', 'done'), ('blocked', 'done'), ('done', 'done'), ('idle', 'idle'), ('', 'idle')):
            self.status(self.two, before)
            self.assertEqual(self.turn_over(self.two), after)

    def test_workspace_that_no_terminal_shows_is_not_visible(self):
        self.status(self.one, 'working')
        self.assertEqual(self.turn_over(self.one), 'done')

    def test_tab_on_screen_without_focus_is_not_visible(self):
        focus = self.terminal_client('alpha')
        focus(False)
        self.status(self.one, 'working')
        self.assertEqual(self.turn_over(self.one), 'done')
        self.tmux('select-window', 'alpha:two')
        self.tmux('select-window', 'alpha:one')
        self.assertEqual(self.status(self.one), 'done')
        focus(True)
        self.wait_for(lambda: self.status(self.one), 'idle')

    def test_viewing_a_tab_makes_all_its_done_panes_idle(self):
        self.control_client('alpha')
        other = self.pane('split-window', 'alpha:two')
        blocked = self.pane('split-window', 'alpha:two')
        working = self.pane('split-window', 'alpha:two')
        for pane, status in ((self.two, 'done'), (other, 'done'), (blocked, 'blocked'), (working, 'working')):
            self.status(pane, status)
        self.tmux('select-window', 'alpha:two')
        self.wait_for(lambda: (self.status(self.two), self.status(other)), ('idle', 'idle'))
        self.assertEqual((self.status(blocked), self.status(working)), ('blocked', 'working'))

    def test_switching_workspace_makes_done_panes_idle(self):
        client = self.control_client('alpha')
        self.run_tmux('new-session', '-d', '-s', 'beta', SLEEP)
        self.status('beta', 'done')
        self.tmux('switch-client', 'beta', '-c', client)
        self.wait_for(lambda: self.status('beta'), 'idle')

    def test_parked_pane_becomes_idle_when_it_joins_a_visible_tab(self):
        self.control_client('alpha')
        self.run_tmux('new-session', '-d', '-s', '__parked', SLEEP)
        parked = self.expand('__parked', '#{pane_id}')
        self.tmux('set-option', parked, '-p', '@workspace_home', 'alpha')
        self.status(parked, 'working')
        self.assertEqual(self.turn_over(parked), 'done')
        self.tmux('select-window', 'alpha:two')
        self.tmux('select-window', 'alpha:one')
        self.assertEqual(self.status(parked), 'done')
        self.assertEqual(self.expand('alpha', '#{E:@agent_workspace}'), DONE)
        self.tmux('join-pane', 'alpha:one', '-d', '-s', parked)
        self.wait_for(lambda: self.status(parked), 'idle')


class Sound(ScratchTmux):
    def setUp(self):
        super().setUp()
        folder = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.log = folder / 'log'
        stub = folder / 'stub'
        stub.write_text(f'#!/bin/sh\necho "$1" >> {self.log}\n')
        stub.chmod(0o700)
        for name, value in (('cmd', stub), ('blocked', 'BLOCKED'), ('done', 'DONE')):
            self.run_tmux('set-option', '-g', f'@agent_sound_{name}', str(value))
        self.hidden = self.pane('new-window', 'alpha', '-n', 'two')

    def played(self):
        return self.log.read_text().split() if self.log.exists() else []

    def heard(self, pane):
        return self.tmux('show-options', pane, '-pqv', '@agent_heard')

    def test_blocked_plays_once_per_change(self):
        self.status(self.hidden, 'working')
        self.status(self.hidden, 'blocked')
        self.wait_for(self.played, ['BLOCKED'])
        self.status(self.hidden, 'blocked')
        self.wait_for(lambda: self.heard(self.hidden), 'blocked')
        time.sleep(0.3)
        self.assertEqual(self.played(), ['BLOCKED'])

    def test_done_plays_only_for_a_pane_that_is_not_visible(self):
        self.status(self.hidden, 'working')
        self.assertEqual(self.turn_over(self.hidden), 'done')
        self.wait_for(self.played, ['DONE'])
        self.control_client('alpha')
        shown = self.tmux('display-message', 'alpha:one', '-p', '#{pane_id}')
        self.status(shown, 'working')
        self.assertEqual(self.turn_over(shown), 'idle')
        self.wait_for(lambda: self.heard(shown), 'idle')
        time.sleep(0.3)
        self.assertEqual(self.played(), ['DONE'])

    def test_starting_working_and_idle_are_silent(self):
        for value in ('idle', 'working', 'idle'):
            self.status(self.hidden, value)
        self.wait_for(lambda: self.heard(self.hidden), 'idle')
        time.sleep(0.3)
        self.assertEqual(self.played(), [])

    def test_mute_is_silent_and_still_follows_the_status(self):
        self.run_tmux('set-option', '-g', '@agent_sound', 'off')
        self.status(self.hidden, 'blocked')
        self.wait_for(lambda: self.heard(self.hidden), 'blocked')
        time.sleep(0.3)
        self.assertEqual(self.played(), [])

    def test_pane_at_the_shell_is_silent(self):
        shell = self.pane('new-window', 'alpha', '-n', 'three', shell=SHELL)
        self.wait_for(lambda: self.expand(shell, '#{pane_current_command}'), 'zsh')
        self.status(shell, 'blocked')
        self.wait_for(lambda: self.heard(shell), 'blocked')
        time.sleep(0.3)
        self.assertEqual(self.played(), [])


if __name__ == '__main__':
    unittest.main()
