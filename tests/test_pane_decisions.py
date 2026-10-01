"""Pane decisions preserve consent and route bounded, source-backed judgments."""
import json
import os
from pathlib import Path
import sys
import unicodedata
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import ai_core
import panes_ai


def pane(number=0, **changes):
    value = {
        'pane_id': f'%{number}', 'pane_pid': str(1000 + number),
        'workspace_id': f'workspace-{number}', 'session_name': 'synthetic',
        'window_id': '@0', 'window_name': 'shell', 'window_index': '0',
        'pane_index': str(number), 'cwd': '/tmp', 'label': 'shell',
        'command': 'zsh', 'parked': False, 'shared': True, 'home': '',
        'captured_at': 1700000000, 'fingerprint': f'identity-{number}',
        'app_kind': '', 'excerpt': 'Please approve the invoice migration.',
    }
    value.update(changes)
    return value


def choice_answer(question, selected):
    return {
        'type': 'choice', 'choice': selected, 'confidence': 1,
        'probabilities': {label: float(label == selected) for label in question['criteria']},
    }


def attention_response(state, questions, decisions=None):
    """Supply native external judgments; unspecified conditions have no evidence."""
    decisions = decisions or {}
    answers = {}
    for key, question in questions.items():
        name, condition, *evidence = key.split('_')
        probability, selected = decisions.get((name, condition), (0.0, 'none'))
        answers[key] = (choice_answer(question, selected) if evidence else
                        {'type': 'noul', 'noul': probability})
    return {'answers': answers}


