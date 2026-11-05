"""Platform helpers and other behaviour that differs between machines. The config lookup is tested in Rust."""
import json
import os
import shutil
import subprocess
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'legacy' / 'lib'))
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

    def test_snapshot_loads_agent_hook_from_the_repo(self):
        with patch.object(paths, 'REPO', ROOT):
            self.assertEqual(paths.agent_hook_path(), ROOT / 'legacy' / 'integrations' / 'agent-hook.py')

    def test_ai_tools_explain_an_unset_model(self):
        self.write(self.tmp / 'xdg' / 'tmux-workspace' / 'config.json', {'model': ''})
        ai_core.config.cache_clear()
        self.addCleanup(ai_core.config.cache_clear)
        with self.assertRaisesRegex(ai_core.AIError, 'Set "model" in .*config.json to use AI tools'):
            ai_core.config()


class Helpers(unittest.TestCase):
    """bin/tw-clip and bin/tw-sound pick a tool from PATH. Stubs log how they were called."""

    def setUp(self):
        self.tmp = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.bin = self.tmp / 'bin'
        self.bin.mkdir()
        self.log = self.tmp / 'log'
        for tool in ('base64', 'tr', 'cat'):  # The only real tools the helpers need.
            (self.bin / tool).symlink_to(shutil.which(tool))
        self.system('Linux')

    def stub(self, name, body=''):
        path = self.bin / name
        path.write_text(f'#!/bin/sh\necho "{name} $*" >> {self.log}\n{body}\ncat >/dev/null 2>&1\nexit 0\n')
        path.chmod(0o755)

    def system(self, name):
        path = self.bin / 'uname'
        path.write_text(f'#!/bin/sh\necho {name}\n')
        path.chmod(0o755)

    def run_helper(self, name, *args, data=b'', **env):
        environment = {'PATH': str(self.bin), 'HOME': str(self.tmp), **env}
        return subprocess.run(['/bin/sh', str(ROOT / 'bin' / name), *args], input=data, env=environment,
                              capture_output=True, timeout=10)

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def test_clip_prefers_pbcopy(self):
        self.stub('pbcopy')
        self.stub('xclip')
        self.assertEqual(self.run_helper('tw-clip', 'copy', data=b'hi', DISPLAY=':0').returncode, 0)
        self.assertEqual(self.calls(), ['pbcopy '])

    def test_clip_uses_wl_copy_on_wayland(self):
        self.stub('wl-copy')
        self.stub('xclip')
        self.run_helper('tw-clip', 'copy', data=b'hi', WAYLAND_DISPLAY='wayland-0', DISPLAY=':0')
        self.assertEqual(self.calls(), ['wl-copy '])

    def test_clip_uses_xclip_on_x11(self):
        self.stub('xclip')
        self.run_helper('tw-clip', 'copy', data=b'hi', DISPLAY=':0')
        self.assertEqual(self.calls(), ['xclip -selection clipboard'])

    def test_clip_ignores_x11_tools_without_a_display(self):
        self.stub('xclip')
        tty = self.tmp / 'tty'
        tty.write_bytes(b'')
        self.stub('tmux', f'echo {tty}')
        self.run_helper('tw-clip', 'copy', data=b'hi')
        self.assertNotIn('xclip -selection clipboard', self.calls())

    def test_clip_falls_back_to_osc52(self):
        tty = self.tmp / 'tty'
        tty.write_bytes(b'')
        self.stub('tmux', f'echo {tty}')
        result = self.run_helper('tw-clip', 'copy', data=b'hello')
        self.assertEqual(result.returncode, 0)
        self.assertEqual(tty.read_bytes(), b'\x1b]52;c;aGVsbG8=\x07')

    def test_clip_paste_reads_pbpaste(self):
        self.stub('pbpaste', 'printf pasted')
        self.assertEqual(self.run_helper('tw-clip', 'paste').stdout, b'pasted')

    def test_clip_paste_falls_back_to_the_tmux_buffer(self):
        self.stub('tmux')
        self.run_helper('tw-clip', 'paste')
        self.assertEqual(self.calls(), ['tmux save-buffer -'])

    def test_clip_which_names_the_tool(self):
        self.stub('wl-copy')
        self.assertEqual(self.run_helper('tw-clip', 'which', WAYLAND_DISPLAY='w').stdout.strip(), b'wl-copy')
        self.assertEqual(self.run_helper('tw-clip', 'which').stdout.strip(), b'OSC 52')

    def test_sound_is_silent_without_tools(self):
        result = self.run_helper('tw-sound', 'blocked')
        self.assertEqual((result.returncode, self.calls()), (0, []))

    def test_sound_uses_afplay_on_macos(self):
        self.system('Darwin')
        self.stub('afplay')
        self.run_helper('tw-sound', 'blocked')
        self.run_helper('tw-sound', 'done')
        self.assertEqual(self.calls(), ['afplay /System/Library/Sounds/Funk.aiff', 'afplay /System/Library/Sounds/Glass.aiff'])

    def test_sound_uses_pipewire_then_pulseaudio_on_linux(self):
        sounds = self.tmp / 'sounds'
        sounds.mkdir()
        for name in ('dialog-warning.oga', 'complete.oga'):
            (sounds / name).write_bytes(b'')
        self.stub('paplay')
        self.run_helper('tw-sound', 'done', TW_SOUND_DIR=str(sounds))
        self.stub('pw-play')
        self.run_helper('tw-sound', 'blocked', TW_SOUND_DIR=str(sounds))
        self.assertEqual(self.calls(), [f'paplay {sounds}/complete.oga', f'pw-play {sounds}/dialog-warning.oga'])

    def test_sound_ignores_unknown_status(self):
        self.system('Darwin')
        self.stub('afplay')
        self.assertEqual(self.run_helper('tw-sound', 'working').returncode, 0)
        self.assertEqual(self.calls(), [])


