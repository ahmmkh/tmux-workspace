import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader('install_integrations', str(ROOT / 'bin/install-integrations'))
spec = importlib.util.spec_from_loader(loader.name, loader)
installer = importlib.util.module_from_spec(spec)
loader.exec_module(installer)


class Install(unittest.TestCase):
    def setUp(self):
        self.enterContext(mock.patch.dict(os.environ))
        os.environ.pop('CODEX_HOME', None)

    def test_install_preserves_other_hooks_backs_up_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(io.StringIO()):
            home = Path(directory)
            hooks = home / '.codex/hooks.json'
            hooks.parent.mkdir()
            existing = {'description': 'my hooks', 'hooks': {'Stop': [{'hooks': [
                {'type': 'command', 'command': 'existing-hook', 'timeout': 10}]}]}}
            original = json.dumps(existing).encode()
            hooks.write_bytes(original)
            installer.install(home, system='Darwin')
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
            installer.install(home, system='Darwin')
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

    def test_rendered_codex_command_matches_the_legacy_absolute_form(self):
        data = installer.render(ROOT / 'integrations/codex-hooks.json', Path('/Users/someone'))
        command = data['hooks']['SessionStart'][0]['hooks'][0]['command']
        self.assertIn('/Users/someone/.local/lib/tmux-workspace/agent-hook.py codex', command)
        self.assertNotIn('{{', json.dumps(data))

    def test_templates_name_no_author_paths(self):
        for name in ('codex-hooks.json', 'claude-hooks.json'):
            self.assertNotIn('/Users/', (ROOT / 'integrations' / name).read_text())

    def test_merge_replaces_old_agent_hook_entries_instead_of_duplicating(self):
        old = {'type': 'command', 'command': '/usr/bin/python3 /old/agent-hook.py codex', 'timeout': 3}
        mine = {'type': 'command', 'command': 'existing-hook', 'timeout': 10}
        existing = {'hooks': {'Stop': [{'hooks': [mine, old]}]}}
        wanted = {'Stop': [{'hooks': [{'type': 'command', 'command': 'python3 /new/agent-hook.py codex', 'timeout': 3}]}]}
        data, changed = installer.merge_hooks(existing, wanted)
        commands = [h['command'] for g in data['hooks']['Stop'] for h in g['hooks']]
        self.assertTrue(changed)
        self.assertEqual(commands, ['existing-hook', 'python3 /new/agent-hook.py codex'])
        self.assertEqual(installer.merge_hooks(data, wanted), (data, False))

    def claude_install(self, settings=None):
        directory = self.enterContext(tempfile.TemporaryDirectory())
        home = Path(directory)
        (home / '.claude').mkdir()
        if settings is not None:
            (home / '.claude/settings.json').write_text(json.dumps(settings))
        with contextlib.redirect_stdout(io.StringIO()):
            actions = installer.install(home, system='Linux')
        return home, json.loads((home / '.claude/settings.json').read_text()), actions

    def test_claude_hooks_and_status_line_are_registered(self):
        home, data, _ = self.claude_install({'model': 'opus'})
        hook = str(home / '.local/lib/tmux-workspace/agent-hook.py')
        self.assertEqual(data['model'], 'opus')
        self.assertIn(hook + ' claude-status', data['statusLine']['command'])
        for event in ('SessionStart', 'SessionEnd', 'CwdChanged', 'PermissionRequest', 'StopFailure', 'PreToolUse'):
            self.assertTrue(any(hook in h['command'] for g in data['hooks'][event] for h in g['hooks']), event)
        self.assertEqual(data['hooks']['PreToolUse'][0]['matcher'], 'AskUserQuestion|ExitPlanMode')

    def test_existing_claude_status_line_is_kept(self):
        _, data, actions = self.claude_install({'statusLine': {'type': 'command', 'command': 'mine'}})
        self.assertEqual(data['statusLine']['command'], 'mine')
        self.assertIn('Claude statusLine kept: mine', actions)

    def test_linux_skips_iterm_and_absent_tools(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(io.StringIO()):
            home = Path(directory)
            installer.install(home, system='Linux')
            self.assertFalse((home / 'Library').exists())
            self.assertFalse((home / '.claude').exists())
            self.assertFalse((home / '.codex').exists())
            self.assertFalse((home / '.omp').exists())
            self.assertEqual((home / '.local/lib/tmux-workspace/agent-hook.py').resolve(), ROOT / 'integrations/agent-hook.py')

    def test_omp_and_neovim_are_linked_when_present(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(io.StringIO()):
            home = Path(directory)
            (home / '.omp').mkdir()
            (home / '.config/nvim').mkdir(parents=True)
            installer.install(home, system='Linux')
            self.assertEqual((home / '.omp/agent/extensions/tmux-workspace.ts').resolve(), ROOT / 'integrations/tmux-workspace.ts')
            self.assertEqual((home / '.config/nvim/lua/custom/tmux_workspace.lua').resolve(), ROOT / 'integrations/tmux_workspace.lua')

    def test_codex_home_is_respected(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.redirect_stdout(io.StringIO()):
            home = Path(directory)
            codex = home / 'custom-codex'
            codex.mkdir()
            with mock.patch.dict(os.environ, {'CODEX_HOME': str(codex)}):
                installer.install(home, system='Linux')
            self.assertIn('agent-hook.py codex', (codex / 'hooks.json').read_text())


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


    def doctor_output(self, home, version='tmux 3.5a', clip='pbcopy', sound='afplay'):
        import sys
        sys.path.insert(0, str(ROOT / 'lib'))
        import diagnostics

        def run(argv, **kwargs):
            name = Path(argv[0]).name
            if argv[:2] == ['tmux', '-V']:
                return mock.Mock(returncode=0, stdout=version + '\n', stderr='')
            if name in ('tw-clip', 'tw-sound'):
                return mock.Mock(returncode=0, stdout=(clip if name == 'tw-clip' else sound) + '\n', stderr='')
            return mock.Mock(returncode=1, stdout='', stderr='no server running')
        with mock.patch.object(diagnostics.Path, 'home', return_value=home), \
                mock.patch.object(diagnostics.subprocess, 'run', side_effect=run), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            diagnostics.doctor({'SELF': str(ROOT / 'bin/tmux-workspace'), 'TMUX': 'tmux'})
        return output.getvalue()

    def test_doctor_checks_the_tmux_floor(self):
        with tempfile.TemporaryDirectory() as directory:
            text = self.doctor_output(Path(directory), version='tmux 3.2a')
        self.assertIn('WARN  tmux version: tmux 3.2a (needs 3.3 or newer)', text)
        with tempfile.TemporaryDirectory() as directory:
            self.assertIn('OK  tmux version: tmux next-3.6', self.doctor_output(Path(directory), version='tmux next-3.6'))

    def test_doctor_names_clipboard_and_sound_tools(self):
        with tempfile.TemporaryDirectory() as directory:
            text = self.doctor_output(Path(directory), clip='OSC 52', sound='none')
        self.assertIn('OK  clipboard: OSC 52', text)
        self.assertIn('sounds: none (agent sounds stay silent)', text)
        self.assertNotIn('pbcopy', text.replace('clipboard', ''))

    def test_doctor_checks_the_installer_links(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            (home / '.local/bin').mkdir(parents=True)
            (home / '.local/bin/tmux-workspace').symlink_to(ROOT / 'bin/tmux-workspace')
            text = self.doctor_output(home)
        self.assertIn('OK  ' + str(home / '.local/bin/tmux-workspace'), text)
        self.assertIn('WARN  ' + str(home / '.config/tmux-workspace/agent-status.conf'), text)
        self.assertIn('WARN  ' + str(home / '.tmux.conf'), text)


    def test_doctor_skips_codex_when_it_is_not_installed(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(os.environ):
            os.environ.pop('CODEX_HOME', None)
            text = self.doctor_output(Path(directory))
        self.assertNotIn('WARN  Codex hooks', text)
        self.assertIn('Codex: not installed', text)

    def test_doctor_accepts_rendered_codex_hooks(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(os.environ):
            os.environ.pop('CODEX_HOME', None)
            home = Path(directory)
            (home / '.codex').mkdir()
            with contextlib.redirect_stdout(io.StringIO()):
                installer.install(home, system='Linux')
            text = self.doctor_output(home)
        self.assertIn('OK  Codex hooks: all workspace hooks installed', text)


if __name__ == '__main__':
    unittest.main()
