"""Retained-history privacy, filesystem bounds, and source fidelity through search."""
import copy
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import ai_core
import history_ai


class RetainedHistoryBoundaries(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        # macOS's temporary-directory ancestors can themselves be symlinks.
        self.base = Path(temporary.name).resolve()
        self.root = self.base / 'state'
        self.root.mkdir(mode=0o700)
        (self.root / 'snapshots').mkdir(mode=0o700)
        self.panes = [self.pane(1)]
        self.live_reads = 0
        self.live = lambda: self.panes
        self.submissions = []
        self.respond = self.direct_matches
        for replacement in (
            patch.object(ai_core, 'collect_panes', side_effect=self.collect),
            patch.object(ai_core, 'api_key', return_value='synthetic-history-test-key'),
            patch.object(ai_core, '_request', side_effect=self.request),
        ):
            replacement.start()
            self.addCleanup(replacement.stop)

    @staticmethod
    def pane(number, shared=True):
        return {
            'workspace_id': f'{number:032x}', 'pane_id': f'%{number}',
            'pane_pid': str(1000 + number), 'shared': shared,
            'label': f'pane {number}', 'session_name': 'synthetic',
            'window_id': f'@{number}', 'window_name': 'synthetic',
        }

    def collect(self, *args, **kwargs):
        self.live_reads += 1
        return copy.deepcopy(self.live())

    def request(self, state, questions):
        self.submissions.append(copy.deepcopy(state))
        return self.respond(state, questions)

    @staticmethod
    def direct_matches(state, questions):
        return {'answers': {
            key: {'type': 'score', 'score': 2,
                  'probabilities': {'0': 0, '1': 0, '2': 1}, 'confidence': 1}
            for key in questions
        }}

    @staticmethod
    def write_private(path, content):
        path.write_bytes(content.encode('utf-8') if isinstance(content, str) else content)
        path.chmod(0o600)

    def archive(self, name='100', sources=None, saved=100, labels=None):
        if sources is None:
            sources = {self.panes[0]['workspace_id']: 'needle retained evidence\n'}
        directory = self.root / 'snapshots' / name
        directory.mkdir(mode=0o700)
        labels = labels or {}
        for uid, content in sources.items():
            self.write_private(directory / (uid + '.txt'), content)
        metadata = {
            'version': 1, 'saved_at': saved, 'directory': str(directory),
            'panes': [{'id': uid, 'label': labels.get(uid, 'synthetic pane')}
                      for uid in sources],
        }
        self.write_private(directory / 'workspace.json', json.dumps(metadata))
        return directory

    def metadata(self, snapshot, **changes):
        path = snapshot / 'workspace.json'
        data = json.loads(path.read_text())
        data.update(changes)
        self.write_private(path, json.dumps(data))

    def search(self, query='needle', root=None):
        return history_ai.search({'ROOT': root if root is not None else self.root}, query)

    def submitted_text(self):
        return json.dumps(self.submissions, ensure_ascii=False)

    def assert_not_disclosed(self, marker, items):
        escaped = json.dumps(marker, ensure_ascii=False)[1:-1]
        sources = [state.get('candidates', []) for state in self.submissions]
        self.assertNotIn(escaped, json.dumps(sources, ensure_ascii=False))
        self.assertNotIn(escaped, json.dumps(items, ensure_ascii=False))

    def assert_rejected(self, root=None):
        self.submissions.clear()
        items, notice = self.search(root=root)
        self.assertEqual(items, [])
        self.assertEqual(self.submissions, [])
        return notice

    def test_unshared_pane_cannot_join_an_authorized_panes_history(self):
        private = self.pane(2, shared=False)
        self.panes.append(private)
        self.archive(sources={
            self.panes[0]['workspace_id']: 'needle PUBLIC_ARCHIVE\n',
            private['workspace_id']: 'needle PRIVATE_ARCHIVE\n',
        })
        items, _ = self.search()
        self.assertEqual([item['search_text'] for item in items], ['needle PUBLIC_ARCHIVE\n'])
        self.assert_not_disclosed('PRIVATE_ARCHIVE', items)

    def test_duplicate_live_identity_is_ambiguous_even_if_one_pane_is_shared(self):
        copied = {**self.panes[0], 'pane_id': '%copied', 'pane_pid': '9002', 'shared': False}
        independent = self.pane(2)
        self.panes.extend([copied, independent])
        self.archive(sources={
            copied['workspace_id']: 'needle AMBIGUOUS_ARCHIVE\n',
            independent['workspace_id']: 'needle DISTINCT_ARCHIVE\n',
        })
        items, _ = self.search()
        self.assertEqual([item['pane']['workspace_id'] for item in items],
                         [independent['workspace_id']])
        self.assert_not_disclosed('AMBIGUOUS_ARCHIVE\n', items)

    def test_revocation_before_source_read_prevents_read_and_submission(self):
        self.archive()
        original = copy.deepcopy(self.panes)
        revoked = [{**original[0], 'shared': False}]
        self.live = lambda: original if self.live_reads == 1 else revoked
        notice = self.assert_rejected()
        # Scope counts are observable privacy evidence, not an internal read spy.
        self.assertRegex(notice, r'\b0 source bytes read\b')

    def test_revocation_after_read_removes_only_revoked_panes_before_submission(self):
        self.panes.append(self.pane(2))
        self.archive(sources={
            self.panes[0]['workspace_id']: 'needle REVOKED_ARCHIVE\n',
            self.panes[1]['workspace_id']: 'needle STILL_SHARED_ARCHIVE\n',
        })
        original = copy.deepcopy(self.panes)
        revoked = [{**original[0], 'shared': False}, original[1]]
        self.live = lambda: original if self.live_reads <= 3 else revoked
        items, _ = self.search()
        self.assertEqual([item['search_text'] for item in items], ['needle STILL_SHARED_ARCHIVE\n'])
        self.assert_not_disclosed('REVOKED_ARCHIVE', items)

    def test_reused_pane_id_with_new_process_cannot_send_predecessors_archive(self):
        self.archive()
        original = copy.deepcopy(self.panes)
        replaced = [{**original[0], 'pane_pid': '9999'}]
        self.live = lambda: original if self.live_reads <= 2 else replaced
        self.assert_rejected()

    def test_changed_live_pane_id_blocks_archive_read_despite_matching_stable_id(self):
        self.archive()
        original = copy.deepcopy(self.panes)
        replaced = [{**original[0], 'pane_id': '%999'}]
        self.live = lambda: original if self.live_reads == 1 else replaced
        notice = self.assert_rejected()
        self.assertRegex(notice, r'\b0 source bytes read\b')

    def test_revocation_during_credential_resolution_wins_before_model_submission(self):
        self.archive(sources={self.panes[0]['workspace_id']: 'needle REVOKED_DURING_AUTH\n'})

        def credentials():
            self.panes[0]['shared'] = False
            return 'synthetic-history-test-key'

        with patch.object(ai_core, 'api_key', side_effect=credentials):
            items, _ = self.search()
        self.assert_not_disclosed('REVOKED_DURING_AUTH', items)
        self.assertEqual(self.submissions, [])

    def test_symlink_ancestor_cannot_redirect_state_directory(self):
        self.archive()
        alias = self.base / 'alias'
        alias.symlink_to(self.base, target_is_directory=True)
        self.assert_rejected(root=alias / 'state')

    def test_symlink_snapshot_directory_cannot_redirect_authorized_archive(self):
        directory = self.archive()
        target = self.base / 'relocated-snapshot'
        directory.rename(target)
        directory.symlink_to(target, target_is_directory=True)
        self.assert_rejected()

    def test_symlink_source_and_metadata_are_never_followed(self):
        directory = self.archive()
        for name in (self.panes[0]['workspace_id'] + '.txt', 'workspace.json'):
            with self.subTest(source=name):
                path = directory / name
                target = self.base / ('target-' + name)
                path.rename(target)
                path.symlink_to(target)
                try:
                    self.assert_rejected()
                finally:
                    path.unlink()
                    target.rename(path)

    def test_hardlinked_source_and_metadata_are_rejected(self):
        directory = self.archive()
        for name in (self.panes[0]['workspace_id'] + '.txt', 'workspace.json'):
            with self.subTest(source=name):
                alias = self.base / ('hardlink-' + name)
                os.link(directory / name, alias)
                try:
                    self.assert_rejected()
                finally:
                    alias.unlink()

    def test_group_writable_archive_components_cannot_authorize_reads(self):
        directory = self.archive()
        paths = (self.root, directory.parent, directory,
                 directory / 'workspace.json', directory / (self.panes[0]['workspace_id'] + '.txt'))
        for path in paths:
            with self.subTest(path=path.relative_to(self.base)):
                original_mode = path.stat().st_mode & 0o777
                path.chmod(original_mode | 0o020)
                try:
                    self.assert_rejected()
                finally:
                    path.chmod(original_mode)

    def test_corrupt_metadata_does_not_hide_other_valid_retained_sources(self):
        uid = self.panes[0]['workspace_id']
        self.archive('100', {uid: 'needle VALID_ARCHIVE\n'})
        bad = self.archive('200', {uid: 'needle CORRUPT_METADATA_ARCHIVE\n'})
        self.write_private(bad / 'workspace.json', b'{"version": 1, "panes": [')
        items, notice = self.search()
        self.assertEqual([item['search_text'] for item in items], ['needle VALID_ARCHIVE\n'])
        self.assert_not_disclosed('CORRUPT_METADATA_ARCHIVE', items)
        self.assertRegex(notice.lower(), r'corrupt|invalid|skipped')

    def test_boolean_schema_version_cannot_masquerade_as_version_one(self):
        directory = self.archive()
        self.metadata(directory, version=True)
        self.assert_rejected()

    def test_unsupported_version_and_nonlist_panes_do_not_reach_model(self):
        unsupported = self.archive('200')
        self.metadata(unsupported, version=2)
        malformed = self.archive('100')
        self.metadata(malformed, panes={'id': self.panes[0]['workspace_id']})
        self.assert_rejected()

    def test_invalid_timestamp_types_nonfinite_values_and_overflow_are_rejected(self):
        for name, timestamp in (('300', True), ('200', float('inf')), ('100', 1e100)):
            directory = self.archive(name)
            self.metadata(directory, saved_at=timestamp)
        self.assert_rejected()

    def test_metadata_directory_cannot_escape_or_alias_snapshot_location(self):
        escaped = self.archive('200')
        outside = self.base / 'outside'
        outside.mkdir(mode=0o700)
        self.write_private(outside / (self.panes[0]['workspace_id'] + '.txt'), 'needle OUTSIDE_ARCHIVE\n')
        self.metadata(escaped, directory=str(outside))
        traversal = self.archive('100')
        self.metadata(traversal, directory=str(traversal / '..' / '100'))
        self.assert_rejected()

    def test_invalid_and_duplicate_pane_records_cannot_expand_source_set(self):
        uid = self.panes[0]['workspace_id']
        directory = self.archive(sources={uid: 'needle VALID_ID_ARCHIVE\n'})
        self.write_private(self.root / 'outside.txt', 'needle OUTSIDE_ID_ARCHIVE\n')
        self.metadata(directory, panes=[
            None, 'not a pane', {'id': '../../outside'}, {'id': 1},
            {'id': uid, 'label': 'original'}, {'id': uid, 'label': 'duplicate'},
        ])
        items, _ = self.search()
        self.assertEqual([item['search_text'] for item in items], ['needle VALID_ID_ARCHIVE\n'])
        self.assertEqual(items[0]['detail'].count(f'snapshots/100/{uid}.txt'), 1)
        self.assert_not_disclosed('OUTSIDE_ID_ARCHIVE', items)

    def test_newest_three_snapshots_use_numeric_order_and_preserve_tie_order(self):
        uid = self.panes[0]['workspace_id']
        for name in ('9', '2', '100', '10'):
            self.archive(name, {uid: f'needle snapshot {name}\n'}, saved=int(name))
        items, notice = self.search()
        self.assertEqual([item['search_text'] for item in items],
                         ['needle snapshot 100\n', 'needle snapshot 10\n', 'needle snapshot 9\n'])
        self.assert_not_disclosed('needle snapshot 2\n', items)
        self.assertRegex(notice.lower(), r'newest|retention|retained')

    def test_corrupt_newest_snapshot_does_not_extend_retention_into_older_saves(self):
        uid = self.panes[0]['workspace_id']
        for name in ('1', '2', '3', '4'):
            directory = self.archive(name, {uid: f'needle save {name}\n'})
        self.write_private(directory / 'workspace.json', 'not JSON')
        items, _ = self.search()
        self.assertEqual([item['search_text'] for item in items], ['needle save 3\n', 'needle save 2\n'])
        self.assert_not_disclosed('needle save 1\n', items)

    def test_duplicate_quotes_keep_all_snapshot_and_character_locations(self):
        uid = self.panes[0]['workspace_id']
        lead = 'needle repeated\n'
        quote = lead + 'x' * (history_ai.CHUNK_CHARS - len(lead) - 1) + '\n'
        self.archive('100', {uid: quote * 2}, saved=100, labels={uid: 'older label'})
        self.archive('200', {uid: quote * 2}, saved=200, labels={uid: 'newer label'})
        items, _ = self.search()
        self.assertEqual([item['search_text'] for item in items], [quote])
        detail = items[0]['detail']
        for snapshot, stamp, label in (
            ('200', '1970-01-01T00:03:20+00:00', 'newer label'),
            ('100', '1970-01-01T00:01:40+00:00', 'older label'),
        ):
            for line, character in ((1, 1), (3, history_ai.CHUNK_CHARS + 1)):
                self.assertIn(f'snapshots/{snapshot}/{uid}.txt | line {line}, character {character}', detail)
            self.assertIn(stamp, detail)
            self.assertIn(label, detail)
        self.assertEqual(detail.count('snapshots/'), 4)
        self.assertIn('historical', items[0]['title'].lower())

    def test_redaction_does_not_shift_raw_source_locations(self):
        uid = self.panes[0]['workspace_id']
        first = '\x1b[31mneedle colored\x1b[0m\npassword=synthetic-private-value\n'
        first += 'x' * (history_ai.CHUNK_CHARS - len(first) - 1) + '\n'
        second = 'needle exact second quote\n'
        self.archive(sources={uid: first + second})
        items, _ = self.search()
        by_quote = {item['search_text']: item for item in items}
        self.assertIn(second, by_quote)
        self.assertIn(f'line 4, character {history_ai.CHUNK_CHARS + 1}', by_quote[second]['detail'])
        self.assert_not_disclosed('synthetic-private-value', items)
        self.assert_not_disclosed('\x1b', items)
        self.assertIn('needle colored', self.submitted_text())

    def test_corrupt_utf8_and_incomplete_file_suffix_are_not_fabricated_as_quotes(self):
        self.panes.extend([self.pane(2), self.pane(3)])
        self.archive(sources={
            self.panes[0]['workspace_id']: b'needle CORRUPT_SOURCE\xff\n',
            self.panes[1]['workspace_id']: b'needle INCOMPLETE_SOURCE\xe7',
            self.panes[2]['workspace_id']: 'needle intact multilingual 界\n',
        })
        items, notice = self.search()
        self.assertEqual([item['search_text'] for item in items], ['needle intact multilingual 界\n'])
        self.assert_not_disclosed('CORRUPT_SOURCE', items)
        self.assert_not_disclosed('INCOMPLETE_SOURCE', items)
        self.assertNotIn('\ufffd', self.submitted_text())
        self.assertRegex(notice.lower(), r'corrupt|invalid|skipped')

    def test_source_cap_drops_only_cut_multibyte_suffix_without_replacement_text(self):
        uid = self.panes[0]['workspace_id']
        prefix = 'needle valid prefix\n'
        prefix += 'x' * (history_ai.SOURCE_BYTES - len(prefix.encode('utf-8')) - 1)
        self.archive(sources={uid: prefix + '界\nAFTER_SOURCE_CAP\n'})
        items, notice = self.search('needle AFTER_SOURCE_CAP')
        self.assertTrue(any('needle valid prefix' in item['search_text'] for item in items))
        self.assert_not_disclosed('AFTER_SOURCE_CAP', items)
        self.assert_not_disclosed('界', items)
        self.assertNotIn('\ufffd', self.submitted_text())
        self.assertTrue(all(len(item['search_text']) <= history_ai.CHUNK_CHARS for item in items))
        self.assertRegex(notice.lower(), r'limited|truncat')

    def test_metadata_size_cap_rejects_valid_but_oversized_json(self):
        directory = self.archive()
        self.metadata(directory, padding='x' * history_ai.METADATA_BYTES)
        self.assert_rejected()

    def test_total_source_budget_excludes_remaining_file_even_when_best_lexical_match(self):
        full_files = history_ai.TOTAL_BYTES // history_ai.SOURCE_BYTES
        self.panes = [self.pane(i + 1) for i in range(full_files + 1)]
        sources = {pane['workspace_id']: chr(65 + i) * history_ai.SOURCE_BYTES
                   for i, pane in enumerate(self.panes[:-1])}
        sources[self.panes[-1]['workspace_id']] = 'needle AFTER_TOTAL_BUDGET\n'
        self.archive(sources=sources)
        items, notice = self.search('needle AFTER_TOTAL_BUDGET')
        self.assertEqual({item['pane']['workspace_id'] for item in items},
                         {pane['workspace_id'] for pane in self.panes[:-1]})
        self.assert_not_disclosed('AFTER_TOTAL_BUDGET', items)
        self.assertRegex(notice.lower(), r'total.*budget.*reached|budget.*exhaust')

    def test_shortlist_prioritizes_overlap_then_newest_and_is_repeatable(self):
        count = history_ai.CANDIDATES
        self.panes = [self.pane(i + 1) for i in range(count + 2)]
        self.archive('200', {pane['workspace_id']: f'needle newest {i}\n'
                             for i, pane in enumerate(self.panes[:count])})
        self.archive('100', {
            self.panes[count]['workspace_id']: 'needle exact older priority\n',
            self.panes[count + 1]['workspace_id']: 'needle older tied exclusion\n',
        })
        first, notice = self.search('needle exact')
        second, _ = self.search('needle exact')
        expected = [self.panes[count]['workspace_id']] + [pane['workspace_id'] for pane in self.panes[:count - 1]]
        self.assertEqual([item['pane']['workspace_id'] for item in first], expected)
        self.assertEqual(second, first)
        self.assertEqual(sum(len(state['candidates']) for state in self.submissions), count * 2)
        self.assert_not_disclosed('needle older tied exclusion', first)
        self.assertRegex(notice.lower(), r'not.*exhaustive|incomplete|shortlist')

    def test_model_failure_keeps_exact_sources_as_explicit_local_results(self):
        uid = self.panes[0]['workspace_id']
        self.archive('100', {uid: 'needle exact older evidence\n'})
        self.archive('200', {uid: 'unrelated newest evidence\n'})

        def unavailable(state, questions):
            raise ai_core.AIError('Synthetic provider unavailable')

        self.respond = unavailable
        items, notice = self.search('needle exact')
        self.assertEqual([item['search_text'] for item in items],
                         ['needle exact older evidence\n', 'unrelated newest evidence\n'])
        self.assertEqual([item['relevance'] for item in items], ['local', 'local'])
        self.assertTrue(all('judgment' not in item for item in items))
        self.assertEqual(len(self.submissions), 1)
        self.assertRegex(notice.lower(), r'local.*not.*(?:ai|model)|not.*ranked')
        self.assertIn(f'snapshots/100/{uid}.txt', items[0]['detail'])

    def test_invalid_model_answers_cannot_become_semantic_source_evidence(self):
        self.archive()
        self.respond = lambda state, questions: {'answers': {
            key: {'type': 'score', 'score': 2,
                  'probabilities': {'0': 1, '1': 0, '2': 0}, 'confidence': 1}
            for key in questions
        }}
        items, notice = self.search()
        self.assertEqual([item['search_text'] for item in items], ['needle retained evidence\n'])
        self.assertEqual(items[0]['relevance'], 'local')
        self.assertNotIn('judgment', items[0])
        self.assertRegex(notice.lower(), r'local|unclassified')

    def test_missing_credentials_do_not_claim_chunks_were_sent(self):
        self.archive()
        with patch.object(ai_core, 'api_key', side_effect=ai_core.AIError('Synthetic missing credentials')):
            items, notice = self.search()
        self.assertEqual(self.submissions, [])
        self.assertEqual([item['relevance'] for item in items], ['local'])
        self.assertEqual([item['search_text'] for item in items], ['needle retained evidence\n'])
        self.assertNotRegex(notice, r'\b[1-9]\d*\s+(?:chunks|candidates)\s+sent\b')


if __name__ == '__main__':
    unittest.main()
