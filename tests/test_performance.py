"""Bound expensive operations without timing assertions tied to host load."""
import contextlib
import importlib.util
import json
from pathlib import Path
import runpy
import sqlite3
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('perf_hook', ROOT / 'integrations/agent-hook.py')
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)


class ProcessLookups(unittest.TestCase):
    def test_hook_checks_only_ancestors_and_still_rejects_nested_agents(self):
        tree = {300: (200, '??', '/usr/bin/python3'),
                200: (100, 'ttys001', 'claude'), 100: (1, 'ttys001', '/bin/zsh')}
        def processes(pid=None):
            self.assertIsNotNone(pid, 'status callback scanned every process')
            return {pid: tree[pid]} if pid in tree else {}
        def run(args):
            if args[-1] == 'args=':
                return 'claude' if args[3] == '200' else 'codex'
            return '200 200' if args[-1] == 'pgid=,tpgid=' else 'birth'
        with patch.object(hook, 'process_table', side_effect=processes) as scan, \
                patch.object(hook, 'run', side_effect=run):
            self.assertEqual(hook.owner('claude', 100, '/dev/ttys001', start_pid=300), (200, 'birth'))
            self.assertEqual([call.args[0] for call in scan.call_args_list], [300, 200, 100])
            tree[300] = (200, 'ttys001', 'codex')
            self.assertIsNone(hook.owner('claude', 100, '/dev/ttys001', start_pid=300))

    def test_native_executables_do_not_spawn_ps(self):
        with patch.object(hook, 'run', side_effect=AssertionError('unnecessary ps')):
            for command in ('/bin/zsh', '/usr/bin/python3', '/usr/bin/sleep'):
                self.assertIsNone(hook.process_kind(12, command))
            self.assertEqual(hook.process_kind(12, '/Users/me/.local/share/claude/versions/2.1.296'), 'claude')

    def test_resolver_reuses_supplied_process_table(self):
        with patch.object(hook, 'process_table', side_effect=AssertionError('repeated process scan')):
            self.assertIsNone(hook.resolve_codex(1, '/dev/ttys001', '', processes={}))

    def test_detached_daemon_database_uses_only_unique_current_title(self):
        session = '01a128f8-8621-7561-8972-8d02a1234567'
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'state_5.sqlite'
            with sqlite3.connect(path) as db:
                db.execute('CREATE TABLE threads (id TEXT, cwd TEXT, source TEXT)')
                db.execute('INSERT INTO threads VALUES (?, ?, ?)', (session, '/work', 'cli'))
            db.close()
            with patch.dict('os.environ', CODEX_HOME=folder), \
                    patch.object(hook, 'owner', return_value=(100, 'birth')), \
                    patch.object(hook, 'run', side_effect=lambda args: 'birth' if args[0] == hook.PS else 'fcwd\nn/work'):
                def resolve():
                    return hook.resolve_codex(100, '/dev/ttys001', 'codex | ' + session[:29] + '...',
                                              processes={100: (1, 'ttys001', 'codex')})
                self.assertEqual(resolve().get('id'), session)
                with sqlite3.connect(path) as db:
                    db.execute('INSERT INTO threads VALUES (?, ?, ?)', (session[:-1] + '8', '/other', 'cli'))
                db.close()
                self.assertIn('error', resolve())


