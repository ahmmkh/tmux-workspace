"""Opt-in, bounded search of retained terminal snapshots; never current status."""
import codecs
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
from datetime import datetime, timezone

import ai_core as core


SNAPSHOTS = 3
METADATA_BYTES = 1024 * 1024
SOURCE_BYTES = 256 * 1024
TOTAL_BYTES = 4 * 1024 * 1024
CHUNK_CHARS = 2500
CANDIDATES = 32
ID = re.compile(r'[0-9a-f]{32}\Z')


def _owned(fd, directory=False):
    info = os.fstat(fd)
    expected = stat.S_ISDIR if directory else stat.S_ISREG
    if not expected(info.st_mode) or info.st_uid != os.getuid():
        raise ValueError('not an owned ' + ('directory' if directory else 'regular file'))
    if info.st_mode & 0o022:
        raise ValueError('group/world-writable snapshot path')
    if not directory and info.st_nlink != 1:
        raise ValueError('hard-linked snapshot file')
    return info


def _directory(name, parent=None, owned=True):
    fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
    try:
        if owned:
            _owned(fd, directory=True)
        return fd
    except Exception:
        os.close(fd)
        raise


def _root(path):
    # Walk descriptors rather than resolve(): no symlink, including ancestors,
    # may redirect an authorized read after a path check.
    path = Path(path).expanduser()
    if '..' in path.parts or any(not c.isprintable() for c in str(path)):
        raise ValueError('unsafe state directory')
    path = path.absolute()
    fd = _directory(path.anchor, owned=False)
    try:
        for part in path.parts[1:]:
            child = _directory(part, fd, owned=False)
            os.close(fd)
            fd = child
        _owned(fd, directory=True)
        return fd, path
    except Exception:
        os.close(fd)
        raise


def _read(parent, name, limit):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
    try:
        info = _owned(fd)
        with os.fdopen(fd, 'rb', closefd=False) as stream:
            content = stream.read(limit)
        return content, info.st_size > len(content)
    finally:
        os.close(fd)


def _authorized(ws):
    panes = core.collect_panes(ws, excerpts=False)
    grouped = {}
    for pane in panes:
        uid = pane.get('workspace_id', '')
        if isinstance(uid, str) and ID.fullmatch(uid):
            grouped.setdefault(uid, []).append(pane)
    # A copied stable ID is ambiguous, even if one of its panes is opted in.
    return {uid: values[0] for uid, values in grouped.items()
            if len(values) == 1 and values[0].get('shared') is True}


def _same_pane(before, after):
    return all(before.get(key) == after.get(key)
               for key in ('workspace_id', 'pane_id', 'pane_pid'))


def _chunks(text):
    start = 0
    line = 1
    while start < len(text):
        end = min(start + CHUNK_CHARS, len(text))
        if end < len(text):
            boundary = text.rfind('\n', start, end)
            if boundary > start:
                end = boundary + 1
        chunk = text[start:end]
        if chunk.strip():
            yield start, line, chunk
        line += chunk.count('\n')
        start = end


def _one_line(text):
    return ' '.join(core.clean(str(text)).split())


