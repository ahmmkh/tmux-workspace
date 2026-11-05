"""Helpers shared by the tests that drive a scratch tmux server."""
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
# The Python controller, which only dash, dashboard, ai and cleanup still run.
SELF = ROOT / 'legacy' / 'bin' / 'tmux-workspace'
# Panes end by themselves, so a leftover server does too. exec, because dash (Debian's sh) does not
# hand the pane over to its last command, and the pane would show sh instead of sleep.
SLEEP = 'exec sleep 300'


class ScratchTmux(unittest.TestCase):
    """A tmux server on its own socket that never loads the user's config or state."""
    CONFIGS = ()  # Files sourced once the server runs.
    ENV = {}  # Extra environment for the server and the controller.

    def setUp(self):
        self.name = f'tw-test-{os.getpid()}-{uuid.uuid4().hex[:8]}'
        self.state = tempfile.mkdtemp()
        self.addCleanup(self.remove_state)
        self.env = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE')}
        self.env['TMUX_WORKSPACE_STATE_DIR'] = self.state
        self.env['TERM'] = 'xterm-256color'
        self.env.update(self.ENV)
        self.run_tmux('new-session', '-d', '-s', 'alpha', '-n', 'one', '-x', '120', '-y', '40', SLEEP)
        self.addCleanup(self.stop_server, self.socket())
        for path in self.CONFIGS:
            self.run_tmux('source-file', str(path))  # no -t: source-file -t needs tmux 3.4

    def remove_state(self):
        subprocess.run(['rm', '-rf', self.state], check=False)

    def socket(self):
        return self.tmux('display-message', 'alpha', '-p', '#{socket_path}')

    def argv(self, *args):
        # -u: UTF-8 output even without a UTF-8 locale; tmux 3.3 prints ● as _ otherwise.
        return ['tmux', '-u', '-L', self.name, '-f', '/dev/null', *args]

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
        pane = self.tmux(command, target, '-d', '-P', '-F', '#{pane_id}', *args, shell)
        if shell == SLEEP:
            # A newborn pane may still be a shell, whose agent notifications are deliberately suppressed.
            self.wait_for(lambda: self.expand(pane, '#{pane_current_command}'), 'sleep')
        return pane

    def expand(self, target, text):
        return self.tmux('display-message', target, '-p', text)

    def wait_for(self, read, expected, timeout=3):
        deadline = time.monotonic() + timeout
        while read() != expected and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(read(), expected)

    def controller_env(self, pane=None):
        """Environment in which legacy/bin/tmux-workspace talks to this server."""
        env = dict(self.env, TMUX=self.socket() + ',0,0')
        if pane:
            env['TMUX_PANE'] = pane
        return env
