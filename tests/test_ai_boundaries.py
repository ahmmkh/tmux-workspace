"""Untrusted decisions cannot expand choices, leak secrets, or invent tab names."""
import io
import json
import os
import sys
from pathlib import Path
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import ai_core
import panes_ai


class ModelOutputBoundaries(unittest.TestCase):
    def test_closed_judgments_reject_missing_extra_and_invented_answers(self):
        questions = {'match': {'type': 'choice', 'instructions': 'Does this match?', 'criteria': {'yes': None, 'no': None}}}
        invalid = ({}, {'match': 'yes'}, {'match': True},
                   {'match': {'type': 'choice', 'choice': 'execute', 'probabilities': {'yes': 1, 'no': 0}, 'confidence': 1}},
                   {'match': {'type': 'noul', 'noul': 1}}, {'match': {}, 'command': 'kill-pane'})
        for answers in invalid:
            with self.subTest(answers=answers), patch.object(ai_core, '_request', return_value={'answers': answers}):
                with self.assertRaises(ai_core.AIError):
                    ai_core.judge({}, questions)

    def test_invalid_probabilities_and_inconsistent_scores_are_rejected(self):
        for probability in (float('nan'), float('inf'), -0.1, 1.1, True):
            with self.subTest(probability=probability), patch.object(ai_core, '_request', return_value={'answers': {'q': {'type': 'noul', 'noul': probability}}}):
                with self.assertRaises(ai_core.AIError):
                    ai_core.judge({}, {'q': {'type': 'noul', 'instructions': 'Does this match?'}})
        for probabilities in ({'0': 0, '1': 0}, {'0': 1, '1': 0}, {'0': 0, 'other': 1}):
            answer = {'type': 'score', 'score': 1, 'probabilities': probabilities, 'confidence': 1}
            with self.subTest(probabilities=probabilities), patch.object(ai_core, '_request', return_value={'answers': {'q': answer}}):
                with self.assertRaises(ai_core.AIError):
                    ai_core.judge({}, {'q': {'type': 'score', 'instructions': 'How relevant?', 'criteria': ['Unrelated', 'Direct']}})

    def test_non_jev_response_cannot_silently_replace_decisions(self):
        response = io.BytesIO(json.dumps({'model': 'openai/gpt-4.1-mini', 'answers': {}}).encode())
        with patch.object(ai_core, 'api_key', return_value='synthetic-test-key'), patch.object(ai_core.urllib.request, 'urlopen', return_value=response):
            with self.assertRaises(ai_core.AIError):
                ai_core.judge({}, {'q': {'type': 'noul', 'instructions': 'Ready?'}})

    def test_ranking_orders_expected_relevance_not_input_order(self):
        records = [{'id': name, 'title': name, 'detail': name} for name in ('weak', 'strong', 'unrelated')]
        distributions = ({'0': 0.1, '1': 0.3, '2': 0.6}, {'0': 0, '1': 0.1, '2': 0.9}, {'0': 1, '1': 0, '2': 0})
        answers = {f'candidate_{i}': {'type': 'score', 'score': value, 'probabilities': values, 'confidence': 0.5} for i, (value, values) in enumerate(zip((1.5, 1.9, 0), distributions))}
        with patch.object(ai_core, 'api_key', return_value='synthetic-test-key'), patch.object(ai_core, '_request', return_value={'answers': answers}):
            items, _ = ai_core.rank('task', records)
        self.assertEqual([item['id'] for item in items], ['strong', 'weak'])

    def test_tab_selection_cannot_invent_unsafe_names_or_read_unshared_text(self):
        pane = {'pane_id': '%0', 'window_id': '@0', 'window_name': 'shell', 'label': 'shell',
                'cwd': '/tmp', 'command': 'zsh', 'parked': False, 'shared': False,
                'fingerprint': 'fixture', 'excerpt': 'UNSHARED_PRIVATE_TEXT'}
        for hint in ('$(touch /tmp/owned)', '#{pane_id}', 'secret=do-not-copy', 'x' * 41):
            with self.subTest(hint=hint), patch.dict(os.environ, {'TMUX_PANE': '%0'}), patch.object(panes_ai, 'collect_panes', return_value=[pane]):
                items, _ = panes_ai.name_tab({}, hint)
                self.assertEqual(items, [])

    def test_inherited_tmux_option_cannot_grant_terminal_access(self):
        row = dict.fromkeys(('pid', 'session_name', 'window_id', 'window_name', 'window_index',
                            'pane_id', 'pane_index', 'pane_pid', 'pane_current_path', 'pane_current_command',
                            '@workspace_id', '@workspace_label', '@workspace_home', '@workspace_app'), '')
        row.update({'pane_id': '%0', '@workspace_ai_share': '1'})
        def terminal(*args):
            self.fail('Unshared pane output was read through an inherited option')
        workspace = {'tm': terminal, 'option': lambda *args: '',
                     'fields': lambda *args: [row], 'process_table': lambda: {},
                     'live_app': lambda *args: None, 'PARKING': '__parked'}
        panes = ai_core.collect_panes(workspace, excerpts=True)
        self.assertFalse(panes[0]['shared'])
        self.assertEqual(panes[0]['excerpt'], '')

    def test_common_secret_and_terminal_sequences_are_not_exposed(self):
        secret = 'sk-or-v1-' + 'a' * 32
        output = ai_core.clean('\x1b[31mhello\x1b[0m\nOPENROUTER_API_KEY=' + secret + '\nBearer abcdef1234\npostgres://alice:privatepass@db/app')
        self.assertNotIn(secret, output)
        self.assertNotIn('abcdef1234', output)
        self.assertNotIn('privatepass', output)
        self.assertNotIn('\x1b', output)
        self.assertIn('hello', output)


if __name__ == '__main__':
    unittest.main()
