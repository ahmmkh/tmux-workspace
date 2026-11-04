"""Config lookup, platform helpers and other behaviour that differs between machines."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'lib'))
import ai_core
import paths


class Config(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.enterContext(patch.dict(os.environ, {'HOME': str(self.tmp), 'XDG_CONFIG_HOME': str(self.tmp / 'xdg')}))
        os.environ.pop('TMUX_WORKSPACE_PROJECTS', None)
        self.repo = self.tmp / 'repo'
        self.repo.mkdir()
        (self.repo / 'config.example.json').write_text('{"projects_dir": "~/Projects"}')
        self.enterContext(patch.object(paths, 'REPO', self.repo))

    def write(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))
        return path

    def test_xdg_config_wins_over_legacy_and_example(self):
        self.write(self.repo / 'config.json', {})
        xdg = self.write(self.tmp / 'xdg' / 'tmux-workspace' / 'config.json', {})
        self.assertEqual(paths.config_path(), xdg)

    def test_legacy_repo_config_beats_the_example(self):
        legacy = self.write(self.repo / 'config.json', {})
        self.assertEqual(paths.config_path(), legacy)

    def test_example_is_the_last_resort(self):
        self.assertEqual(paths.config_path().name, 'config.example.json')

    def test_config_dir_defaults_to_dot_config(self):
        del os.environ['XDG_CONFIG_HOME']
        self.assertEqual(paths.config_dir(), self.tmp / '.config' / 'tmux-workspace')

    def test_unreadable_config_names_the_file(self):
        bad = self.tmp / 'xdg' / 'tmux-workspace' / 'config.json'
        bad.parent.mkdir(parents=True)
        bad.write_text('{not json')
        with self.assertRaisesRegex(ValueError, 'Cannot read .*config.json'):
            paths.load_config()

    def test_projects_dir_env_beats_config(self):
        self.write(self.tmp / 'xdg' / 'tmux-workspace' / 'config.json', {'projects_dir': '~/src'})
        os.environ['TMUX_WORKSPACE_PROJECTS'] = str(self.tmp / 'code')
        self.assertEqual(paths.projects_dir(), self.tmp / 'code')

    def test_projects_dir_expands_tilde_from_config(self):
        self.write(self.tmp / 'xdg' / 'tmux-workspace' / 'config.json', {'projects_dir': '~/src'})
        self.assertEqual(paths.projects_dir(), self.tmp / 'src')

    def test_projects_dir_defaults_to_home_projects(self):
        self.assertEqual(paths.projects_dir(), self.tmp / 'Projects')

    def test_projects_dir_survives_a_broken_config(self):
        (self.tmp / 'xdg' / 'tmux-workspace').mkdir(parents=True)
        (self.tmp / 'xdg' / 'tmux-workspace' / 'config.json').write_text('{not json')
        self.assertEqual(paths.projects_dir(), self.tmp / 'Projects')

    def test_snapshot_loads_agent_hook_from_the_repo(self):
        with patch.object(paths, 'REPO', ROOT):
            self.assertEqual(paths.agent_hook_path(), ROOT / 'integrations' / 'agent-hook.py')

    def test_ai_tools_explain_an_unset_model(self):
        self.write(self.tmp / 'xdg' / 'tmux-workspace' / 'config.json', {'model': ''})
        ai_core.config.cache_clear()
        self.addCleanup(ai_core.config.cache_clear)
        with self.assertRaisesRegex(ai_core.AIError, 'Set "model" in .*config.json to use AI tools'):
            ai_core.config()


if __name__ == '__main__':
    unittest.main()
