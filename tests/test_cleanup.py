import contextlib
import io
import os
from pathlib import Path
import runpy
import sys
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import cleanup


class Cleanup(unittest.TestCase):
    def setUp(self):
        self.rows = [dict(pane_id='%' + str(i), pane_pid=str(i * 100),
                          pane_current_command='codex', pane_dead='0', pane_tty='/dev/ttys001',
                          session_name='work', window_index=str(i), pane_index='1',
                          **{'@workspace_label': 'task', '@workspace_agent': status, '@workspace_home': ''})
                     for i, status in enumerate(('idle', 'working', 'blocked', ''), 1)]
        self.ws = {'fields': Mock(return_value=self.rows), 'tm': Mock()}
        self.enterContext(patch.dict(os.environ, TMUX_PANE='%99'))

    def test_lists_unknown_but_excludes_busy_and_counts_child_memory(self):
        result = Mock(stdout='100 1 1024 codex\n101 100 2048 helper\n400 1 4096 zsh\n')
        with patch.object(cleanup.subprocess, 'run', return_value=result):
            rows = cleanup.candidates(self.ws)
        self.assertEqual([r['pane_id'] for r in rows], ['%1', '%4'])
        self.assertEqual(rows[0]['memory'], 3)
        self.assertEqual(len(rows[0]['processes']), 2)

    def test_cancel_closes_nothing_and_new_work_invalidates_selection(self):
        ps = Mock(stdout='100 1 1024 codex\n')
        selection = Mock(returncode=0, stdout='%1\ttask\n')
        for answer in ('', 'close'):
            self.rows[0]['@workspace_agent'] = 'idle'
            def confirm(_prompt):
                self.rows[0]['@workspace_agent'] = 'working'
                return answer
            with patch.object(cleanup.subprocess, 'run', side_effect=[ps, selection, ps]), \
                    patch.object(cleanup.sys.stdin, 'isatty', return_value=True), \
                    patch.object(cleanup.shutil, 'which', return_value='fzf'), \
                    patch('builtins.input', side_effect=confirm), contextlib.redirect_stdout(io.StringIO()):
                cleanup.cleanup(self.ws)
            self.ws['tm'].assert_not_called()

    def test_confirmed_selection_uses_exact_pane_and_atomic_status_guard(self):
        ps = Mock(stdout='100 1 1024 codex\n')
        with patch.object(cleanup.subprocess, 'run', side_effect=[ps, Mock(returncode=0, stdout='%1\ttask\n'), ps]), \
                patch.object(cleanup.sys.stdin, 'isatty', return_value=True), \
                patch.object(cleanup.shutil, 'which', return_value='fzf'), \
                patch('builtins.input', return_value='close'), contextlib.redirect_stdout(io.StringIO()):
            cleanup.cleanup(self.ws)
        args = self.ws['tm'].call_args.args
        self.assertEqual(args[:4], ('if-shell', '-F', '-t', '%1'))
        self.assertIn('blocked', args[4])
        self.assertEqual(args[5], 'kill-pane -t %1')


class Picker(unittest.TestCase):
    def test_initialized_picker_does_not_wait_for_snapshot_lock(self):
        ws = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'bin/tmux-workspace'))
        with patch.dict(ws['initialize'].__globals__, option=lambda *_: '1',
                        lock=Mock(side_effect=AssertionError('picker waited for autosave'))):
            ws['initialize']()


if __name__ == '__main__':
    unittest.main()
