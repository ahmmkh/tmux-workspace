"""User-approved actions apply only to the exact still-current tmux context."""
from concurrent.futures import ThreadPoolExecutor
import copy
import io
import os
import sys
from pathlib import Path
from queue import Queue
from threading import Event
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import ai
import ai_core


class Terminal(io.StringIO):
    def isatty(self):
        return True


class FakeTmux:
    """Stateful external boundary; production collection and action code stay real."""

    def __init__(self):
        self.panes = {}
        self.windows = {}
        self.options = {}
        self.global_options = {}
        self.focus = {'session': 'work', 'window': '@1', 'pane': '%1'}
        self.add('%1', '@1', 'work', 'editor')
        self.add('%2', '@2', '__parked', 'selected')
        self.add('%3', '@3', '__parked', 'recent')
        self.add('%4', '@4', 'other', 'review')
        self.workspace = {
            'tm': self.tm, 'fields': self.fields, 'option': self.option,
            'process_table': lambda: {}, 'live_app': lambda *args: None,
            'PARKING': '__parked',
        }

    def add(self, pane_id, window_id, session, name):
        self.windows[window_id] = name
        self.panes[pane_id] = {
            'pid': '700', 'session_name': session, 'window_id': window_id,
            'window_index': window_id[1:], 'pane_id': pane_id,
            'pane_index': '0', 'pane_pid': str(1000 + int(pane_id[1:])),
            'pane_current_path': '/synthetic/' + name,
            'pane_current_command': 'zsh', '@workspace_id': 'stable-' + pane_id,
            '@workspace_label': name, '@workspace_app': '',
        }
        self.options[pane_id] = {}
        if session == '__parked':
            self.options[pane_id].update({
                '@workspace_parked': '1', '@workspace_home': 'work:@1',
            })

    def fields(self, command, names, *args):
        if command == 'list-windows':
            rows = [{'window_id': key, 'window_name': value}
                    for key, value in self.windows.items()]
        elif command == 'list-panes':
            rows = []
            for pane_id, pane in self.panes.items():
                row = dict(pane, window_name=self.windows[pane['window_id']])
                for name in names:
                    if name not in row:
                        row[name] = self.options[pane_id].get(
                            name, self.global_options.get(name, ''))
                rows.append(row)
        else:
            raise AssertionError('Unexpected tmux read: ' + command)
        return [{name: row.get(name, '') for name in names} for row in rows]

    def option(self, name, pane_id):
        return self.options.get(pane_id, {}).get(name, '')

    def tm(self, command, *args):
        target = args[args.index('-t') + 1] if '-t' in args else None
        if command == 'switch-client':
            self.focus['session'] = target.removeprefix('=')
        elif command == 'select-window':
            self.focus['window'] = target
        elif command == 'select-pane':
            self.focus['pane'] = target
        elif command == 'join-pane':
            source = args[args.index('-s') + 1]
            if source not in self.panes or target not in self.panes:
                raise RuntimeError('tmux target disappeared')
            destination = self.panes[target]
            self.panes[source].update({key: destination[key] for key in (
                'session_name', 'window_id', 'window_index')})
            self.focus = {'session': destination['session_name'],
                          'window': destination['window_id'], 'pane': source}
        elif command == 'set-option':
            name = args[args.index('-t') + 2]
            if target not in self.panes:
                raise RuntimeError('tmux target disappeared')
            if '-pu' in args:
                self.options[target].pop(name, None)
            else:
                self.options[target][name] = args[-1]
        elif command == 'rename-window':
            if target not in self.windows:
                raise RuntimeError('tmux target disappeared')
            self.windows[target] = args[-1]
        elif command == 'capture-pane':
            return 'Synthetic output for ' + target
        else:
            raise AssertionError('Unexpected tmux mutation: ' + command)
        return ''

    def snapshot(self):
        return copy.deepcopy((self.panes, self.windows, self.options,
                              self.global_options, self.focus))

    def pane(self, pane_id):
        return next(pane for pane in ai_core.collect_panes(self.workspace)
                    if pane['pane_id'] == pane_id)

    def proposal(self, window_id='@1'):
        return {'window_id': window_id, 'previous_name': self.windows[window_id],
                'name': 'Billing review',
                'panes': {pane['pane_id']: pane['fingerprint']
                          for pane in ai_core.collect_panes(self.workspace)
                          if pane['window_id'] == window_id}}

    def change(self, pane_id, kind):
        pane = self.panes[pane_id]
        if kind == 'deleted':
            del self.panes[pane_id]
            del self.options[pane_id]
        elif kind == 'reused':
            pane['pane_pid'] = '9999'
            pane['@workspace_id'] = 'replacement'
        elif kind == 'cwd':
            pane['pane_current_path'] = '/synthetic/new-task'
        elif kind == 'process':
            pane['pane_current_command'] = 'python'
        elif kind == 'server-restarted':
            pane['pid'] = '701'
        elif kind == 'moved':
            pane.update({'window_id': '@4', 'window_index': '4',
                         'session_name': 'other'})
        elif kind == 'parked':
            pane.update({'window_id': '@3', 'window_index': '3',
                         'session_name': '__parked'})
        else:
            raise AssertionError(kind)


