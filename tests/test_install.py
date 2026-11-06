"""install.sh on a temporary HOME. PATH holds stubs and core utilities only, never the machine's tools.

Nothing here downloads from GitHub: releases come from TMUX_WORKSPACE_DIST, a folder that scripts/package.sh
fills with the debug build `make build` made.

Nothing here writes the checkout's bin/tmux-workspace either: in a clone that is also the live install, tmux
runs it in every new pane. The installer runs from a copy of the checkout (checkout())."""
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import termios
import time
import unittest
import urllib.parse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'legacy' / 'lib'))
import paths  # noqa: E402

CORE = ['uname', 'id', 'dirname', 'basename', 'readlink', 'mkdir', 'ln', 'mv', 'cp', 'chmod', 'grep', 'awk',
        'sed', 'tr', 'date', 'find', 'sort', 'mktemp', 'cat', 'rm', 'env', 'bash', 'sh', 'ls', 'head', 'base64',
        'tar', 'gzip', 'sha256sum', 'shasum', 'cmp']
BLOCK = '# >>> tmux-workspace >>>'
# What an upgrade ends with: terminals that were open before it still have the work command they started with.
RELOAD_SHELLS = 'reload   run exec $SHELL in terminals that are already open, so they get the new work command'
# The debug build in Cargo's target folder, or the binary that TMUX_WORKSPACE_BINARY names.
BINARY = Path(os.environ.get('TMUX_WORKSPACE_BINARY')
              or Path(os.environ.get('CARGO_TARGET_DIR') or ROOT / 'target') / 'debug' / 'tmux-workspace').resolve()
