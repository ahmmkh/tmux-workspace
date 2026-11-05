"""scripts/demo.py must never reach the real home: no XDG directory may point outside its sandbox."""
import os
from pathlib import Path
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import demo  # noqa: E402


class Sandbox(unittest.TestCase):
    def test_every_xdg_directory_is_inside_the_sandbox(self):
        real = {name: '/real/' + name for name in ('XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME')}
        with mock.patch.dict(os.environ, real):
            sandbox = demo.Demo()
        self.addCleanup(sandbox.close)
        for name in real:
            with self.subTest(name=name):
                self.assertTrue(Path(sandbox.env[name]).is_relative_to(sandbox.root), sandbox.env[name])

    def test_only_a_known_set_of_variables_reaches_the_sandbox(self):
        with mock.patch.dict(os.environ, {'TERM_PROGRAM': 'ghostty', 'SSH_AUTH_SOCK': '/x', 'HOSTNAME': 'mine'}):
            sandbox = demo.Demo()
        self.addCleanup(sandbox.close)
        for name in ('TERM_PROGRAM', 'SSH_AUTH_SOCK', 'HOSTNAME', 'USER'):
            self.assertNotIn(name, sandbox.env)

    def test_captures_show_the_sandbox_home_as_a_tilde(self):
        sandbox = demo.Demo()
        self.addCleanup(sandbox.close)
        self.assertEqual(sandbox.private(f'link {sandbox.home}/.tmux.conf -> {sandbox.home}/tmux-workspace/tmux.conf'),
                         'link ~/.tmux.conf -> ~/tmux-workspace/tmux.conf')

    def test_frames_are_captured_with_wrapped_lines_joined(self):
        """A long path wraps on screen; captured unjoined, its halves escape the ~ substitution."""
        sandbox = demo.Demo()
        self.addCleanup(sandbox.close)
        with mock.patch.object(sandbox, 'outer', return_value=f'link {sandbox.home}/x') as outer:
            self.assertEqual(sandbox.screen(), 'link ~/x')
        self.assertIn('-J', outer.call_args.args)

    def test_home_and_sockets_are_inside_the_sandbox(self):
        sandbox = demo.Demo()
        self.addCleanup(sandbox.close)
        for name in ('HOME', 'TMUX_TMPDIR', 'TMUX_WORKSPACE_STATE_DIR'):
            self.assertTrue(Path(sandbox.env[name]).is_relative_to(sandbox.root), name)
        self.assertNotIn('TMUX', sandbox.env)


if __name__ == '__main__':
    unittest.main()
