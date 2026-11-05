"""scripts/stats.py records install and traffic numbers. These tests use fixture JSON, never the network:
`record` and `show` take the function that calls `gh api`, and the command line runs against a stub `gh`."""
import csv
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import stats  # noqa: E402

REPO = 'ahmmkh/tmux-workspace'
HISTORY = 'ahmmkh/tmux-workspace-stats'
ARCHIVES = ('aarch64-apple-darwin', 'x86_64-apple-darwin', 'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl')


def day(n):
    """The n-th day of November 2026 as GitHub's traffic API stamps it."""
    return f'2026-11-{n:02d}T00:00:00Z'


def clones(*days):
    """days: (day number, count, uniques)."""
    return {'count': sum(d[1] for d in days), 'uniques': sum(d[2] for d in days),
            'clones': [{'timestamp': day(n), 'count': c, 'uniques': u} for n, c, u in days]}


def views(*days):
    return {'count': sum(d[1] for d in days), 'uniques': sum(d[2] for d in days),
            'views': [{'timestamp': day(n), 'count': c, 'uniques': u} for n, c, u in days]}


def asset(name, downloads):
    return {'name': name, 'download_count': downloads}


def release(tag, *assets, draft=False):
    return {'tag_name': tag, 'draft': draft, 'prerelease': False, 'assets': list(assets)}


def archives(downloads):
    return [asset(f'tmux-workspace-{target}.tar.gz', n) for target, n in zip(ARCHIVES, downloads)]


class FakeGh:
    """Answers `gh api` paths with fixtures and remembers every path asked for."""

    def __init__(self, clones_=None, views_=None, stars=0, forks=0, releases=None, files=None):
        self.answers = {
            f'repos/{REPO}/traffic/clones': clones_ if clones_ is not None else clones(),
            f'repos/{REPO}/traffic/views': views_ if views_ is not None else views(),
            f'repos/{REPO}': {'stargazers_count': stars, 'forks_count': forks},
            f'repos/{REPO}/releases?per_page=100': releases if releases is not None else [],
        }
        for name, text in (files or {}).items():
            self.answers[f'repos/{HISTORY}/contents/{name}'] = text
        self.calls = []

    def __call__(self, path, raw=False):
        self.calls.append(path)
        if path not in self.answers:
            raise stats.GhError(f'gh api {path}: HTTP 404: Not Found')
        return self.answers[path]


def traffic_table(text):
    """{'last 14 days': [clones, clone uniques, views, view uniques], ...} from the text show prints."""
    table = {}
    for line in text.splitlines():
        match = re.match(r'\s*(last \d+ days|all recorded)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)', line)
        if match:
            table[match.group(1)] = [int(n) for n in match.groups()[1:]]
    return table


def read(path):
    with open(path, newline='') as handle:
        return list(csv.DictReader(handle))