VERSION = re.search(r'^version = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M).group(1)
TARGETS = {('Darwin', 'arm64'): 'aarch64-apple-darwin', ('Darwin', 'x86_64'): 'x86_64-apple-darwin',
           ('Linux', 'x86_64'): 'x86_64-unknown-linux-musl', ('Linux', 'aarch64'): 'aarch64-unknown-linux-musl',
           ('Linux', 'arm64'): 'aarch64-unknown-linux-musl'}
TARGET = TARGETS.get((platform.system(), platform.machine()), 'unknown')
ARCHIVE = f'tmux-workspace-{TARGET}.tar.gz'


def package(outdir):
    """scripts/package.sh for this machine's target, with the debug build instead of a release build."""
    env = {key: value for key, value in os.environ.items() if not key.startswith('TMUX')}
    env['TMUX_WORKSPACE_BINARY'] = str(BINARY)
    return subprocess.run(['sh', str(ROOT / 'scripts' / 'package.sh'), TARGET, str(outdir)], env=env,
                          capture_output=True, text=True, timeout=120)


_DIST = []


def release_dist():
    """The release folder scripts/package.sh makes: the archive, SHA256SUMS and VERSION. Made once."""
    if not _DIST:
        if not BINARY.is_file():
            raise unittest.SkipTest(f'{BINARY} is not built: run make build (make test does)')
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

    def test_members_belong_to_root_whoever_packed_them(self):
        # Unpacked as root, an archive restores its owners: those of the build machine are nobody's here.
        with tarfile.open(release_dist() / ARCHIVE) as tar:
            for member in tar.getmembers():
                self.assertEqual((member.uid, member.gid), (0, 0), member.name)
                self.assertIn(member.uname, ('', 'root'), member.name)
                self.assertIn(member.gname, ('', 'root'), member.name)

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


def git_of_checkout(*args):
    """The output of `git -C ROOT ARGS`. When git fails, say why and what usually fixes it: most tests start from
    checkout(), and a CalledProcessError says neither. In a container that runs as root over a checkout that
    another user made (a CI job's), git refuses with "detected dubious ownership" until the checkout is trusted."""
    result = subprocess.run(['git', '-C', str(ROOT), *args], capture_output=True)
    if result.returncode != 0:
        raise RuntimeError(
            f'git {" ".join(args)} failed in {ROOT} (exit status {result.returncode}): '
            f'{result.stderr.decode(errors="replace").strip()}\n'
            f'These tests need git to read the checkout. If git says "dubious ownership", the checkout belongs to '
            f'another user than the one running the tests: run  git config --global --add safe.directory {ROOT}\n'
            f'If it does not know a commit, the clone is shallow: fetch the whole history (git fetch --unshallow).')
    return result.stdout


def copy_tree(dest):
    """The working tree as a clone has it: tracked and new files, none that git ignores (no binary, no target/)."""
    listed = git_of_checkout('ls-files', '-z', '--cached', '--others', '--exclude-standard').decode().split('\0')
    for name in filter(None, listed):
        source = ROOT / name
        if source.is_file():
            (dest / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, dest / name)
    return dest


_CHECKOUT = []


def checkout():
    """A copy of the checkout with the debug build in its bin/, made once: what the installer tests install
    from. Never the checkout itself, whose bin/tmux-workspace may be the one a live install runs."""
    if not _CHECKOUT:
        tmp = Path(tempfile.mkdtemp()).resolve()
        unittest.addModuleCleanup(shutil.rmtree, tmp, True)
        tree = copy_tree(tmp / 'tmux-workspace')
        shutil.copy2(BINARY, tree / 'bin' / 'tmux-workspace')
        _CHECKOUT.append(tree)
    return _CHECKOUT[0]


# A UTF-8 locale this system has: glyphs and the spinner need one.
UTF8 = 'en_US.UTF-8' if platform.system() == 'Darwin' else 'C.UTF-8'
# Escape sequences that move the cursor, clear or colour: what a person never reads.
CONTROL = re.compile(r'\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[78]')
COLOUR = re.compile(r'\x1b\[[0-9;]*m')


class Terminal:
    """A program on a pseudo-terminal, as a person runs it: the terminal is its controlling one, so /dev/tty works,
    keys go in, and what the terminal shows comes out. SCRIPT, when given, arrives on standard input, as with
    `curl … | bash`; otherwise standard input is the terminal too."""

    def __init__(self, argv, env, script=None, size=(50, 120)):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', size[0], size[1], 0, 0))
        self.before = termios.tcgetattr(slave)

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        self.proc = subprocess.Popen(argv, stdin=slave if script is None else subprocess.PIPE, stdout=slave,
                                     stderr=slave, env=env, preexec_fn=controlling_terminal)
        os.close(slave)
        if script is not None:
            self.proc.stdin.write(script.encode())
            self.proc.stdin.close()
        self.raw = b''
        self.seen = 0
        self.closed = False
        self.returncode = None

    def pump(self, timeout):
        """Read what the terminal shows, for up to TIMEOUT seconds."""
        if self.closed:
            return
        ready, _, _ = select.select([self.master], [], [], timeout)
        if not ready:
            return
        try:
            chunk = os.read(self.master, 65536)
        except OSError:  # EIO: every process that had the terminal is gone
            chunk = b''
        if chunk:
            self.raw += chunk
        else:
            self.closed = True

    @property
    def text(self):
        """Everything shown so far, without escape sequences; a line rewritten after \\r shows its last version."""
        shown = CONTROL.sub('', self.raw.decode(errors='replace').replace('\r\n', '\n'))
        return '\n'.join(line.rsplit('\r', 1)[-1] for line in shown.split('\n'))

    def expect(self, wanted, timeout=60):
        """Wait until WANTED shows after what the last expect found."""
        deadline = time.monotonic() + timeout
        while True:
            at = self.text.find(wanted, self.seen)
            if at >= 0:
                self.seen = at + len(wanted)
                return
            if self.closed or time.monotonic() > deadline:
                raise AssertionError(f'{wanted!r} never showed. The terminal showed:\n{self.text[self.seen:]}')
            self.pump(0.1)

    def idle(self, seconds):
        """Wait SECONDS, reading what the terminal shows meanwhile."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.pump(0.05)

    def press(self, *keys):
        for key in keys:
            os.write(self.master, key.encode())
            time.sleep(0.05)

    def line(self, part):
        """The last line shown that holds PART."""
        found = [line for line in self.text.split('\n') if part in line]
        if not found:
            raise AssertionError(f'no line with {part!r}. The terminal showed:\n{self.text}')
        return found[-1]

    def finish(self, timeout=120):
        """Read until the program exits; its exit status. self.after is the terminal's settings then."""
        deadline = time.monotonic() + timeout
        while not self.closed and time.monotonic() < deadline:
            self.pump(0.1)
            if self.proc.poll() is not None:
                self.pump(0.2)
                break
        self.returncode = self.proc.wait(timeout=max(1, deadline - time.monotonic()))
        self.pump(0.1)
        self.after = termios.tcgetattr(self.master)
        os.close(self.master)
        return self.returncode

    def close(self):
        if self.returncode is None:
            self.proc.kill()
            # Read on meanwhile: a process that exits with output nobody read waits for it on macOS.
            deadline = time.monotonic() + 10
            while self.proc.poll() is None and time.monotonic() < deadline:
                self.pump(0.1)
            self.proc.wait(timeout=10)
            os.close(self.master)


class Installer(unittest.TestCase):
    def setUp(self):
        # install.sh copies the debug build into bin/ of the checkout it runs from, links it, and runs it for
        # the agent integrations.
        if not BINARY.is_file():
            self.fail(f'{BINARY} is not built: run make build (make test does)')
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
        return self.run_script(['/bin/bash', str(checkout() / 'install.sh'), *flags], check, **extra)

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
        self.assertEqual((self.home / '.tmux.conf').resolve(), checkout() / 'tmux.conf')
        self.assertEqual((self.home / '.local/bin/tmux-workspace').resolve(), checkout() / 'bin/tmux-workspace')
        self.assertEqual((self.home / '.local/bin/tw-clip').resolve(), checkout() / 'bin/tw-clip')
        self.assertEqual((self.home / '.config/tmux-workspace/agent-status.conf').resolve(),
                         checkout() / 'agent-status.conf')
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
        self.assertEqual((self.home / '.tmux.conf').resolve(), checkout() / 'tmux.conf')
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
        (self.home / '.tmux.conf').symlink_to(checkout() / 'tmux.conf')
        self.install('--yes')
        self.assertFalse(self.backups())

    def test_link_to_other_location_is_backed_up_and_restored(self):
        other = self.home / 'dotfiles' / 'tmux.conf'
        other.parent.mkdir()
        other.write_text('other\n')
        (self.home / '.tmux.conf').symlink_to(other)
        self.install('--yes')
        self.assertEqual((self.home / '.tmux.conf').resolve(), checkout() / 'tmux.conf')
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

    def test_rust_floor_matches_cargo_toml(self):
        floor = re.search(r'^rust-version = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M).group(1)
        self.assertIn(f'RUST_MIN={floor}\n', (ROOT / 'install.sh').read_text())

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
        self.assertEqual(mine.resolve(), checkout() / 'sesh.toml')
        self.install('--uninstall')
        self.assertEqual(mine.read_text(), 'mine\n')

    def test_deps_installs_missing_recommended_tools(self):
        (self.bin / 'fzf').unlink()
        brew = self.home.parent / (self.home.name + '-brew.log')
        self.addCleanup(lambda: brew.unlink(missing_ok=True))
        self.stub('brew', f'echo "$*" >> {brew}')
        self.install('--deps', '--yes')
        self.assertIn('install fzf zoxide', brew.read_text())

    def test_deps_installs_procps_on_alpine_where_ps_is_busybox(self):
        apk = self.work / 'apk.log'
        self.stub('apk', f'echo "$*" >> {apk}')
        self.stub('sudo', 'exec "$@"')
        self.stub('zoxide')
        self.stub('ps', 'exit 1')  # BusyBox's: no -p, -t, lstart or tpgid, which agent status and save use
        out = self.install('--yes').stdout
        # install.sh puts sudo in front of the package manager, unless it runs as root (a container does).
        sudo = '' if os.geteuid() == 0 else 'sudo '
        self.assertIn(f'procps: agent status and snapshots need its ps ({sudo}apk add procps)', out)
        self.install('--deps', '--yes')
        self.assertEqual(apk.read_text(), 'add -q procps\n')
        apk.unlink()
        self.stub('ps', 'exit 0')  # procps
        out = self.install('--deps', '--yes').stdout
        self.assertFalse(apk.exists())
        self.assertNotIn('procps', out)

    def test_cancelling_the_projects_question_still_finishes_the_install(self):
        import fcntl
        import pty
        import termios
        master, slave = pty.openpty()

        def controlling_terminal():  # the pty becomes /dev/tty for install.sh
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        proc = subprocess.Popen(['/bin/bash', str(checkout() / 'install.sh')], stdin=slave, stdout=slave, stderr=slave,
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

    def test_reinstalling_the_current_version_keeps_the_one_before(self):
        self.release('2.0.0')
        self.release('2.0.1')
        self.release('2.0.1')
        self.assertEqual(sorted(path.name for path in self.share.iterdir()), ['2.0.0', '2.0.1', 'current'])
        self.assertEqual(os.readlink(self.share / 'current'), '2.0.1')

    def test_pruning_spares_what_is_not_an_unpacked_release(self):
        # TMUX_WORKSPACE_HOME may name a folder that holds other things too.
        share = self.home / '.local/share'
        mine = {'other-app': 'not ours', '9.9.9': '9.9.9', '1.2.3': '4.5.6'}
        for name, version in mine.items():
            (share / name).mkdir(parents=True)
            (share / name / 'VERSION').write_text(version + '\n')
        # A folder named like a version, with a VERSION to match, but not a release of ours.
        # And one that is a release but under another name, and a link to a release elsewhere.
        shutil.copytree(self.unpacked('2.0.0'), share / '1.2.3', dirs_exist_ok=True)
        (share / '1.2.3' / 'VERSION').write_text('4.5.6\n')
        (share / '1.0.0').symlink_to(self.unpacked('2.0.0'))
        for version in ('2.0.1', '2.0.2', '2.0.3'):
            self.release(version, TMUX_WORKSPACE_HOME=str(share))
        self.assertEqual(sorted(path.name for path in share.iterdir()),
                         ['1.0.0', '1.2.3', '2.0.2', '2.0.3', '9.9.9', 'current', 'other-app'])
        for name, version in mine.items():
            self.assertEqual((share / name / 'VERSION').read_text(), version + '\n')
        self.assertTrue((share / '1.2.3' / 'bin/tmux-workspace').exists())
        self.assertTrue((self.unpacked('2.0.0') / 'bin/tmux-workspace').exists(), 'the link was not followed')

    def test_a_copy_left_by_an_interrupted_install_is_cleaned_up(self):
        self.release('2.0.0')
        leftover = self.share / '.incoming-99999999'  # no such process
        shutil.copytree(self.share / '2.0.0', leftover)
        running = self.share / f'.incoming-{os.getpid()}'  # a process that runs: it may still be copying
        shutil.copytree(self.share / '2.0.0', running)
        self.release('2.0.1')
        self.assertFalse(leftover.exists())
        self.assertTrue(running.exists())

    def unpacked(self, version):
        """A release of version VERSION unpacked outside HOME."""
        folder = self.work / f'unpacked-{version}'
        if not folder.exists():
            folder.mkdir()
            with tarfile.open(self.newer(version) / ARCHIVE) as tar:
                # Extraction filters came with Python 3.12 and security releases of 3.9 to 3.11; Debian 12's 3.11.2 has none.
                tar.extractall(folder, **({'filter': 'tar'} if hasattr(tarfile, 'tar_filter') else {}))
        return folder / f'tmux-workspace-{version}'

    def test_unpacking_restores_no_owners(self):
        log = self.work / 'tar.log'
        real = shutil.which('tar')
        self.stub('tar', f'printf "%s\\n" "$*" >> "{log}"\nexec "{real}" "$@"')
        self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist()))
        unpack = [line for line in log.read_text().splitlines() if ' -x' in f' {line}']
        self.assertTrue(unpack, log.read_text())
        self.assertTrue(all('--no-same-owner' in line for line in unpack), unpack)
        # Nor does the copy into the install home (cp -p would keep the owners of the download).
        script = (ROOT / 'install.sh').read_text()
        self.assertIn('cp -R "$REPO"', script)
        self.assertNotIn('cp -pR', script)

    def test_checksum_mismatch_writes_nothing(self):
        result = self.piped('--yes', check=False, TMUX_WORKSPACE_DIST=str(self.corrupt_dist()))
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn(f'checksum mismatch for {ARCHIVE}; nothing was installed', result.stderr)
        self.assertEqual(self.tree(), {}, 'no ~/.local/bin/tmux-workspace, no share folder, nothing')
        self.assertEqual(list(self.tmp.iterdir()), [])

    def test_unsupported_platform_without_cargo_exits_3(self):
        self.stub('uname', 'case "$1" in -m) echo sparc ;; *) echo SunOS ;; esac')
        message = 'no prebuilt binary for SunOS/sparc; install Rust (https://rustup.rs) and rerun'
        clone = self.clone()
        checkout = self.run_script(['/bin/bash', str(clone / 'install.sh'), '--yes'], False, TMUX_WORKSPACE_BINARY=None)
        for result in (self.piped('--yes', check=False), checkout):
            self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
            self.assertIn(message, result.stderr)
            self.assertEqual(self.tree(), {})
        self.assertFalse((clone / 'bin/tmux-workspace').exists())

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

    # -- Updates: `tmux-workspace update` runs install.sh --update next to the binary. ----------------------

    def release(self, version, *flags, **extra):
        """A piped install of release VERSION."""
        dist = self.work / version
        if not dist.exists():
            renumbered_dist(version, dist)
        return self.piped('--yes', *flags, TMUX_WORKSPACE_DIST=str(dist), **extra)

    def update(self, *args, check=True, dist=None, **extra):
        """`tmux-workspace update ARGS`, with the releases of DIST."""
        extra.setdefault('TMUX_WORKSPACE_DIST', str(dist or release_dist()))
        result = self.tool('update', *args, **extra)
        if check:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def newer(self, version='2.0.1'):
        dist = self.work / version
        return dist if dist.exists() else renumbered_dist(version, dist)

    def test_update_installs_the_newer_release(self):
        self.release('2.0.0')
        out = self.update(dist=self.newer('2.0.1')).stdout
        self.assertIn('updated 2.0.0 → 2.0.1', out)
        self.assertEqual(os.readlink(self.share / 'current'), '2.0.1')
        self.assertEqual(sorted(path.name for path in self.share.iterdir()), ['2.0.0', '2.0.1', 'current'])
        self.assertEqual((self.home / '.local/bin/tmux-workspace').resolve(), self.share / '2.0.1/bin/tmux-workspace')
        self.assertEqual((self.home / '.tmux.conf').resolve(), self.share / '2.0.1/tmux.conf')
        self.assertEqual(self.tool('help-topic', 'park').returncode, 0)
        self.assertEqual(list(self.tmp.iterdir()), [])

    def test_an_update_to_a_newer_release_says_to_reload_open_terminals(self):
        self.release('2.0.0')
        out = self.update(dist=self.newer('2.0.1')).stdout
        self.assertEqual(out.splitlines()[-2:], ['done     updated 2.0.0 → 2.0.1', RELOAD_SHELLS])

    def test_only_an_upgrade_says_to_reload_open_terminals(self):
        # A first install ends with "next  open a new terminal", which covers it.
        out = self.install('--yes').stdout
        self.assertIn('open a new terminal', out)
        self.assertNotIn('exec $SHELL', out)
        out = self.install('--yes').stdout
        self.assertIn('already installed', out)
        self.assertNotIn('exec $SHELL', out)
        out = self.release('2.0.0').stdout
        self.assertIn('open a new terminal', out)
        self.assertNotIn('exec $SHELL', out)
        out = self.release('2.0.0').stdout
        self.assertIn('already installed', out)
        self.assertNotIn('exec $SHELL', out)
        # Nothing changes when there is nothing newer, or when an update is only asked about or only shown.
        out = self.update(dist=self.newer('2.0.0')).stdout
        self.assertIn('already up to date (2.0.0)', out)
        self.assertNotIn('exec $SHELL', out)
        out = self.update('--check', dist=self.newer('2.0.1')).stdout
        self.assertIn('update available', out)
        self.assertNotIn('exec $SHELL', out)
        out = self.piped('--update', '--dry-run', TMUX_WORKSPACE_DIST=str(self.newer('2.0.1'))).stdout
        self.assertIn('dry run: nothing changed', out)
        self.assertNotIn('exec $SHELL', out)

    def test_update_when_already_up_to_date_changes_nothing(self):
        self.release('2.0.1')
        before = self.tree()
        out = self.update(dist=self.newer('2.0.1')).stdout
        self.assertIn('already up to date (2.0.1)', out)
        self.assertEqual(self.tree(), before)

    def test_update_does_not_go_back_to_an_older_latest(self):
        self.release('2.0.2')
        out = self.update(dist=self.newer('2.0.1')).stdout
        self.assertIn('already up to date (2.0.2)', out)
        self.assertEqual(os.readlink(self.share / 'current'), '2.0.2')

    def test_update_check_changes_nothing(self):
        self.release('2.0.0')
        before = self.tree()
        out = self.update('--check', dist=self.newer('2.0.1')).stdout
        self.assertIn('update available: 2.0.0 → 2.0.1', out)
        self.assertEqual(self.tree(), before)
        self.assertEqual(list(self.tmp.iterdir()), [])

    def test_update_check_reads_the_latest_version_from_the_release(self):
        self.release('2.0.0')
        log = self.recording_curl()
        result = self.update('--check', check=False, TMUX_WORKSPACE_DIST=None)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertEqual(self.urls(log), ['https://github.com/ahmmkh/tmux-workspace/releases/latest/download/VERSION'])

    def test_update_keeps_the_choices_made_at_install(self):
        self.release('2.0.0', '--no-shell', '--no-tmux-conf')
        (self.home / '.config/fish').mkdir(parents=True)  # fish appeared since
        out = self.update(dist=self.newer('2.0.1')).stdout
        self.assertFalse(os.path.lexists(self.home / '.zshrc'))
        self.assertFalse(os.path.lexists(self.home / '.tmux.conf'))
        self.assertFalse(os.path.lexists(self.home / '.config/fish/functions/work.fish'))
        self.assertNotIn('projects folder', out, 'onboarding is not asked again')

    def test_update_registers_the_integrations_again_only_where_they_were(self):
        (self.home / '.claude').mkdir()
        self.release('2.0.0')
        self.update(dist=self.newer('2.0.1'))
        self.assertFalse((self.home / '.claude/settings.json').exists())

        self.release('2.0.1', '--agents')
        settings = self.home / '.claude/settings.json'
        data = json.loads(settings.read_text())
        del data['statusLine']
        settings.write_text(json.dumps(data))
        out = self.update(dist=self.newer('2.0.2')).stdout
        self.assertIn('agents', out)
        self.assertIn(' agent-hook claude-status', json.loads(settings.read_text())['statusLine']['command'])

    def test_integration_links_keep_working_through_two_updates_and_uninstall_removes_them(self):
        # Neovim and omp only: no Claude or Codex hook for the update to notice.
        (self.home / '.config/nvim').mkdir(parents=True)
        (self.home / '.omp').mkdir()
        links = [self.home / '.config/nvim/lua/custom/tmux_workspace.lua',
                 self.home / '.omp/agent/extensions/tmux-workspace.ts']
        if platform.system() == 'Darwin':
            links.append(self.home / 'Library/Application Support/iTerm2/DynamicProfiles/tmux-workspace.json')
        self.release('2.0.0', '--agents')
        for version in ['2.0.1', '2.0.2']:
            self.update(dist=self.newer(version))
            self.assertEqual(os.readlink(self.share / 'current'), version)
            for link in links:
                self.assertTrue(link.is_file(), f'{link} after the update to {version}')
                self.assertTrue(os.readlink(link).startswith(f'{self.share}/current/'), os.readlink(link))
        self.assertFalse((self.share / '2.0.0').exists(), 'the second update removed the first version')
        self.run_script(['/bin/bash', str(self.share / 'current/install.sh'), '--uninstall'], True)
        for link in links:
            self.assertFalse(os.path.lexists(link), link)

    def test_update_reloads_a_running_server_that_uses_its_tmux_conf(self):
        self.release('2.0.0')
        self.logging_tmux()
        out = self.update(dist=self.newer('2.0.1')).stdout
        self.assertIn(f'source-file {self.home}/.tmux.conf', self.tmux_log.read_text())
        self.assertIn('reload', out)

    def test_update_leaves_a_tmux_conf_of_the_users_alone(self):
        self.release('2.0.0', '--no-tmux-conf')
        (self.home / '.tmux.conf').write_text('mine\n')
        self.logging_tmux()
        self.update(dist=self.newer('2.0.1'))
        self.assertNotIn('source-file', self.tmux_log.read_text())

    def test_a_piped_update_is_the_update_of_the_installed_release(self):
        self.release('2.0.0')
        before = self.tree()
        out = self.piped('--update', '--check', TMUX_WORKSPACE_DIST=str(self.newer('2.0.1'))).stdout
        self.assertIn('update available: 2.0.0 → 2.0.1', out)
        self.assertEqual(self.tree(), before)
        out = self.piped('--update', TMUX_WORKSPACE_DIST=str(self.newer('2.0.1'))).stdout
        self.assertIn('updated 2.0.0 → 2.0.1', out)

    def test_a_piped_update_without_an_install_says_so(self):
        result = self.piped('--update', check=False, TMUX_WORKSPACE_DIST=str(release_dist()))
        self.assertEqual(result.returncode, 1)
        self.assertIn('nothing to update', result.stderr)
        self.assertEqual(self.tree(), {})

    def test_update_flags_are_checked(self):
        self.assertEqual(self.install('--check', check=False).returncode, 2)
        self.assertEqual(self.install('--update', '--uninstall', check=False).returncode, 2)

    # -- A clone updates with git: fast-forward only, and the new binary first. ----------------------------

    def real_git(self):
        (self.bin / 'git').unlink()
        (self.bin / 'git').symlink_to(shutil.which('git'))

    def git(self, cwd, *args):
        env = {'HOME': str(self.work), 'PATH': os.environ['PATH'], 'LANG': 'C', 'GIT_CONFIG_NOSYSTEM': '1',
               'GIT_AUTHOR_NAME': 'Test', 'GIT_AUTHOR_EMAIL': 'test@example.com',
               'GIT_COMMITTER_NAME': 'Test', 'GIT_COMMITTER_EMAIL': 'test@example.com'}
        return subprocess.run(['git', '-C', str(cwd), *args], env=env, capture_output=True, text=True,
                              check=True).stdout.strip()

    def commit(self, repo, fill, message):
        """Replace the files of REPO with what FILL(folder) writes, and commit them."""
        for child in repo.iterdir():
            if child.name != '.git':
                shutil.rmtree(child) if child.is_dir() and not child.is_symlink() else child.unlink()
        fill(repo)
        self.git(repo, 'add', '-A')
        self.git(repo, 'commit', '-q', '-m', message)
        return self.git(repo, 'rev-parse', 'HEAD')

    @staticmethod
    def v2(version):
        def fill(folder):
            copy_tree(folder)
            cargo = folder / 'Cargo.toml'
            cargo.write_text(re.sub(r'^version = ".*"$', f'version = "{version}"', cargo.read_text(), count=1,
                                    flags=re.M))
        return fill

    @staticmethod
    def v1(folder):
        """The Python version, 1.0.0, as a v1 clone has it."""
        archive = git_of_checkout('archive', '30b1401')
        subprocess.run(['tar', '-x', '-C', str(folder)], input=archive, check=True)

    def upstream_and_clone(self, first, clone):
        """An upstream repository whose main has commit FIRST, and a shallow clone of it at CLONE."""
        self.real_git()
        upstream = self.work / 'upstream'
        upstream.mkdir()
        self.git(upstream, 'init', '-q', '-b', 'main')
        self.commit(upstream, first, 'first')
        clone.parent.mkdir(parents=True, exist_ok=True)
        self.git(self.work, 'clone', '-q', '--depth', '1', f'file://{upstream}', str(clone))
        return upstream

    def clone_env(self, **extra):
        return dict(GIT_CONFIG_NOSYSTEM='1', **extra)

    def test_update_of_a_clone_pulls_and_installs_the_new_version(self):
        clone = self.work / 'checkout'
        upstream = self.upstream_and_clone(self.v2('2.0.0'), clone)
        self.run_script(['/bin/bash', str(clone / 'install.sh'), '--yes'], True, **self.clone_env())
        head = self.commit(upstream, self.v2('2.0.1'), 'second')
        check = self.tool('update', '--check', **self.clone_env())
        self.assertIn(f'update available: 2.0.0 {self.git(clone, "rev-parse", "--short", "HEAD")} → 2.0.1 {head[:7]}',
                      check.stdout, check.stderr)
        out = self.tool('update', **self.clone_env())
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual(self.git(clone, 'rev-parse', 'HEAD'), head)
        self.assertIn('updated 2.0.0', out.stdout)
        self.assertIn(f'→ 2.0.1 {head[:7]}', out.stdout)
        self.assertEqual(out.stdout.splitlines()[-1], RELOAD_SHELLS)
        self.assertEqual((clone / 'bin/tmux-workspace').read_bytes(), BINARY.read_bytes())
        again = self.tool('update', **self.clone_env())
        self.assertIn(f'already up to date (2.0.1 {head[:7]})', again.stdout)
        self.assertNotIn('exec $SHELL', again.stdout)

    def test_update_of_a_clone_builds_the_new_version_before_the_pull(self):
        clone = self.work / 'checkout'
        upstream = self.upstream_and_clone(self.v2('2.0.0'), clone)
        self.run_script(['/bin/bash', str(clone / 'install.sh'), '--yes'], True, **self.clone_env())
        before = self.git(clone, 'rev-parse', 'HEAD')
        head = self.commit(upstream, self.v2('2.0.1'), 'second')
        log = self.work / 'cargo.log'
        # A cargo that writes down where it builds, into which target folder, the clone's commit at that
        # moment and the version it builds, then "builds" the binary.
        self.stub('cargo', f'echo "$PWD|$CARGO_TARGET_DIR|$(git -C "{clone}" rev-parse HEAD)|'
                           f'$(sed -n "s/^version = //p" Cargo.toml | head -n 1)|$*" >> "{log}"\n'
                           'mkdir -p "$CARGO_TARGET_DIR/release"\n'
                           f'cp "{BINARY}" "$CARGO_TARGET_DIR/release/tmux-workspace"')
        out = self.tool('update', TMUX_WORKSPACE_BINARY=None, **self.clone_env())
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        builds = log.read_text().splitlines()
        self.assertEqual(len(builds), 1, 'the installer after the pull builds nothing again')
        where, target, head_then, version, args = builds[0].split('|')
        self.assertNotEqual(Path(where), clone, 'built from git archive, outside the clone')
        self.assertEqual(target, str(clone / 'target'))
        self.assertEqual(head_then, before, 'built before the pull')
        self.assertEqual(version, '"2.0.1"', 'the new version is what is built')
        self.assertEqual(args, 'build --release --locked')
        self.assertEqual(self.git(clone, 'rev-parse', 'HEAD'), head)
        self.assertEqual((clone / 'bin/tmux-workspace').read_bytes(), BINARY.read_bytes())

    def test_update_check_of_a_clone_reports_and_names_what_an_update_would_refuse(self):
        clone = self.work / 'checkout'
        upstream = self.upstream_and_clone(self.v2('2.0.0'), clone)
        self.run_script(['/bin/bash', str(clone / 'install.sh'), '--yes'], True, **self.clone_env())
        head = self.commit(upstream, self.v2('2.0.1'), 'second')
        before = self.git(clone, 'rev-parse', 'HEAD')
        (clone / 'tmux.conf').write_text('changed\n')
        result = self.tool('update', '--check', **self.clone_env())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f'update available: 2.0.0 {before[:7]} → 2.0.1 {head[:7]}', result.stdout)
        self.assertIn('has uncommitted changes, which an update refuses', result.stdout)
        self.assertEqual((clone / 'tmux.conf').read_text(), 'changed\n')
        self.git(clone, 'checkout', '-q', '--', 'tmux.conf')
        self.git(clone, 'checkout', '-q', '-b', 'mine')
        result = self.tool('update', '--check', **self.clone_env())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f'update available: 2.0.0 {before[:7]} → 2.0.1 {head[:7]}', result.stdout)
        self.assertIn('is on mine, not main; an update needs main', result.stdout)
        self.assertEqual(self.git(clone, 'rev-parse', 'HEAD'), before)

    def test_update_refuses_a_clone_with_changes_or_on_another_branch(self):
        clone = self.work / 'checkout'
        upstream = self.upstream_and_clone(self.v2('2.0.0'), clone)
        self.run_script(['/bin/bash', str(clone / 'install.sh'), '--yes'], True, **self.clone_env())
        self.commit(upstream, self.v2('2.0.1'), 'second')
        head = self.git(clone, 'rev-parse', 'HEAD')
        (clone / 'tmux.conf').write_text('changed\n')
        result = self.tool('update', **self.clone_env())
        self.assertEqual(result.returncode, 1)
        self.assertIn('has uncommitted changes; commit or stash them', result.stderr)
        self.git(clone, 'checkout', '-q', '--', 'tmux.conf')
        self.git(clone, 'checkout', '-q', '-b', 'mine')
        result = self.tool('update', **self.clone_env())
        self.assertEqual(result.returncode, 1)
        self.assertIn('is on mine, not main', result.stderr)
        self.assertEqual(self.git(clone, 'rev-parse', 'HEAD'), head)

    # -- v1: a piped install cloned to ~/.local/share/tmux-workspace. Rerunning the piped install upgrades
    #    that clone in place. -------------------------------------------------------------------------------

    def v1_install(self, *flags):
        """A v1 piped install: the clone at the install home, set up by its own (Python era) install.sh.
        Returns the upstream repository and the clone's commit."""
        # A checkout that git cannot read at all (dubious ownership) fails here, with its reason, instead of
        # being taken for a clone that lacks the commit.
        git_of_checkout('rev-parse', '--git-dir')
        known = subprocess.run(['git', '-C', str(ROOT), 'cat-file', '-e', '30b1401^{commit}'], capture_output=True)
        if known.returncode != 0:
            self.skipTest('v1 (commit 30b1401) is not in this clone; a shallow checkout needs fetch-depth: 0')
        upstream = self.upstream_and_clone(self.v1, self.share)
        self.run_script(['/bin/bash', str(self.share / 'install.sh'), '--yes', *flags], True, **self.clone_env())
        self.assertEqual((self.home / '.local/bin/tmux-workspace').resolve(), self.share / 'bin/tmux-workspace')
        return upstream, self.git(self.share, 'rev-parse', 'HEAD')

    def test_a_v1_clone_is_upgraded_in_place(self):
        upstream, v1 = self.v1_install()
        head = self.commit(upstream, self.v2(VERSION), 'the Rust port')
        out = self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist()), **self.clone_env()).stdout
        self.assertEqual(self.git(self.share, 'rev-parse', 'HEAD'), head)
        self.assertIn(f'from 1.0.0 {v1[:7]} to {VERSION} {head[:7]}', out)
        self.assertIn('tmux-workspace installed', out, 'the pull changed it, so it is not "already installed"')
        self.assertIn(RELOAD_SHELLS, out.splitlines(), 'terminals that are open still have the work of v1')
        self.assertEqual((self.share / 'bin/tmux-workspace').read_bytes(), BINARY.read_bytes())
        self.assertEqual((self.home / '.local/bin/tmux-workspace').resolve(), self.share / 'bin/tmux-workspace')
        self.assertFalse(os.path.lexists(self.share / 'current'), 'no release unpacked beside the clone')
        self.assertEqual(self.git(self.share, 'status', '--porcelain'), '', 'the binary is not a change')
        topic = self.tool('help-topic', 'park')
        self.assertEqual(topic.returncode, 0, topic.stderr)
        self.assertEqual(list(self.tmp.iterdir()), [])

    def test_v1_hook_entries_keep_working_after_the_upgrade(self):
        (self.home / '.claude').mkdir()
        upstream, _ = self.v1_install('--agents')
        settings = self.home / '.claude/settings.json'
        status_line = json.loads(settings.read_text())['statusLine']['command']
        self.assertIn('agent-hook.py claude-status', status_line)
        self.commit(upstream, self.v2(VERSION), 'the Rust port')
        self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist()), **self.clone_env())
        # The v1 entry, unchanged without --agents, now runs the shim, which runs the binary (the shim
        # itself is tested in tests/cli.rs). Outside a Claude pane the hook says nothing and succeeds.
        self.assertEqual(json.loads(settings.read_text())['statusLine']['command'], status_line)
        helper = self.home / '.local/lib/tmux-workspace/agent-hook.py'
        self.assertEqual(helper.resolve(), self.share / 'integrations/agent-hook.py')
        self.assertEqual(helper.read_text(), (ROOT / 'integrations/agent-hook.py').read_text())
        self.assertEqual((self.home / '.local/bin/tmux-workspace').resolve().read_bytes(), BINARY.read_bytes())
        hook = subprocess.run(['/bin/sh', '-c', status_line], input='{"session_id": "0123456789abcdef"}',
                              env=self.env(), capture_output=True, text=True, timeout=30)
        self.assertEqual((hook.returncode, hook.stdout, hook.stderr), (0, '', ''))

    def test_a_v1_clone_is_not_pulled_when_the_new_binary_cannot_be_had(self):
        upstream, v1 = self.v1_install()
        self.commit(upstream, self.v2(VERSION), 'the Rust port')
        result = self.piped('--yes', check=False, TMUX_WORKSPACE_DIST=str(self.corrupt_dist()), **self.clone_env())
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn('checksum mismatch', result.stderr)
        self.assertEqual(self.git(self.share, 'rev-parse', 'HEAD'), v1)
        self.assertEqual(self.git(self.share, 'status', '--porcelain'), '')
        # The link still leads to the v1 program, which is still there.
        self.assertTrue((self.home / '.local/bin/tmux-workspace').resolve().is_file())

    def test_a_v1_upgrade_takes_the_release_over_a_cargo_that_cannot_build(self):
        upstream, _ = self.v1_install()
        head = self.commit(upstream, self.v2(VERSION), 'the Rust port')
        log = self.failing_cargo()
        self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist()), **self.clone_env())
        self.assertFalse(log.exists(), 'a release fits, so cargo does not run')
        self.assertEqual(self.git(self.share, 'rev-parse', 'HEAD'), head)
        self.assertEqual((self.share / 'bin/tmux-workspace').read_bytes(), BINARY.read_bytes())
        # A later piped run neither builds nor downloads again.
        out = self.piped('--yes', TMUX_WORKSPACE_DIST=str(self.work / 'nowhere'), **self.clone_env()).stdout
        self.assertFalse(log.exists())
        self.assertIn('already installed', out)

    def test_a_v1_upgrade_without_a_release_or_a_working_cargo_changes_nothing(self):
        upstream, v1 = self.v1_install()
        self.commit(upstream, self.v2(VERSION), 'the Rust port')
        log = self.failing_cargo()
        self.stub('uname', 'case "$1" in -m) echo sparc ;; *) echo SunOS ;; esac')
        result = self.piped('--yes', check=False, **self.clone_env())
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertTrue(log.exists(), 'without a release, cargo is tried')
        self.assertIn('no prebuilt binary for SunOS/sparc, and cargo could not build it', result.stderr)
        self.assertIn('https://rustup.rs', result.stderr)
        self.assertIn(f'{self.share} is unchanged', result.stderr)
        self.assertEqual(self.git(self.share, 'rev-parse', 'HEAD'), v1)
        self.assertEqual(self.git(self.share, 'status', '--porcelain'), '')

    def test_the_new_binary_is_in_bin_before_the_pull(self):
        # So that after the pull, which removes the v1 program, only a rename is left to do.
        upstream, _ = self.v1_install()
        self.commit(upstream, self.v2(VERSION), 'the Rust port')
        seen = self.work / 'at-merge'
        hook = self.share / '.git/hooks/post-merge'
        hook.write_text(f'#!/bin/sh\nls bin > "{seen}"\n')
        hook.chmod(0o755)
        self.piped('--yes', TMUX_WORKSPACE_DIST=str(release_dist()), **self.clone_env())
        staged = [name for name in seen.read_text().split() if name.startswith('tmux-workspace.new.')]
        self.assertEqual(len(staged), 1, seen.read_text())
        self.assertEqual(sorted(path.name for path in (self.share / 'bin').iterdir()),
                         ['tmux-workspace', 'tmux-workspace.version', 'tw-clip', 'tw-sound'])

    def test_a_pull_that_fails_leaves_the_old_program_and_no_new_binary(self):
        upstream, v1 = self.v1_install()
        self.commit(upstream, self.v2(VERSION), 'the Rust port')
        (self.share / 'Cargo.toml').write_text('mine\n')  # untracked, and in the way of the pull
        result = self.piped('--yes', check=False, TMUX_WORKSPACE_DIST=str(release_dist()), **self.clone_env())
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn('could not fast-forward', result.stderr)
        self.assertEqual(self.git(self.share, 'rev-parse', 'HEAD'), v1)
        self.assertEqual(sorted(path.name for path in (self.share / 'bin').iterdir()),
                         ['install-integrations', 'tmux-workspace', 'tw-clip', 'tw-sound'])
        self.assertTrue((self.home / '.local/bin/tmux-workspace').resolve().is_file())
        self.assertEqual(list(self.tmp.iterdir()), [])

    def test_a_v1_clone_with_changes_is_left_alone(self):
        upstream, v1 = self.v1_install()
        self.commit(upstream, self.v2(VERSION), 'the Rust port')
        (self.share / 'tmux.conf').write_text('mine\n')
        result = self.piped('--yes', check=False, TMUX_WORKSPACE_DIST=str(release_dist()), **self.clone_env())
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(f'{self.share} has uncommitted changes', result.stderr)
        self.assertEqual(self.git(self.share, 'rev-parse', 'HEAD'), v1)
        self.assertEqual((self.share / 'tmux.conf').read_text(), 'mine\n')

    def failing_cargo(self):
        """A cargo too old for this crate: it writes down that it ran, and fails as Debian 12's 1.63 does."""
        log = self.work / 'cargo.log'
        self.stub('cargo', f'echo "$PWD $*" >> "{log}"\n'
                           'echo "error: failed to parse lock file at: Cargo.lock" >&2\nexit 101')
        return log

    def test_checkout_installs_the_release_when_cargo_cannot_build(self):
        clone = self.clone()
        log = self.failing_cargo()
        out = self.run_script(['/bin/bash', str(clone / 'install.sh'), '--yes'], True, TMUX_WORKSPACE_BINARY=None,
                              TMUX_WORKSPACE_DIST=str(release_dist())).stdout
        self.assertEqual(log.read_text(), f'{clone} build --release --locked\n', 'cargo comes first in a checkout')
        self.assertIn('cargo could not build it', out)
        self.assertIn(f'bin/tmux-workspace (release {VERSION})\n', out)
        self.assertEqual((clone / 'bin/tmux-workspace').read_bytes(), BINARY.read_bytes())
        self.assertEqual((clone / 'bin/tmux-workspace.version').read_text(), VERSION + '\n')

    def test_checkout_without_a_release_or_a_working_cargo_says_what_to_do(self):
        clone = self.clone()
        self.failing_cargo()
        self.stub('uname', 'case "$1" in -m) echo sparc ;; *) echo SunOS ;; esac')
        result = self.run_script(['/bin/bash', str(clone / 'install.sh'), '--yes'], False, TMUX_WORKSPACE_BINARY=None)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn('no prebuilt binary for SunOS/sparc, and cargo could not build it', result.stderr)
        self.assertIn('https://rustup.rs', result.stderr)
        self.assertFalse((clone / 'bin/tmux-workspace').exists())
        self.assertEqual(self.tree(), {})

    def test_script_is_valid_bash_32_syntax(self):
        subprocess.run(['/bin/bash', '-n', str(ROOT / 'install.sh')], check=True)

    # -- On a terminal: a checklist, then a line per step, then a summary. /bin/bash runs it: 3.2 on macOS. ------

    def terminal_tools(self):
        """What a terminal session has beyond CORE: stty to read keys, sleep for the spinner."""
        for tool in ('stty', 'sleep'):
            found = shutil.which(tool)
            if found and not os.path.lexists(self.bin / tool):
                (self.bin / tool).symlink_to(found)

    def guided(self, *flags, script=None, utf8=True, **extra):
        """install.sh of the checkout on a terminal, or, with SCRIPT, piped into bash as `curl … | bash` does."""
        self.terminal_tools()
        extra.setdefault('TERM', 'xterm-256color')
        extra.setdefault('LANG', UTF8 if utf8 else 'C')
        if script is None:
            argv = ['/bin/bash', str(checkout() / 'install.sh'), *flags]
        else:
            argv = ['/bin/bash', '-s', '--', *flags]
        term = Terminal(argv, self.env(**extra), script=script)
        self.addCleanup(term.close)
        return term

    def brew(self, body=''):
        """A Homebrew that writes down what it is asked to install, then runs BODY."""
        log = self.work / 'brew.log'
        self.stub('brew', f'echo "$*" >> "{log}"\n{body}')
        return log

    def machine(self):
        """A machine with brew, git, tmux 3.5a and python3, and Claude Code, but no fzf, zoxide, lazygit or sesh."""
        (self.bin / 'fzf').unlink()
        (self.home / '.claude').mkdir()
        return self.brew()

    def projects_set(self):
        """A config that names the projects folder already, so the checklist does not ask for it."""
        config = self.home / '.config/tmux-workspace/config.json'
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text('{"projects_dir": "~/code"}\n')

    @staticmethod
    def install_from_list(term, projects=''):
        """Enter in the checklist, then PROJECTS for the projects folder (Enter alone takes ~/Projects)."""
        term.press('\r')
        term.expect('Projects folder [~/Projects]: ')
        term.press(projects + '\r')

    def test_the_checklist_shows_what_is_needed_recommended_and_optional(self):
        self.machine()
        term = self.guided()
        term.expect(f'tmux-workspace {VERSION} installer', timeout=30)
        term.expect('What should I set up?')
        term.expect('sesh')
        self.assertRegex(term.line('found:'), r' · zsh · brew · found: tmux 3\.5a, git, python3 3\.\d+$')
        rows = [term.line(name) for name in ('] tmux-workspace', '] tmux config', '] work command', '] fzf',
                                             '] zoxide', '] Claude Code status', '] lazygit', '] sesh')]
        self.assertRegex(rows[0], r'^  \[■\] tmux-workspace +~/\.local/bin/tmux-workspace$')
        self.assertRegex(rows[1], r'^  \[■\] tmux config +~/\.tmux\.conf$')
        self.assertRegex(rows[2], r'^  \[■\] work command +~/\.zshrc$')
        self.assertRegex(rows[3], r'^› \[x\] fzf +fuzzy pickers · brew install fzf$', 'the cursor starts here')
        self.assertRegex(rows[4], r'^  \[x\] zoxide +.* · brew install zoxide$')
        self.assertRegex(rows[5], r'^  \[x\] Claude Code status +found ~/\.claude$')
        self.assertRegex(rows[6], r'^  \[ \] lazygit +Ctrl\+a g popup · brew install lazygit$')
        self.assertRegex(rows[7], r'^  \[ \] sesh +picker fallback · brew install sesh$')
        screen = term.text
        order = [screen.rindex(part) for part in ('Needed for tmux-workspace to work', '] tmux-workspace',
                                                  'Recommended', '] fzf', 'Optional', '] lazygit')]
        self.assertEqual(order, sorted(order))
        self.assertNotIn('] tmux 3.3+', screen, 'tmux is there and new enough')
        self.assertNotIn('] git ', screen, 'git is there')
        term.press('q')
        self.assertEqual(term.finish(), 0, term.text)

    def test_a_missing_tmux_is_a_needed_row(self):
        self.machine()
        (self.bin / 'tmux').unlink()
        term = self.guided()
        term.expect('Recommended', timeout=30)
        self.assertRegex(term.line('] tmux 3.3+'), r'^  \[■\] tmux 3\.3\+ +brew install tmux$')
        term.press('q')
        self.assertEqual(term.finish(), 0, term.text)

    def test_q_quits_and_changes_nothing(self):
        brew = self.machine()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('q')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertIn('Nothing was changed.', term.text)
        self.assertEqual(self.tree(), {'.claude': 'dir'})
        self.assertFalse(brew.exists())

    def test_esc_alone_does_not_quit_and_a_split_arrow_key_still_moves(self):
        self.machine()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.expect('sesh')
        term.press('\x1b')
        time.sleep(0.4)  # the rest of the key, late, as over a slow ssh link
        term.press('[B')
        term.expect('› [x] zoxide')
        time.sleep(0.5)
        self.assertIsNone(term.proc.poll(), 'still asking')
        term.press('\x1b')
        time.sleep(0.4)
        self.assertIsNone(term.proc.poll(), 'Esc alone does not quit')
        term.press('q')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual(self.tree(), {'.claude': 'dir'})

    @staticmethod
    def installed(log):
        """The packages a package manager stub was asked to install, whatever the calls."""
        return sorted(word for line in log.read_text().splitlines() for word in line.split()[1:])

    def test_the_cursor_never_stops_on_a_needed_row(self):
        brew = self.machine()
        self.projects_set()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('k', 'k', '\x1b[A', ' ')  # up from the first row it can tick, then space
        term.expect('[ ] fzf')
        for name in ('tmux-workspace', 'tmux config', 'work command'):
            self.assertIn(f'[■] {name}', term.line(f'] {name}'))
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual(self.installed(brew), ['zoxide'])
        self.assertTrue(os.path.lexists(self.home / '.local/bin/tmux-workspace'), 'the needed rows are set up')

    def test_ticking_an_optional_row_installs_it(self):
        brew = self.machine()
        self.projects_set()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press(*'jjjjjjjjjj', ' ')  # down to the last row, sesh
        term.expect('[x] sesh')
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual(self.installed(brew), ['fzf', 'sesh', 'zoxide'])
        self.assertRegex(term.line('sesh  '), r'^  ✓ sesh +brew install sesh$')
        self.assertNotIn('lazygit  ', term.text.split('What should I set up?')[-1].split('Optional')[-1].split('sesh')[-1])

    def test_unticking_an_agent_row_skips_the_integrations(self):
        self.machine()
        self.projects_set()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('j', 'j', ' ')  # fzf, zoxide, then Claude Code
        term.expect('[ ] Claude Code status')
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertFalse((self.home / '.claude/settings.json').exists())
        self.assertNotIn('agent status', term.text)
        if platform.system() == 'Darwin':
            self.assertIn('[ ] iTerm2 profile', term.line('] iTerm2 profile'), 'the agent rows go together')

    def test_enter_with_the_defaults_does_what_agents_and_deps_do(self):
        brew = self.machine()
        code = self.home / 'code'
        self.install('--agents', '--deps', '--yes', '--projects', str(code))
        plain, plain_packages = self.tree(), self.installed(brew)
        self.assertIn('.claude/settings.json', plain)
        # The same machine again, and Enter.
        shutil.rmtree(self.home)
        self.home.mkdir()
        (self.home / '.claude').mkdir()
        brew.unlink()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        self.install_from_list(term, '~/code')
        self.assertEqual(term.finish(), 0, term.text)
        guided = self.tree()
        # The one difference: the log of the guided run, and the folders made for it.
        for path in ('.local/state/tmux-workspace/install.log', '.local/state/tmux-workspace', '.local/state'):
            self.assertIn(path, guided)
            if path not in plain:
                del guided[path]
        self.assertEqual(guided, plain)
        self.assertEqual(self.installed(brew), plain_packages)

    def test_the_projects_folder_is_asked_after_enter_unless_it_is_set(self):
        (self.home / 'notes').write_text('a file\n')
        for typed, wanted in (('', '~/Projects'), ('~/code', '~/code')):
            with self.subTest(typed=typed):
                (self.home / '.config/tmux-workspace/config.json').unlink(missing_ok=True)
                term = self.guided()
                term.expect('What should I set up?', timeout=30)
                term.press('\r')
                term.expect('Projects folder [~/Projects]: ')
                if typed:
                    term.press('~/notes\r')
                    term.expect('~/notes is a file, not a folder. Choose another folder.')
                term.press(typed + '\r')
                self.assertEqual(term.finish(), 0, term.text)
                config = json.loads((self.home / '.config/tmux-workspace/config.json').read_text())
                self.assertEqual(config['projects_dir'], wanted)
                self.assertRegex(term.line('projects folder  '), rf'^  ✓ projects folder +{re.escape(wanted)}$')
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertNotIn('Projects folder [', term.text, 'the config names one now')

    def failing_brew(self, host, token, email, noise=0):
        """A brew that fails, and says things in its output that name this person and machine. First, NOISE
        lines of 400 characters that each take 3 bytes in a link."""
        user = subprocess.run(['id', '-un'], capture_output=True, text=True).stdout.strip()
        return self.brew(f'i=0; while [ $i -lt {noise} ]; do i=$((i + 1)); echo "progress $i {"#" * 400}"; done\n'
                         f'echo "installing $* for tester ({user}) on {host} into {self.home}/Downloads" >&2\n'
                         f'echo "token {token}, mail {email}, mirror 192.168.1.20" >&2\n'
                         'echo "Error: fzf: no bottle for this machine" >&2\nexit 1')

    def test_a_failing_step_shows_the_end_of_the_log_and_installs_nothing(self):
        (self.bin / 'fzf').unlink()
        self.failing_brew('pats-mbp.example.lan', 'ghp_' + 'A1b2C3d4' * 5, 'pat@example.com')
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        self.install_from_list(term)
        self.assertEqual(term.finish(), 1, term.text)
        self.assertRegex(term.line('✗ '), r'^  ✗ fzf +brew install fzf$')
        self.assertIn('    Error: fzf: no bottle for this machine', term.text, 'the end of the log, as it is')
        self.assertIn('brew could not install fzf. Fix what the log says (or leave fzf unticked), then run the '
                      f'installer again: {checkout()}/install.sh', term.text)
        self.assertIn('Log: ~/.local/state/tmux-workspace/install.log', term.text)
        log = self.home / '.local/state/tmux-workspace/install.log'
        self.assertIn('Error: fzf: no bottle for this machine', log.read_text())
        self.assertEqual(log.stat().st_mode & 0o777, 0o600)
        self.assertEqual(log.parent.stat().st_mode & 0o777, 0o700)
        # Nothing half-installed: the tools come first, and nothing else was set up.
        self.assertEqual(sorted(self.tree()), ['.local', '.local/state', '.local/state/tmux-workspace',
                                               '.local/state/tmux-workspace/install.log'])
        self.assertNotIn('installed.', term.text)

    def test_a_failure_offers_an_anonymized_report_link(self):
        (self.bin / 'fzf').unlink()
        host, token, email = 'pats-mbp.example.lan', 'ghp_' + 'A1b2C3d4' * 5, 'pat@example.com'
        self.failing_brew(host, token, email, noise=30)
        self.stub('hostname', f'echo {host}')
        clipboard = self.work / 'clipboard'
        self.stub('pbcopy', f'cat > "{clipboard}"')
        # A long log before the failure: the link keeps its newest lines, within 8000 bytes.
        log = self.home / '.local/state/tmux-workspace/install.log'
        log.parent.mkdir(parents=True)
        log.write_text('an earlier run\n' * 50)
        term = self.guided(USER='tester', LOGNAME='tester')
        term.expect('What should I set up?', timeout=30)
        self.install_from_list(term)
        self.assertEqual(term.finish(), 1, term.text)
        link = term.line('Report it: ').split('Report it: ', 1)[1]
        self.assertTrue(link.startswith('https://github.com/ahmmkh/tmux-workspace/issues/new?template=bug.yml&title='),
                        link)
        self.assertLessEqual(len(link.encode()), 8000)
        self.assertRegex(link, r'^[A-Za-z0-9._~:/?&=%-]+$', 'percent-encoded')
        fields = urllib.parse.parse_qs(urllib.parse.urlsplit(link).query)
        self.assertEqual(fields['title'], ['install failed: fzf'])
        environment, diagnostics = fields['environment'][0], fields['diagnostics'][0]
        self.assertIn(f'tmux-workspace: {VERSION}', environment)
        self.assertIn('tmux: 3.5a', environment)
        self.assertIn('package manager: brew', environment)
        self.assertIn(f'bash: {"3.2" if platform.system() == "Darwin" else ""}', environment)
        self.assertIn('Error: fzf: no bottle for this machine', diagnostics)
        self.assertIn('into ~/Downloads', diagnostics)
        self.assertIn('for <user> (<user>) on <host>', diagnostics)
        self.assertIn('token <token>, mail <email>, mirror <ip>', diagnostics)
        self.assertNotIn('an earlier run', diagnostics, 'only this run')
        kept = diagnostics.splitlines()
        self.assertEqual(kept[-1], 'Error: fzf: no bottle for this machine', 'the newest lines stay')
        progress = [int(line.split()[1]) for line in kept if line.startswith('progress ')]
        self.assertTrue(progress and progress == list(range(progress[0], 31)), 'the oldest lines went first')
        self.assertGreater(progress[0], 30 - 20 + 4, 'more than the 20 lines of the tail would hold')
        user = subprocess.run(['id', '-un'], capture_output=True, text=True).stdout.strip()
        for secret in (str(self.home), host, 'pats-mbp', token, email, '192.168.1.20'):
            self.assertNotIn(secret, link)
        for name in ('tester', user):
            self.assertNotRegex(diagnostics, rf'\b{re.escape(name)}\b')
        # The clipboard holds the whole tail, anonymized: what the link had no room for too.
        copied = clipboard.read_text().rstrip('\n')
        self.assertEqual(len(copied.splitlines()), 20)
        self.assertTrue(copied.endswith(diagnostics))
        for secret in (str(self.home), host, token, email, '192.168.1.20'):
            self.assertNotIn(secret, copied)
        self.assertIn('The end of the log, anonymized, is on the clipboard as well.', term.text)

    def test_ctrl_c_in_the_checklist_restores_the_terminal_and_changes_nothing(self):
        brew = self.machine()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('j', ' ')
        term.expect('[ ] zoxide')
        term.press('\x03')
        self.assertEqual(term.finish(), 130, term.text)
        self.assertIn('Cancelled. Nothing was changed.', term.text)
        modes = termios.ECHO | termios.ICANON | termios.ISIG
        self.assertEqual(term.after[3] & modes, term.before[3] & modes, 'echo and line editing are back')
        self.assertGreater(term.raw.rfind(b'\x1b[?25h'), term.raw.rfind(b'\x1b[?25l'), 'the cursor shows again')
        self.assertEqual(self.tree(), {'.claude': 'dir'})
        self.assertFalse(brew.exists())

    def test_without_utf8_the_screen_is_ascii(self):
        self.machine()
        self.projects_set()
        term = self.guided(utf8=False)
        term.expect('What should I set up?', timeout=30)
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertTrue(term.raw.isascii(), [c for c in term.raw.decode() if not c.isascii()])
        self.assertIn('What should I set up?   up/down move - space toggle - enter install - q quit', term.text)
        self.assertRegex(term.line('] tmux-workspace'), r'^  \[#\] tmux-workspace ')
        self.assertRegex(term.text, r'(?m)^> \[x\] fzf +fuzzy pickers - brew install fzf$')
        self.assertRegex(term.line('ok fzf'), r'^  ok fzf +brew install fzf$')
        self.assertIsNotNone(COLOUR.search(term.raw.decode()), 'colour stays')

    def test_no_color_leaves_the_colour_out(self):
        self.machine()
        self.projects_set()
        term = self.guided(NO_COLOR='1')
        term.expect('What should I set up?', timeout=30)
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertIsNone(COLOUR.search(term.raw.decode()))
        self.assertRegex(term.line('✓ fzf'), r'^  ✓ fzf +brew install fzf$', 'UTF-8 keeps its glyphs')

    def test_without_tmux_or_a_package_manager_it_says_what_to_run_first(self):
        (self.bin / 'tmux').unlink()
        term = self.guided()
        self.assertEqual(term.finish(), 3, term.text)
        self.assertNotIn('What should I set up?', term.text)
        self.assertIn('tmux-workspace needs tmux 3.3 or newer, and no package manager here can install it.', term.text)
        if platform.system() == 'Darwin':
            self.assertIn('Install Homebrew (https://brew.sh), then run:\n    brew install tmux', term.text)
        self.assertIn(f'Then run this installer again:\n    {checkout()}/install.sh', term.text)
        self.assertEqual(self.tree(), {})

    def test_a_piped_install_on_a_terminal_downloads_then_shows_the_checklist(self):
        self.machine()
        script = (ROOT / 'install.sh').read_text()
        piped = dict(script=script, TMUX_WORKSPACE_BINARY=None, TMUX_WORKSPACE_DIST=str(release_dist()))
        term = self.guided(**piped)
        term.expect(f'tmux-workspace {VERSION} installer', timeout=60)
        term.press('q')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual(self.tree(), {'.claude': 'dir'}, 'q after the download: nothing, not even a log')
        self.assertEqual(list(self.tmp.iterdir()), [])

        self.projects_set()
        term = self.guided(**piped)
        term.expect(f'tmux-workspace {VERSION} installer', timeout=60)
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertRegex(term.line('✓ download'), rf'^  ✓ download +tmux-workspace {re.escape(VERSION)}, checked '
                                                  r'against SHA256SUMS$')
        self.assertRegex(term.line('✓ unpack'), rf'^  ✓ unpack +tmux-workspace {re.escape(VERSION)} to '
                                                r'~/\.local/share/tmux-workspace$')
        self.assertIn('Undo       ~/.local/share/tmux-workspace/current/install.sh --uninstall', term.text)
        self.assertEqual(os.readlink(self.share / 'current'), VERSION)
        self.assertEqual(os.readlink(self.home / '.tmux.conf'), str(self.share / 'current/tmux.conf'))
        self.assertEqual(list(self.tmp.iterdir()), [])

    def on_terminal(self, *argv, **extra):
        """ARGV on a terminal with UTF-8, as a person runs it."""
        self.terminal_tools()
        term = Terminal(list(argv), self.env(TERM='xterm-256color', LANG=UTF8, **extra))
        self.addCleanup(term.close)
        return term

    def test_an_update_on_a_terminal_shows_a_line_per_step_and_no_checklist(self):
        self.release('2.0.0')
        script = str(self.share / 'current/install.sh')  # what `work update` runs, with bash 3.2 here
        term = self.on_terminal('/bin/bash', script, '--update', TMUX_WORKSPACE_DIST=str(self.newer('2.0.1')))
        self.assertEqual(term.finish(), 0, term.text)
        self.assertNotIn('What should I set up?', term.text)
        self.assertRegex(term.line('✓ latest release'), r'^  ✓ latest release +2\.0\.1$')
        self.assertRegex(term.line('✓ download'), r'^  ✓ download +tmux-workspace 2\.0\.1$')
        self.assertRegex(term.line('✓ unpack'), r'^  ✓ unpack +tmux-workspace 2\.0\.1 to ~/\.local/share/tmux-workspace$')
        self.assertIn('Updated 2.0.0 to 2.0.1.', term.text)
        self.assertIn('Run exec $SHELL in terminals that are already open, so they get the new work command.', term.text)
        self.assertNotIn('done     ', term.text, 'the plain lines go to the log')
        log = (self.home / '.local/state/tmux-workspace/install.log').read_text()
        self.assertIn('done     updated 2.0.0 → 2.0.1', log)
        self.assertEqual(os.readlink(self.share / 'current'), '2.0.1')

        script = str(self.share / 'current/install.sh')
        term = self.on_terminal('/bin/bash', script, '--update', TMUX_WORKSPACE_DIST=str(self.newer('2.0.1')))
        self.assertEqual(term.finish(), 0, term.text)
        self.assertIn('tmux-workspace 2.0.1 is up to date.', term.text)

    def test_an_uninstall_on_a_terminal_asks_once_then_shows_a_line_per_step(self):
        (self.home / '.tmux.conf').write_text('mine\n')
        self.install('--yes')
        before = self.tree()
        uninstall = ('/bin/bash', str(checkout() / 'install.sh'), '--uninstall')
        term = self.on_terminal(*uninstall)
        term.expect('Remove tmux-workspace?', timeout=30)
        term.press('\r')  # no is the default
        self.assertEqual(term.finish(), 0, term.text)
        self.assertIn('Nothing was changed.', term.text)
        self.assertEqual(self.tree(), before)

        term = self.on_terminal(*uninstall)
        term.expect('Remove tmux-workspace?', timeout=30)
        term.press('y\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertRegex(term.line('✓ links'), r'^  ✓ links +~/\.local/bin/tmux-workspace, ~/\.tmux\.conf and the rest$')
        self.assertRegex(term.line('✓ work command'), r'^  ✓ work command +~/\.zshrc$')
        self.assertRegex(term.line('✓ backups'), r'^  ✓ backups +put back$')
        self.assertIn('tmux-workspace is removed. Snapshots and ~/.config/tmux-workspace stay.', term.text)
        self.assertNotIn('remove   ', term.text, 'the plain lines go to the log')
        self.assertEqual((self.home / '.tmux.conf').read_text(), 'mine\n')
        self.assertFalse(os.path.lexists(self.home / '.local/bin/tmux-workspace'))
        self.assertNotIn(BLOCK, (self.home / '.zshrc').read_text())

    def test_a_tmux_conf_of_yours_moves_to_a_backup_and_a_sesh_config_of_yours_stays(self):
        (self.home / '.tmux.conf').write_text('mine\n')
        self.stub('sesh')
        sesh = self.home / '.config/sesh/sesh.toml'
        sesh.parent.mkdir(parents=True)
        sesh.write_text('mine too\n')
        self.projects_set()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.expect('] work command')
        self.assertRegex(term.line('] tmux config'), r'^  \[■\] tmux config +~/\.tmux\.conf \(yours moves to a backup\)$')
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual((self.home / '.tmux.conf').resolve(), checkout() / 'tmux.conf')
        self.assertEqual(len(self.backups()), 1)
        self.assertRegex(term.line('Backups '), r'^  Backups +~/\.local/state/tmux-workspace/backups/install-[0-9-]+$')
        self.assertEqual(sesh.read_text(), 'mine too\n')
        self.assertRegex(term.line('✓ sesh config'), r'\(kept yours\)$')
        self.assertIn('Kept       your ~/.config/sesh/sesh.toml; rerun with --yes to replace it', term.text)
        self.assertIn(f'Undo       {checkout()}/install.sh --uninstall', term.text)
        self.install('--uninstall')
        self.assertEqual((self.home / '.tmux.conf').read_text(), 'mine\n')

    def test_a_choice_flag_on_a_terminal_gets_the_plain_installer(self):
        # Whoever gives one has decided: a script on a terminal nobody watches must not wait for keys.
        brew = self.machine()
        for flags in (['--deps'], ['--agents'], ['--no-shell'], ['--no-tmux-conf'],
                      ['--projects', str(self.home / 'code')]):
            with self.subTest(flags=flags):
                term = self.guided(*flags)
                if '--projects' not in flags:
                    term.expect('Where do you keep your projects?', timeout=30)
                    term.press('\x04')  # Ctrl+D: the first `work` asks
                self.assertEqual(term.finish(), 0, term.text)
                self.assertNotIn('What should I set up?', term.text)
                self.assertRegex(term.text, r'(?m)^done     tmux-workspace (already )?installed$')
        self.assertIn('fzf', self.installed(brew))
        self.assertTrue((self.home / '.claude/settings.json').exists())

    def test_without_a_package_manager_missing_tools_are_on_a_not_found_line(self):
        (self.bin / 'fzf').unlink()
        (self.bin / 'python3').unlink()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.expect('] iTerm2 profile' if platform.system() == 'Darwin' else '] work command')
        self.assertIn('· zsh · no package manager · found: tmux 3.5a, git', term.text)
        # The tools and where to get them, wrapped to the width of the terminal (120 columns here).
        self.assertIn('not found: fzf (github.com/junegunn/fzf), zoxide (github.com/ajeetdsouza/zoxide), '
                      'python3 (python.org),\n'
                      '           lazygit (github.com/jesseduffield/lazygit), sesh (github.com/joshmedeski/sesh)\n',
                      term.text)
        self.assertNotIn('] fzf', term.text)
        term.press('q')
        self.assertEqual(term.finish(), 0, term.text)

    def test_a_missing_python_is_a_recommended_row(self):
        brew = self.brew()
        (self.bin / 'python3').unlink()
        self.projects_set()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.expect('sesh')
        self.assertRegex(term.line('] python3'), r'^  \[x\] python3 3\.9\+ +dash, ai and cleanup need it · brew install python3$')
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertIn('python3', self.installed(brew))
        self.assertIn('Note       dash, ai and cleanup need python3 3.9+', term.text, 'the stub installed nothing')

    def test_a_narrow_terminal_gets_lines_that_fit(self):
        self.machine()
        self.terminal_tools()
        term = Terminal(['/bin/bash', str(checkout() / 'install.sh')],
                        self.env(TERM='xterm-256color', LANG=UTF8), size=(40, 50))
        self.addCleanup(term.close)
        term.expect('What should I set up?', timeout=30)
        term.expect('sesh')
        term.press('q')
        self.assertEqual(term.finish(), 0, term.text)
        lines = term.text.split('\n')
        self.assertEqual([line for line in lines if len(line) > 49], [])
        self.assertRegex(term.line('] zoxide'), r'…$')

    def test_a_slow_step_spins_until_it_is_done(self):
        clone = self.clone()
        self.stub('cargo', 'sleep 1\nmkdir -p target/release\n'
                           f'cp "{BINARY}" target/release/tmux-workspace')
        self.terminal_tools()
        self.projects_set()
        term = Terminal(['/bin/bash', str(clone / 'install.sh')],
                        self.env(TERM='xterm-256color', LANG=UTF8, TMUX_WORKSPACE_BINARY=None))
        self.addCleanup(term.close)
        term.expect('What should I set up?', timeout=30)
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertRegex(term.line('✓ build'), r'^  ✓ build +cargo build --release --locked$')
        frames = re.findall(r'\r  ([⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]) build ', term.raw.decode())
        self.assertGreater(len(set(frames)), 3, frames)
        self.assertEqual((clone / 'bin/tmux-workspace').read_bytes(), BINARY.read_bytes())
        log = (self.home / '.local/state/tmux-workspace/install.log').read_text()
        self.assertIn('build    cargo build --release --locked', log)
        self.assertNotIn('Terminated', log)

    def test_ctrl_c_in_a_step_stops_there_and_restores_the_terminal(self):
        (self.bin / 'fzf').unlink()
        self.brew('sleep 30')
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        self.install_from_list(term)
        term.expect(' fzf ')
        time.sleep(0.5)
        term.press('\x03')
        self.assertEqual(term.finish(timeout=20), 130, term.text)
        self.assertRegex(term.line('✗ '), r'^  ✗ fzf +brew install fzf$')
        self.assertIn('Stopped. What was done so far stays; run the installer again to finish', term.text)
        self.assertNotIn('Report it', term.text)
        modes = termios.ECHO | termios.ICANON
        self.assertEqual(term.after[3] & modes, term.before[3] & modes)
        self.assertFalse(os.path.lexists(self.home / '.local/bin/tmux-workspace'))

    def test_a_piped_download_that_fails_keeps_its_log(self):
        piped = dict(script=(ROOT / 'install.sh').read_text(), TMUX_WORKSPACE_BINARY=None,
                     TMUX_WORKSPACE_DIST=str(self.corrupt_dist()))
        term = self.guided(**piped)
        self.assertEqual(term.finish(), 3, term.text)
        self.assertRegex(term.line('✗ '), r'^  ✗ download +tmux-workspace \(the latest release\)$')
        self.assertIn(f'checksum mismatch for {ARCHIVE}; nothing was installed', term.text)
        self.assertIn('Log: ~/.local/state/tmux-workspace/install.log', term.text)
        self.assertIn('Report it: https://github.com/ahmmkh/tmux-workspace/issues/new?template=bug.yml'
                      '&title=install%20failed%3A%20download', term.text)
        log = self.home / '.local/state/tmux-workspace/install.log'
        self.assertIn(f'checksum mismatch for {ARCHIVE}', log.read_text())
        self.assertEqual(sorted(self.tree()), ['.local', '.local/state', '.local/state/tmux-workspace',
                                               '.local/state/tmux-workspace/install.log'])
        self.assertEqual(list(self.tmp.iterdir()), [])

    def test_keys_typed_before_the_checklist_never_answer_it(self):
        brew = self.machine()
        (self.home / '.tmux.conf').write_text('mine\n')
        term = self.guided()
        term.press('\r\r')  # ./install.sh, Enter, Enter: typed before the list is drawn
        term.expect('What should I set up?', timeout=30)
        term.expect('sesh')
        time.sleep(1)
        self.assertIsNone(term.proc.poll(), 'the checklist waits for a key pressed after it showed')
        self.assertNotIn('Projects folder [', term.text)
        term.press('q')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual((self.home / '.tmux.conf').read_text(), 'mine\n')
        self.assertFalse(brew.exists())

    def test_enter_during_a_piped_download_does_not_answer_the_checklist(self):
        self.machine()
        dist = release_dist()
        # A download that takes a while, as a real one does; the files come from the release folder.
        self.stub('curl', f'sleep 1\nfor a; do before=$last; last=$a; done\ncp "{dist}/${{last##*/}}" "$before"')
        term = self.guided(script=(ROOT / 'install.sh').read_text(), TMUX_WORKSPACE_BINARY=None,
                           TMUX_WORKSPACE_DIST='https://example.invalid/download')
        term.expect('download', timeout=30)
        term.press('\r')
        term.expect('What should I set up?', timeout=30)
        term.expect('sesh')
        time.sleep(1)
        self.assertIsNone(term.proc.poll())
        self.assertNotIn('Projects folder [', term.text, 'the Enter did not answer the checklist')
        term.press('q')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual(self.tree(), {'.claude': 'dir'})

    def test_the_projects_folder_is_checked_as_setup_will(self):
        (self.home / 'notes').write_text('a file\n')
        locked = self.work / 'locked'
        locked.mkdir()
        locked.chmod(0o555)
        self.addCleanup(locked.chmod, 0o755)
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('\r')
        term.expect('Projects folder [~/Projects]: ')
        term.press(' ~/notes \r')  # spaces around it, as setup trims them
        term.expect('~/notes is a file, not a folder. Choose another folder.')
        term.expect('Projects folder [~/Projects]: ')
        term.press('~/notes/projects\r')
        term.expect('~/notes/projects cannot be made: ~/notes is not a folder you can write to. '
                    'Choose another folder.')
        term.expect('Projects folder [~/Projects]: ')
        if os.geteuid() != 0:  # root may write into any folder
            term.press(f'{locked}/projects\r')
            term.expect(f'{locked}/projects cannot be made: {locked} is not a folder you can write to. '
                        'Choose another folder.')
            term.expect('Projects folder [~/Projects]: ')
        term.press('  ~/code\r')
        self.assertEqual(term.finish(), 0, term.text)
        config = json.loads((self.home / '.config/tmux-workspace/config.json').read_text())
        self.assertEqual(config['projects_dir'], '~/code')
        self.assertTrue((self.home / 'code').is_dir())

    def test_a_projects_folder_setup_refuses_is_left_to_the_first_work(self):
        # A setup that fails anyway (a folder that went away, a disk that filled up) fails no install.
        clone = self.clone()
        binary = self.work / 'refusing-setup'
        binary.write_text(f'#!/bin/sh\nif [ "$1" = setup ]; then echo "Workspace error: no" >&2; exit 1; fi\n'
                          f'exec "{BINARY}" "$@"\n')
        binary.chmod(0o755)
        self.terminal_tools()
        term = Terminal(['/bin/bash', str(clone / 'install.sh')],
                        self.env(TERM='xterm-256color', LANG=UTF8, TMUX_WORKSPACE_BINARY=str(binary)))
        self.addCleanup(term.close)
        term.expect('What should I set up?', timeout=30)
        self.install_from_list(term, '~/code')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertRegex(term.line('✓ projects folder'), r'^  ✓ projects folder +~/code \(setup refused it; '
                                                         r'the first `work` asks\)$')
        self.assertNotIn('✗', term.text)
        self.assertNotIn('Report it', term.text)
        self.assertIn(f'tmux-workspace {VERSION} is installed.', term.text)

    def test_the_projects_prompt_edits_the_line_with_the_arrow_keys(self):
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('\r')
        term.expect('Projects folder [~/Projects]: ')
        term.press('~/cde', '\x1b[D', '\x1b[D', 'o', '\r')  # back past e and d, then the missing o
        self.assertEqual(term.finish(), 0, term.text)
        config = json.loads((self.home / '.config/tmux-workspace/config.json').read_text())
        self.assertEqual(config['projects_dir'], '~/code')

    def fake_sudo(self, remembers):
        """A sudo that asks for a password on the terminal unless it remembers one (sudo -v); REMEMBERS False
        forgets it after each command, as timestamp_timeout=0 does."""
        stamp = self.work / 'sudo.stamp'
        forget = '' if remembers else f'rm -f "{stamp}"'
        self.stub('sudo', f"""stamp="{stamp}"
ask() {{ printf 'Password: ' >/dev/tty; read -r secret </dev/tty; : >"$stamp"; }}
if [ "$1" = -v ]; then [ -f "$stamp" ] || ask; exit 0; fi
if [ "$1" = -n ]; then
    shift
    [ -f "$stamp" ] || {{ echo 'sudo: a password is required' >&2; exit 1; }}
else
    [ -f "$stamp" ] || ask
fi
{forget}
exec "$@"
""")
        log = self.work / 'apt.log'
        self.stub('apt-get', f'echo "$*" >> "{log}"')
        (self.bin / 'fzf').unlink()
        return log

    def test_a_sudo_password_prompt_is_never_under_a_spinner(self):
        if os.geteuid() == 0:
            self.skipTest('root needs no sudo')
        apt = self.fake_sudo(remembers=False)
        self.projects_set()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('\r')
        for _ in range(4):  # once before the steps, then the package lists, fzf and zoxide
            term.expect('Password: ', timeout=20)
            term.idle(0.5)  # long enough for a spinner to write over it
            self.assertTrue(term.text.rstrip().endswith('Password:'), repr(term.text[-200:]))
            term.press('secret\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual(apt.read_text().splitlines(),
                         ['update -qq', 'install -y -qq fzf', 'install -y -qq zoxide'])

    def test_sudo_asks_once_when_it_remembers_the_password(self):
        if os.geteuid() == 0:
            self.skipTest('root needs no sudo')
        apt = self.fake_sudo(remembers=True)
        self.projects_set()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('\r')
        term.expect('Installing fzf zoxide takes sudo.')
        term.expect('Password: ', timeout=20)
        term.press('secret\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual(term.raw.count(b'Password: '), 1)
        self.assertRegex(term.line('✓ fzf'), r'^  ✓ fzf +sudo apt-get install -y fzf$')
        self.assertIn('install -y -qq zoxide', apt.read_text())

    def test_a_git_prompt_in_an_update_is_never_under_a_spinner(self):
        clone = self.work / 'checkout'
        upstream = self.upstream_and_clone(self.v2('2.0.0'), clone)
        self.run_script(['/bin/bash', str(clone / 'install.sh'), '--yes'], True, **self.clone_env())
        self.commit(upstream, self.v2('2.0.1'), 'second')
        real = shutil.which('git')
        (self.bin / 'git').unlink()
        # What ssh does for a key with a passphrase, or git for credentials: ask on the terminal.
        self.stub('git', 'case " $* " in *" fetch "*) printf "Enter passphrase for key: " >/dev/tty; '
                         f'read -r secret </dev/tty ;; esac\nexec "{real}" "$@"')
        term = self.on_terminal('/bin/bash', str(clone / 'install.sh'), '--update', **self.clone_env())
        term.expect('Enter passphrase for key: ', timeout=30)
        term.idle(0.5)
        self.assertTrue(term.text.rstrip().endswith('Enter passphrase for key:'), repr(term.text[-200:]))
        term.press('secret\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertIn('Updated 2.0.0', term.text)

    def test_the_summary_counts_packages_installed(self):
        self.install('--yes', '--projects', str(self.home / 'code'))
        brew = self.brew()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.expect('sesh')
        # Untick zoxide (and the iTerm2 profile on macOS), tick lazygit.
        term.press(*([' ', 'j', ' ', 'j', ' '] if platform.system() == 'Darwin' else [' ', 'j', ' ']))
        term.expect('[x] lazygit')
        term.press('\r')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertEqual(self.installed(brew), ['lazygit'])
        self.assertIn(f'tmux-workspace {VERSION} is installed.', term.text)
        self.assertRegex(term.line('Changed '), r'^  Changed +lazygit$')

    def test_ctrl_backslash_in_the_checklist_restores_the_terminal(self):
        self.machine()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.press('\x1c')  # Ctrl+\
        self.assertEqual(term.finish(timeout=20), 131, term.text)
        modes = termios.ECHO | termios.ICANON
        self.assertEqual(term.after[3] & modes, term.before[3] & modes)
        self.assertGreater(term.raw.rfind(b'\x1b[?25h'), term.raw.rfind(b'\x1b[?25l'))
        self.assertEqual(self.tree(), {'.claude': 'dir'})

    def test_ctrl_z_in_the_checklist_gives_the_terminal_back_until_it_continues(self):
        import signal
        self.machine()
        term = self.guided()
        term.expect('What should I set up?', timeout=30)
        term.expect('sesh')
        term.press('\x1a')  # Ctrl+Z
        time.sleep(1)
        term.pump(0.2)
        modes = termios.ECHO | termios.ICANON
        self.assertEqual(termios.tcgetattr(term.master)[3] & modes, term.before[3] & modes, 'stopped: as it was')
        self.assertGreater(term.raw.rfind(b'\x1b[?25h'), term.raw.rfind(b'\x1b[?25l'), 'cursor shown')
        os.killpg(term.proc.pid, signal.SIGCONT)
        term.expect('What should I set up?', timeout=10)  # the list again
        term.press('j', ' ')
        term.expect('[ ] zoxide')
        term.press('q')
        self.assertEqual(term.finish(timeout=20), 0, term.text)

    def test_a_terminal_too_short_for_the_checklist_gets_the_plain_installer(self):
        self.machine()
        self.terminal_tools()
        term = Terminal(['/bin/bash', str(checkout() / 'install.sh')],
                        self.env(TERM='xterm-256color', LANG=UTF8), size=(12, 100))
        self.addCleanup(term.close)
        term.expect('Where do you keep your projects?', timeout=30)
        term.press('\x04')
        self.assertEqual(term.finish(), 0, term.text)
        self.assertNotIn('What should I set up?', term.text)
        self.assertIn('done     tmux-workspace installed', term.text)

    def test_a_dumb_terminal_gets_the_plain_installer(self):
        term = self.guided(TERM='dumb')
        term.expect('Where do you keep your projects?', timeout=30)
        term.press('\x04')  # Ctrl+D
        self.assertEqual(term.finish(), 0, term.text)
        self.assertNotIn('What should I set up?', term.text)
        self.assertIn('done     tmux-workspace installed', term.text)


class Report(unittest.TestCase):
    """The pieces of a bug report link, run from install.sh as they are."""

    def run_function(self, name, *args):
        text = (ROOT / 'install.sh').read_text()
        body = re.search(rf'^{name}\(\) \($.*?^\)$', text, re.M | re.S).group(0)
        return subprocess.run(['bash', '-c', body + f'\n{name} "$@"', name, *args], capture_output=True,
                              env={'PATH': os.environ['PATH'], 'LANG': 'C.UTF-8'}, check=True).stdout

    def test_urlencode_gives_the_utf8_bytes(self):
        # musl's C locale reads bytes over 127 as U+DF80 and up; bash 3.2 as negative numbers.
        self.assertEqual(self.run_function('urlencode', 'a→b é ~-_.'), b'a%E2%86%92b%20%C3%A9%20~-_.')


if __name__ == '__main__':
    unittest.main()