class PaneDecisionTests(unittest.TestCase):
    def setUp(self):
        self.enterContext(patch.object(ai_core, 'api_key', return_value='synthetic-pane-test-key'))

    def test_revocation_during_credentials_aborts_terminal_workflows(self):
        for feature in ('find', 'attention', 'name_tab'):
            with self.subTest(feature=feature):
                source = pane(excerpt='Please approve invoice reconciliation.')
                def revoke():
                    source['shared'] = False
                    return 'synthetic-pane-test-key'
                with self.collect([source]), patch.dict(os.environ, {'TMUX_PANE': '%0'}), \
                        patch.object(ai_core, 'api_key', side_effect=revoke), \
                        patch.object(ai_core, '_request', side_effect=AssertionError('Revoked content reached model')):
                    if feature == 'attention':
                        items, _ = panes_ai.attention({})
                    elif feature == 'find':
                        items, _ = panes_ai.find({}, 'invoice reconciliation')
                    else:
                        items, _ = panes_ai.name_tab({}, 'Invoice reconciliation')
                self.assertFalse(source['shared'])
                self.assertEqual(items, [])

    def collect(self, panes):
        def collected(workspace, excerpts=False, pane_ids=None):
            return [value.copy() for value in panes
                    if pane_ids is None or value['pane_id'] in pane_ids]
        return patch.object(panes_ai, 'collect_panes', side_effect=collected)

    def assert_private_absent(self, requests, marker):
        self.assertNotIn(marker, json.dumps(requests, ensure_ascii=False))

    def test_attention_routes_all_independent_conditions_for_one_pane(self):
        source = 'Please approve deployment.\nBlocked by missing schema.\nReview completed changes.'
        decisions = {('P000', condition): (0.9, line)
                     for condition, line in [('input', 'L001'), ('blocker', 'L002'), ('review', 'L003')]}
        with self.collect([pane(excerpt=source)]), patch.object(
                ai_core, '_request', side_effect=lambda state, questions:
                attention_response(state, questions, decisions)):
            items, _ = panes_ai.attention({})
        self.assertEqual({item['id'] for item in items}, {
            'attention:%0:input', 'attention:%0:blocker', 'attention:%0:review'})
        for item in items:
            selected = item['judgments'][item['id'].rsplit(':', 1)[1] + '_evidence']['choice']
            self.assertIn(source.splitlines()[int(selected[1:]) - 1], item['detail'])
            self.assertNotIn('rename', item)

    def test_attention_keeps_supported_and_disputed_conditions_separate(self):
        decisions = {('P000', 'input'): (0.8, 'L001'),
                     ('P000', 'blocker'): (0.9, 'none'),
                     ('P000', 'review'): (0.1, 'unknown')}
        with self.collect([pane()]), patch.object(ai_core, '_request', side_effect=
                lambda state, questions: attention_response(state, questions, decisions)):
            items, _ = panes_ai.attention({})
        self.assertEqual({item['id'] for item in items},
                         {'attention:%0:input', 'attention:%0:unclassified'})

    def test_attention_threshold_does_not_turn_tie_into_supported_finding(self):
        decisions = {('P000', 'input'): (0.5, 'L001'),
                     ('P001', 'input'): (0.500001, 'L001'),
                     ('P002', 'input'): (0.499999, 'none')}
        with self.collect([pane(n) for n in range(3)]), patch.object(
                ai_core, '_request', side_effect=lambda state, questions:
                attention_response(state, questions, decisions)):
            items, _ = panes_ai.attention({})
        self.assertEqual({item['id'] for item in items},
                         {'attention:%0:unclassified', 'attention:%1:input'})

    def test_attention_low_probability_with_selected_source_is_unclassified(self):
        decisions = {('P000', 'input'): (0.1, 'L001')}
        with self.collect([pane()]), patch.object(ai_core, '_request', side_effect=
                lambda state, questions: attention_response(state, questions, decisions)):
            items, _ = panes_ai.attention({})
        self.assertEqual([item['id'] for item in items], ['attention:%0:unclassified'])

    def test_attention_resolved_and_empty_output_do_not_become_ready_states(self):
        panes = [pane(excerpt='Approval needed.\nApproval received; deployment continued.'),
                 pane(1, excerpt=' \n\t\n')]
        with self.collect(panes), patch.object(ai_core, '_request', side_effect=attention_response):
            items, notice = panes_ai.attention({})
        self.assertEqual(items, [])
        self.assertTrue(notice)

    def test_attention_excludes_unshared_output_from_outgoing_state(self):
        private = 'PRIVATE_TERMINAL_MARKER'
        requests = []
        def respond(state, questions):
            requests.append((state, questions))
            return attention_response(state, questions, {('P000', 'input'): (0.9, 'L001')})
        with self.collect([pane(), pane(1, shared=False, excerpt=private)]), patch.object(
                ai_core, '_request', side_effect=respond):
            items, _ = panes_ai.attention({})
        self.assertEqual([item['id'] for item in items], ['attention:%0:input'])
        self.assertTrue(requests)
        self.assert_private_absent(requests, private)
        self.assertNotIn(private, '\n'.join(item['detail'] for item in items))

    def test_attention_without_consent_never_calls_model(self):
        def forbidden(*args):
            self.fail('No consent permits no attention request')
        with self.collect([pane(shared=False)]), patch.object(ai_core, '_request', side_effect=forbidden):
            items, notice = panes_ai.attention({})
        self.assertEqual(items, [])
        self.assertTrue(notice)

    def test_attention_out_of_set_source_degrades_to_unclassified(self):
        def invalid(state, questions):
            response = attention_response(state, questions)
            response['answers']['P000_input'] = {'type': 'noul', 'noul': 1}
            response['answers']['P000_input_evidence']['choice'] = 'L999'
            return response
        with self.collect([pane()]), patch.object(ai_core, '_request', side_effect=invalid):
            items, notice = panes_ai.attention({})
        self.assertEqual([item['id'] for item in items], ['attention:%0:unclassified'])
        self.assertIn('Please approve the invoice migration.', items[0]['detail'])
        self.assertTrue(notice)

    def test_attention_failed_batch_does_not_discard_other_batches(self):
        panes = [pane(n, excerpt=f'Approval request {n}') for n in range(17)]
        requests = []
        failed = set()
        def respond(state, questions):
            requests.append((state, questions))
            if 'P008' in state['panes']:
                failed.update(int(source['source_lines']['L001'].rsplit(' ', 1)[1])
                              for source in state['panes'].values())
                raise ai_core.AIError('Synthetic outage')
            decisions = {(name, 'input'): (0.9, 'L001') for name in state['panes']}
            return attention_response(state, questions, decisions)
        with self.collect(panes), patch.object(ai_core, '_request', side_effect=respond):
            items, notice = panes_ai.attention({})
        self.assertEqual({item['id'] for item in items}, {
            f'attention:%{n}:' + ('unclassified' if n in failed else 'input')
            for n in range(17)})
        self.assertGreater(len(requests), 1)
        self.assertTrue(all(len(state['panes']) <= 8 for state, _ in requests))
        self.assertIn('Synthetic outage', notice)

    def test_attention_oversized_sources_stay_local_while_small_source_is_judged(self):
        long_lines = '\n'.join(f'OVERSIZED_SOURCE {n}' for n in range(254))
        long_bytes = 'BYTE_LIMIT_SOURCE ' + 'x' * 65000
        requests = []
        def respond(state, questions):
            requests.append((state, questions))
            decisions = {(name, 'input'): (0.9, 'L001') for name in state['panes']}
            return attention_response(state, questions, decisions)
        with self.collect([pane(excerpt=long_lines), pane(1, excerpt=long_bytes), pane(2)]), patch.object(
                ai_core, '_request', side_effect=respond):
            items, _ = panes_ai.attention({})
        self.assertEqual({item['id'] for item in items}, {
            'attention:%0:unclassified', 'attention:%1:unclassified', 'attention:%2:input'})
        self.assert_private_absent(requests, 'OVERSIZED_SOURCE')
        self.assert_private_absent(requests, 'BYTE_LIMIT_SOURCE')
        self.assertIn(long_lines, next(item['detail'] for item in items if item['pane']['pane_id'] == '%0'))

    def test_find_ranks_shared_and_private_metadata_without_private_excerpt(self):
        private = 'PRIVATE_SEARCH_MARKER'
        panes = [pane(label='related'), pane(1, label='direct', parked=True),
                 pane(2, label='irrelevant'), pane(3, label='tied', shared=False, excerpt=private)]
        distributions = [(0.1, 0.7, 0.2), (0.0, 0.1, 0.9), (1.0, 0.0, 0.0), (0.5, 0.0, 0.5)]
        requests = []
        def respond(state, questions):
            requests.append((state, questions))
            answers = {}
            for (key, _), values in zip(questions.items(), distributions):
                answers[key] = {'type': 'score', 'confidence': 0.8,
                                'score': sum(i * value for i, value in enumerate(values)),
                                'probabilities': {str(i): value for i, value in enumerate(values)}}
            return {'answers': answers}
        with self.collect(panes), patch.object(ai_core, 'api_key', return_value='synthetic'), patch.object(
                ai_core, '_request', side_effect=respond):
            items, _ = panes_ai.find({}, 'invoice')
        self.assertEqual([item['id'] for item in items], ['pane:%1', 'pane:%0'])
        self.assertEqual([item['relevance'] for item in items], ['direct', 'related'])
        self.assert_private_absent(requests, private)
        self.assertIn('tied', json.dumps(requests))
        self.assertIn('Please approve the invoice migration.', json.dumps(requests))

    def test_find_service_failure_returns_explicit_local_results_without_private_excerpt(self):
        private = 'PRIVATE_FALLBACK_MARKER'
        panes = [pane(), pane(1, shared=False, excerpt=private)]
        with self.collect(panes), patch.object(ai_core, 'api_key', return_value='synthetic'), patch.object(
                ai_core, '_request', side_effect=ai_core.AIError('Synthetic outage')):
            items, notice = panes_ai.find({}, 'invoice')
        self.assertEqual({item['id'] for item in items}, {'pane:%0', 'pane:%1'})
        self.assertEqual({item['relevance'] for item in items}, {'local'})
        self.assertNotIn(private, '\n'.join(item['detail'] for item in items))
        self.assertIn('Synthetic outage', notice)

    def test_find_batches_large_catalog_without_losing_candidates(self):
        panes = [pane(n, label=f'project-{n}', excerpt='synthetic context ' * 230) for n in range(40)]
        requests = []
        def respond(state, questions):
            requests.append((state, questions))
            return {'answers': {key: {'type': 'score', 'score': 2, 'confidence': 1,
                                      'probabilities': {'0': 0, '1': 0, '2': 1}}
                                for key in questions}}
        with self.collect(panes), patch.object(ai_core, 'api_key', return_value='synthetic'), patch.object(
                ai_core, '_request', side_effect=respond):
            items, _ = panes_ai.find({}, 'synthetic')
        self.assertEqual({item['id'] for item in items}, {f'pane:%{n}' for n in range(40)})
        self.assertGreater(len(requests), 1)
        self.assertEqual(sum(len(state['candidates']) for state, _ in requests), 40)
        self.assertTrue(all(len(json.dumps(state).encode()) < 65000 for state, _ in requests))

    def naming(self, panes, hint, selected, requests):
        def respond(state, questions):
            requests.append((state, questions))
            if selected == 'none':
                key = 'none'
            else:
                key = next((key for key, candidate in state['candidates'].items()
                            if candidate['text'] == selected), None)
                self.assertIsNotNone(key, f'Expected source-backed candidate {selected!r}')
            return {'answers': {'title': choice_answer(questions['title'], key)}}
        with patch.dict(os.environ, {'TMUX_PANE': '%0'}), self.collect(panes), patch.object(
                ai_core, '_request', side_effect=respond):
            return panes_ai.name_tab({}, hint)

    def test_name_tab_selects_unicode_and_combining_marks_without_generation(self):
        for hint in ('مراجعة الفواتير', 'Cafe\u0301 billing', 'हिन्दी भुगतान', 'client-onboarding'):
            with self.subTest(hint=hint):
                expected = unicodedata.normalize('NFC', hint)
                requests = []
                items, _ = self.naming([pane(shared=False)], hint, expected, requests)
                self.assertEqual([item['rename']['name'] for item in items], [expected])
                self.assertLessEqual(len(items[0]['rename']['name']), 40)
                self.assertIn(expected, [candidate['text'] for candidate in requests[0][0]['candidates'].values()])

    def test_name_tab_none_and_existing_name_produce_no_rename(self):
        for selected, current in [('none', 'shell'), ('Invoice review', 'Invoice review')]:
            with self.subTest(selected=selected):
                items, notice = self.naming([pane(window_name=current)], 'Invoice review', selected, [])
                self.assertEqual(items, [])
                self.assertTrue(notice)

    def test_name_tab_generic_context_cannot_create_name(self):
        def forbidden(*args):
            self.fail('Generic-only source must not create candidate judgments')
        with patch.dict(os.environ, {'TMUX_PANE': '%0'}), self.collect([
                pane(shared=False, excerpt='PRIVATE_GENERIC_MARKER')]), patch.object(
                ai_core, '_request', side_effect=forbidden):
            items, notice = panes_ai.name_tab({}, 'shell agent terminal')
        self.assertEqual(items, [])
        self.assertTrue(notice)

    def test_name_tab_unsafe_hints_and_excerpts_never_enter_candidate_state(self):
        unsafe = [
            '$(touch /tmp/SHELL_MARKER)', '#{TMUX_MARKER}',
            'password=PASSWORD_MARKER', 'Bearer BEARER_MARKER',
            'curl https://EXFILTRATION_MARKER.invalid', 'git checkout COMMAND_MARKER',
            '```\nFENCED_MARKER\n```', '    INDENTED_MARKER',
        ]
        markers = ['SHELL_MARKER', 'TMUX_MARKER', 'PASSWORD_MARKER', 'BEARER_MARKER',
                   'EXFILTRATION_MARKER', 'COMMAND_MARKER', 'FENCED_MARKER', 'INDENTED_MARKER']
        for hint in unsafe[:6]:
            with self.subTest(hint=hint):
                requests = []
                items, _ = self.naming([pane(label='Invoice review', excerpt='\n'.join(unsafe))],
                                       hint, 'Invoice review', requests)
                self.assertEqual([item['rename']['name'] for item in items], ['Invoice review'])
                for marker in markers:
                    self.assert_private_absent(requests, marker)

    def test_name_tab_excludes_private_and_other_window_sources(self):
        panes = [pane(shared=False, excerpt='PRIVATE_NAMING_MARKER'),
                 pane(1, label='Billing review', shared=True, excerpt='Payment reconciliation'),
                 pane(2, window_id='@2', label='OTHER_WINDOW_MARKER', excerpt='OTHER_EXCERPT_MARKER')]
        requests = []
        items, _ = self.naming(panes, '', 'Payment reconciliation', requests)
        self.assertEqual([item['rename']['name'] for item in items], ['Payment reconciliation'])
        self.assertEqual(set(items[0]['rename']['panes']), {'%0', '%1'})
        for marker in ('PRIVATE_NAMING_MARKER', 'OTHER_WINDOW_MARKER', 'OTHER_EXCERPT_MARKER'):
            self.assert_private_absent(requests, marker)
            self.assertNotIn(marker, items[0]['detail'])

    def test_name_tab_preserves_metadata_coverage_with_bounded_source_backed_candidates(self):
        hint = 'Invoice migration acceptance planning'
        source_lines = [f'Billing reconciliation phase {n} progress' for n in range(100)]
        panes = [pane(label='Ledger migration', cwd='/work/ledger-project', excerpt='\n'.join(source_lines)),
                 pane(1, label='Customer onboarding', cwd='/work/customer-project', excerpt='')]
        requests = []
        items, _ = self.naming(panes, hint, 'Customer onboarding', requests)
        candidates = requests[0][0]['candidates']
        texts = {candidate['text'] for candidate in candidates.values()}
        self.assertTrue({hint, 'Ledger migration', 'ledger-project', 'Customer onboarding',
                         'customer-project'} <= texts)
        self.assertLessEqual(len(candidates), 200)
        safe_sources = [hint, 'Ledger migration', 'ledger-project', 'Customer onboarding',
                        'customer-project', *source_lines]
        for text in texts:
            self.assertLessEqual(len(text), 40)
            self.assertLessEqual(len(text.split()), 6)
            self.assertTrue(any(text in source for source in safe_sources), text)
        self.assertEqual(items[0]['rename']['name'], 'Customer onboarding')

    def test_name_tab_oversized_sources_cannot_supply_partial_secret_like_names(self):
        requests = []
        items, _ = self.naming([pane(label='Invoice review', excerpt='OVERSIZED_MARKER ' * 100)],
                               'LONG_HINT_MARKER ' * 100, 'Invoice review', requests)
        self.assertEqual(items[0]['rename']['name'], 'Invoice review')
        self.assert_private_absent(requests, 'OVERSIZED_MARKER')
        self.assert_private_absent(requests, 'LONG_HINT_MARKER')

    def test_name_tab_service_failure_does_not_generate_local_replacement(self):
        with patch.dict(os.environ, {'TMUX_PANE': '%0'}), self.collect([pane(label='Invoice review')]), patch.object(
                ai_core, '_request', side_effect=ai_core.AIError('Synthetic outage')):
            items, notice = panes_ai.name_tab({})
        self.assertEqual(items, [])
        self.assertIn('Synthetic outage', notice)


if __name__ == '__main__':
    unittest.main()
