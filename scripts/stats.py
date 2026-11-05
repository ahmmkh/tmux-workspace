#!/usr/bin/env python3
"""Install and traffic numbers of the GitHub repository, kept as two CSV files. Dev tooling: stdlib and `gh api`.

  stats.py record DIR [--repo R]              fetch the numbers and merge them into DIR/traffic.csv and DIR/snapshots.csv
  stats.py show [--repo R] [--history REPO]   print the downloads, the traffic and the stars

GitHub keeps only 14 days of traffic and no history of stars or downloads, so `record` (run daily by
.github/workflows/stats.yml, into a private repository) is what makes longer windows possible.

  traffic.csv    date,clones,clone_uniques,views,view_uniques
                 One row per date. A fetch covers 14 days; where it overlaps recorded days, the newest fetch wins.
  snapshots.csv  date,stars,forks,downloads_total,downloads_by_release
                 One row per day, replaced when recorded again that day. downloads_by_release is compact JSON,
                 {"v2.0.0": 42}. A download is a release archive (*.tar.gz): every install and every update
                 fetches one. VERSION and SHA256SUMS are bookkeeping and not counted.

`gh` authenticates itself (GH_TOKEN, or the user's login); this script never reads or prints the token.
"""
import argparse
import csv
import datetime
import io
import json
import os
import subprocess
import sys

REPO = 'ahmmkh/tmux-workspace'
TRAFFIC_FILE = 'traffic.csv'
SNAPSHOT_FILE = 'snapshots.csv'
TRAFFIC_HEADER = ['date', 'clones', 'clone_uniques', 'views', 'view_uniques']
SNAPSHOT_HEADER = ['date', 'stars', 'forks', 'downloads_total', 'downloads_by_release']
METRICS = TRAFFIC_HEADER[1:]
WINDOWS = (14, 30, 90)


class StatsError(Exception):
    """Something this script can explain in one line."""


class GhError(StatsError):
    """A `gh api` call failed."""


# ---------------------------------------------------------------- gh

def scrub(text):
    """The message without the token, should gh or the OS ever echo it."""
    token = os.environ.get('GH_TOKEN', '')
    return text.replace(token, '***') if token else text


def parse_pages(text):
    """The JSON of `gh api --paginate`: one value, or the pages printed back to back, which are joined."""
    decoder = json.JSONDecoder()
    values = []
    index = 0
    while True:
        while index < len(text) and text[index].isspace():
            index += 1
        if index >= len(text):
            break
        value, index = decoder.raw_decode(text, index)
        values.append(value)
    if not values:
        return []
    if len(values) == 1:
        return values[0]
    if all(isinstance(value, list) for value in values):
        return [item for page in values for item in page]
    return values[0]


def gh_api(path, raw=False):
    """GET path with `gh api`: parsed JSON, or with raw=True the body as text (file contents)."""
    command = ['gh', 'api']
    command += ['-H', 'Accept: application/vnd.github.raw+json'] if raw else ['--paginate']
    command.append(path)
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=120)
    except FileNotFoundError:
        raise GhError('gh is not installed (https://cli.github.com)')
    except subprocess.TimeoutExpired:
        raise GhError(f'gh api {path}: timed out')
    if result.returncode != 0:
        detail = result.stderr.strip() or f'exit {result.returncode}'
        raise GhError(scrub(f'gh api {path}: {detail}'))
    if raw:
        return result.stdout
    try:
        return parse_pages(result.stdout)
    except ValueError as error:
        raise GhError(f'gh api {path}: not JSON ({error})')


# ---------------------------------------------------------------- traffic

def traffic_days(payload, key):
    """{date: (count, uniques)} from the clones or views answer."""
    days = {}
    for entry in (payload or {}).get(key) or []:
        days[entry['timestamp'][:10]] = (int(entry['count']), int(entry['uniques']))
    return days


def merge_traffic(rows, clone_payload, view_payload):
    """rows ({date: row}) with the fetched days on top. A metric the fetch does not mention keeps its value."""
    merged = {date: dict(row) for date, row in rows.items()}
    for key, payload, fields in (('clones', clone_payload, ('clones', 'clone_uniques')),
                                 ('views', view_payload, ('views', 'view_uniques'))):
        for date, numbers in traffic_days(payload, key).items():
            row = merged.setdefault(date, dict(date=date, **dict.fromkeys(METRICS, 0)))
            row[fields[0]], row[fields[1]] = numbers
    return merged


def window_totals(rows, days, today):
    """Sums of the rows (a list) in the last `days` calendar days up to and including today; None means all of them."""
    totals = dict.fromkeys(METRICS, 0)
    last = datetime.date.fromisoformat(today)
    first = last - datetime.timedelta(days=days - 1) if days else None
    for row in rows:
        date = datetime.date.fromisoformat(row['date'])
        if (first is None or date >= first) and (days is None or date <= last):
            for metric in METRICS:
                totals[metric] += row[metric]
    return totals


