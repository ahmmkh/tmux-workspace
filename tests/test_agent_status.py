"""Check how agent-hook.py maps Claude Code's state to a pane status and how the pickers
roll status up and build their rows. No tmux server runs."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('agent_hook', ROOT / 'integrations' / 'agent-hook.py')
assert SPEC is not None and SPEC.loader is not None
hook = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(hook)

SESSION, PID, BIRTH, CWD = 'session-1', 4242, 'Thu Oct  1 20:23:29 2026', '/work'
APP = json.dumps({'kind': 'claude', 'id': SESSION, 'pid': PID, 'cwd': CWD, 'birth': BIRTH}, separators=(',', ':'))


class Mapping(unittest.TestCase):
    def wanted(self, state, wait='', failed='', turn_over='done'):
        return hook.wanted_status(state, SESSION, wait, failed, turn_over)

    def test_busy_is_working(self):
        self.assertEqual(self.wanted({'status': 'busy', 'statusUpdatedAt': 5}), 'working')

    def test_waiting_is_blocked(self):
        self.assertEqual(self.wanted({'status': 'waiting', 'statusUpdatedAt': 5}), 'blocked')

    def test_idle_and_shell_apply_turn_over(self):
        for status in ('idle', 'shell'):
            self.assertEqual(self.wanted({'status': status}, turn_over='done'), 'done')
            self.assertEqual(self.wanted({'status': status}, turn_over='idle'), 'idle')

    def test_failed_turn_stays_blocked_while_the_file_is_idle(self):
        self.assertIsNone(self.wanted({'status': 'idle', 'statusUpdatedAt': 1100}, wait='1000', failed=SESSION))

    def test_failed_turn_ends_when_a_later_turn_finished_between_two_calls(self):
        state = {'status': 'idle', 'statusUpdatedAt': 1000 + hook.SETTLE + 1}
        self.assertEqual(self.wanted(state, wait='1000', failed=SESSION), 'done')
        self.assertEqual(self.wanted({'status': 'busy', 'statusUpdatedAt': 1001}, wait='1000', failed=SESSION), 'working')

    def test_failure_of_another_session_does_not_count(self):
        self.assertEqual(self.wanted({'status': 'idle'}, failed='older-session'), 'done')

    def test_missing_file_or_missing_status_changes_nothing(self):
        for state in (None, {}, {'pid': PID}, {'status': 'unknown'}, []):
            self.assertIsNone(self.wanted(state))

    def test_blocked_by_a_hook_returns_to_working_only_when_the_file_is_newer(self):
        self.assertIsNone(self.wanted({'status': 'busy', 'statusUpdatedAt': 100}, wait='100'))
        self.assertIsNone(self.wanted({'status': 'busy', 'statusUpdatedAt': 99}, wait='100'))
        self.assertEqual(self.wanted({'status': 'busy', 'statusUpdatedAt': 101}, wait='100'), 'working')

    def test_turn_over_without_the_tmux_formats_changes_nothing(self):
        self.assertIsNone(self.wanted({'status': 'idle'}, turn_over=''))


class Helper(unittest.TestCase):
    """Run main() with a fake tmux. The fake records every call that is not a read."""

    def setUp(self):
        self.sessions = tempfile.TemporaryDirectory()
        self.addCleanup(self.sessions.cleanup)
        self.pane = {'agent': '', 'wait': '', 'failed': '', 'turn_over': 'done', 'app': APP}
        self.writes = []
        for patch in (mock.patch.object(hook, 'run', self.run_tmux),
                      mock.patch.object(hook, 'owner', lambda *_: (PID, BIRTH)),
                      mock.patch.object(hook, 'SESSIONS', self.sessions.name),
                      mock.patch.dict(os.environ, {'TMUX': '/tmp/scratch-socket,1,0', 'TMUX_PANE': '%7'})):
            patch.start()
            self.addCleanup(patch.stop)

    def run_tmux(self, argv):
        if 'display-message' in argv:
            fields = ('99', '/dev/ttys001', self.pane['agent'], self.pane['wait'], self.pane['failed'],
                      self.pane['turn_over'], self.pane['app'])
            return '\t'.join(fields).strip()
        if 'show-option' in argv:
            return self.pane['app']
        self.writes.append(' '.join(argv))
        return ''

    def call(self, mode, **data):
        payload = {'session_id': SESSION, 'cwd': CWD}
        payload.update(data)
        out = io.StringIO()
        with mock.patch.object(sys, 'argv', ['agent-hook.py', mode]), \
                mock.patch.object(sys, 'stdin', io.StringIO(json.dumps(payload))), \
                contextlib.redirect_stdout(out):
            hook.main()
        return out.getvalue()

    def file(self, **state):
        Path(self.sessions.name, '%d.json' % PID).write_text(json.dumps(state))

    def test_status_call_writes_working_as_a_compare_and_set(self):
        self.file(status='busy', statusUpdatedAt=5)
        self.pane['agent'] = 'idle'
        self.assertEqual(self.call('claude-status'), 'Claude | session-\n')
        self.assertEqual(len(self.writes), 1)
        self.assertIn('if-shell', self.writes[0])
        self.assertIn('#{==:#{@workspace_agent},idle}', self.writes[0])
        self.assertIn("@workspace_agent 'working'", self.writes[0])

    def test_status_write_is_guarded_by_the_wait_value_that_was_read(self):
        self.file(status='busy', statusUpdatedAt=9)
        self.pane.update(agent='blocked', wait='5')
        self.call('claude-status')
        self.assertEqual(len(self.writes), 1)
        self.assertIn('#{&&:#{==:#{@workspace_agent},blocked},#{==:#{@workspace_agent_wait},5}}',
                      self.writes[0])

    def test_status_call_issues_no_write_when_nothing_changed(self):
        self.file(status='busy', statusUpdatedAt=5)
        self.pane['agent'] = 'working'
        self.assertEqual(self.call('claude-status'), 'Claude | session-\n')
        self.assertEqual(self.writes, [])

    def test_status_call_without_a_state_file_leaves_the_status(self):
        self.pane['agent'] = 'working'
        self.call('claude-status')
        self.assertEqual(self.writes, [])

    def test_status_call_writes_the_identity_only_when_it_changed(self):
        self.pane['app'] = ''
        self.call('claude-status')
        self.assertEqual(len(self.writes), 1)
        self.assertIn('#{==:#{@workspace_app},}', self.writes[0])
        self.assertIn('@workspace_app', self.writes[0])

    def blocked(self, event, **data):
        self.pane['agent'] = 'working'
        self.assertEqual(self.call('claude', hook_event_name=event, **data), '')
        return self.writes

    def test_permission_request_blocks_and_records_the_time(self):
        writes = self.blocked('PermissionRequest', tool_name='Bash')
        self.assertEqual(len(writes), 1)
        self.assertIn('@workspace_agent blocked', writes[0])
        self.assertRegex(writes[0], r'@workspace_agent_wait \d{13}\b')
        self.assertNotIn('@workspace_agent_failed', writes[0])

    def test_question_and_plan_block(self):
        for tool in ('AskUserQuestion', 'ExitPlanMode'):
            self.writes.clear()
            self.assertIn('@workspace_agent blocked', self.blocked('PreToolUse', tool_name=tool)[0])

    def test_other_tools_do_not_block(self):
        self.assertEqual(self.blocked('PreToolUse', tool_name='Bash'), [])

    def test_stop_failure_blocks_and_marks_the_turn_failed(self):
        writes = self.blocked('StopFailure')
        self.assertIn('@workspace_agent blocked', writes[0])
        self.assertIn('@workspace_agent_failed ' + SESSION, writes[0])

    def test_subagent_approval_counts_for_blocked(self):
        self.assertIn('@workspace_agent blocked', self.blocked('PermissionRequest', agent_id='sub-1')[0])

    def test_nested_claude_changes_nothing(self):
        self.assertEqual(self.blocked('PermissionRequest', session_id='nested-print-session'), [])
        self.assertEqual(self.blocked('StopFailure', session_id='nested-print-session'), [])

    def test_pane_without_an_identity_changes_nothing(self):
        self.pane['app'] = ''
        self.assertEqual(self.blocked('PermissionRequest'), [])

    def test_session_end_clears_the_status_with_the_identity(self):
        self.assertEqual(self.call('claude', hook_event_name='SessionEnd'), '')
        self.assertEqual(len(self.writes), 1)
        for option in ('@workspace_app', '@workspace_agent', '@workspace_agent_wait', '@workspace_agent_failed'):
            self.assertIn('set-option -pu -t %7 ' + option, self.writes[0])


def load_workspace():
    """Load bin/tmux-workspace as a module. It has no .py suffix and sets the umask on import."""
    path = str(ROOT / 'bin' / 'tmux-workspace')
    spec = importlib.util.spec_from_loader('tmux_workspace', importlib.machinery.SourceFileLoader('tmux_workspace', path))
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    mask = os.umask(0)
    try:
        spec.loader.exec_module(module)
    finally:
        os.umask(mask)
    return module


ws = load_workspace()


def pane(pane_id, status, window='@1', session='alpha', command='omp', home=''):
    return {'session_name': session, 'window_id': window, 'pane_id': pane_id, 'pane_current_command': command,
            '@workspace_agent': status, '@workspace_home': home}


class RollUp(unittest.TestCase):
    def test_tab_takes_the_most_urgent_status(self):
        order = ['idle', 'working', 'done', 'blocked']
        for count in range(1, 5):
            rows = [pane('%%%d' % i, status) for i, status in enumerate(order[:count])]
            self.assertEqual(ws.roll_up(rows)[1], {'@1': order[count - 1]})

    def test_workspace_rolls_up_its_tabs(self):
        rows = [pane('%1', 'working', '@1'), pane('%2', 'done', '@2'), pane('%3', 'idle', '@3', session='beta')]
        panes, tabs, workspaces = ws.roll_up(rows)
        self.assertEqual(panes, {'%1': 'working', '%2': 'done', '%3': 'idle'})
        self.assertEqual(tabs, {'@1': 'working', '@2': 'done', '@3': 'idle'})
        self.assertEqual(workspaces, {'alpha': 'done', 'beta': 'idle'})

    def test_pane_at_the_shell_has_no_status(self):
        rows = [pane('%1', 'blocked', command='zsh'), pane('%2', ''), pane('%3', 'unknown')]
        self.assertEqual(ws.roll_up(rows), ({}, {}, {}))

    def test_parked_pane_counts_toward_its_home_workspace(self):
        rows = [pane('%1', 'working'), pane('%2', 'done', '@9', session=ws.PARKING, home='beta')]
        panes, _, workspaces = ws.roll_up(rows)
        self.assertEqual(panes['%2'], 'done')
        self.assertEqual(workspaces, {'alpha': 'working', 'beta': 'done'})

    def test_failing_read_gives_no_status(self):
        with mock.patch.object(ws, 'tm', side_effect=subprocess.CalledProcessError(1, 'tmux')):
            self.assertEqual(ws.agent_status(), ({}, {}, {}))
            self.assertEqual(ws.workspace_row('alpha', '● alpha', ws.agent_status()[2].get('alpha', '')), 'alpha\t● alpha\t')

    def test_read_asks_tmux_once(self):
        line = ws.SEP.join(('alpha', '@1', '%1', 'omp', 'blocked', ''))
        with mock.patch.object(ws, 'tm', return_value=line) as tm:
            self.assertEqual(ws.agent_status(), ({'%1': 'blocked'}, {'@1': 'blocked'}, {'alpha': 'blocked'}))
        self.assertEqual(tm.call_count, 1)
        self.assertEqual(tm.call_args[0][:2], ('list-panes', '-a'))


class Rows(unittest.TestCase):
    def test_workspace_row_hides_the_name_in_the_first_field(self):
        row = ws.workspace_row('odoo-project', '● odoo-project', 'blocked')
        name, label, status = row.split('\t')
        self.assertEqual((name, label), ('odoo-project', '● odoo-project'))
        self.assertIn('blocked', status)
        self.assertTrue(status.startswith('\x1b['))
        self.assertEqual(ws.workspace_row('notes', '● notes'), 'notes\t● notes\t')

    def test_picked_name_is_the_last_non_empty_line(self):
        self.assertEqual(ws.picked_name('odoo\nodoo-project\n'), 'odoo-project')
        self.assertEqual(ws.picked_name('odoo-project\n'), 'odoo-project')
        self.assertEqual(ws.picked_name('new-client\n\n'), 'new-client')
        self.assertEqual(ws.picked_name('done\n'), 'done')
        self.assertEqual(ws.picked_name(' \n'), '')
        self.assertEqual(ws.picked_name(''), '')

    @unittest.skipUnless(shutil.which('fzf'), 'fzf is not installed')
    def test_status_word_never_reaches_the_returned_name(self):
        rows = '\n'.join((ws.workspace_row('odoo-project', '● odoo-project', 'blocked'), ws.workspace_row('notes', '● notes')))

        def filtered(query):
            return subprocess.run(ws.WORKSPACE_FZF + ['--filter=' + query], input=rows, capture_output=True, text=True).stdout

        self.assertEqual(ws.picked_name(filtered('odoo')), 'odoo-project')
        self.assertEqual(ws.picked_name(filtered('blocked')), '')
        self.assertIn('--bind=enter:accept-or-print-query', ws.WORKSPACE_FZF)

    def test_tab_index_is_the_same_with_and_without_a_status_word(self):
        plain = ws.tab_row('3', 'editor', '2', '1:omp, 2:shell', False)
        marked = ws.tab_row('3', 'editor', '2', '1:omp, 2:shell', True, 'working')
        self.assertNotIn('working', plain)
        self.assertLess(marked.index('editor'), marked.index('working'))
        self.assertEqual((ws.tab_index(plain), ws.tab_index(marked)), ('3', '3'))

    def test_parked_row_shows_the_status_word_and_keeps_its_number(self):
        parked = {'label': 'logs', 'home': 'alpha'}
        plain = ws.parked_row(2, parked)
        self.assertEqual(plain, ' 2. logs  [alpha]')
        marked = ws.parked_row(2, parked, 'done')
        self.assertTrue(marked.startswith(plain))
        self.assertIn('done', marked)


if __name__ == '__main__':
    unittest.main()
