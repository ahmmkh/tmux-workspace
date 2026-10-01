"""On-demand workspace tools. Models select evidence; only users select actions."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import uuid

from ai_core import AIError, HOME, api_key, clean, collect_panes, config, judge


TOOLS = {
    'find': 'Jev live finder: type a task for fast live/parked pane reranking',
    'attention': 'Show agents that need input, are blocked, or need review',
    'search-history': 'Search retained history of opted-in panes',
    'suggest': 'Find an existing shortcut or installed skill',
    'name-tab': 'Suggest a short tab name and confirm before renaming',
    'share': 'Manage per-pane terminal-content permission',
    'check': 'Check OpenRouter credentials and native Jev judgments',
}


def choose(items, title):
    if not items:
        return None
    if not shutil.which('fzf'):
        for i, item in enumerate(items, 1):
            print(f"{i}. {clean(item['title'])}")
        selected = input('Number (Enter cancels): ').strip()
        return items[int(selected) - 1] if selected.isdigit() and 1 <= int(selected) <= len(items) else None
    with tempfile.TemporaryDirectory(prefix='tmux-ai-') as directory:
        for i, item in enumerate(items):
            Path(directory, f'{i}.txt').write_text(clean(item['detail']))
        lines = '\n'.join(f"{i}\t{item.get('relevance', '')} {clean(item['title']).replace(chr(10), ' ').replace(chr(9), ' ')}" for i, item in enumerate(items))
        result = subprocess.run([
            'fzf', '--delimiter=\t', '--with-nth=2..', '--layout=reverse', '--height=100%',
            '--header=' + clean(title), '--prompt=Workspace AI > ',
            '--preview=cat -- ' + shlex.quote(directory) + '/{1}.txt',
            '--preview-window=right:60%:wrap',
        ], input=lines, text=True, capture_output=True)
        if result.returncode or not result.stdout.strip():
            return None
        index = result.stdout.split('\t', 1)[0]
        return items[int(index)] if index.isdigit() and 0 <= int(index) < len(items) else None


@contextmanager
def _finder_directory(directory):
    path = Path(directory)
    if not path.is_absolute() or path.resolve() != path or not path.name.startswith('tmux-jev-find-'):
        raise ValueError('Invalid finder directory.')
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = os.fstat(descriptor)
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
            raise ValueError('Finder directory must be private and owned by this user.')
        yield descriptor
    finally:
        os.close(descriptor)


def _finder_line(record_id, item):
    title = clean(item['title']).replace('\n', ' ').replace('\t', ' ')
    return f"{record_id}\t{item.get('relevance', 'local')} {title}"


def _finder_rows(directory, items, notice):
    # --with-nth=2.. applies to fzf header lines as well as selectable rows.
    lines = ['\t' + clean(notice).replace('\n', ' ').replace('\t', ' ')]
    with _finder_directory(directory) as descriptor:
        for item in items:
            # Immutable IDs bind each displayed row to its exact source snapshot.
            # Obsolete reloads may finish writing, but cannot replace any record.
            record_id = uuid.uuid4().hex
            fd = os.open(record_id + '.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=descriptor)
            with os.fdopen(fd, 'w') as stream:
                json.dump(item, stream, ensure_ascii=False)
                os.fchmod(stream.fileno(), 0o400)
            lines.append(_finder_line(record_id, item))
    return '\n'.join(lines) + '\n'


def _finder_record(directory, record_id):
    if not re.fullmatch(r'[0-9a-f]{32}', record_id):
        raise ValueError('Invalid finder record ID.')
    with _finder_directory(directory) as descriptor:
        fd = os.open(record_id + '.json', os.O_RDONLY | os.O_NOFOLLOW, dir_fd=descriptor)
        with os.fdopen(fd) as stream:
            info = os.fstat(stream.fileno())
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                    or info.st_nlink != 1 or stat.S_IMODE(info.st_mode) != 0o400):
                raise ValueError('Invalid finder record file.')
            item = json.load(stream)
    if not isinstance(item, dict) or not isinstance(item.get('pane'), dict):
        raise ValueError('Invalid finder pane record.')
    if item.get('id') != 'pane:' + item['pane'].get('pane_id', ''):
        raise ValueError('Finder pane identity does not match its record.')
    return item


def _finder_catalog(ws):
    from panes_ai import _record
    return [_record(pane) for pane in collect_panes(ws)]


def _finder_private(ws, argv):
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument('directory')
    parser.add_argument('value')
    args = parser.parse_args(argv[1:])
    try:
        if argv[0] == '--_find-preview':
            print(clean(_finder_record(args.directory, args.value)['detail']))
            return
        # fzf kills the previous reload process group when the query changes.
        # Debounce before collecting evidence or sending anything to Jev.
        time.sleep(0.25)
        with _finder_directory(args.directory):
            pass
        query = args.value.strip()
        if query:
            items, notice = run_feature(ws, 'find', query)
            notice = f'Query: {query} | {notice}'
        else:
            items = _finder_catalog(ws)
            notice = 'Local live/parked catalog; empty query sends nothing to Jev.'
        print(_finder_rows(args.directory, items, notice), end='')
    except (AIError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        # A closed picker may already have removed its directory. Never recreate
        # it, navigate, or invoke the workspace command's tmux error popup here.
        try:
            print('Finder refresh unavailable; edit the query to retry.', flush=True)
        except OSError:
            pass


def choose_live_find(ws, query):
    with tempfile.TemporaryDirectory(prefix='tmux-jev-find-') as temporary:
        directory = str(Path(temporary).resolve())
        os.chmod(directory, 0o700)
        lines = _finder_rows(directory, _finder_catalog(ws),
                             'Local live/parked catalog | Metadata only until you type; excerpts require pane opt-in.')
        environment = os.environ.copy()
        try:
            # Resolve Doppler once per picker, not again after every keystroke pause.
            # Only this picker's child processes inherit the key; no files or argv.
            environment['OPENROUTER_API_KEY'] = api_key()
        except AIError:
            pass  # Local catalog remains usable; reload reports credential failure.
        command = [sys.executable, str(HOME / 'bin' / 'tmux-workspace'), 'ai']
        # fzf quotes placeholders; do not quote {q} again. The terminator keeps
        # a query beginning with "-" from becoming a private command option.
        reload_command = 'exec ' + shlex.join(command + ['--_find-reload', directory, '--']) + ' {q}'
        preview_command = 'exec ' + shlex.join(command + ['--_find-preview', directory, '--']) + ' {1}'
        bindings = ['--bind=change:reload-sync:' + reload_command]
        if query:
            bindings.append('--bind=load:unbind(load)+reload-sync:' + reload_command)
        result = subprocess.run([
            'fzf', '--delimiter=\t', '--with-nth=2..', '--layout=reverse', '--height=100%',
            '--disabled', '--no-sort', '--no-multi', '--print-query', '--print0', '--header-lines=1',
            '--query=' + query, '--prompt=Jev find > ',
            '--header=Type: fast semantic rerank after 250ms | Enter: inspect | Esc: cancel',
            '--preview=' + preview_command, '--preview-window=right:60%:wrap',
            '--with-shell=/bin/sh -c', '--bind=esc:abort', *bindings,
        ], input=lines, text=True, capture_output=True, env=environment)
        if result.returncode:
            return None, query
        fields = result.stdout.split('\0')
        if len(fields) != 3 or fields[-1]:
            return None, query
        query, line = fields[:2]
        record_id = line.split('\t', 1)[0]
        selected = _finder_record(directory, record_id)
        if line != _finder_line(record_id, selected):
            raise ValueError('Finder selection changed; run the search again.')
        return selected, query


def navigate(ws, pane):
    current = next((p for p in collect_panes(ws) if p['pane_id'] == pane['pane_id']), None)
    if not current or current['fingerprint'] != pane['fingerprint']:
        print('Pane identity or working context changed. Run the search again.')
        return False
    tm = ws['tm']
    if current['parked']:
        target = os.environ.get('TMUX_PANE')
        destination = next((p for p in collect_panes(ws) if p['pane_id'] == target), None)
        if not destination or destination['parked']:
            print('Open this tool from a normal workspace to bring back a parked pane.')
            return False
        if input('Bring this parked pane into the current window? [y/N] ').strip().lower() != 'y':
            return False
        latest = {p['pane_id']: p for p in collect_panes(ws)}
        for before in (current, destination):
            after = latest.get(before['pane_id'])
            if not after or any(after[key] != before[key] for key in (
                    'fingerprint', 'window_id', 'session_name', 'parked')):
                print('Source or destination changed during confirmation; nothing moved.')
                return False
        # Use the exact selected ID, never the most recently parked pane.
        tm('join-pane', '-h', '-s', current['pane_id'], '-t', target, '-l', '40%')
        tm('set-option', '-pu', '-t', current['pane_id'], '@workspace_parked')
        tm('set-option', '-pu', '-t', current['pane_id'], '@workspace_home')
    else:
        tm('switch-client', '-t', '=' + current['session_name'])
        tm('select-window', '-t', current['window_id'])
        tm('select-pane', '-t', current['pane_id'])
    return True


def rename_tab(ws, proposal):
    windows = ws['fields']('list-windows', ['window_id', 'window_name'], '-a')
    current = next((w for w in windows if w['window_id'] == proposal['window_id']), None)
    panes = {p['pane_id']: p['fingerprint'] for p in collect_panes(ws) if p['window_id'] == proposal['window_id']}
    if not current or current['window_name'] != proposal['previous_name'] or panes != proposal['panes']:
        print('Window name or task context changed. Request a new suggestion.')
        return False
    if input(f"Rename {clean(current['window_name'])!r} to {proposal['name']!r}? [y/N] ").strip().lower() != 'y':
        return False
    # Revalidate task identity as well as its display name after confirmation.
    latest = ws['fields']('list-windows', ['window_id', 'window_name'], '-a')
    latest_panes = {p['pane_id']: p['fingerprint'] for p in collect_panes(ws) if p['window_id'] == proposal['window_id']}
    if not any(w == current for w in latest) or latest_panes != proposal['panes']:
        print('Window changed during confirmation; nothing renamed.')
        return False
    ws['tm']('rename-window', '-t', proposal['window_id'], proposal['name'])
    print('Tab renamed: ' + proposal['name'])
    return True


def sharing(ws, pane_id=None, allow=False, off=False):
    panes = collect_panes(ws)
    if pane_id is None:
        records = [{'id': p['pane_id'], 'title': f"{'SHARED' if p['shared'] else 'private'} | {p['session_name']}:{p['window_index']}.{p['pane_index']} | {p['label']}", 'detail': 'Current permission: ' + ('shared' if p['shared'] else 'private') + '\nPane: ' + p['pane_id'], 'pane': p} for p in panes]
        selected = choose(records, 'Choose pane to change content permission')
        if not selected:
            return
        pane_id = selected['id']
    pane = next((p for p in panes if p['pane_id'] == pane_id), None)
    if not pane:
        raise ValueError('Select an existing pane ID from this tmux server.')
    if off or (pane['shared'] and not allow):
        ws['tm']('set-option', '-pu', '-t', pane_id, '@workspace_ai_share')
        print('Content sharing disabled for ' + pane_id)
        return
    if not pane['workspace_id']:
        raise ValueError('Pane has no stable workspace identity; save the workspace before sharing.')
    if not allow:
        if not sys.stdin.isatty():
            raise ValueError('Sharing requires an interactive confirmation or explicit --allow.')
        print('This permits recent terminal output AND retained snapshot history for this pane to be sent to OpenRouter and its model provider when you run AI tools. Output may contain secrets or client data. Redaction is best-effort, not a guarantee. Permission lasts until revoked or tmux restarts. Already-sent requests cannot be recalled.')
        if input('Type SHARE to allow: ').strip() != 'SHARE':
            print('No permission changed.')
            return
    current = next((p for p in collect_panes(ws) if p['pane_id'] == pane_id), None)
    if not current or current['fingerprint'] != pane['fingerprint']:
        raise ValueError('Pane changed during confirmation; choose it again.')
    ws['tm']('set-option', '-p', '-t', pane_id, '@workspace_ai_share', '1')
    print('Content sharing enabled for ' + pane_id + '; revoke with: tmux-workspace ai share ' + pane_id + ' --off')


def run_feature(ws, command, query):
    if command == 'find':
        from panes_ai import find
        return find(ws, query)
    if command == 'attention':
        from panes_ai import attention
        return attention(ws)
    if command == 'name-tab':
        from panes_ai import name_tab
        return name_tab(ws, query)
    if command == 'search-history':
        from history_ai import search
        return search(ws, query)
    from suggest_ai import suggest
    return suggest(ws, query)


def main(ws, argv):
    if argv and argv[0] in ('--_find-reload', '--_find-preview'):
        _finder_private(ws, argv)
        return
    parser = argparse.ArgumentParser(prog='tmux-workspace ai', description='Fast Jev judgments through OpenRouter. Content is private until you opt in a pane.')
    parser.add_argument('command', nargs='?', choices=list(TOOLS) + ['home'], default=None)
    parser.add_argument('query', nargs='*', help='Search text; for share, an exact pane ID.')
    parser.add_argument('--json', action='store_true', help='Print results without interactive navigation.')
    permission = parser.add_mutually_exclusive_group()
    permission.add_argument('--allow', action='store_true', help='Explicitly permit content sharing for selected pane.')
    permission.add_argument('--off', action='store_true', help='Revoke pane content permission.')
    args = parser.parse_args(argv)
    command = args.command
    if not command:
        if args.json or not sys.stdin.isatty():
            print(json.dumps({'home': str(HOME), 'tools': TOOLS, 'usage': 'tmux-workspace ai <tool> [query] [--json]'}, indent=2))
            return
        selected = choose([{'id': name, 'title': name + ' | ' + description, 'detail': description + '\n\nMetadata is sent for semantic search. Terminal content requires per-pane permission. No model output is executed.'} for name, description in TOOLS.items()], 'Jev tools | Find reranks live as you type | Esc closes | Config: ' + str(HOME))
        if not selected:
            return
        command = selected['id']
    if (args.allow or args.off) and command != 'share':
        parser.error('--allow/--off apply only to share')
    if command == 'home':
        print(HOME)
        return
    if command == 'share':
        if len(args.query) > 1:
            parser.error('share takes one pane ID')
        sharing(ws, args.query[0] if args.query else None, args.allow, args.off)
        return
    if command == 'check':
        try:
            answer = judge({'text': 'The workspace helper is ready.'}, {'check': {
                'type': 'noul',
                'instructions': 'Does `text` say the workspace helper is ready?',
                'criteria': {'true': 'Text says it is ready.', 'false': 'Text does not say it is ready.'},
            }})
            if not answer['check']['noul'] > 0.5:
                raise AIError('Native Noul output was valid but the readiness probability was not above 0.5.')
        except AIError as exc:
            if args.command is None:
                print(str(exc))
                input('Press Enter to close.')
            raise ValueError(str(exc)) from exc
        print('OpenRouter ' + config()['model'] + ': ready (native Jev Noul verified).')
        if args.command is None:
            input('Press Enter to close.')
        return
    query = ' '.join(args.query).strip()
    interactive = sys.stdin.isatty() and sys.stdout.isatty() and not args.json
    live_find = command == 'find' and interactive and shutil.which('fzf')
    if not live_find:
        if command == 'name-tab' and not query and sys.stdin.isatty() and not args.json:
            query = input('Task hint (optional; Enter uses window context): ').strip()
        if command not in ('attention', 'name-tab') and not query:
            if not sys.stdin.isatty() or args.json:
                parser.error(command + ' requires a query')
            query = input('What are you looking for? ').strip()
            if not query:
                return
        if sys.stdin.isatty() and not args.json:
            print('Reading evidence and requesting Jev judgments...', flush=True)
        try:
            items, notice = run_feature(ws, command, query)
        except AIError as exc:
            raise ValueError(str(exc)) from exc
        if not interactive:
            print(json.dumps({'tool': command, 'notice': clean(notice), 'items': items}, ensure_ascii=False, indent=2))
            return
        print(clean(notice))
        if not items:
            input('Press Enter to close.')
            return
    while True:
        if live_find:
            selected, query = choose_live_find(ws, query)
        else:
            selected = choose(items, notice)
        if not selected:
            return
        print('\n' + clean(selected['detail']) + '\n')
        if selected.get('rename'):
            action = input('[r] Rename tab  [Enter] Back  [q] Close: ').strip().lower()
            if action == 'r' and rename_tab(ws, selected['rename']):
                return
        elif selected.get('pane'):
            action = input('[g] Go to pane  [Enter] Back  [q] Close: ').strip().lower()
            if action == 'g' and navigate(ws, selected['pane']):
                return
        else:
            action = input('[Enter] Back  [q] Close: ').strip().lower()
        if action == 'q':
            return
