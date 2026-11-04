"""Saving and restoring workspaces, and how a pane starts."""
import contextlib
import io
import os
from pathlib import Path
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))  # tests/support.py
from support import load_workspace

ws = load_workspace()


class ShellFallback(unittest.TestCase):
    """A new pane execs the user's login shell, and never ends up dead."""

    def start(self, shell):
        env = {k: v for k, v in os.environ.items() if k != 'SHELL'}
        env['TMUX_PANE'] = '%1'
        if shell is not None:
            env['SHELL'] = shell
        with mock.patch.dict(os.environ, env, clear=True), \
                mock.patch.object(ws, 'identity'), \
                mock.patch.object(ws, 'option', return_value='label'), \
                mock.patch.object(ws.os, 'execv') as execv:
            ws.pane_start()
        return execv

    def test_pane_start_execs_shell_from_environment(self):
        shell = '/bin/bash' if Path('/bin/bash').exists() else '/bin/sh'
        self.start(shell).assert_called_once_with(shell, [shell, '-l'])

    def test_pane_start_falls_back_to_bin_sh(self):
        for value in (None, '', '/nonexistent/zsh', '/etc'):
            with self.subTest(shell=value):
                self.start(value).assert_called_once_with('/bin/sh', ['/bin/sh', '-l'])


if __name__ == '__main__':
    unittest.main()


sys.path.insert(0, str(Path(__file__).resolve().parent))
import json  # noqa: E402
import subprocess  # noqa: E402
import tempfile  # noqa: E402
import time  # noqa: E402

import support  # noqa: E402


class SnapshotRestore(support.ScratchTmux):
    """Save and restore on a scratch server with an empty state directory."""
    ENV = {'SHELL': '/bin/sh'}  # Restored panes exec a login shell; never the user's rc files.

    def setUp(self):
        super().setUp()
        self.run_tmux('new-session', '-d', '-s', 'beta', '-n', 'code', '-x', '120', '-y', '40', support.SLEEP)
        api = self.expand('beta:code', '#{pane_id}')
        logs = self.pane('split-window', api, '-h', '-l', '30%')  # tiled would be 50/50
        psql = self.pane('new-window', 'beta', '-n', 'db')
        for pane, label in ((api, 'api'), (logs, 'logs'), (psql, 'psql')):
            self.tmux('set-option', pane, '-p', '@workspace_label', label)

    def controller(self, *args):
        result = subprocess.run([sys.executable, str(support.SELF), *args], env=self.controller_env(),
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def layout(self):
        # Pane geometry, not window_layout: the layout string names pane ids, which a restore renews.
        fmt = '#{window_index}:#{window_name}:#{pane_index}:#{pane_left},#{pane_top},#{pane_width}x#{pane_height}'
        return self.run_tmux('list-panes', '-s', '-t', '=beta', '-F', fmt).stdout.split()

    def labels(self):
        return self.run_tmux('list-panes', '-s', '-t', '=beta', '-F', '#{window_name}:#{@workspace_label}').stdout.split()

    def has_beta(self):
        return self.run_tmux('has-session', '-t', '=beta', check=False).returncode == 0

    def test_round_trip_restores_sessions_tabs_labels_and_layout(self):
        layout, labels = self.layout(), self.labels()
        self.assertEqual(labels, ['code:api', 'code:logs', 'db:psql'])
        self.controller('save')
        self.run_tmux('kill-session', '-t', '=beta')
        self.controller('restore')
        self.wait_for(self.labels, labels, timeout=10)
        self.assertEqual(self.layout(), layout)

    def test_initialize_restores_once_per_server(self):
        self.controller('save')
        self.run_tmux('kill-session', '-t', '=beta')
        self.controller('init')
        self.wait_for(self.has_beta, True, timeout=10)
        self.run_tmux('kill-session', '-t', '=beta')
        self.controller('init')
        time.sleep(0.5)
        self.assertFalse(self.has_beta(), 'a second init restored a workspace the user closed')

    def test_snapshot_files_are_private(self):
        self.controller('save')
        root = Path(self.state)
        latest = json.loads((root / 'latest.json').read_text())
        directory = Path(latest['path']).parent
        self.assertEqual(root.stat().st_mode & 0o777, 0o700)
        self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
        self.assertEqual((root / 'latest.json').stat().st_mode & 0o777, 0o600)
        for path in directory.iterdir():
            self.assertEqual(path.stat().st_mode & 0o777, 0o600, path.name)


class PaneStart(unittest.TestCase):
    """pane_start reopens exactly the saved conversation, or explains why it cannot."""

    def start(self, app):
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(subprocess.run, ['rm', '-rf', str(directory)])
        snapshot = directory / 'snapshot.json'
        record = {'id': 'u' * 32, 'label': 'agent', 'parked': '', 'home': '', 'cwd': str(directory), 'app': app}
        snapshot.write_text(json.dumps({'version': 1, 'directory': str(directory), 'saved_at': time.time(),
                                        'panes': [record]}))
        out = io.StringIO()
        with mock.patch.dict(os.environ, {'TMUX_PANE': '%1', 'SHELL': '/bin/sh'}), \
                mock.patch.object(ws, 'put'), mock.patch.object(ws, 'set_label'), \
                mock.patch.object(ws, 'option', return_value=''), \
                mock.patch.object(ws.subprocess, 'run', return_value=mock.Mock(returncode=0)) as run, \
                mock.patch.object(ws.os, 'execv') as execv, contextlib.redirect_stdout(out):
            ws.pane_start(str(snapshot), 'u' * 32)
        execv.assert_called_once_with('/bin/sh', ['/bin/sh', '-l'])
        return run, out.getvalue()

    def test_resume_commands(self):
        for kind, command in (('claude', ['claude', '--resume', 'abc']), ('codex', ['codex', 'resume', 'abc'])):
            with self.subTest(kind=kind):
                run, out = self.start({'kind': kind, 'id': 'abc', 'cwd': '/tmp'})
                run.assert_called_once_with(command, cwd='/tmp')
                self.assertIn('Reopening ' + kind, out)

    def test_missing_omp_session_file_needs_attention(self):
        run, out = self.start({'kind': 'omp', 'id': '/nonexistent/session.jsonl'})
        run.assert_not_called()
        self.assertIn('Saved session file no longer exists', out)

    def test_identity_starting_with_dash_is_refused(self):
        run, out = self.start({'kind': 'claude', 'id': '--dangerously-skip-permissions'})
        run.assert_not_called()
        self.assertIn('Missing or invalid exact recovery identity', out)

    def test_recorded_error_is_shown_instead_of_guessing(self):
        run, out = self.start({'kind': 'codex', 'error': 'No exact conversation identity was recorded'})
        run.assert_not_called()
        self.assertIn('Recovery needs attention: No exact conversation identity was recorded', out)