class TempDirCase(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.dir = Path(temp.name)


class Traffic(TempDirCase):
    def record(self, gh, today='2026-11-14'):
        stats.record(str(self.dir), gh, REPO, today)
        return read(self.dir / 'traffic.csv')

    def test_first_record_creates_the_file_with_a_header_and_sorted_rows(self):
        rows = self.record(FakeGh(clones(*[(n, 2, 1) for n in (3, 1, 2)]), views((2, 5, 3))))
        self.assertEqual((self.dir / 'traffic.csv').read_text().splitlines()[0], 'date,clones,clone_uniques,views,view_uniques')
        self.assertEqual([r['date'] for r in rows], ['2026-11-01', '2026-11-02', '2026-11-03'])
        self.assertEqual(rows[1], {'date': '2026-11-02', 'clones': '2', 'clone_uniques': '1', 'views': '5', 'view_uniques': '3'})

    def test_a_day_with_clones_but_no_views_has_zero_views(self):
        rows = self.record(FakeGh(clones((1, 4, 2)), views()))
        self.assertEqual((rows[0]['views'], rows[0]['view_uniques']), ('0', '0'))

    def test_overlapping_windows_keep_the_days_that_scrolled_out(self):
        self.record(FakeGh(clones(*[(n, 1, 1) for n in range(1, 15)])), today='2026-11-14')
        rows = self.record(FakeGh(clones(*[(n, 1, 1) for n in range(8, 22)])), today='2026-11-21')
        self.assertEqual([r['date'] for r in rows], [f'2026-11-{n:02d}' for n in range(1, 22)])

    def test_the_newest_fetch_wins_for_overlapping_dates(self):
        self.record(FakeGh(clones((10, 3, 2), (11, 1, 1)), views((10, 7, 4))))
        rows = self.record(FakeGh(clones((10, 9, 5), (11, 2, 2)), views((10, 6, 3))), today='2026-11-15')
        by_date = {r['date']: r for r in rows}
        self.assertEqual((by_date['2026-11-10']['clones'], by_date['2026-11-10']['clone_uniques']), ('9', '5'))
        self.assertEqual((by_date['2026-11-10']['views'], by_date['2026-11-10']['view_uniques']), ('6', '3'))
        self.assertEqual(by_date['2026-11-11']['clones'], '2')

    def test_a_newer_fetch_may_report_less(self):
        # GitHub revises a day; the later number is the truth, whichever way it moved.
        self.record(FakeGh(clones((10, 9, 5))))
        rows = self.record(FakeGh(clones((10, 4, 2))), today='2026-11-15')
        self.assertEqual(rows[0]['clones'], '4')

    def test_a_metric_the_fetch_does_not_mention_keeps_its_recorded_value(self):
        self.record(FakeGh(clones((10, 3, 2)), views((10, 7, 4))))
        rows = self.record(FakeGh(clones((10, 3, 2)), views()), today='2026-11-15')
        self.assertEqual((rows[0]['views'], rows[0]['view_uniques']), ('7', '4'))

    def test_empty_traffic_writes_only_the_header(self):
        self.record(FakeGh())
        self.assertEqual((self.dir / 'traffic.csv').read_text(), 'date,clones,clone_uniques,views,view_uniques\n')

    def test_empty_traffic_leaves_recorded_days_alone(self):
        self.record(FakeGh(clones((3, 1, 1)), views((3, 2, 1))))
        before = (self.dir / 'traffic.csv').read_text()
        self.record(FakeGh(), today='2026-11-15')
        self.assertEqual((self.dir / 'traffic.csv').read_text(), before)

    def test_the_directory_is_created(self):
        stats.record(str(self.dir / 'nested' / 'stats'), FakeGh(clones((1, 1, 1))), REPO, '2026-11-14')
        self.assertTrue((self.dir / 'nested' / 'stats' / 'traffic.csv').is_file())


class Snapshots(TempDirCase):
    def record(self, gh, today):
        stats.record(str(self.dir), gh, REPO, today)
        return read(self.dir / 'snapshots.csv')

    def test_a_snapshot_has_stars_forks_downloads_and_the_split_by_release(self):
        gh = FakeGh(stars=12, forks=3, releases=[release('v2.0.0', *archives((10, 5, 20, 7))),
                                                 release('v1.0.0', asset('tmux-workspace-x86_64-unknown-linux-musl.tar.gz', 4))])
        rows = self.record(gh, '2026-11-14')
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['date'], '2026-11-14')
        self.assertEqual((rows[0]['stars'], rows[0]['forks'], rows[0]['downloads_total']), ('12', '3', '46'))
        self.assertEqual(rows[0]['downloads_by_release'], '{"v2.0.0":42,"v1.0.0":4}')
        self.assertEqual((self.dir / 'snapshots.csv').read_text().splitlines()[0],
                         'date,stars,forks,downloads_total,downloads_by_release')

    def test_several_assets_add_up_but_bookkeeping_files_are_not_downloads(self):
        gh = FakeGh(releases=[release('v2.0.0', *archives((1, 2, 3, 4)), asset('SHA256SUMS', 90), asset('VERSION', 80))])
        rows = self.record(gh, '2026-11-14')
        self.assertEqual(rows[0]['downloads_total'], '10')
        self.assertEqual(json.loads(rows[0]['downloads_by_release']), {'v2.0.0': 10})

    def test_a_draft_release_is_not_counted_and_no_releases_means_zero(self):
        rows = self.record(FakeGh(releases=[release('v3.0.0', *archives((9, 9, 9, 9)), draft=True)]), '2026-11-14')
        self.assertEqual((rows[0]['downloads_total'], rows[0]['downloads_by_release']), ('0', '{}'))

    def test_recording_twice_in_a_day_leaves_one_row_with_the_later_numbers(self):
        self.record(FakeGh(stars=1), '2026-11-14')
        rows = self.record(FakeGh(stars=2), '2026-11-14')
        self.assertEqual([(r['date'], r['stars']) for r in rows], [('2026-11-14', '2')])

    def test_recording_twice_in_a_day_changes_nothing_when_nothing_changed(self):
        gh = FakeGh(clones((1, 1, 1)), stars=5, releases=[release('v2.0.0', *archives((1, 1, 1, 1)))])
        stats.record(str(self.dir), gh, REPO, '2026-11-14')
        before = {name: (self.dir / name).read_text() for name in ('traffic.csv', 'snapshots.csv')}
        stats.record(str(self.dir), gh, REPO, '2026-11-14')
        self.assertEqual({name: (self.dir / name).read_text() for name in before}, before)

    def test_each_day_adds_a_row_in_date_order(self):
        self.record(FakeGh(stars=1), '2026-11-14')
        self.record(FakeGh(stars=3), '2026-11-16')
        rows = self.record(FakeGh(stars=2), '2026-11-15')
        self.assertEqual([(r['date'], r['stars']) for r in rows], [('2026-11-14', '1'), ('2026-11-15', '2'), ('2026-11-16', '3')])

    def test_release_pages_that_gh_prints_back_to_back_are_read(self):
        pages = json.dumps([release('v2.0.0', *archives((1, 1, 1, 1)))]) + json.dumps([release('v1.0.0', *archives((2, 0, 0, 0)))])
        self.assertEqual([r['tag_name'] for r in stats.parse_pages(pages)], ['v2.0.0', 'v1.0.0'])
        self.assertEqual(stats.parse_pages('[]'), [])
        self.assertEqual(stats.parse_pages('{"a": 1}'), {'a': 1})
        self.assertEqual(stats.parse_pages(''), [])