def search(ws, query):
    """Return historical source chunks, restricted to current explicit opt-ins."""
    query = core.clean(str(query)).strip()
    if not query:
        return [], 'Enter a query to search retained terminal snapshots.'
    authorized = _authorized(ws)
    if not authorized:
        return [], ('No unambiguous live pane with a stable workspace ID is opted in. '
                    'Opt in a pane to search its retained terminal snapshots; no archive text was read.')

    notices = []
    records = {}
    consumed = 0
    files = 0
    snapshots = 0
    truncated = 0
    skipped = 0
    root_fd = snapshots_fd = None
    try:
        root_fd, root_path = _root(ws['ROOT'])
        snapshots_fd = _directory('snapshots', root_fd)
        names = []
        with os.scandir(snapshots_fd) as entries:
            for index, entry in enumerate(entries):
                if index >= 4096:
                    notices.append('Snapshot directory listing stopped at 4096 entries; newest coverage is not guaranteed.')
                    break
                if re.fullmatch(r'[0-9]{1,30}', entry.name):
                    names.append(entry.name)
        names.sort(key=lambda name: int(name), reverse=True)
        if len(names) > SNAPSHOTS:
            notices.append(f'Only newest {SNAPSHOTS} of {len(names)} numeric snapshot entries considered.')
        for name in names[:SNAPSHOTS]:
            directory_fd = None
            try:
                directory_fd = _directory(name, snapshots_fd)
                raw, limited = _read(directory_fd, 'workspace.json', METADATA_BYTES)
                if limited:
                    raise ValueError('metadata exceeds 1 MiB limit')
                data = json.loads(raw.decode('utf-8'))
                if (not isinstance(data, dict) or type(data.get('version')) is not int
                        or data['version'] != 1 or not isinstance(data.get('panes'), list)):
                    raise ValueError('invalid snapshot schema/version')
                saved = data.get('saved_at')
                if isinstance(saved, bool) or not isinstance(saved, (int, float)) or not math.isfinite(saved):
                    raise ValueError('invalid saved timestamp')
                timestamp = datetime.fromtimestamp(saved, timezone.utc).isoformat(timespec='seconds')
                # Never follow data['directory']; it must name this exact directory.
                directory = data.get('directory')
                if (not isinstance(directory, str) or not directory
                        or any(not c.isprintable() for c in directory)
                        or '..' in Path(directory).parts
                        or Path(directory).absolute() != root_path / 'snapshots' / name):
                    raise ValueError('metadata directory does not match snapshot location')
                snapshots += 1
                seen = set()
                for pane in data['panes']:
                    if not isinstance(pane, dict):
                        skipped += 1
                        continue
                    uid = pane.get('id')
                    if not isinstance(uid, str) or not ID.fullmatch(uid):
                        skipped += 1
                        continue
                    if uid not in authorized or uid in seen:
                        continue
                    seen.add(uid)
                    remaining = TOTAL_BYTES - consumed
                    if remaining <= 0:
                        skipped += 1
                        continue
                    # Recheck consent immediately before each archive text read.
                    current = _authorized(ws)
                    if uid not in current or not _same_pane(authorized[uid], current[uid]):
                        skipped += 1
                        continue
                    try:
                        raw, limited = _read(directory_fd, uid + '.txt', min(SOURCE_BYTES, remaining))
                        consumed += len(raw)
                        files += 1
                        truncated += int(limited)
                        # Preserve source boundaries; only a cut UTF-8 suffix can be
                        # incomplete. Other corruption is reported, not fabricated.
                        if limited:
                            text = codecs.getincrementaldecoder('utf-8')().decode(raw, final=False)
                        else:
                            text = raw.decode('utf-8')
                    except (OSError, ValueError, UnicodeError):
                        skipped += 1
                        continue
                    label = _one_line(pane.get('label', 'shell'))[:160] or 'shell'
                    for offset, line, chunk in _chunks(text):
                        key = (uid, hashlib.sha256(chunk.encode('utf-8')).hexdigest())
                        location = {'snapshot': name, 'saved': timestamp, 'label': label,
                                    'line': line, 'offset': offset}
                        if key in records:
                            records[key]['locations'].append(location)
                            continue
                        quote = core.clean(chunk)
                        if not quote.strip():
                            continue
                        records[key] = {'uid': uid, 'quote': quote, 'locations': [location],
                                        'pane': current[uid]}
            except (OSError, ValueError, TypeError, OverflowError):
                notices.append(f'Snapshot {name}: missing, unsafe, corrupt, or unsupported metadata; skipped.')
            finally:
                if directory_fd is not None:
                    os.close(directory_fd)
    except FileNotFoundError:
        notices.append('No retained snapshot directory found; save a workspace snapshot first.')
    except (OSError, ValueError, TypeError):
        notices.append('Snapshot directory unavailable or unsafe (ownership, permissions, or symlink); no further archive reads.')
    finally:
        if snapshots_fd is not None:
            os.close(snapshots_fd)
        if root_fd is not None:
            os.close(root_fd)

    # Sharing revoked or identity changed while reading: never transmit its text.
    current = _authorized(ws)
    eligible = [record for record in records.values()
                if record['uid'] in current and _same_pane(record['pane'], current[record['uid']])]
    removed = len(records) - len(eligible)
    if removed:
        notices.append(f'{removed} source chunks excluded after sharing/identity changed.')
    terms = set(re.findall(r'\w+', query.casefold()))
    eligible.sort(key=lambda record: len(terms & set(re.findall(
        r'\w+', record['quote'].casefold()))), reverse=True)
    selected = eligible[:CANDIDATES]
    candidates = []
    for index, record in enumerate(selected):
        latest = record['locations'][0]
        locations = '\n'.join(
            f"Saved {loc['saved']} | pane {loc['label']} | snapshots/{loc['snapshot']}/{record['uid']}.txt "
            f"| line {loc['line']}, character {loc['offset'] + 1}"
            for loc in record['locations'])
        candidates.append({
            'id': f'history:{index}',
            'title': f"Historical | {latest['saved']} | {latest['label']} | line {latest['line']}",
            'detail': ('HISTORICAL TERMINAL TEXT — not evidence of current pane status.\n'
                       + locations + '\n\nSource quote (terminal controls removed; common secrets redacted):\n'
                       + record['quote']),
            'search_text': record['quote'][:4000],
            'pane': current[record['uid']],
        })
    scope = (f'Historical search: {files} authorized text files from {snapshots} retained snapshots; '
             f'{consumed} source bytes read. {len(eligible)} unique authorized chunks; '
             f'{len(selected)} chunks selected for ranking. Snapshot retention normally keeps only three saves. '
             'Quotes are historical, not current failures.')
    if len(eligible) > CANDIDATES:
        scope += (' Local lexical-overlap shortlist (newest first on ties); '
                  'not an exhaustive semantic search of retained history.')
    if truncated:
        scope += f' {truncated} text files limited to their first {SOURCE_BYTES} bytes or remaining byte budget.'
    if consumed >= TOTAL_BYTES:
        scope += f' Total source-read budget reached ({TOTAL_BYTES} bytes); remaining files excluded.'
    if skipped:
        scope += f' {skipped} missing, corrupt, unsafe, duplicate-invalid, or over-budget source records skipped.'
    notices.insert(0, scope)
    if not candidates:
        notices.append('No usable authorized historical source chunks found.')
        return [], ' '.join(notices)
    def check_consent():
        latest = _authorized(ws)
        if any(record['uid'] not in latest or not _same_pane(record['pane'], latest[record['uid']])
               for record in selected):
            raise core.EvidenceChanged('History sharing or pane identity changed; run the search again.')
    ranked, ranking_notice = core.rank(query, candidates, before_request=check_consent)
    if ranking_notice:
        notices.append(ranking_notice)
    return ranked, ' '.join(notices)