class ActionSafety(unittest.TestCase):
    def setUp(self):
        self.tmux = FakeTmux()
        self.ws = self.tmux.workspace
        self.enterContext(patch.dict(os.environ, {'TMUX_PANE': '%1', 'PATH': ''}))
        self.enterContext(patch.object(sys, 'stdout', Terminal()))
        self.enterContext(patch.object(sys, 'stdin', Terminal()))

    def during_confirmation(self, change, answer):
        def confirm(_prompt):
            change()
            self.after_external_change = self.tmux.snapshot()
            return answer
        return patch('builtins.input', side_effect=confirm)

    def test_live_navigation_selects_exact_session_window_and_pane(self):
        target = self.tmux.pane('%4')
        with patch('builtins.input', side_effect=AssertionError('Unexpected prompt')):
            self.assertTrue(ai.navigate(self.ws, target))
        self.assertEqual(self.tmux.focus,
                         {'session': 'other', 'window': '@4', 'pane': '%4'})

    def test_stale_live_pane_is_not_selected(self):
        for change in ('deleted', 'reused', 'cwd', 'process', 'server-restarted'):
            with self.subTest(change=change):
                tmux = FakeTmux()
                selected = tmux.pane('%4')
                tmux.change('%4', change)
                before = tmux.snapshot()
                self.assertFalse(ai.navigate(tmux.workspace, selected))
                self.assertEqual(tmux.snapshot(), before)

    def test_parked_cancel_or_nonaffirmative_keeps_all_panes_parked(self):
        for answer in ('', 'n', 'yes', 'SHARE'):
            with self.subTest(answer=answer), patch('builtins.input', return_value=answer):
                before = self.tmux.snapshot()
                self.assertFalse(ai.navigate(self.ws, self.tmux.pane('%2')))
                self.assertEqual(self.tmux.snapshot(), before)

    def test_confirmed_unpark_moves_exact_selected_pane_not_latest(self):
        other_parked = copy.deepcopy(self.tmux.panes['%3'])
        other_options = copy.deepcopy(self.tmux.options['%3'])
        with patch('builtins.input', return_value=' Y '):
            self.assertTrue(ai.navigate(self.ws, self.tmux.pane('%2')))
        self.assertEqual(self.tmux.panes['%2']['window_id'], '@1')
        self.assertEqual(self.tmux.panes['%2']['session_name'], 'work')
        self.assertFalse(self.tmux.pane('%2')['parked'])
        self.assertNotIn('@workspace_parked', self.tmux.options['%2'])
        self.assertNotIn('@workspace_home', self.tmux.options['%2'])
        self.assertEqual(self.tmux.panes['%3'], other_parked)
        self.assertEqual(self.tmux.options['%3'], other_options)

    def test_unpark_rejects_missing_or_parked_destination(self):
        for target in ('', '%404', '%3'):
            with self.subTest(target=target), patch.dict(os.environ, {'TMUX_PANE': target}):
                before = self.tmux.snapshot()
                with patch('builtins.input', side_effect=AssertionError('Must not confirm invalid destination')):
                    self.assertFalse(ai.navigate(self.ws, self.tmux.pane('%2')))
                self.assertEqual(self.tmux.snapshot(), before)

    def test_unpark_rejects_stale_source_before_confirmation(self):
        for kind in ('deleted', 'reused', 'cwd', 'process'):
            with self.subTest(kind=kind):
                tmux = FakeTmux()
                selected = tmux.pane('%2')
                tmux.change('%2', kind)
                before = tmux.snapshot()
                with patch('builtins.input', side_effect=AssertionError('Stale source prompted')):
                    self.assertFalse(ai.navigate(tmux.workspace, selected))
                self.assertEqual(tmux.snapshot(), before)

    def test_unpark_revalidates_source_during_confirmation(self):
        for kind in ('deleted', 'reused', 'cwd', 'process', 'moved', 'server-restarted'):
            with self.subTest(kind=kind):
                self.tmux = FakeTmux()
                self.ws = self.tmux.workspace
                selected = self.tmux.pane('%2')
                with self.during_confirmation(lambda: self.tmux.change('%2', kind), 'y'):
                    self.assertFalse(ai.navigate(self.ws, selected))
                self.assertEqual(self.tmux.snapshot(), self.after_external_change)

    def test_unpark_revalidates_destination_during_confirmation(self):
        for kind in ('deleted', 'reused', 'cwd', 'process', 'moved', 'parked'):
            with self.subTest(kind=kind):
                self.tmux = FakeTmux()
                self.ws = self.tmux.workspace
                selected = self.tmux.pane('%2')
                with self.during_confirmation(lambda: self.tmux.change('%1', kind), 'y'):
                    self.assertFalse(ai.navigate(self.ws, selected))
                self.assertEqual(self.tmux.snapshot(), self.after_external_change)

    def test_rename_requires_affirmative_confirmation_and_changes_only_window_name(self):
        proposal = self.tmux.proposal()
        for answer in ('', 'n', 'yes'):
            with self.subTest(answer=answer), patch('builtins.input', return_value=answer):
                before = self.tmux.snapshot()
                self.assertFalse(ai.rename_tab(self.ws, proposal))
                self.assertEqual(self.tmux.snapshot(), before)
        panes = copy.deepcopy(self.tmux.panes)
        with patch('builtins.input', return_value='y'):
            self.assertTrue(ai.rename_tab(self.ws, proposal))
        self.assertEqual(self.tmux.windows,
                         {'@1': 'Billing review', '@2': 'selected', '@3': 'recent', '@4': 'review'})
        self.assertEqual(self.tmux.panes, panes)

    def test_rename_rejects_context_changed_before_confirmation(self):
        for kind in ('deleted', 'reused', 'cwd', 'process', 'moved'):
            with self.subTest(kind=kind):
                tmux = FakeTmux()
                proposal = tmux.proposal()
                tmux.change('%1', kind)
                before = tmux.snapshot()
                with patch('builtins.input', side_effect=AssertionError('Stale context prompted')):
                    self.assertFalse(ai.rename_tab(tmux.workspace, proposal))
                self.assertEqual(tmux.snapshot(), before)

    def test_rename_rejects_stale_or_deleted_window_before_confirmation(self):
        for change in ('renamed', 'deleted'):
            with self.subTest(change=change):
                tmux = FakeTmux()
                proposal = tmux.proposal()
                if change == 'renamed':
                    tmux.windows['@1'] = 'Manual name'
                else:
                    tmux.change('%1', 'deleted')
                    del tmux.windows['@1']
                before = tmux.snapshot()
                with patch('builtins.input', side_effect=AssertionError('Stale window prompted')):
                    self.assertFalse(ai.rename_tab(tmux.workspace, proposal))
                self.assertEqual(tmux.snapshot(), before)

    def test_manual_rename_during_confirmation_wins(self):
        proposal = self.tmux.proposal()
        with self.during_confirmation(lambda: self.tmux.windows.update({'@1': 'Manual name'}), 'y'):
            self.assertFalse(ai.rename_tab(self.ws, proposal))
        self.assertEqual(self.tmux.snapshot(), self.after_external_change)

    def test_rename_revalidates_task_context_during_confirmation(self):
        for kind in ('deleted', 'reused', 'cwd', 'process', 'moved', 'server-restarted'):
            with self.subTest(kind=kind):
                self.tmux = FakeTmux()
                self.ws = self.tmux.workspace
                proposal = self.tmux.proposal()
                with self.during_confirmation(lambda: self.tmux.change('%1', kind), 'y'):
                    self.assertFalse(ai.rename_tab(self.ws, proposal))
                self.assertEqual(self.tmux.snapshot(), self.after_external_change)

    def test_rename_rejects_new_pane_joining_during_confirmation(self):
        proposal = self.tmux.proposal()
        with self.during_confirmation(lambda: self.tmux.add('%5', '@1', 'work', 'editor'), 'y'):
            self.assertFalse(ai.rename_tab(self.ws, proposal))
        self.assertEqual(self.tmux.snapshot(), self.after_external_change)

    def test_rename_rejects_reused_same_name_window_during_confirmation(self):
        proposal = self.tmux.proposal()
        def replace_window():
            self.tmux.change('%1', 'deleted')
            del self.tmux.windows['@1']
            self.tmux.add('%5', '@1', 'work', 'editor')
        with self.during_confirmation(replace_window, 'y'):
            self.assertFalse(ai.rename_tab(self.ws, proposal))
        self.assertEqual(self.tmux.snapshot(), self.after_external_change)

    def test_share_picker_cancellation_does_not_grant_permission(self):
        before = self.tmux.snapshot()
        with patch('builtins.input', return_value=''):
            ai.sharing(self.ws)
        self.assertEqual(self.tmux.snapshot(), before)

    def test_share_exact_opt_in_and_revoke_are_per_pane(self):
        with patch('builtins.input', side_effect=['2', 'SHARE']):
            ai.sharing(self.ws)
        self.assertEqual([pane['pane_id'] for pane in ai_core.collect_panes(self.ws)
                          if pane['shared']], ['%2'])
        excerpts = {pane['pane_id']: pane['excerpt']
                    for pane in ai_core.collect_panes(self.ws, excerpts=True)}
        self.assertEqual(excerpts['%2'], 'Synthetic output for %2')
        self.assertEqual({key: value for key, value in excerpts.items() if key != '%2'},
                         {'%1': '', '%3': '', '%4': ''})
        ai.sharing(self.ws, '%2', off=True)
        self.assertFalse(self.tmux.pane('%2')['shared'])
        self.assertTrue(all(not pane['excerpt'] for pane in ai_core.collect_panes(self.ws, excerpts=True)))

    def test_share_requires_exact_acknowledgment(self):
        for answer in ('', 'y', 'yes', 'share'):
            with self.subTest(answer=answer), patch('builtins.input', return_value=answer):
                before = self.tmux.snapshot()
                ai.sharing(self.ws, '%1')
                self.assertEqual(self.tmux.snapshot(), before)

    def test_noninteractive_share_requires_explicit_allow(self):
        with patch.object(sys, 'stdin', io.StringIO()):
            before = self.tmux.snapshot()
            with self.assertRaises(ValueError):
                ai.sharing(self.ws, '%1')
            self.assertEqual(self.tmux.snapshot(), before)
            ai.sharing(self.ws, '%1', allow=True)
        self.assertTrue(self.tmux.pane('%1')['shared'])
        self.assertFalse(self.tmux.pane('%2')['shared'])

    def test_share_rejects_missing_pane_or_unstable_identity_even_with_allow(self):
        self.tmux.panes['%1']['@workspace_id'] = ''
        for pane_id in ('%404', '%1'):
            with self.subTest(pane_id=pane_id):
                before = self.tmux.snapshot()
                with self.assertRaises(ValueError):
                    ai.sharing(self.ws, pane_id, allow=True)
                self.assertEqual(self.tmux.snapshot(), before)

    def test_share_revalidates_identity_during_confirmation(self):
        for kind in ('deleted', 'reused', 'cwd', 'process', 'server-restarted'):
            with self.subTest(kind=kind):
                self.tmux = FakeTmux()
                self.ws = self.tmux.workspace
                with self.during_confirmation(lambda: self.tmux.change('%1', kind), 'SHARE'):
                    with self.assertRaises(ValueError):
                        ai.sharing(self.ws, '%1')
                self.assertEqual(self.tmux.snapshot(), self.after_external_change)

    def test_share_toggle_revokes_and_inherited_permission_cannot_regrant(self):
        self.tmux.global_options['@workspace_ai_share'] = '1'
        self.assertTrue(all(not pane['shared'] for pane in ai_core.collect_panes(self.ws)))
        ai.sharing(self.ws, '%1', allow=True)
        self.assertTrue(self.tmux.pane('%1')['shared'])
        with patch('builtins.input', side_effect=AssertionError('Revocation must not prompt')):
            ai.sharing(self.ws, '%1')
            ai.sharing(self.ws, '%1', off=True)
        self.assertTrue(all(not pane['shared'] for pane in ai_core.collect_panes(self.ws)))
        self.assertTrue(all(not pane['excerpt'] for pane in ai_core.collect_panes(self.ws, excerpts=True)))

    def test_share_permission_does_not_survive_tmux_restart(self):
        ai.sharing(self.ws, '%1', allow=True)
        old_record = self.tmux.pane('%1')
        self.tmux.change('%1', 'server-restarted')
        self.tmux.options['%1'].clear()
        fresh = self.tmux.pane('%1')
        self.assertNotEqual(fresh['fingerprint'], old_record['fingerprint'])
        self.assertFalse(fresh['shared'])
        self.assertEqual(next(pane for pane in ai_core.collect_panes(self.ws, excerpts=True)
                              if pane['pane_id'] == '%1')['excerpt'], '')

    def test_menu_check_waits_for_dismissal_but_direct_command_exits(self):
        for args, failed in (([], False), (['check'], False), ([], True), (['check'], True)):
            with self.subTest(args=args, failed=failed):
                choices = iter(['7'] if not args else [])
                events, dismissed = Queue(), Event()

                def user_input(_prompt):
                    try:
                        return next(choices)
                    except StopIteration:
                        events.put('waiting')
                        if not dismissed.wait(5):
                            raise AssertionError('Diagnostic did not receive dismissal')
                        return ''

                def run_check():
                    try:
                        ai.main(self.ws, args)
                    finally:
                        events.put('closed')

                with patch.object(ai_core, '_request', return_value={
                        'answers': {'check': {'type': 'noul', 'noul': 1}}},
                        side_effect=ai_core.AIError('Provider unavailable') if failed else None), \
                        patch('builtins.input', side_effect=user_input), \
                        ThreadPoolExecutor(max_workers=1) as pool:
                    result = pool.submit(run_check)
                    try:
                        self.assertEqual(events.get(timeout=5), 'closed' if args else 'waiting')
                    finally:
                        dismissed.set()
                    if failed:
                        with self.assertRaises(ValueError):
                            result.result(timeout=5)
                    else:
                        result.result(timeout=5)
                    if not args:
                        self.assertEqual(events.get(timeout=5), 'closed')

    def test_cli_result_selection_alone_cannot_navigate(self):
        def rank_candidates(_state, questions):
            return {'answers': {name: {'type': 'score', 'score': 2,
                    'probabilities': {'0': 0, '1': 0, '2': 1}, 'confidence': 1}
                    for name in questions}}
        for answers, expected in ((['4', 'q'], '%1'), (['4', '', ''], '%1'),
                                  (['4', 'yes', ''], '%1'), (['4', 'g'], '%4')):
            with self.subTest(answers=answers):
                tmux = FakeTmux()
                before = tmux.snapshot()
                with patch.dict(os.environ, {'OPENROUTER_API_KEY': 'synthetic-test-key'}), \
                        patch.object(ai_core, '_request', side_effect=rank_candidates), \
                        patch('builtins.input', side_effect=answers):
                    ai_core.api_key.cache_clear()
                    try:
                        ai.main(tmux.workspace, ['find', 'review'])
                    finally:
                        ai_core.api_key.cache_clear()
                self.assertEqual(tmux.focus['pane'], expected)
                if expected == '%1':
                    self.assertEqual(tmux.snapshot(), before)


if __name__ == '__main__':
    unittest.main()