class Pickers(unittest.TestCase):
    def test_workspace_picker_lists_recent_sessions_before_other_directories(self):
        ws = runpy.run_path(str(ROOT / 'bin/tmux-workspace'))
        sessions = '\n'.join(ws['SEP'].join(row) for row in (
            ('alphabetical-first', '100', '1'), ('recent', '300', '1'),
            ('__parked', '999', '1'), ('previous', '200', '1'), ('never-attached', '', '1')))
        with tempfile.TemporaryDirectory() as folder:
            projects = Path(folder) / 'projects'
            projects.mkdir()
            (projects / 'new-project').mkdir()
            other = Path(folder) / 'other'
            other.mkdir()
            def run(args, **kwargs):
                if args[0] == 'zoxide':
                    return subprocess.CompletedProcess(args, 0, str(other) + '\n')
                return subprocess.CompletedProcess(args, 130, '')
            with patch.dict(ws['choose_or_create_workspace'].__globals__,
                            PROJECTS_ROOT=projects, tm=Mock(return_value=sessions),
                            agent_status=lambda: ({}, {}, {'recent': 'blocked'})), \
                    patch('sys.stdin.isatty', return_value=True), patch('sys.stdout.isatty', return_value=True), \
                    patch('shutil.which', return_value='fzf'), patch('subprocess.run', side_effect=run) as launch:
                ws['choose_or_create_workspace']()
            call = launch.call_args
            names = [line.split('\t')[0] for line in call.kwargs['input'].splitlines()]
            self.assertEqual(names, ['recent', 'previous', 'alphabetical-first', 'never-attached',
                                     str(other), 'new-project'])
            self.assertIn('blocked', call.kwargs['input'])
            self.assertIn('--tiebreak=index', call.args[0])

    def test_tab_picker_uses_constant_tmux_reads(self):
        ws = runpy.run_path(str(ROOT / 'bin/tmux-workspace'))
        calls = []
        def tm(*args, **kw):
            calls.append(args)
            if args[0] == 'display-message':
                return 'work'
            if args[0] == 'list-windows':
                return '\n'.join(f'{i}\ttab{i}\t0\t1\t@{i}' for i in range(80))
            if args[0] == 'list-panes':
                return ''
            raise AssertionError(args)
        rows = [dict(session_name='work', window_id='@'+str(i), window_index=str(i),
                     window_name='tab'+str(i), window_active='0', window_panes='1',
                     pane_id='%'+str(i), pane_index='1', pane_current_command='codex',
                     **{'@workspace_label': 'agent', '@workspace_agent': 'blocked', '@workspace_home': ''})
                for i in range(80)]
        fields = Mock(return_value=rows)
        picker = Mock(return_value=subprocess.CompletedProcess([], 130, ''))
        with patch.dict(ws['pick_window'].__globals__, tm=tm, fields=fields), \
                patch('sys.stdin.isatty', return_value=True), patch('sys.stdout.isatty', return_value=True), \
                patch('shutil.which', return_value='fzf'), patch('subprocess.run', picker):
            ws['pick_window']()
        self.assertLessEqual(len(calls) + fields.call_count, 3)
        self.assertIn('tab79', picker.call_args.kwargs['input'])
        self.assertIn('blocked', picker.call_args.kwargs['input'])


class Saves(unittest.TestCase):
    def test_identity_timeout_still_saves_history_with_explicit_recovery_error(self):
        ws = runpy.run_path(str(ROOT / 'bin/tmux-workspace'))
        pane = dict(session_name='work', window_index='1', pane_index='1', pane_id='%1',
                    pane_pid='123', pane_tty='/dev/ttys001', pane_title='codex | unknown',
                    pane_current_command='codex', pane_current_path='/tmp', pane_active='1',
                    **{'@workspace_id': 'a'*32, '@workspace_label': 'agent', '@workspace_app': '',
                       '@workspace_parked': '', '@workspace_home': ''})
        helper = dict(process_table=Mock(return_value={}), resolve_codex=Mock(
            side_effect=subprocess.TimeoutExpired('ps', 2)))
        with tempfile.TemporaryDirectory() as folder, patch.dict(ws['snapshot'].__globals__,
                ROOT=Path(folder), lock=contextlib.nullcontext, option=lambda *_: '', put=Mock(),
                identity=lambda *_: 'a'*32, panes=lambda: [pane], process_table=lambda: {},
                tm=lambda *a, **kw: 'saved text' if a[0] == 'capture-pane' else '', fields=lambda *a: []), \
                patch('runpy.run_path', return_value=helper):
            ws['snapshot']()
            latest = json.loads((Path(folder) / 'latest.json').read_text())
            data = json.loads(Path(latest['path']).read_text())
            self.assertIn('error', data['panes'][0]['app'])
            self.assertEqual((Path(data['directory']) / ('a'*32 + '.txt')).read_text(), 'saved text\n')
            before = (Path(folder) / 'latest.json').read_bytes()
            with patch.dict(ws['snapshot'].__globals__, tm=Mock(side_effect=subprocess.CalledProcessError(1, 'capture-pane'))):
                with self.assertRaises(subprocess.CalledProcessError):
                    ws['snapshot'](force=True)
            self.assertEqual((Path(folder) / 'latest.json').read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