# ---------------------------------------------------------------- releases and snapshots

def release_downloads(releases):
    """{tag: downloads of its archives} for the published releases, in the order GitHub lists them."""
    downloads = {}
    for release in releases or []:
        if release.get('draft'):
            continue
        archives = [a for a in release.get('assets') or [] if a['name'].endswith('.tar.gz')]
        downloads[release['tag_name']] = sum(int(a['download_count']) for a in archives)
    return downloads


def snapshot(date, info, releases):
    by_release = release_downloads(releases)
    return {'date': date, 'stars': int(info['stargazers_count']), 'forks': int(info['forks_count']),
            'downloads_total': sum(by_release.values()), 'downloads_by_release': by_release}


def growth(snapshots, field, current, days, today):
    """(change, date) of `field` since the latest snapshot at least `days` old, else since the first one; None when there is none."""
    if not snapshots:
        return None
    cutoff = (datetime.date.fromisoformat(today) - datetime.timedelta(days=days)).isoformat()
    old = [s for s in snapshots if s['date'] <= cutoff]
    base = old[-1] if old else snapshots[0]
    if base['date'] >= today:
        return None
    return current - base[field], base['date']


# ---------------------------------------------------------------- files

def parse_rows(text, name, header, convert):
    reader = csv.DictReader(io.StringIO(text))
    if reader.fieldnames != header:
        raise StatsError(f'{name}: the header is {",".join(reader.fieldnames or [])!r}, expected {",".join(header)!r}')
    rows = {}
    for row in reader:
        try:
            rows[row['date']] = convert(row)
        except (ValueError, TypeError, KeyError) as error:
            raise StatsError(f'{name}: line {reader.line_num}: {error}')
    return rows


def parse_traffic(text, name=TRAFFIC_FILE):
    def convert(row):
        datetime.date.fromisoformat(row['date'])
        return dict({'date': row['date']}, **{metric: int(row[metric]) for metric in METRICS})
    return parse_rows(text, name, TRAFFIC_HEADER, convert)


def parse_snapshots(text, name=SNAPSHOT_FILE):
    def convert(row):
        datetime.date.fromisoformat(row['date'])
        return {'date': row['date'], 'stars': int(row['stars']), 'forks': int(row['forks']),
                'downloads_total': int(row['downloads_total']), 'downloads_by_release': json.loads(row['downloads_by_release'])}
    return parse_rows(text, name, SNAPSHOT_HEADER, convert)


def read_file(path, parse):
    try:
        with open(path, newline='') as handle:
            text = handle.read()
    except FileNotFoundError:
        return {}
    return parse(text, os.path.basename(path))


def csv_text(header, rows):
    out = io.StringIO()
    writer = csv.writer(out, lineterminator='\n')
    writer.writerow(header)
    for row in rows:
        writer.writerow(row)
    return out.getvalue()


def write_file(path, text):
    """Replace the file whole, or leave it alone when it already says this."""
    try:
        with open(path, newline='') as handle:
            if handle.read() == text:
                return
    except FileNotFoundError:
        pass
    temp = f'{path}.{os.getpid()}.tmp'
    try:
        with open(temp, 'w', newline='') as handle:
            handle.write(text)
        os.replace(temp, path)
    finally:
        if os.path.exists(temp):
            os.remove(temp)


# ---------------------------------------------------------------- commands

def fetch(api, repo):
    """Everything `record` and `show` need. All of it or an error: a half record would be worse than none."""
    return {'clones': api(f'repos/{repo}/traffic/clones'), 'views': api(f'repos/{repo}/traffic/views'),
            'info': api(f'repos/{repo}'), 'releases': api(f'repos/{repo}/releases?per_page=100')}


def record(directory, api, repo, today):
    data = fetch(api, repo)
    traffic_path = os.path.join(directory, TRAFFIC_FILE)
    snapshot_path = os.path.join(directory, SNAPSHOT_FILE)
    traffic = merge_traffic(read_file(traffic_path, parse_traffic), data['clones'], data['views'])
    snapshots = read_file(snapshot_path, parse_snapshots)
    snapshots[today] = snapshot(today, data['info'], data['releases'])
    os.makedirs(directory, exist_ok=True)
    write_file(traffic_path, csv_text(TRAFFIC_HEADER, ([traffic[d][h] for h in TRAFFIC_HEADER] for d in sorted(traffic))))
    write_file(snapshot_path, csv_text(SNAPSHOT_HEADER, (
        [s['date'], s['stars'], s['forks'], s['downloads_total'], json.dumps(s['downloads_by_release'], separators=(',', ':'))]
        for s in (snapshots[d] for d in sorted(snapshots)))))
    today_row = snapshots[today]
    return (f'recorded {today} in {directory}: {len(traffic)} traffic days, stars {today_row["stars"]}, '
            f'forks {today_row["forks"]}, downloads {today_row["downloads_total"]}')