class ShellIntegration(unittest.TestCase):
    """shell/work.sh gives bash and zsh the same work command; fish gets shell/work.fish."""

    def setUp(self):
        self.home = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.log = self.home / 'log'
        tool = self.home / '.local' / 'bin' / 'tmux-workspace'
        tool.parent.mkdir(parents=True)
        tool.write_text(f'#!/bin/sh\necho "$*" >> {self.log}\n')
        tool.chmod(0o755)

    def work(self, shell, *args):
        if not shutil.which(shell):
            self.skipTest(shell + ' is not installed')
        if shell == 'fish':
            script = f'source {ROOT}/shell/work.fish; work {" ".join(args)}'
            argv = ['fish', '--no-config', '-c', script]
        else:
            script = f'. {ROOT}/shell/work.sh; work {" ".join(args)}'
            argv = [shell, '-f' if shell == 'zsh' else '--norc', '-c', script]
        env = {'HOME': str(self.home), 'PATH': '/usr/bin:/bin'}
        subprocess.run(argv, env=env, check=True, capture_output=True, timeout=10)
        return self.log.read_text().splitlines()

    def test_work_opens_and_removes_in_every_shell(self):
        for shell in ('bash', 'zsh', 'fish'):
            with self.subTest(shell=shell):
                self.log.unlink(missing_ok=True)
                self.assertEqual(self.work(shell, 'api'), ['open api'])
                self.assertEqual(self.work(shell, 'rm', 'api'), ['open api', 'kill-workspace api'])

    def test_work_update_updates_in_every_shell(self):
        for shell in ('bash', 'zsh', 'fish'):
            with self.subTest(shell=shell):
                self.log.unlink(missing_ok=True)
                self.assertEqual(self.work(shell, 'update'), ['update'])
                self.assertEqual(self.work(shell, 'update', '--check')[-1], 'update --check')
                # A workspace named update opens through the path form, which gives open its name after --:
                # a bare `open update` is what a shell from before 2.0 runs for `work update`.
                self.assertEqual(self.work(shell, './update')[-1], 'open -- update')
                self.assertEqual(self.work(shell, './update', 'x')[-1], 'open -- update x')
                self.assertEqual(self.work(shell, 'update')[-1], 'update')

    def test_work_sh_puts_local_bin_on_path(self):
        for shell, flag in (('bash', '--norc'), ('zsh', '-f')):
            if not shutil.which(shell):
                continue
            with self.subTest(shell=shell):
                env = {'HOME': str(self.home), 'PATH': '/usr/bin:/bin'}
                result = subprocess.run([shell, flag, '-c', f'. {ROOT}/shell/work.sh; . {ROOT}/shell/work.sh; echo $PATH'],
                                        env=env, capture_output=True, text=True, check=True)
                self.assertEqual(result.stdout.strip(), f'{self.home}/.local/bin:/usr/bin:/bin')

    def test_zsh_shim_still_defines_work(self):
        if not shutil.which('zsh'):
            self.skipTest('zsh is not installed')
        env = {'HOME': str(self.home), 'PATH': '/usr/bin:/bin'}
        subprocess.run(['zsh', '-f', '-c', f'source {ROOT}/shell.zsh; work demo'], env=env, check=True, timeout=10)
        self.assertEqual(self.log.read_text().splitlines(), ['open demo'])


if __name__ == '__main__':
    unittest.main()
