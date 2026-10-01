"""Exercise public decision/ranking APIs across the untrusted HTTP boundary."""
import io
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import ai_core as core

MODEL = 'typesafe/jev-1.13'
QUESTION = {'q': {'type': 'noul', 'instructions': 'Does the supplied text ask for input?'}}


def response(answers, model=MODEL):
    return io.BytesIO(json.dumps({'model': model, 'answers': answers}).encode())


def score_response(request):
    payload = json.loads(request.data)
    return response({key: {'type': 'score', 'score': 2, 'confidence': 1,
                           'probabilities': {'0': 0, '1': 0, '2': 1}}
                     for key in payload['questions']})


class TransportBoundaries(unittest.TestCase):
    def setUp(self):
        self.enterContext(patch.object(core, 'api_key', return_value='synthetic-key'))
        self.enterContext(patch.object(core, 'config', return_value={'model': MODEL}))

    def test_huge_json_numbers_are_rejected_without_arithmetic_crash(self):
        with patch.object(core.urllib.request, 'urlopen', return_value=response({'q': {'type': 'noul', 'noul': 10 ** 400}})):
            with self.assertRaises(core.AIError):
                core.judge({'text': 'Approve?'}, QUESTION)

    def test_http_failures_keep_complete_local_catalog_without_echoing_provider_body(self):
        records = [{'id': 'one', 'title': 'billing', 'detail': 'billing task'},
                   {'id': 'two', 'title': 'other', 'detail': 'other task'}]
        errors = [urllib.error.HTTPError(core.ENDPOINT, status, 'rejected', {}, io.BytesIO(b'PRIVATE_ECHO'))
                  for status in (401, 402, 429, 503)]
        errors += [TimeoutError('PRIVATE_ECHO'), urllib.error.URLError('PRIVATE_ECHO')]
        for error in errors:
            with self.subTest(error=type(error).__name__, code=getattr(error, 'code', None)), patch.object(core.urllib.request, 'urlopen', side_effect=error):
                items, notice = core.rank('billing', records)
            self.assertEqual({item['id'] for item in items}, {'one', 'two'})
            self.assertTrue(all(item['relevance'] == 'local' for item in items))
            self.assertNotIn('PRIVATE_ECHO', notice)
            self.assertNotIn('synthetic-key', notice)

    def test_bad_json_wrong_backend_and_incomplete_responses_fail_closed(self):
        bodies = [b'not json', b'null', b'[]', b'{"model":"typesafe/jev-1.13"}',
                  b'{"model":"openai/gpt-4.1-mini","answers":{"q":{"type":"noul","noul":1}}}',
                  b'{"model":"typesafe/jev-1.13","answers":{"q":{"type":"noul","noul":1},"other":{}}}']
        for body in bodies:
            with self.subTest(body=body), patch.object(core.urllib.request, 'urlopen', return_value=io.BytesIO(body)):
                with self.assertRaises(core.AIError):
                    core.judge({'text': 'Approve?'}, QUESTION)

    def test_oversize_evidence_is_rejected_before_transmission(self):
        def network(*args, **kwargs):
            self.fail('Oversize input reached the network')
        with patch.object(core.urllib.request, 'urlopen', side_effect=network):
            with self.assertRaises(core.AIError):
                core.judge({'text': 'x' * 180001}, QUESTION)

    def test_blank_search_does_not_transmit_catalog(self):
        records = [{'id': 'private', 'title': 'private', 'detail': 'PRIVATE_CATALOG'}]
        with patch.object(core.urllib.request, 'urlopen', side_effect=AssertionError('Unexpected transmission')):
            items, _ = core.rank('   ', records)
        self.assertEqual([item['id'] for item in items], ['private'])
        self.assertNotIn('judgment', items[0])

    def test_source_secrets_are_redacted_before_ranking_request(self):
        secret = 'sk-or-v1-' + 'z' * 40
        observed = []
        def network(request, **kwargs):
            observed.append(request.data.decode())
            return score_response(request)
        records = [{'id': 'one', 'title': 'billing ' + secret,
                    'detail': 'OPENROUTER_API_KEY=' + secret}]
        with patch.object(core.urllib.request, 'urlopen', side_effect=network):
            items, _ = core.rank('billing', records)
        self.assertEqual([item['id'] for item in items], ['one'])
        self.assertNotIn(secret, ''.join(observed))

    def test_multibyte_catalog_is_fully_ranked_across_bounded_requests(self):
        records = [{'id': str(i), 'title': 'invoice ' + str(i), 'detail': 'فاتورة' * 650} for i in range(75)]
        transmitted = []
        def network(request, **kwargs):
            self.assertLessEqual(len(request.data), 180000)
            transmitted.extend(candidate['title'] for candidate in json.loads(request.data)['state']['candidates'])
            return score_response(request)
        with patch.object(core.urllib.request, 'urlopen', side_effect=network):
            items, _ = core.rank('invoice', records)
        self.assertEqual({item['id'] for item in items}, {str(i) for i in range(75)})
        self.assertCountEqual(transmitted, ['invoice ' + str(i) for i in range(75)])

    def test_one_failed_batch_does_not_hide_catalog_entries(self):
        records = [{'id': str(i), 'title': 'task ' + str(i), 'detail': 'data ' * 800} for i in range(50)]
        def network(request, **kwargs):
            titles = [item['title'] for item in json.loads(request.data)['state']['candidates']]
            if 'task 49' in titles:
                raise TimeoutError('synthetic timeout')
            return score_response(request)
        with patch.object(core.urllib.request, 'urlopen', side_effect=network):
            items, _ = core.rank('task', records)
        self.assertEqual({item['id'] for item in items}, {str(i) for i in range(50)})
        self.assertTrue(all(item['relevance'] == 'local' for item in items))
        self.assertTrue(all('judgment' not in item for item in items))

    def test_tied_no_match_does_not_become_recommendation(self):
        answer = {'candidate_0': {'type': 'score', 'score': 1, 'confidence': 0,
                                 'probabilities': {'0': 0.5, '1': 0, '2': 0.5}}}
        with patch.object(core.urllib.request, 'urlopen', return_value=response(answer)):
            items, _ = core.rank('task', [{'id': 'one', 'title': 'one', 'detail': 'source'}])
        self.assertEqual(items, [])


if __name__ == '__main__':
    unittest.main()
