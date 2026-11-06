"""Check agent-status.conf on a scratch tmux server that never loads the real config."""
import importlib.util
import os
from pathlib import Path
import pty
import re
import shlex
import subprocess
import sys
import tempfile
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))  # tests/support.py
import support
from support import SLEEP

ROOT = Path(__file__).resolve().parents[1]
CONF = ROOT / 'agent-status.conf'
SPEC = importlib.util.spec_from_file_location('agent_hook', ROOT / 'legacy' / 'integrations' / 'agent-hook.py')
assert SPEC is not None and SPEC.loader is not None
hook = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(hook)
BLOCKED, DONE, WORKING, IDLE = '#[fg=#f7768e]●', '#[fg=#7dcfff]●', '#[fg=#e0af68]●', '#[fg=#9ece6a]○'
ORDER = [('blocked', BLOCKED), ('done', DONE), ('working', WORKING), ('idle', IDLE)]
# The one pane that must look like a shell. The zsh builtin read keeps zsh in the foreground.
SHELL = "exec zsh -fc 'read -t 300'"


class ScratchTmux(support.ScratchTmux):
    CONFIGS = (CONF,)

    def setUp(self):
        super().setUp()
        self.run_tmux('set-option', '-g', '@agent_sound_cmd', 'true')  # Tests never play a real sound.

    def status(self, pane, value=None):
        if value is None:
            return self.tmux('show-options', pane, '-pqv', '@workspace_agent')
        self.tmux('set-option', pane, *(('-p', '@workspace_agent', value) if value else ('-pu', '@workspace_agent')))

    def turn_over(self, pane):
        self.tmux('set-option', pane, '-pF', '@workspace_agent', hook.TURN_OVER)
        return self.status(pane)

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
            self.run_tmux('source-file', '-n', str(path))

    def test_config_has_no_author_paths(self):
        for name in ('tmux.conf', 'agent-status.conf', 'shell.zsh', 'shell/work.sh', 'shell/work.fish'):
            with self.subTest(name=name):
                self.assertNotIn('/Users/', (ROOT / name).read_text())

    def test_tmux_runs_commands_with_posix_sh(self):
        """Bindings are sh syntax; a nu, elvish or pwsh login shell must not run them."""
        conf = (ROOT / 'tmux.conf').read_text()
        self.assertIn('set -g default-shell /bin/sh\n', conf)
        self.assertIn('tmux-workspace" exec-shell', conf)
        self.assertNotIn('"$SHELL" -i', conf)

    def report_popup_command(self):
        """The command of the popup that Ctrl+a F opens, as sh gets it from tmux.conf."""
        conf = (ROOT / 'tmux.conf').read_text()
        bindings = [line for line in conf.splitlines() if re.match(r'bind(-key)?\s+(-\w+\s+)*F\s', line)]
        self.assertEqual(len(bindings), 1, 'F is bound once')
        found = re.fullmatch(r"bind F display-popup -E (?:-\w \S+ )*'(.*)'", bindings[0])
        self.assertIsNotNone(found, bindings[0])
        return found.group(1)

    def test_prefix_capital_f_reports_a_bug_in_a_popup(self):
        command = self.report_popup_command()
        self.assertTrue(command.startswith('"$HOME/.local/bin/tmux-workspace" bug'), command)
        self.assertNotIn('bug --print', command)

    def test_the_bug_popup_stays_open_until_a_key_is_pressed(self):
        with tempfile.TemporaryDirectory() as home:
            tool = Path(home) / '.local' / 'bin' / 'tmux-workspace'
            tool.parent.mkdir(parents=True)
            tool.write_text('#!/bin/sh\necho "ran: $*"\n')
            tool.chmod(0o755)
            master, slave = pty.openpty()
            self.addCleanup(os.close, master)
            popup = subprocess.Popen(['/bin/sh', '-c', self.report_popup_command()],
                                     env={'HOME': home, 'PATH': '/usr/bin:/bin'},
                                     stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
            os.close(slave)
            self.addCleanup(popup.kill)
            shown = b''
            deadline = time.monotonic() + 5
            while b'any key' not in shown and time.monotonic() < deadline:
                shown += os.read(master, 4096)
            self.assertIn(b'ran: bug', shown)
            self.assertIn(b'any key', shown)
            time.sleep(0.5)
            self.assertIsNone(popup.poll(), 'the popup waits for a key')
            os.write(master, b'x')
            self.assertEqual(popup.wait(timeout=5), 0)

    def test_status_regex_matches_every_known_shell(self):
        for shell in ('zsh', 'bash', 'fish', 'sh', 'dash', 'ksh', 'tcsh', 'nu', 'elvish', 'xonsh', 'pwsh'):
            with self.subTest(shell=shell):
                self.assertIn(shell, CONF.read_text().split('\n')[2].split('$,')[0])

    def test_config_loads_without_errors(self):
        # Real sourcing reports unknown options, which parsing alone does not.
        with tempfile.TemporaryDirectory() as home:
            conf = Path(home) / '.config' / 'tmux-workspace'
            conf.mkdir(parents=True)
            (conf / 'agent-status.conf').symlink_to(CONF)
            # tmux expands ~ with the server's HOME, so this server starts inside the fake home.
            env = dict(self.env, HOME=home)
            argv = ['tmux', '-L', self.name + '-home', '-f', '/dev/null']
            subprocess.run(argv + ['new-session', '-d', '-s', 'conf', 'sleep 30'], env=env, check=True)
            try:
                result = subprocess.run(argv + ['source-file', str(ROOT / 'tmux.conf')], env=env, capture_output=True, text=True)
            finally:
                subprocess.run(argv + ['kill-server'], env=env, capture_output=True)
        self.assertEqual((result.returncode, result.stderr.strip(), result.stdout.strip()), (0, '', ''))

    def test_parse_rejects_an_unclosed_brace(self):
        with tempfile.NamedTemporaryFile('w', suffix='.conf') as bad:
            bad.write("set-hook -g client-focus-in { if -F 1 {\n")
            bad.flush()
            self.assertNotEqual(self.run_tmux('source-file', '-n', bad.name, check=False).returncode, 0)


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

    def test_bash_and_zsh_panes_show_no_mark(self):
        for shell, name in (("exec bash --norc --noprofile -c 'read -t 300'", 'bash'), ("exec zsh -fc 'read -t 300'", 'zsh')):
            with self.subTest(shell=name):
                pane = self.pane('split-window', 'alpha', shell=shell)
                self.wait_for(lambda: self.expand(pane, '#{pane_current_command}'), name)
                self.status(pane, 'working')
                self.assertEqual(self.expand(pane, '#{E:@agent_pane}'), '')

    def test_pane_at_the_shell_shows_no_mark(self):
        shell = self.pane('split-window', 'alpha:one', shell=SHELL)
        self.wait_for(lambda: self.expand(shell, '#{pane_current_command}'), 'zsh')
        self.status(shell, 'working')
        self.assertEqual(self.expand(shell, '#{E:@agent_pane}'), '')
        self.assertEqual(self.expand('alpha:one', '#{E:@agent_tab}'), ' ')
        self.assertEqual(self.expand('alpha', '#{E:@agent_workspace}'), ' ')

    def test_status_left_fits_a_long_workspace_name_and_separates_the_mark(self):
        name = 'n' * 20
        self.run_tmux('new-session', '-d', '-s', name, SLEEP)
        self.status(name, 'blocked')
        length = int((ROOT / 'tmux.conf').read_text().split('status-left-length ')[1].split()[0])
        self.assertLessEqual(int(self.expand(name, '#{w:#{E:status-left}}')), length)
        self.assertTrue(self.expand(name, '#{E:status-left}').endswith(BLOCKED + '#[default] │ '))

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


def tmux_version():
    found = re.search(r'(\d+)\.(\d+)', subprocess.run(['tmux', '-V'], capture_output=True, text=True).stdout)
    return tuple(map(int, found.groups())) if found else (99, 0)


class PaneZoom(ScratchTmux):
    @unittest.skipIf(tmux_version() < (3, 4), 'send-keys -K, which drives the binding here, needs tmux 3.4')
    def test_zoom_shortcut_restores_layout_and_keeps_processes(self):
        settings = [line for line in (ROOT / 'tmux.conf').read_text().splitlines()
                    if line.startswith(('set -g prefix ', 'bind z ', 'set -g status-right '))]
        self.assertTrue(any(line.startswith('bind z ') for line in settings))
        for line in settings:
            self.run_tmux(*shlex.split(line))
        pane = self.pane('split-window', 'alpha:one', '-h')
        client = self.control_client('alpha')
        self.tmux('select-pane', pane)
        layout = self.expand(pane, '#{window_layout}')
        processes = self.tmux('list-panes', 'alpha:one', '-F', '#{pane_id}:#{pane_pid}:#{pane_dead}')
        for expected in ('1', '0'):
            self.run_tmux('send-keys', '-K', '-c', client, 'C-a', 'z')
            self.wait_for(lambda: self.expand(pane, '#{window_zoomed_flag}'), expected)
            self.assertEqual('ZOOM' in self.expand(pane, '#{E:status-right}'), expected == '1')
            if expected == '1':
                self.assertEqual(self.expand(pane, '#{pane_width}'), self.expand(pane, '#{window_width}'))
        self.assertEqual(self.expand(pane, '#{window_layout}'), layout)
        self.assertEqual(self.tmux('list-panes', 'alpha:one', '-F', '#{pane_id}:#{pane_pid}:#{pane_dead}'), processes)


class Sound(ScratchTmux):
    def setUp(self):
        super().setUp()
        folder = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.log = folder / 'log'
        stub = folder / 'stub'
        stub.write_text(f'#!/bin/sh\necho "$1" >> {self.log}\n')
        stub.chmod(0o700)
        self.run_tmux('set-option', '-g', '@agent_sound_cmd', str(stub))
        self.hidden = self.pane('new-window', 'alpha', '-n', 'two')

    def played(self):
        return self.log.read_text().split() if self.log.exists() else []

    def heard(self, pane):
        return self.tmux('show-options', pane, '-pqv', '@agent_heard')

    def test_blocked_plays_once_per_change(self):
        self.status(self.hidden, 'working')
        self.status(self.hidden, 'blocked')
        self.wait_for(self.played, ['blocked'])
        self.status(self.hidden, 'blocked')
        self.wait_for(lambda: self.heard(self.hidden), 'blocked')
        time.sleep(0.3)
        self.assertEqual(self.played(), ['blocked'])

    def test_done_plays_only_for_a_pane_that_is_not_visible(self):
        self.status(self.hidden, 'working')
        self.assertEqual(self.turn_over(self.hidden), 'done')
        self.wait_for(self.played, ['done'])
        self.control_client('alpha')
        shown = self.tmux('display-message', 'alpha:one', '-p', '#{pane_id}')
        self.status(shown, 'working')
        self.assertEqual(self.turn_over(shown), 'idle')
        self.wait_for(lambda: self.heard(shown), 'idle')
        time.sleep(0.3)
        self.assertEqual(self.played(), ['done'])

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
