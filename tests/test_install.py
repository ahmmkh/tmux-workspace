"""install.sh on a temporary HOME. PATH holds stubs and core utilities only, never the machine's tools.

Nothing here downloads from GitHub: releases come from TMUX_WORKSPACE_DIST, a folder that scripts/package.sh
fills with the binary `make build` made."""
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'legacy' / 'lib'))
import paths  # noqa: E402

CORE = ['uname', 'id', 'dirname', 'basename', 'readlink', 'mkdir', 'ln', 'mv', 'cp', 'chmod', 'grep', 'awk',
        'sed', 'tr', 'date', 'find', 'sort', 'mktemp', 'cat', 'rm', 'env', 'bash', 'sh', 'ls', 'head', 'base64',
        'tar', 'gzip', 'sha256sum', 'shasum', 'cmp']
BLOCK = '# >>> tmux-workspace >>>'
BINARY = ROOT / 'bin' / 'tmux-workspace'
VERSION = re.search(r'^version = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M).group(1)
TARGETS = {('Darwin', 'arm64'): 'aarch64-apple-darwin', ('Darwin', 'x86_64'): 'x86_64-apple-darwin',
           ('Linux', 'x86_64'): 'x86_64-unknown-linux-musl', ('Linux', 'aarch64'): 'aarch64-unknown-linux-musl',
           ('Linux', 'arm64'): 'aarch64-unknown-linux-musl'}
TARGET = TARGETS.get((platform.system(), platform.machine()), 'unknown')
ARCHIVE = f'tmux-workspace-{TARGET}.tar.gz'


def package(outdir):
    """scripts/package.sh for this machine's target, with the binary `make build` made instead of a release build."""
    env = {key: value for key, value in os.environ.items() if not key.startswith('TMUX')}
    env['TMUX_WORKSPACE_BINARY'] = str(BINARY)
    return subprocess.run(['sh', str(ROOT / 'scripts' / 'package.sh'), TARGET, str(outdir)], env=env,
                          capture_output=True, text=True, timeout=120)


_DIST = []


def release_dist():
    """The release folder scripts/package.sh makes: the archive, SHA256SUMS and VERSION. Made once."""
    if not _DIST:
        if not BINARY.is_file():
            raise unittest.SkipTest('bin/tmux-workspace is not built: run make build (make test does)')
        tmp = Path(tempfile.mkdtemp()).resolve()
        unittest.addModuleCleanup(shutil.rmtree, tmp, True)
        result = package(tmp / 'dist')
        assert result.returncode == 0, result.stdout + result.stderr
        _DIST.append(tmp / 'dist')
    return _DIST[0]


def renumbered_dist(version, dest):
    """The release of release_dist() as version VERSION: its folder, VERSION file and checksums."""
    old_top, new_top = f'tmux-workspace-{VERSION}', f'tmux-workspace-{version}'
    dest.mkdir(parents=True)
    with tarfile.open(release_dist() / ARCHIVE) as source, tarfile.open(dest / ARCHIVE, 'w:gz') as copy:
        for member in source.getmembers():
            data = source.extractfile(member).read() if member.isfile() else None
            member.name = new_top + member.name[len(old_top):]
            if member.name == f'{new_top}/VERSION':
                data = (version + '\n').encode()
                member.size = len(data)
            copy.addfile(member, io.BytesIO(data) if data is not None else None)
    digest = hashlib.sha256((dest / ARCHIVE).read_bytes()).hexdigest()
    (dest / 'SHA256SUMS').write_text(f'{digest}  {ARCHIVE}\n')
    (dest / 'VERSION').write_text(version + '\n')
    return dest


class Package(unittest.TestCase):
    """scripts/package.sh TARGET OUTDIR: the archive a release publishes for one target."""

    def test_archive_has_the_release_layout(self):
        top = f'tmux-workspace-{VERSION}'
        with tarfile.open(release_dist() / ARCHIVE) as tar:
            members = {member.name.rstrip('/'): member for member in tar.getmembers()}
            version = tar.extractfile(f'{top}/VERSION').read()
            binary = tar.extractfile(f'{top}/bin/tmux-workspace').read()
        names = set(members)
        self.assertTrue(all(name == top or name.startswith(top + '/') for name in names), sorted(names))
        for path in ['bin/tmux-workspace', 'bin/tw-clip', 'bin/tw-sound', 'tmux.conf', 'agent-status.conf',
                     'sesh.toml', 'shell/work.sh', 'shell/work.fish', 'shell.zsh', 'integrations/agent-hook.py',
                     'integrations/claude-hooks.json', 'integrations/codex-hooks.json',
                     'integrations/tmux-workspace.ts', 'legacy/bin/tmux-workspace', 'legacy/lib/paths.py',
                     'legacy/integrations/agent-hook.py', 'config.example.json', 'install.sh', 'README.md',
                     'LICENSE', 'CHANGELOG.md', 'WORKSPACE_GUIDE.md', 'VERSION']:
            self.assertIn(f'{top}/{path}', names)
        self.assertFalse([name for name in names if '__pycache__' in name or name.endswith('.pyc')])
        self.assertEqual(version, (VERSION + '\n').encode())
        self.assertEqual(binary, BINARY.read_bytes())
        for path in ['bin/tmux-workspace', 'bin/tw-clip', 'bin/tw-sound', 'install.sh']:
            self.assertTrue(members[f'{top}/{path}'].mode & 0o111, path)

    def test_modes_do_not_depend_on_the_umask_of_the_build(self):
        executables = {'bin/tmux-workspace', 'bin/tw-clip', 'bin/tw-sound', 'install.sh', 'legacy/bin/tmux-workspace'}
        with tarfile.open(release_dist() / ARCHIVE) as tar:
            for member in tar.getmembers():
                path = member.name.split('/', 1)[1] if '/' in member.name.rstrip('/') else ''
                wanted = 0o755 if member.isdir() or path in executables else 0o644
                self.assertEqual(oct(member.mode & 0o777), oct(wanted), member.name)

    def test_checksums_are_in_sha256sum_format(self):
        dist = release_dist()
        digest = hashlib.sha256((dist / ARCHIVE).read_bytes()).hexdigest()
        self.assertEqual((dist / 'SHA256SUMS').read_text(), f'{digest}  {ARCHIVE}\n')
        self.assertEqual((dist / 'VERSION').read_text(), VERSION + '\n')

    def test_packaging_again_replaces_the_checksum_of_that_archive(self):
        out = Path(self.enterContext(tempfile.TemporaryDirectory()))
        (out / 'SHA256SUMS').write_text(f'{"0" * 64}  tmux-workspace-other.tar.gz\n{"1" * 64}  {ARCHIVE}\n')
        result = package(out)
        self.assertEqual(result.returncode, 0, result.stderr)
        digest = hashlib.sha256((out / ARCHIVE).read_bytes()).hexdigest()
        self.assertEqual((out / 'SHA256SUMS').read_text(),
                         f'{"0" * 64}  tmux-workspace-other.tar.gz\n{digest}  {ARCHIVE}\n')

    def test_a_missing_target_is_a_usage_error(self):
        result = subprocess.run(['sh', str(ROOT / 'scripts' / 'package.sh')], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('usage: scripts/package.sh TARGET [OUTDIR]', result.stderr)


def copy_tree(dest):
    """The working tree as a clone has it: tracked and new files, none that git ignores (no binary, no target/)."""
    listed = subprocess.run(['git', '-C', str(ROOT), 'ls-files', '-z', '--cached', '--others', '--exclude-standard'],
                            capture_output=True, check=True).stdout.decode().split('\0')
    for name in filter(None, listed):
        source = ROOT / name
        if source.is_file():
            (dest / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, dest / name)
    return dest


class Installer(unittest.TestCase):
    def setUp(self):
        # install.sh links the binary that make build copies here, and runs it for the agent integrations.
        if not BINARY.is_file():
            self.fail('bin/tmux-workspace is not built: run make build (make test does)')
        self.home = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(shutil.rmtree, self.home, True)
        self.bin = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(shutil.rmtree, self.bin, True)
        # TMPDIR of install.sh: what it downloads goes here, and must be gone when it is done.
        self.tmp = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        # Files of the test itself, outside HOME and TMPDIR.
        self.work = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(shutil.rmtree, self.work, True)
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
        path.unlink(missing_ok=True)  # never write through a link to the real program
        path.write_text('#!/bin/sh\n' + body + '\n')
        path.chmod(0o755)

    def tmux(self, version):
        # -V reports the version; every other call fails, as when no server runs.
        self.stub('tmux', f'[ "$1" = -V ] && {{ echo "tmux {version}"; exit 0; }}\nexit 1')

    def env(self, **extra):
        """The environment of install.sh: this HOME and PATH, and the built binary in place of a build.
        A value of None leaves that variable out."""
        env = {'HOME': str(self.home), 'PATH': str(self.bin), 'SHELL': '/bin/zsh', 'LANG': 'C',
               'TMPDIR': str(self.tmp), 'TMUX_WORKSPACE_BINARY': str(BINARY)}
        env.update(extra)
        return {key: value for key, value in env.items() if value is not None}

    def run_script(self, argv, check, script_input=None, **extra):
        result = subprocess.run(argv, env=self.env(**extra), input=script_input, capture_output=True, text=True,
                                timeout=120, start_new_session=True)  # no terminal to ask on
        if check:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def install(self, *flags, check=True, **extra):
        return self.run_script(['/bin/bash', str(ROOT / 'install.sh'), *flags], check, **extra)

    def piped(self, *flags, check=True, **extra):
        """install.sh the way `curl … | bash` runs it: the script on standard input, no clone around it."""
        extra.setdefault('TMUX_WORKSPACE_BINARY', None)
        return self.run_script(['/bin/bash', '-s', '--', *flags], check, (ROOT / 'install.sh').read_text(), **extra)

    @property
    def share(self):
        return self.home / '.local/share/tmux-workspace'

    def tool(self, *args, **extra):
        """Run the installed ~/.local/bin/tmux-workspace."""
        return subprocess.run([str(self.home / '.local/bin/tmux-workspace'), *args], env=self.env(**extra),
                              capture_output=True, text=True, timeout=60, start_new_session=True)

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
        proc = subprocess.Popen(['/bin/bash', str(ROOT / 'install.sh')], stdin=slave, stdout=slave, stderr=slave,
                                env=self.env(), preexec_fn=controlling_terminal)
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

    # -- Releases: a piped install downloads, checks and unpacks the archive of this platform. --------------

    def corrupt_dist(self):
        """A copy of the release whose SHA256SUMS does not match its archive."""
        dist = self.work / 'corrupt'
        shutil.copytree(release_dist(), dist)
        sums = (dist / 'SHA256SUMS').read_text()
        (dist / 'SHA256SUMS').write_text(('1' if sums[0] != '1' else '2') + sums[1:])
        return dist

    def test_remote_install_verifies_and_extracts_the_archive(self):
        out = self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist())).stdout
        current = self.share / 'current'
        self.assertEqual(os.readlink(current), VERSION)
        self.assertEqual((self.share / VERSION / 'VERSION').read_text(), VERSION + '\n')
        self.assertEqual((self.share / VERSION / 'bin/tmux-workspace').read_bytes(), BINARY.read_bytes())
        self.assertEqual(os.readlink(self.home / '.local/bin/tmux-workspace'), str(current / 'bin/tmux-workspace'))
        self.assertEqual(os.readlink(self.home / '.local/bin/tw-clip'), str(current / 'bin/tw-clip'))
        self.assertEqual(os.readlink(self.home / '.tmux.conf'), str(current / 'tmux.conf'))
        self.assertEqual(os.readlink(self.home / '.config/tmux-workspace/agent-status.conf'),
                         str(current / 'agent-status.conf'))
        self.assertIn(f'. "{current}/shell/work.sh"', (self.home / '.zshrc').read_text())
        topic = self.tool('help-topic', 'park')
        self.assertEqual(topic.returncode, 0, topic.stderr)
        self.assertIn('Park Pane', topic.stdout)
        self.assertIn('tmux-workspace installed', out)
        self.assertEqual(list(self.tmp.iterdir()), [], 'the download is cleaned up')

    def test_a_second_remote_install_changes_nothing(self):
        self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist()))
        before = self.tree()
        out = self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist())).stdout
        self.assertIn('already installed', out)
        self.assertEqual(self.tree(), before)

    def test_a_newer_release_replaces_the_older_and_keeps_one_before_it(self):
        for version in ('2.0.1', '2.0.2', '2.0.3'):
            self.piped('--yes', TMUX_WORKSPACE_DIST=str(renumbered_dist(version, self.work / version)))
        self.assertEqual(os.readlink(self.share / 'current'), '2.0.3')
        self.assertEqual(sorted(path.name for path in self.share.iterdir()), ['2.0.2', '2.0.3', 'current'])
        self.assertEqual((self.home / '.local/bin/tmux-workspace').resolve(), self.share / '2.0.3/bin/tmux-workspace')

    def test_checksum_mismatch_writes_nothing(self):
        result = self.piped('--yes', check=False, TMUX_WORKSPACE_DIST=str(self.corrupt_dist()))
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn(f'checksum mismatch for {ARCHIVE}; nothing was installed', result.stderr)
        self.assertEqual(self.tree(), {}, 'no ~/.local/bin/tmux-workspace, no share folder, nothing')
        self.assertEqual(list(self.tmp.iterdir()), [])

    def test_unsupported_platform_without_cargo_exits_3(self):
        self.stub('uname', 'case "$1" in -m) echo sparc ;; *) echo SunOS ;; esac')
        message = 'no prebuilt binary for SunOS/sparc; install Rust (https://rustup.rs) and rerun'
        for result in (self.piped('--yes', check=False), self.install('--yes', check=False, TMUX_WORKSPACE_BINARY=None)):
            self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
            self.assertIn(message, result.stderr)
            self.assertEqual(self.tree(), {})

    def test_missing_downloader_exits_3(self):
        self.assertIsNone(shutil.which('curl', path=str(self.bin)))
        self.assertIsNone(shutil.which('wget', path=str(self.bin)))
        result = self.piped('--yes', check=False)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn('need curl or wget to download tmux-workspace', result.stderr)
        self.assertEqual(self.tree(), {})

    def recording_curl(self):
        """A curl that writes down its arguments and fails, as without a network."""
        log = self.work / 'curl.log'
        self.stub('curl', f'printf "%s\\n" "$@" >> "{log}"\nexit 22')
        return log

    def urls(self, log):
        return [arg for arg in log.read_text().splitlines() if '://' in arg]

    def test_version_flag_selects_the_release(self):
        log = self.recording_curl()
        result = self.piped('--yes', '--version', 'v9.9.9', check=False)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertEqual(self.urls(log)[0],
                         f'https://github.com/ahmmkh/tmux-workspace/releases/download/v9.9.9/{ARCHIVE}')
        self.assertEqual(self.tree(), {})

    def test_without_a_version_the_latest_release_is_fetched(self):
        log = self.recording_curl()
        self.assertEqual(self.piped('--yes', check=False).returncode, 3)
        self.assertEqual(self.urls(log)[0],
                         f'https://github.com/ahmmkh/tmux-workspace/releases/latest/download/{ARCHIVE}')

    def test_a_malformed_version_is_a_usage_error(self):
        result = self.piped('--version', 'latest', check=False)
        self.assertEqual(result.returncode, 2)
        self.assertIn('--version needs a release like v2.0.0', result.stderr)

    def test_remote_dry_run_writes_nothing(self):
        out = self.piped('--yes', '--dry-run', TMUX_WORKSPACE_DIST=str(release_dist())).stdout
        self.assertEqual(self.tree(), {})
        self.assertIn(f'would    unpack tmux-workspace {VERSION} to {self.share}/{VERSION}', out)
        self.assertIn(f'would    link {self.home}/.local/bin/tmux-workspace -> {self.share}/current/bin/tmux-workspace',
                      out)
        self.assertEqual(list(self.tmp.iterdir()), [])

    def test_a_release_install_uninstalls(self):
        self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist()))
        result = self.run_script(['/bin/bash', str(self.share / 'current/install.sh'), '--uninstall'], True)
        self.assertIn('tmux-workspace removed', result.stdout)
        self.assertFalse(os.path.lexists(self.home / '.local/bin/tmux-workspace'))
        self.assertFalse(os.path.lexists(self.home / '.tmux.conf'))
        self.assertFalse(os.path.lexists(self.home / '.config/tmux-workspace/agent-status.conf'))
        self.assertNotIn(BLOCK, (self.home / '.zshrc').read_text())
        self.assertTrue((self.share / 'current').is_dir(), 'the program files stay, as a clone did')

    def test_a_stray_stage_variable_removes_nothing(self):
        self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist()))
        mine = self.work / 'mine'
        (mine / 'release').mkdir(parents=True)
        (mine / 'release' / 'notes').write_text('keep\n')
        self.run_script(['/bin/bash', str(self.share / 'current/install.sh'), '--yes'], True,
                        TMUX_WORKSPACE_STAGE=str(mine))
        self.assertEqual((mine / 'release' / 'notes').read_text(), 'keep\n')

    def test_a_piped_uninstall_finds_the_release_install(self):
        self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist()))
        out = self.piped('--uninstall').stdout
        self.assertIn('tmux-workspace removed', out)
        self.assertFalse(os.path.lexists(self.home / '.local/bin/tmux-workspace'))

    def test_python_is_optional(self):
        (self.bin / 'python3').unlink()
        out = self.install('--yes').stdout
        self.assertIn('python3 3.9+ not found: dash, ai and cleanup need it until they are ported', out)
        self.assertIn('tmux-workspace installed', out)

    def test_an_old_python_only_warns(self):
        (self.bin / 'python3').unlink()
        self.stub('python3', 'echo 3.8')
        out = self.install('--yes').stdout
        self.assertIn('python3 3.9+ not found', out)
        self.assertIn('tmux-workspace installed', out)

    def test_deps_no_longer_installs_python(self):
        (self.bin / 'python3').unlink()
        brew = self.work / 'brew.log'
        self.stub('brew', f'echo "$*" >> {brew}')
        self.install('--deps', '--yes')
        self.assertNotIn('python', brew.read_text())

    # -- A clone: the binary is given, built with cargo, or downloaded for the version in Cargo.toml. -------

    def clone(self):
        """A copy of this checkout without its binary, as a fresh clone is."""
        return copy_tree(self.work / 'clone')

    def test_checkout_copies_the_given_binary_into_bin(self):
        clone = self.clone()
        script = ['/bin/bash', str(clone / 'install.sh'), '--yes']
        self.run_script(script, True)
        self.assertEqual((clone / 'bin/tmux-workspace').read_bytes(), BINARY.read_bytes())
        self.assertEqual((self.home / '.local/bin/tmux-workspace').resolve(), clone / 'bin/tmux-workspace')
        self.assertIn('already installed', self.run_script(script, True).stdout)

    def test_checkout_without_cargo_downloads_its_version_once(self):
        clone = self.clone()
        script = ['/bin/bash', str(clone / 'install.sh'), '--yes']
        self.run_script(script, True, TMUX_WORKSPACE_BINARY=None, TMUX_WORKSPACE_DIST=str(release_dist()))
        self.assertEqual((clone / 'bin/tmux-workspace').read_bytes(), BINARY.read_bytes())
        self.assertEqual(list(self.tmp.iterdir()), [])
        # The binary of that version is there: a second run needs no download.
        out = self.run_script(script, True, TMUX_WORKSPACE_BINARY=None,
                              TMUX_WORKSPACE_DIST=str(self.work / 'nowhere')).stdout
        self.assertIn('already installed', out)

    def test_checkout_with_cargo_builds_the_release_binary(self):
        clone = self.clone()
        built = clone / 'target/release/tmux-workspace'
        self.stub('cargo', f'echo "$PWD $*" > "{self.work}/cargo.log"\nmkdir -p target/release\n'
                           f'cp "{BINARY}" target/release/tmux-workspace')
        self.run_script(['/bin/bash', str(clone / 'install.sh'), '--yes'], True, TMUX_WORKSPACE_BINARY=None)
        self.assertEqual((self.work / 'cargo.log').read_text(), f'{clone} build --release --locked\n')
        self.assertEqual((clone / 'bin/tmux-workspace').read_bytes(), built.read_bytes())

    def test_script_is_valid_bash_32_syntax(self):
        subprocess.run(['/bin/bash', '-n', str(ROOT / 'install.sh')], check=True)


if __name__ == '__main__':
    unittest.main()
