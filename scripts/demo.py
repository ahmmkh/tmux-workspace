#!/usr/bin/env python3
"""Build demo workspaces on a private tmux server and capture the README screenshots, and record
a first run (install, onboarding, work, the dashboard) as an animated SVG.

    python3 scripts/demo.py            (or: make screenshots)

Everything runs under a temporary HOME and TMUX_TMPDIR, so the demo can never reach a real tmux
server or real snapshots. A second, plain tmux server shows the demo client in a pane; capturing
that pane gets the whole screen: status line, pane borders and popups.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(Path(__file__).resolve().parent))
from ansi2svg import animate, convert  # noqa: E402

WIDTH, HEIGHT = 132, 30
IMAGES = REPO / 'docs' / 'images'

PANES = {
    # workspace: [(tab, [(label, agent status, agent kind, screen text)])]
    'api': [('code', [
        ('agent', 'blocked', 'claude',
         '> add rate limiting to POST /login\n\n'
         '  Plan: token bucket per client IP, 5 requests a minute\n'
         '  Edit middleware/ratelimit.go  (+42 -3)\n\n'
         '  Allow this edit?  [y] yes   [n] no   [e] explain\n'),
        ('tests', '', '',
         '$ go test ./...\nok   api/middleware   0.41s\nok   api/handlers     1.02s\nok   api/store        0.33s\n'),
    ]), ('server', [
        ('server', '', '',
         '$ make run\nlistening on :8080\nGET  /health        200   1ms\nPOST /login         200  38ms\n'
         'POST /login         429   0ms\nGET  /v1/orders     200  12ms\n'),
    ])],
    'frontend': [('code', [
        ('agent', 'done', 'codex', '> migrate the settings page to the new form library\n\n  Done: 6 files changed, tests pass.\n'),
        ('vite', '', '', '$ npm run dev\n  VITE ready in 412 ms\n  ->  Local: http://localhost:5173/\n'),
    ])],
    'infra': [('deploy', [
        ('agent', 'working', 'omp', '> plan the staging database upgrade\n\n  Reading terraform/modules/db ...\n'),
    ])],
    'docs': [('write', [
        ('agent', 'idle', 'omp', '> \n'),
    ])],
}


class Demo:
    def __init__(self):
        # resolve(): macOS reaches /var through /private/var, and tmux reports the real path.
        self.root = Path(tempfile.mkdtemp(prefix='tw-demo-')).resolve()
        self.home = self.root / 'home'
        self.home.mkdir()
        # An allowlist, not a copy of the caller's environment: nothing from the real machine (terminal,
        # host name, config folders) may reach a frame or a tool. A real XDG_CONFIG_HOME once leaked through.
        self.env = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': str(self.home),
                    'LANG': 'en_US.UTF-8', 'TERM': 'xterm-256color', 'SHELL': '/bin/bash',
                    'BASH_SILENCE_DEPRECATION_WARNING': '1',
                    'TMUX_TMPDIR': str(self.root / 'sockets'), 'TMUX_WORKSPACE_STATE_DIR': str(self.root / 'state')}
        for name, folder in (('XDG_CONFIG_HOME', '.config'), ('XDG_DATA_HOME', '.local/share'),
                             ('XDG_STATE_HOME', '.local/state'), ('XDG_CACHE_HOME', '.cache')):
            self.env[name] = str(self.home / folder)
        self.outer_conf = self.root / 'outer.conf'
        self.outer_conf.write_text('set -g default-terminal xterm-256color\nset -g status off\n')
        (self.root / 'sockets').mkdir(mode=0o700)

    def run(self, *args, **kwargs):
        # cwd inside the temporary home, so no pane or server inherits a real path.
        return subprocess.run(args, env=self.env, cwd=self.home, capture_output=True, text=True, check=True,
                              **kwargs).stdout

    def demo(self, *args):
        return self.run('tmux', '-L', 'tw-demo', *args).rstrip('\n')

    def outer(self, *args):
        return self.run('tmux', '-L', 'tw-outer', '-f', str(self.outer_conf), *args).rstrip('\n')

    def private(self, text):
        """Show the sandbox home as ~, as it is for the demo user."""
        return text.replace(str(self.home), '~').replace(str(self.root), '~')

    def screen_command(self, text):
        path = self.root / f'screen-{len(list(self.root.glob("screen-*")))}.txt'
        path.write_text(text)
        return f'cat {path}; exec sleep 86400'  # sleep, not a shell, so agent marks show

    def install(self):
        projects = self.home / 'Projects'
        for name in ('api', 'frontend', 'infra', 'docs', 'blog', 'mobile-app'):
            (projects / name).mkdir(parents=True)
        self.run('bash', str(REPO / 'install.sh'), '--yes', '--no-shell', '--projects', str(projects))

    def build(self):
        conf = str(self.home / '.tmux.conf')
        first = True
        for workspace, tabs in PANES.items():
            cwd = str(self.home / 'Projects' / workspace)
            for tab, panes in tabs:
                for index, (label, status, kind, text) in enumerate(panes):
                    command = self.screen_command(text)
                    if first:
                        pane = self.run('tmux', '-L', 'tw-demo', '-f', conf, 'new-session', '-d', '-P', '-F', '#{pane_id}',
                                        '-s', workspace, '-n', tab, '-c', cwd, '-x', str(WIDTH), '-y', str(HEIGHT), command).strip()
                        self.demo('set', '-g', '@agent_sound_cmd', 'true')  # silent, but not shown as muted
                        first = False
                    elif index == 0 and tab == tabs[0][0]:
                        pane = self.demo('new-session', '-d', '-P', '-F', '#{pane_id}', '-s', workspace, '-n', tab,
                                         '-c', cwd, '-x', str(WIDTH), '-y', str(HEIGHT), command)
                    elif index == 0:
                        pane = self.demo('new-window', '-d', '-P', '-F', '#{pane_id}', '-t', workspace + ':', '-n', tab,
                                         '-c', cwd, command)
                    else:
                        pane = self.demo('split-window', '-d', '-h', '-P', '-F', '#{pane_id}', '-t', f'={workspace}:{tab}',
                                         '-c', cwd, command)
                    self.demo('set', '-p', '-t', pane, '@workspace_label', label)
                    if kind:
                        self.demo('set', '-p', '-t', pane, '@workspace_app', '{"kind": "%s", "id": "demo"}' % kind)
                    if status:
                        self.demo('set', '-p', '-t', pane, '@workspace_agent', status)
        # A parked pane: hidden from its tab, still running, still counted.
        extra = self.demo('split-window', '-d', '-v', '-P', '-F', '#{pane_id}', '-t', '=api:server',
                          '-c', str(self.home / 'Projects' / 'api'), self.screen_command('$ tail -f logs/worker.log\n'))
        self.demo('set', '-p', '-t', extra, '@workspace_label', 'worker-logs')
        socket = self.demo('display-message', '-p', '#{socket_path}')
        subprocess.run([str(REPO / 'bin' / 'tmux-workspace'), 'park', extra], cwd=self.home,
                       env=dict(self.env, TMUX=socket + ',0,0'), check=True, capture_output=True)
        subprocess.run([str(REPO / 'bin' / 'tmux-workspace'), 'save'], cwd=self.home,  # the status line shows the save time
                       env=dict(self.env, TMUX=socket + ',0,0'), check=True, capture_output=True)
        self.demo('select-window', '-t', '=api:code')
        self.demo('select-pane', '-t', '=api:code.1')

    def attach(self):
        """The demo client, inside a plain tmux pane that we can capture whole."""
        self.outer('new-session', '-d', '-s', 'view', '-x', str(WIDTH), '-y', str(HEIGHT),
                   'env -u TMUX tmux -L tw-demo attach -t api')
        time.sleep(1.5)

    def keys(self, *keys, wait=1.5, target='view'):
        for key in keys:
            self.outer('send-keys', '-t', target, key)
            time.sleep(0.2)
        time.sleep(wait)

    def shot(self, name, title):
        text = self.outer('capture-pane', '-e', '-p', '-t', 'view')
        IMAGES.mkdir(parents=True, exist_ok=True)
        (IMAGES / f'{name}.svg').write_text(convert(text, title, cols=WIDTH))
        print('wrote', IMAGES / f'{name}.svg')

    def capture(self):
        self.shot('status-line', 'work api - agent marks on panes, tabs and workspaces')
        self.keys('C-a', 'A', wait=2.5)
        self.shot('dashboard', 'Ctrl+a A - every pane and agent')
        self.keys('q', wait=1)
        self.keys('C-a', 's', wait=2.5)
        self.shot('workspace-picker', 'Ctrl+a s - workspaces with their agent status')
        self.keys('Escape', wait=1)
        self.keys('C-a', 'w', wait=2.5)
        self.shot('window-picker', 'Ctrl+a w - tabs with a live preview')
        self.keys('Escape', wait=0.5)

    # -----------------------------------------------------------------------------------------
    # The first run, as a stranger sees it: install, onboarding, a workspace, the dashboard.

    ONBOARDING_SIZE = (100, 26)

    def screen(self, target='first'):
        # -J joins lines the terminal wrapped, so a long path is whole when private() shortens it.
        return self.private(self.outer('capture-pane', '-e', '-p', '-J', '-t', target))

    def wait_for(self, needle, timeout=30, target='first'):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.outer('capture-pane', '-p', '-t', target):
                return
            time.sleep(0.2)
        raise RuntimeError(f'demo: {needle!r} never appeared:\n' + self.outer('capture-pane', '-p', '-t', target))

    def type(self, frames, text, hold=0.12):
        """Type like a person, one frame per couple of characters."""
        for i in range(0, len(text), 2):
            self.outer('send-keys', '-t', 'first', '-l', text[i:i + 2])
            time.sleep(0.08)
            frames.append((self.screen(), hold))

    def onboarding(self):
        clone = self.home / 'tmux-workspace'
        for name in self.run('git', '-C', str(REPO), 'ls-files').split('\n'):
            if name and (REPO / name).is_file() and not name.startswith('docs/images/'):
                (clone / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(REPO / name, clone / name)
        (self.home / '.bashrc').write_text("PS1='$ '\n")
        (self.home / '.bash_profile').write_text('[ -f ~/.bashrc ] && . ~/.bashrc\n')
        shell = 'env -u TMUX -u TMUX_PANE bash --noprofile --rcfile ~/.bashrc -i'
        cols, rows = self.ONBOARDING_SIZE
        self.outer('new-session', '-d', '-s', 'first', '-x', str(cols), '-y', str(rows), '-c', str(clone), shell)
        frames = []
        self.wait_for('$')
        frames.append((self.screen(), 1.2))
        self.type(frames, './install.sh')
        self.outer('send-keys', '-t', 'first', 'Enter')
        self.wait_for('Where do you keep your projects?')
        time.sleep(0.3)
        frames.append((self.screen(), 3.5))
        self.type(frames, '~/code')
        self.outer('send-keys', '-t', 'first', 'Enter')
        self.wait_for('tmux-workspace installed')
        time.sleep(0.3)
        frames.append((self.screen(), 4.5))
        # A new terminal: its shell now has the work command.
        self.outer('respawn-pane', '-k', '-t', 'first', '-c', str(self.home), shell)
        self.wait_for('$')
        frames.append((self.screen(), 1.0))
        self.type(frames, 'work api')
        self.outer('send-keys', '-t', 'first', 'Enter')
        self.wait_for('Pane label')
        time.sleep(0.3)
        frames.append((self.screen(), 1.5))
        self.type(frames, 'server')
        self.outer('send-keys', '-t', 'first', 'Enter')
        time.sleep(1.5)
        frames.append((self.screen(), 2.0))
        self.keys('C-a', 'A', wait=0, target='first')
        self.wait_for('WORKSPACE')
        time.sleep(0.5)
        frames.append((self.screen(), 4.5))
        self.keys('q', wait=0.3, target='first')
        IMAGES.mkdir(parents=True, exist_ok=True)
        (IMAGES / 'onboarding.svg').write_text(animate(frames, 'first run: install, choose a folder, work, Ctrl+a A', cols=cols))
        print('wrote', IMAGES / 'onboarding.svg', f'({len(frames)} frames)')

    def close(self):
        for args in (('tmux', '-L', 'tw-outer', 'kill-server'), ('tmux', '-L', 'tw-demo', 'kill-server'),
                     ('tmux', 'kill-server')):  # the onboarding's own server, on the sandbox's default socket
            subprocess.run(args, env=self.env, capture_output=True)
        shutil.rmtree(self.root, ignore_errors=True)


def main():
    if not shutil.which('tmux') or not shutil.which('fzf'):
        sys.exit('make screenshots needs tmux and fzf')
    demo = Demo()
    try:
        demo.install()
        demo.build()
        demo.attach()
        demo.capture()
    finally:
        demo.close()
    first = Demo()
    try:
        first.onboarding()
    finally:
        first.close()


if __name__ == '__main__':
    main()
