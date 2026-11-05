"""install.sh on a temporary HOME. PATH holds stubs and core utilities only, never the machine's tools."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'legacy' / 'lib'))
import paths  # noqa: E402

CORE = ['uname', 'id', 'dirname', 'basename', 'readlink', 'mkdir', 'ln', 'mv', 'cp', 'chmod', 'grep', 'awk',
        'sed', 'tr', 'date', 'find', 'sort', 'mktemp', 'cat', 'rm', 'env', 'bash', 'sh', 'ls', 'head', 'base64']
BLOCK = '# >>> tmux-workspace >>>'


class Installer(unittest.TestCase):
    def setUp(self):
        # install.sh links the binary that make build copies here, and runs it for the agent integrations.
        if not (ROOT / 'bin' / 'tmux-workspace').is_file():
            self.fail('bin/tmux-workspace is not built: run make build (make test does)')
        self.home = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(shutil.rmtree, self.home, True)
        self.bin = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(shutil.rmtree, self.bin, True)
        for tool in CORE:
            found = shutil.which(tool)
            if found:
                (self.bin / tool).symlink_to(found)
        (self.bin / 'python3').symlink_to(sys.executable)
        self.tmux('3.5a')
        self.stub('fzf')
        self.stub('git')

    def stub(self, name, body='exit 0'):
        path = self.bin / name
        path.write_text('#!/bin/sh\n' + body + '\n')
        path.chmod(0o755)

    def tmux(self, version):
        # -V reports the version; every other call fails, as when no server runs.
        self.stub('tmux', f'[ "$1" = -V ] && {{ echo "tmux {version}"; exit 0; }}\nexit 1')

    def install(self, *flags, check=True):
        env = {'HOME': str(self.home), 'PATH': str(self.bin), 'SHELL': '/bin/zsh', 'LANG': 'C'}
        result = subprocess.run(['/bin/bash', str(ROOT / 'install.sh'), *flags], env=env, capture_output=True,
                                text=True, timeout=120, start_new_session=True)  # no terminal to ask on
        if check:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def tree(self):
        """Every path under HOME: links by target, files by content."""
        state = {}
        for path in sorted(self.home.rglob('*')):
            key = str(path.relative_to(self.home))
            if path.is_symlink():
                state[key] = 'link:' + os.readlink(path)
            elif path.is_file():
                state[key] = hashlib.sha256(path.read_bytes()).hexdigest()
            else:
                state[key] = 'dir'
        return state

    def backups(self):
        return list((self.home / '.local/state/tmux-workspace/backups').glob('install-*/manifest'))

    def test_fresh_install_links_everything(self):
        out = self.install('--yes').stdout
        self.assertEqual((self.home / '.tmux.conf').resolve(), ROOT / 'tmux.conf')
        self.assertEqual((self.home / '.local/bin/tmux-workspace').resolve(), ROOT / 'bin/tmux-workspace')
        self.assertEqual((self.home / '.local/bin/tw-clip').resolve(), ROOT / 'bin/tw-clip')
        self.assertEqual((self.home / '.config/tmux-workspace/agent-status.conf').resolve(), ROOT / 'agent-status.conf')
        config = self.home / '.config/tmux-workspace/config.json'
        self.assertEqual(config.stat().st_mode & 0o777, 0o600)
        self.assertIn(BLOCK, (self.home / '.zshrc').read_text())
        self.assertIn('tmux-workspace installed', out)
        self.assertFalse(self.backups())

    def test_second_run_changes_nothing(self):
        self.install('--yes')
        before = self.tree()
        out = self.install('--yes').stdout
        self.assertIn('already installed', out)
        self.assertEqual(self.tree(), before)

    def test_existing_tmux_conf_is_backed_up_and_restored_byte_for_byte(self):
        original = b'set -g prefix C-b\n# mine\n'
        (self.home / '.tmux.conf').write_bytes(original)
        self.install('--yes')
        self.assertEqual((self.home / '.tmux.conf').resolve(), ROOT / 'tmux.conf')
        self.assertEqual(len(self.backups()), 1)
        self.install('--uninstall')
        self.assertFalse((self.home / '.tmux.conf').is_symlink())
        self.assertEqual((self.home / '.tmux.conf').read_bytes(), original)

    def test_existing_tmux_conf_is_kept_without_yes_or_a_terminal(self):
        (self.home / '.tmux.conf').write_text('mine\n')
        out = self.install().stdout
        self.assertEqual((self.home / '.tmux.conf').read_text(), 'mine\n')
        self.assertIn('rerun with --yes', out)

    def test_link_into_same_clone_is_not_backed_up(self):
        (self.home / '.tmux.conf').symlink_to(ROOT / 'tmux.conf')
        self.install('--yes')
        self.assertFalse(self.backups())

    def test_link_to_other_location_is_backed_up_and_restored(self):
        other = self.home / 'dotfiles' / 'tmux.conf'
        other.parent.mkdir()
        other.write_text('other\n')
        (self.home / '.tmux.conf').symlink_to(other)
        self.install('--yes')
        self.assertEqual((self.home / '.tmux.conf').resolve(), ROOT / 'tmux.conf')
        self.assertIn('\tlink', self.backups()[0].read_text())
        self.install('--uninstall')
        self.assertEqual(os.readlink(self.home / '.tmux.conf'), str(other))

    def test_symlinked_zshrc_keeps_its_link(self):
        target = self.home / 'dotfiles' / 'zshrc'
        target.parent.mkdir()
        target.write_text('export EDITOR=nvim\n')
        (self.home / '.zshrc').symlink_to(target)
        self.install('--yes')
        self.assertTrue((self.home / '.zshrc').is_symlink())
        self.assertIn(BLOCK, target.read_text())
        self.install('--uninstall')
        self.assertTrue((self.home / '.zshrc').is_symlink())
        self.assertEqual(target.read_text().strip(), 'export EDITOR=nvim')

    def test_shell_block_is_added_once(self):
        self.install('--yes')
        self.install('--yes')
        self.assertEqual((self.home / '.zshrc').read_text().count(BLOCK), 1)

    def test_rc_that_sources_the_legacy_shim_is_left_alone(self):
        rc = self.home / '.zshrc'
        rc.write_text('source ~/Projects/tmux-workspace/shell.zsh\n')
        self.install('--yes')
        self.assertNotIn(BLOCK, rc.read_text())

    def test_bashrc_is_not_created_for_a_zsh_user(self):
        self.install('--yes')
        self.assertFalse((self.home / '.bashrc').exists())

    def test_dry_run_writes_nothing(self):
        (self.home / '.tmux.conf').write_text('mine\n')
        before = self.tree()
        out = self.install('--yes', '--dry-run').stdout
        self.assertEqual(self.tree(), before)
        self.assertIn('would', out)
        self.assertIn('link', out)

    def test_missing_tmux_fails_with_hint(self):
        (self.bin / 'tmux').unlink()
        result = self.install('--yes', check=False)
        self.assertEqual(result.returncode, 3)
        self.assertIn('needs tmux 3.3+', result.stdout + result.stderr)
        self.assertFalse((self.home / '.tmux.conf').exists())

    def test_old_tmux_fails_with_the_floor(self):
        self.tmux('3.2a')
        result = self.install('--yes', check=False)
        self.assertEqual(result.returncode, 3)
        self.assertIn('tmux 3.2 is older than 3.3', result.stdout + result.stderr)

    def test_floor_matches_python(self):
        self.assertIn('TMUX_MIN=%d.%d\n' % paths.TMUX_MIN, (ROOT / 'install.sh').read_text())

    def test_unknown_flag_exits_2(self):
        self.assertEqual(self.install('--frobnicate', check=False).returncode, 2)

    def test_projects_flag_onboards_without_a_prompt(self):
        self.install('--yes', '--projects', str(self.home / 'code'))
        config = json.loads((self.home / '.config/tmux-workspace/config.json').read_text())
        self.assertEqual(config['projects_dir'], '~/code')
        self.assertTrue((self.home / 'code').is_dir())

    def test_unattended_install_leaves_onboarding_to_the_first_work(self):
        out = self.install('--yes').stdout
        config = json.loads((self.home / '.config/tmux-workspace/config.json').read_text())
        self.assertNotIn('projects_dir', config)
        self.assertIn('the first `work` asks', out)

    def test_uninstall_removes_links_and_block_and_keeps_config(self):
        self.install('--yes')
        self.install('--uninstall')
        self.assertFalse((self.home / '.tmux.conf').exists())
        self.assertFalse((self.home / '.local/bin/tmux-workspace').exists())
        self.assertNotIn(BLOCK, (self.home / '.zshrc').read_text())
        self.assertTrue((self.home / '.config/tmux-workspace/config.json').exists())

    def test_agents_flag_registers_claude_when_present(self):
        (self.home / '.claude').mkdir()
        self.install('--yes', '--agents')
        settings = json.loads((self.home / '.claude/settings.json').read_text())
        self.assertIn(' agent-hook claude-status', settings['statusLine']['command'])

    def logging_tmux(self, running=True):
        """A tmux stub that records its calls and, if running, has a server."""
        self.tmux_log = self.home.parent / (self.home.name + '-tmux.log')
        self.addCleanup(lambda: self.tmux_log.unlink(missing_ok=True))
        self.stub('tmux', f'echo "$*" >> {self.tmux_log}\n[ "$1" = -V ] && {{ echo "tmux 3.5a"; exit 0; }}\n'
                          f'[ "$1" = info ] && exit {0 if running else 1}\nexit 0')

    def test_uninstall_tells_a_running_server_to_stop_using_the_removed_commands(self):
        self.install('--yes')
        self.logging_tmux()
        out = self.install('--uninstall').stdout
        calls = self.tmux_log.read_text()
        self.assertIn('set-option -gu default-command', calls)
        self.assertIn('set-hook -gu session-created', calls)
        self.assertIn('restart tmux', out)

    def test_uninstall_removes_agent_integrations(self):
        (self.home / '.claude').mkdir()
        (self.home / '.claude/settings.json').write_text(json.dumps({'statusLine': {'type': 'command', 'command': 'mine'}}))
        self.install('--yes', '--agents')
        self.install('--uninstall')
        settings = json.loads((self.home / '.claude/settings.json').read_text())
        self.assertNotIn(' agent-hook ', json.dumps(settings))
        self.assertNotIn('agent-hook.py', json.dumps(settings))
        self.assertEqual(settings['statusLine']['command'], 'mine')
        self.assertFalse((self.home / '.local/lib/tmux-workspace/agent-hook.py').exists())

    def test_existing_sesh_config_is_kept_without_yes(self):
        self.stub('sesh')
        mine = self.home / '.config/sesh/sesh.toml'
        mine.parent.mkdir(parents=True)
        mine.write_text('mine\n')
        out = self.install().stdout
        self.assertEqual(mine.read_text(), 'mine\n')
        self.assertFalse(mine.is_symlink())
        self.assertIn('sesh.toml exists', out)
        self.install('--yes')
        self.assertEqual(mine.resolve(), ROOT / 'sesh.toml')
        self.install('--uninstall')
        self.assertEqual(mine.read_text(), 'mine\n')

    def test_deps_installs_missing_recommended_tools(self):
        (self.bin / 'fzf').unlink()
        brew = self.home.parent / (self.home.name + '-brew.log')
        self.addCleanup(lambda: brew.unlink(missing_ok=True))
        self.stub('brew', f'echo "$*" >> {brew}')
        self.install('--deps', '--yes')
        self.assertIn('install fzf zoxide', brew.read_text())

    def test_cancelling_the_projects_question_still_finishes_the_install(self):
        import fcntl
        import pty
        import termios
        master, slave = pty.openpty()

        def controlling_terminal():  # the pty becomes /dev/tty for install.sh
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        env = {'HOME': str(self.home), 'PATH': str(self.bin), 'SHELL': '/bin/zsh', 'LANG': 'C'}
        proc = subprocess.Popen(['/bin/bash', str(ROOT / 'install.sh')], stdin=slave, stdout=slave, stderr=slave,
                                env=env, preexec_fn=controlling_terminal)
        os.close(slave)
        output = b''
        try:
            while b'Where do you keep your projects?' not in output:
                chunk = os.read(master, 4096)
                if not chunk:
                    break
                output += chunk
            os.write(master, b'\x04')  # Ctrl+D
            while True:
                try:
                    chunk = os.read(master, 4096)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
        finally:
            proc.wait(timeout=60)
            os.close(master)
        self.assertEqual(proc.returncode, 0, output.decode(errors='replace'))
        self.assertIn(b'tmux-workspace installed', output)

    def test_script_is_valid_bash_32_syntax(self):
        subprocess.run(['/bin/bash', '-n', str(ROOT / 'install.sh')], check=True)


if __name__ == '__main__':
    unittest.main()