def read_history(api, history):
    """(traffic rows or None, snapshots or None, why there is no history). A missing file is not an error."""
    found = {}
    reasons = []
    for name, parse in ((TRAFFIC_FILE, parse_traffic), (SNAPSHOT_FILE, parse_snapshots)):
        try:
            found[name] = parse(api(f'repos/{history}/contents/{name}', raw=True), f'{history}/{name}')
        except GhError as error:
            reasons.append(str(error))
    why = '; '.join(reasons) if not found else ''
    return found.get(TRAFFIC_FILE), found.get(SNAPSHOT_FILE), why


def with_growth(value, snapshots, field, today):
    """The number, and how much it grew in about 30 days when there are snapshots to tell."""
    change = growth(snapshots, field, value, 30, today)
    return f'{value}' if change is None else f'{value} ({change[0]:+d} since {change[1]})'


def show(api, repo, history, today, out):
    live = fetch(api, repo)
    traffic = merge_traffic({}, live['clones'], live['views'])
    recorded_traffic = recorded_snapshots = None
    why = 'pass --history OWNER/REPO for 30 and 90 days, the whole record and growth'
    if history:
        recorded_traffic, recorded_snapshots, why = read_history(api, history)
    if recorded_traffic is not None:
        # The live fetch is newer than anything recorded, so it wins where they overlap.
        traffic = merge_traffic(recorded_traffic, live['clones'], live['views'])
    snapshots = [recorded_snapshots[d] for d in sorted(recorded_snapshots)] if recorded_snapshots else []
    rows = [traffic[d] for d in sorted(traffic)]
    by_release = release_downloads(live['releases'])
    total = sum(by_release.values())

    def line(text=''):
        print(text, file=out)

    line(f'{repo}  {today}')
    line()
    stars = with_growth(int(live['info']['stargazers_count']), snapshots, 'stars', today)
    line(f'Stars {stars}   Forks {int(live["info"]["forks_count"])}')
    line()
    line('Release downloads, by version (installs and updates)')
    if by_release:
        for tag, count in by_release.items():
            line(f'  {tag:<14}{count:>8}')
        grown = growth(snapshots, 'downloads_total', total, 30, today)
        line(f'  {"total":<14}{total:>8}' + (f'  ({grown[0]:+d} since {grown[1]})' if grown else ''))
    else:
        line('  no releases')
    line()
    line('Traffic (uniques are added up day by day)')
    line(f'  {"":<14}{"clones":>8}{"uniques":>9}{"views":>8}{"uniques":>9}')
    windows = list(WINDOWS) + [None] if recorded_traffic is not None else [14]
    for days in windows:
        sums = window_totals(rows, days, today)
        label = f'last {days} days' if days else 'all recorded'
        note = ''
        if days is None and rows:
            first = rows[0]['date']
            span = (datetime.date.fromisoformat(today) - datetime.date.fromisoformat(first)).days + 1
            note = f'  (since {first}, {span} days)'
        line(f'  {label:<14}{sums["clones"]:>8}{sums["clone_uniques"]:>9}{sums["views"]:>8}{sums["view_uniques"]:>9}{note}')
    line()
    if recorded_traffic is None and recorded_snapshots is None:
        line(f'no history: {why}')
    elif history:
        line(f'History: {history}')


def main(argv=None):
    parser = argparse.ArgumentParser(prog='stats.py', description='Install and traffic numbers of the GitHub repository.')
    commands = parser.add_subparsers(dest='command', required=True)
    recording = commands.add_parser('record', help='merge today\'s numbers into DIR/traffic.csv and DIR/snapshots.csv')
    recording.add_argument('directory', metavar='DIR')
    recording.add_argument('--repo', default=REPO, help=f'the repository to read (default {REPO})')
    showing = commands.add_parser('show', help='print downloads, traffic and stars')
    showing.add_argument('--repo', default=REPO, help=f'the repository to read (default {REPO})')
    showing.add_argument('--history', metavar='REPO', help='the repository that holds the recorded CSV files')
    args = parser.parse_args(argv)
    today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    try:
        if args.command == 'record':
            print(record(args.directory, gh_api, args.repo, today))
        else:
            show(gh_api, args.repo, args.history, today, sys.stdout)
    except StatsError as error:
        print(f'stats.py: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