class Failures(TempDirCase):
    def test_one_failing_endpoint_stops_the_record_before_anything_is_written(self):
        gh = FakeGh(clones((1, 1, 1)))
        del gh.answers[f'repos/{REPO}/traffic/views']
        with self.assertRaises(stats.GhError):
            stats.record(str(self.dir), gh, REPO, '2026-11-14')
        self.assertEqual(list(self.dir.iterdir()), [])


class Show(TempDirCase):
    TODAY = '2026-11-30'

    def show(self, gh, history=None):
        out = io.StringIO()
        stats.show(gh, REPO, history, self.TODAY, out)
        return out.getvalue()

    def history_files(self):
        stats.record(str(self.dir), FakeGh(clones(*[(n, n, 1) for n in range(1, 15)]), views(*[(n, 10 * n, 2) for n in range(1, 15)]),
                                           stars=4, releases=[release('v2.0.0', *archives((2, 0, 0, 0)))]), REPO, '2026-11-14')
        stats.record(str(self.dir), FakeGh(stars=9, releases=[release('v2.0.0', *archives((30, 0, 0, 0)))]), REPO, '2026-11-20')
        return {name: (self.dir / name).read_text() for name in ('traffic.csv', 'snapshots.csv')}

    def live(self, **kwargs):
        defaults = dict(clones_=clones(*[(n, 1, 1) for n in range(17, 31)]), views_=views(*[(n, 2, 1) for n in range(17, 31)]),
                        stars=12, forks=3, releases=[release('v2.0.0', *archives((40, 10, 20, 5))), release('v1.0.0', *archives((4, 0, 0, 0)))])
        defaults.update(kwargs)
        return FakeGh(**defaults)

    def test_without_history_it_shows_the_current_fourteen_days_and_says_so(self):
        text = self.show(self.live())
        self.assertIn('Stars 12', text)
        self.assertIn('Forks 3', text)
        self.assertIn('v2.0.0', text)
        self.assertIn('v1.0.0', text)
        self.assertIn('last 14 days', text)
        self.assertNotIn('last 30 days', text)
        self.assertNotIn('last 90 days', text)
        self.assertIn('no history', text)

    def test_downloads_are_listed_by_version_with_a_total(self):
        lines = self.show(self.live()).splitlines()
        v2 = next(line for line in lines if line.strip().startswith('v2.0.0'))
        v1 = next(line for line in lines if line.strip().startswith('v1.0.0'))
        total = next(line for line in lines if line.strip().startswith('total'))
        self.assertEqual(v2.split()[-1], '75')
        self.assertEqual(v1.split()[-1], '4')
        self.assertEqual(total.split()[-1], '79')

    def test_the_fourteen_day_row_adds_up_the_live_window(self):
        text = self.show(self.live())
        self.assertEqual(traffic_table(text), {'last 14 days': [14, 14, 28, 14]})

    def test_a_history_adds_longer_windows_and_the_whole_record(self):
        files = self.history_files()
        gh = self.live(files=files)
        text = self.show(gh, HISTORY)
        self.assertEqual([p for p in gh.calls if 'contents' in p],
                         [f'repos/{HISTORY}/contents/traffic.csv', f'repos/{HISTORY}/contents/snapshots.csv'])
        self.assertNotIn('no history', text)
        table = traffic_table(text)
        # Days 1-14 come from the history, days 17-30 from the live fetch; 2026-11-30 is today.
        self.assertEqual(table['last 14 days'], [14, 14, 28, 14])
        self.assertEqual(table['last 30 days'], [105 + 14, 14 + 14, 1050 + 28, 28 + 14])
        self.assertEqual(table['last 90 days'], table['last 30 days'])
        self.assertEqual(table['all recorded'], table['last 30 days'])
        self.assertIn('since 2026-11-01', text)

    def test_the_windows_count_calendar_days_ending_today(self):
        rows = [{'date': f'2026-11-{n:02d}', 'clones': 1, 'clone_uniques': 1, 'views': 0, 'view_uniques': 0} for n in range(1, 31)]
        self.assertEqual(stats.window_totals(rows, 14, self.TODAY)['clones'], 14)
        self.assertEqual(stats.window_totals(rows, 30, self.TODAY)['clones'], 30)
        self.assertEqual(stats.window_totals(rows, 90, self.TODAY)['clones'], 30)
        self.assertEqual(stats.window_totals(rows, None, self.TODAY)['clones'], 30)
        self.assertEqual(stats.window_totals([], 14, self.TODAY), {'clones': 0, 'clone_uniques': 0, 'views': 0, 'view_uniques': 0})

    def test_growth_is_measured_from_the_snapshot_thirty_days_back_or_the_first_one(self):
        files = self.history_files()
        text = self.show(self.live(files=files), HISTORY)
        # The snapshots are of 2026-11-14 and 2026-11-20: none is 30 days old, so the first one is the base.
        self.assertIn('+8 since 2026-11-14', text)    # stars 4 -> 12
        self.assertIn('+77 since 2026-11-14', text)   # downloads 2 -> 79

    def test_growth_prefers_the_latest_snapshot_that_is_thirty_days_old(self):
        snaps = [{'date': d, 'stars': s, 'forks': 0, 'downloads_total': 0, 'downloads_by_release': {}}
                 for d, s in (('2026-09-01', 1), ('2026-10-30', 5), ('2026-11-10', 8))]
        self.assertEqual(stats.growth(snaps, 'stars', 12, 30, self.TODAY), (7, '2026-10-30'))
        self.assertIsNone(stats.growth([], 'stars', 12, 30, self.TODAY))
        self.assertIsNone(stats.growth([dict(snaps[0], date=self.TODAY)], 'stars', 12, 30, self.TODAY))

    def test_missing_history_files_fall_back_to_the_live_window(self):
        # The history repo exists (the call does not raise) but holds no traffic.csv yet: gh answers 404.
        text = self.show(self.live(), HISTORY)
        self.assertIn('no history', text)
        self.assertIn('last 14 days', text)
        self.assertNotIn('last 30 days', text)

    def test_one_history_file_is_enough_for_that_part(self):
        files = self.history_files()
        text = self.show(self.live(files={'traffic.csv': files['traffic.csv']}), HISTORY)
        self.assertIn('last 30 days', text)
        self.assertNotIn('since 2026-11-14', text)
        self.assertNotIn('no history', text)

    def test_empty_traffic_and_no_releases_still_print(self):
        text = self.show(FakeGh())
        self.assertIn('last 14 days', text)
        self.assertIn('Stars 0', text)
        self.assertIn('no releases', text)


