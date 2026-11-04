#!/usr/bin/env python3
"""Build a demo workspace on a private tmux server and capture the README screenshots.

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
from ansi2svg import convert  # noqa: E402

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
        self.env = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE')}
        # Every place a tool may write is inside the sandbox; a real XDG_CONFIG_HOME once leaked through.
        for name, folder in (('XDG_CONFIG_HOME', '.config'), ('XDG_DATA_HOME', '.local/share'),
                             ('XDG_STATE_HOME', '.local/state'), ('XDG_CACHE_HOME', '.cache')):
            self.env[name] = str(self.home / folder)
        for name in ('ZDOTDIR', 'CODEX_HOME', '_ZO_DATA_DIR', 'TMUX_WORKSPACE_PROJECTS', 'TMUX_WORKSPACE_HOME'):
            self.env.pop(name, None)
        self.env.update(HOME=str(self.home), TMUX_TMPDIR=str(self.root / 'sockets'),
                        TMUX_WORKSPACE_STATE_DIR=str(self.root / 'state'), TERM='xterm-256color',
                        SHELL='/bin/sh', LANG=os.environ.get('LANG', 'en_US.UTF-8'))
        (self.root / 'sockets').mkdir(mode=0o700)

    def run(self, *args, **kwargs):
        # cwd inside the temporary home, so no pane or server inherits a real path.
        return subprocess.run(args, env=self.env, cwd=self.home, capture_output=True, text=True, check=True,
                              **kwargs).stdout

    def demo(self, *args):
        return self.run('tmux', '-L', 'tw-demo', *args).rstrip('\n')

    def outer(self, *args):
        return self.run('tmux', '-L', 'tw-outer', '-f', '/dev/null', *args).rstrip('\n')

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
                   f'env -u TMUX tmux -L tw-demo attach -t api')
        self.outer('set', '-g', 'default-terminal', 'xterm-256color')
        self.outer('set', '-g', 'status', 'off')
        time.sleep(1.5)

    def keys(self, *keys, wait=1.5):
        for key in keys:
            self.outer('send-keys', '-t', 'view', key)
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

    def close(self):
        for args in (('tmux', '-L', 'tw-outer', 'kill-server'), ('tmux', '-L', 'tw-demo', 'kill-server')):
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


if __name__ == '__main__':
    main()
