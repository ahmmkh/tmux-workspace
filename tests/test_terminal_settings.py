import contextlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader('install_terminal', str(ROOT / 'bin/install-terminal-settings'))
spec = importlib.util.spec_from_loader(loader.name, loader)
installer = importlib.util.module_from_spec(spec)
loader.exec_module(installer)


class Install(unittest.TestCase):
    def test_install_preserves_other_hooks_backs_up_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(io.StringIO()):
            home = Path(directory)
            hooks = home / '.codex/hooks.json'
            hooks.parent.mkdir()
            existing = {'description': 'my hooks', 'hooks': {'Stop': [{'hooks': [
                {'type': 'command', 'command': 'existing-hook', 'timeout': 10}]}]}}
            original = json.dumps(existing).encode()
            hooks.write_bytes(original)
            installer.install(home)
            installed = hooks.read_bytes()
            data = json.loads(installed)
            self.assertEqual(data['hooks']['Stop'][0], existing['hooks']['Stop'][0])
            self.assertEqual(data['description'], 'my hooks')
            self.assertEqual(len(data['hooks']['Stop']), 2)
            backups = list((home / '.local/state/tmux-workspace/backups').glob('*/hooks.json'))
            self.assertEqual(len(backups), 1)
            self.assertEqual(backups[0].read_bytes(), original)
            self.assertEqual(hooks.stat().st_mode & 0o777, 0o600)
            self.assertEqual((home / 'Library/Application Support/iTerm2/DynamicProfiles/tmux-workspace.json').resolve(),
                             ROOT / 'integrations/iterm2-profile.json')
            installer.install(home)
            self.assertEqual(hooks.read_bytes(), installed)
            self.assertEqual(list((home / '.local/state/tmux-workspace/backups').glob('*/hooks.json')), backups)
            self.assertFalse((home / '.codex/config.toml').exists())

    def test_invalid_existing_hooks_are_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            hooks = home / '.codex/hooks.json'
            hooks.parent.mkdir()
            hooks.write_text('not json')
            with self.assertRaises(ValueError):
                installer.install(home)
            self.assertEqual(hooks.read_text(), 'not json')


class Diagnostics(unittest.TestCase):
    def test_missing_server_reports_warning_without_starting_a_server(self):
        import sys
        sys.path.insert(0, str(ROOT / 'lib'))
        import diagnostics
        result = mock.Mock(returncode=1, stdout='', stderr='no server running')
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.object(diagnostics.Path, 'home', return_value=Path(directory)), \
                mock.patch.object(diagnostics.subprocess, 'run', return_value=result) as run, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            code = diagnostics.doctor({'SELF': str(ROOT / 'bin/tmux-workspace'), 'TMUX': 'tmux'})
        self.assertEqual(code, 1)
        self.assertIn('WARN  tmux server', output.getvalue())
        self.assertIn('-N', run.call_args.args[0])
        self.assertNotIn('capture-pane', str(run.call_args))


if __name__ == '__main__':
    unittest.main()