class CommandLine(TempDirCase):
    """main() against a `gh` on PATH that prints fixtures, as the real one prints JSON."""

    def stub(self, fail=''):
        bindir = self.dir / 'bin'
        bindir.mkdir()
        answers = {
            f'repos/{REPO}/traffic/clones': clones((13, 2, 1)),
            f'repos/{REPO}/traffic/views': views((13, 8, 4)),
            f'repos/{REPO}': {'stargazers_count': 7, 'forks_count': 1, 'private': False},
            f'repos/{REPO}/releases?per_page=100': [release('v2.0.0', *archives((3, 2, 1, 0)))],
        }
        data = self.dir / 'answers.json'
        data.write_text(json.dumps(answers))
        gh = bindir / 'gh'
        gh.write_text(
            '#!/usr/bin/env python3\n'
            'import json, os, sys\n'
            f'answers = json.load(open({str(data)!r}))\n'
            'args = sys.argv[1:]\n'
            f'open({str(self.dir / "token-seen")!r}, "w").write(os.environ.get("GH_TOKEN", "none"))\n'
            'if args[0] != "api": sys.exit(2)\n'
            f'if {fail!r} and args[-1].endswith({fail!r}):\n'
            '    sys.stderr.write("gh: Resource not accessible by personal access token (HTTP 403)\\n"); sys.exit(1)\n'
            'print(json.dumps(answers[args[-1]]))\n')
        gh.chmod(gh.stat().st_mode | stat.S_IXUSR)
        env = dict(os.environ, PATH=f'{bindir}{os.pathsep}{os.environ["PATH"]}')
        env.pop('GH_TOKEN', None)
        return env

    def run_main(self, env, *args):
        script = str(Path(stats.__file__))
        return subprocess.run([sys.executable, '-I', script, *args], env=env, capture_output=True, text=True, timeout=60)

    def test_record_writes_both_files_and_prints_a_summary(self):
        env = self.stub()
        out = self.dir / 'out'
        result = self.run_main(env, 'record', str(out))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read(out / 'traffic.csv')[0]['date'], '2026-11-13')
        self.assertEqual(read(out / 'snapshots.csv')[0]['downloads_total'], '6')
        self.assertIn('recorded', result.stdout)

    def test_a_403_fails_the_record_and_never_prints_the_token(self):
        env = self.stub(fail='traffic/views')
        env['GH_TOKEN'] = 'ghp_secretsecretsecret'
        result = self.run_main(env, 'record', str(self.dir / 'out'))
        self.assertEqual(result.returncode, 1)
        self.assertEqual((self.dir / 'token-seen').read_text(), 'ghp_secretsecretsecret')  # gh gets it from the environment
        self.assertIn('Resource not accessible', result.stderr)
        self.assertNotIn('secretsecret', result.stdout + result.stderr)
        self.assertFalse((self.dir / 'out' / 'traffic.csv').exists())

    def test_show_prints_the_live_numbers_without_history(self):
        result = self.run_main(self.stub(), 'show', '--repo', REPO)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Stars 7', result.stdout)
        self.assertIn('v2.0.0', result.stdout)

    def test_usage_errors_exit_2(self):
        env = self.stub()
        self.assertEqual(self.run_main(env).returncode, 2)
        self.assertEqual(self.run_main(env, 'record').returncode, 2)
        self.assertEqual(self.run_main(env, 'show', '--bogus').returncode, 2)


if __name__ == '__main__':
    unittest.main()
