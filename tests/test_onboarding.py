"""First-run onboarding: ask where projects live, save it, never ask again."""
import contextlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'lib'))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import onboarding  # noqa: E402
import paths  # noqa: E402
from support import load_workspace  # noqa: E402


class Terminal(io.StringIO):
    def isatty(self):
        return True


class Base(unittest.TestCase):
    def setUp(self):
        self.home = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.enterContext(mock.patch.dict(os.environ, {'HOME': str(self.home)}))
        for name in ('XDG_CONFIG_HOME', 'TMUX_WORKSPACE_PROJECTS'):
            os.environ.pop(name, None)
        self.repo = self.home / 'repo'
        self.repo.mkdir()
        (self.repo / 'config.example.json').write_text('{"model": "", "doppler": null}\n')
        self.enterContext(mock.patch.object(paths, 'REPO', self.repo))
        self.config = self.home / '.config/tmux-workspace/config.json'

    def setup_with(self, answers, *args):
        stdin = Terminal(''.join(a + '\n' for a in answers))
        out = io.StringIO()
        with mock.patch.object(sys, 'stdin', stdin), contextlib.redirect_stdout(out):
            code = onboarding.setup(list(args))
        return code, out.getvalue()


class NeedsOnboarding(Base):
    def test_missing_config_needs_onboarding(self):
        self.assertTrue(paths.needs_onboarding())

    def test_config_without_projects_dir_needs_onboarding(self):
        self.config.parent.mkdir(parents=True)
        self.config.write_text('{"model": ""}')
        self.assertTrue(paths.needs_onboarding())

    def test_broken_config_is_left_alone(self):
        self.config.parent.mkdir(parents=True)
        self.config.write_text('{broken')
        self.assertFalse(paths.needs_onboarding())

    def test_save_config_keeps_other_keys_and_is_private(self):
        self.config.parent.mkdir(parents=True)
        self.config.write_text('{"model": "typesafe/jev-1"}')
        paths.save_config({'projects_dir': '~/code'})
        self.assertEqual(json.loads(self.config.read_text()), {'model': 'typesafe/jev-1', 'projects_dir': '~/code'})
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o600)
        self.assertFalse(paths.needs_onboarding())

    def test_save_config_starts_from_the_legacy_config(self):
        (self.repo / 'config.json').write_text('{"model": "typesafe/jev-2", "doppler": {"project": "p"}}')
        paths.save_config({'projects_dir': '~/Projects'})
        self.assertEqual(json.loads(self.config.read_text())['doppler'], {'project': 'p'})


class Setup(Base):
    def test_enter_keeps_the_default_and_creates_it(self):
        code, out = self.setup_with([''])
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(self.config.read_text())['projects_dir'], '~/Projects')
        self.assertTrue((self.home / 'Projects').is_dir())
        self.assertIn('Where do you keep your projects?', out)
        self.assertIn('work my-project', out)

    def test_typed_folder_is_saved_with_a_tilde(self):
        self.setup_with(['~/code'])
        self.assertEqual(json.loads(self.config.read_text())['projects_dir'], '~/code')
        self.assertEqual(paths.projects_dir(), self.home / 'code')

    def test_relative_answer_is_under_home(self):
        self.setup_with(['src'])
        self.assertEqual(paths.projects_dir(), self.home / 'src')

    def test_folder_outside_home_is_kept_absolute(self):
        target = Path(self.enterContext(tempfile.TemporaryDirectory())) / 'work'
        self.setup_with([str(target)])
        self.assertEqual(json.loads(self.config.read_text())['projects_dir'], str(target))

    def test_a_file_is_refused_and_asked_again(self):
        (self.home / 'notes').write_text('x')
        code, out = self.setup_with(['notes', 'code'])
        self.assertEqual(code, 0)
        self.assertIn('is a file', out)
        self.assertEqual(paths.projects_dir(), self.home / 'code')

    def test_projects_flag_needs_no_terminal(self):
        out = io.StringIO()
        with mock.patch.object(sys, 'stdin', io.StringIO()), contextlib.redirect_stdout(out):
            self.assertEqual(onboarding.setup(['--projects', str(self.home / 'dev')]), 0)
        self.assertEqual(paths.projects_dir(), self.home / 'dev')
        self.assertNotIn('Where do you keep', out.getvalue())

    def test_without_a_terminal_the_default_is_used(self):
        with mock.patch.object(sys, 'stdin', io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(onboarding.setup([]), 0)
        self.assertEqual(paths.projects_dir(), self.home / 'Projects')

    def test_end_of_input_cancels_without_saving(self):
        code, _ = self.setup_with([])
        self.assertEqual(code, 1)
        self.assertFalse(self.config.exists())


class FirstWork(Base):
    def test_first_interactive_work_runs_onboarding_once(self):
        ws = load_workspace()
        with mock.patch.object(ws, 'choose_or_create_workspace'), \
                mock.patch.object(ws.sys.stdin, 'isatty', return_value=True), \
                mock.patch.object(ws.sys.stdout, 'isatty', return_value=True), \
                mock.patch.object(onboarding, 'setup', side_effect=lambda args: paths.save_config({'projects_dir': '~/dev'})) as setup:
            ws.open_workspace()
            ws.open_workspace()
        self.assertEqual(setup.call_count, 1)
        self.assertEqual(ws.PROJECTS_ROOT, (self.home / 'dev').resolve())

    def test_new_workspace_names_its_first_tab(self):
        """Without -n, tmux names the tab after the quoted default command, which shows as \\\\."""
        ws = load_workspace()
        calls = []

        def tm(*args, **kwargs):
            calls.append(args)
            return ''
        with mock.patch.object(ws, 'tm', side_effect=tm), mock.patch.object(ws, 'ROOT', self.home / 'state'), \
                mock.patch.object(ws, 'PROJECTS_ROOT', self.home / 'Projects'), \
                mock.patch.object(ws, 'session_names', return_value=[]), mock.patch.object(ws, 'connect_session'), \
                mock.patch.object(ws, 'initialize'), mock.patch.object(ws.paths, 'needs_onboarding', return_value=False), \
                mock.patch.object(ws.subprocess, 'run', return_value=mock.Mock(returncode=1, stdout='')):
            ws.open_workspace('api')
        created = [c for c in calls if c[0] == 'new-session'][0]
        self.assertEqual(created[created.index('-n') + 1], 'api')

    def test_non_interactive_work_never_prompts(self):
        ws = load_workspace()
        with mock.patch.object(ws, 'choose_or_create_workspace'), \
                mock.patch.object(ws.sys.stdin, 'isatty', return_value=False), \
                mock.patch.object(onboarding, 'setup') as setup:
            ws.open_workspace()
        setup.assert_not_called()


if __name__ == '__main__':
    unittest.main()
