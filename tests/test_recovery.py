"""Saving and restoring workspaces, and how a pane starts."""
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
