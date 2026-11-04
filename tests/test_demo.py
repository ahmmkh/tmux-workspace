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

    def test_home_and_sockets_are_inside_the_sandbox(self):
        sandbox = demo.Demo()
        self.addCleanup(sandbox.close)
        for name in ('HOME', 'TMUX_TMPDIR', 'TMUX_WORKSPACE_STATE_DIR'):
            self.assertTrue(Path(sandbox.env[name]).is_relative_to(sandbox.root), name)
        self.assertNotIn('TMUX', sandbox.env)


if __name__ == '__main__':
    unittest.main()
