"""Agent and workspace dashboard: one full-screen table of every pane, with live agent status.

render() is pure: rows and state in, styled lines out. The curses loop only paints them, so the
layout is tested without a terminal.
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import time
from typing import List, Optional, Tuple
import unicodedata

from workspace_model import AGENT_ORDER, summary

Segment = Tuple[str, str]  # (text, style)
Line = List[Segment]

# Tokyo Night. style -> (foreground, background, bold). ' sel' on a style adds the selection bar.
STYLES = {
    '': ('#c0caf5', None, False),
    'frame': ('#3b4261', None, False),
    'title': ('#7aa2f7', None, True),
    'header': ('#565f89', None, True),
    'dim': ('#565f89', None, False),
    'selected': ('#c0caf5', None, True),
    'blocked': ('#f7768e', None, True),
    'done': ('#7dcfff', None, False),
    'working': ('#e0af68', None, False),
    'idle': ('#9ece6a', None, False),
    'key': ('#7aa2f7', None, False),
    'error': ('#f7768e', None, True),
}
SELECTION = '#283457'
MARKS = {'blocked': '●', 'done': '●', 'working': '●', 'idle': '○'}
HINTS = (('enter', 'jump'), ('/', 'filter'), ('a', 'agents'), ('p', 'park'), ('u', 'unpark'),
         ('r', 'rename'), ('x', 'kill'), ('?', 'help'), ('q', 'quit'))
HINT_PRIORITY = ('enter', 'q', '?', '/', 'p', 'u', 'r', 'x', 'a')  # What a narrow footer keeps first.
HELP = (
    ('j k ↓ ↑', 'Move the selection'),
    ('g G', 'First or last row'),
    ('enter', 'Jump to the pane; a parked pane comes back next to yours'),
    ('/', 'Filter by workspace, tab, label or agent; esc clears'),
    ('a', 'Show only panes that run an agent'),
    ('p', 'Park the pane: hide it, keep it running'),
    ('u', 'Bring a parked pane back next to yours'),
    ('r', 'Rename the pane label'),
    ('x', 'Close the pane (asks first)'),
    ('?', 'Show or hide this help'),
    ('q esc', 'Close the dashboard'),
)
STATUS_W, AGENT_W, PANE_MAX, PATH_MIN = 10, 8, 28, 12


@dataclass
class State:
    selected: Optional[str] = None  # pane id
    origin: str = ''                # the pane the dashboard was opened from
    mode: str = 'normal'            # normal, filter, rename, confirm-kill, help
    text: str = ''                  # what the user is typing in filter or rename mode
    query: str = ''
    agents_only: bool = False
    message: str = ''
    message_until: float = 0.0
    quit: bool = False
    top: int = 0


def char_width(char):
    if unicodedata.combining(char) or unicodedata.category(char) in ('Mn', 'Me', 'Cf'):
        return 0
    return 2 if unicodedata.east_asian_width(char) in ('W', 'F') else 1


def cell_width(text):
    return sum(char_width(c) for c in text)


def fit(text, width):
    """Exactly width cells: cut with an ellipsis when too long, pad with spaces when short."""
    if width <= 0:
        return ''
    if cell_width(text) <= width:
        return text + ' ' * (width - cell_width(text))
    out, used = '', 0
    for char in text:
        w = char_width(char)
        if used + w > width - 1:
            break
        out, used = out + char, used + w
    return out + '…' + ' ' * (width - 1 - used)


def fit_left(text, width):
    """Like fit, but keeps the end of the text: …/deep/directory."""
    if cell_width(text) <= width:
        return fit(text, width)
    out, used = '', 0
    for char in reversed(text):
        w = char_width(char)
        if used + w > width - 1:
            break
        out, used = char + out, used + w
    return ' ' * (width - 1 - used) + '…' + out if width > 0 else ''


def short_path(path):
    home = str(Path.home())
    return '~' + path[len(home):] if path == home or path.startswith(home + '/') else path


def fit_line(line, width):
    """Cut or pad a styled line to exactly width cells."""
    out, used = [], 0
    for text, style in line:
        room = width - used
        if room <= 0:
            break
        if cell_width(text) > room:
            text = fit(text, room)
        out.append((text, style))
        used += cell_width(text)
    if used < width:
        out.append((' ' * (width - used), ''))
    return out


def selected_index(rows, state):
    for i, row in enumerate(rows):
        if row.pane_id == state.selected:
            return i
    return 0


def columns(rows, width):
    """Widths of workspace, tab, pane, agent and path; agent and path are 0 when hidden. STATUS is fixed."""
    inner = width - 6  # '│ ', the two-cell selection marker, ' │'
    agent = AGENT_W if width >= 50 else 0
    ws = min(20, max([9] + [cell_width(workspace_cell(r)) for r in rows]))
    tab = min(16, max([3] + [cell_width(tab_cell(r)) for r in rows]))
    if width < 80:
        ws, tab = min(ws, 12), min(tab, 8)
    gaps = 4 if agent else 3
    pane, path = inner - ws - tab - agent - STATUS_W - gaps, 0
    if pane > PANE_MAX + 1 + PATH_MIN:
        pane, path = PANE_MAX, pane - PANE_MAX - 1
    while pane < 8 and tab > 4:
        tab, pane = tab - 1, pane + 1
    while pane < 8 and ws > 6:
        ws, pane = ws - 1, pane + 1
    return ws, tab, max(pane, 1), agent, path


def workspace_cell(row):
    return '⇣ parked' if row.parked else row.workspace


def tab_cell(row):
    return row.workspace if row.parked else f'{row.window_index}:{row.tab}'


def status_cell(row):
    return f'{MARKS[row.status]} {row.status}' if row.status else '—'


def top_border(rows, width):
    counts = summary(rows)
    right = []
    for status in AGENT_ORDER:
        if counts[status]:
            if right:
                right.append((' · ', 'dim'))
            right.append((f'{counts[status]} {status}', status))
    totals = f"{counts['panes']} pane{'s' * (counts['panes'] != 1)} · {counts['workspaces']} workspace{'s' * (counts['workspaces'] != 1)}"
    for candidate in (right + ([(' · ', 'dim')] if right else []) + [(totals, 'dim')], right, []):
        used = cell_width('┌ tmux-workspace ') + sum(cell_width(t) for t, _ in candidate) + cell_width('  ┐')
        if used <= width:
            fill = '─' * (width - used)
            if not candidate:
                return [('┌ ', 'frame'), ('tmux-workspace', 'title'), (' ' + fill + '──┐', 'frame')]
            return [('┌ ', 'frame'), ('tmux-workspace', 'title'), (' ' + fill + ' ', 'frame')] + candidate + [(' ┐', 'frame')]
    return fit_line([('┌ ', 'frame'), ('tmux-workspace', 'title')], width - 1) + [('┐', 'frame')]


def boxed(content, width):
    return [('│ ', 'frame')] + fit_line(content, width - 4) + [(' │', 'frame')]


def table_lines(rows, state, width, height):
    ws, tab, pane, agent, path = columns(rows, width)
    names = [('WORKSPACE', ws), ('TAB', tab), ('PANE', pane)] + ([('AGENT', agent)] if agent else []) + [('STATUS', STATUS_W)]
    names += [('PATH', path)] if path else []
    header = boxed([('  ', '')] + [(fit(name, w) + (' ' if i < len(names) - 1 else ''), 'header')
                                   for i, (name, w) in enumerate(names)], width)
    body_height = height - 1
    if state.mode == 'help':
        body = [boxed([('  ', ''), (fit(keys, 10), 'key'), (' ' + what, '')], width) for keys, what in HELP]
        return [header] + (body + [boxed([], width)] * body_height)[:body_height]
    if not rows:
        message = 'No panes match the filter' if (state.query or state.agents_only) else 'No panes'
        return [header] + ([boxed([('  ' + message, 'dim')], width)] + [boxed([], width)] * body_height)[:body_height]
    chosen = selected_index(rows, state)
    first = min(max(0, state.top), chosen)
    if chosen >= first + body_height:
        first = chosen - body_height + 1
    body = []
    for i, row in enumerate(rows[first:first + body_height], first):
        sel = ' sel' if i == chosen else ''
        base = ('selected' if sel else '') + sel
        cells = [(fit(workspace_cell(row), ws) + ' ', 'dim' + sel if row.parked else base),
                 (fit(tab_cell(row), tab) + ' ', base),
                 (fit(f'{row.pane_index}:{row.label}', pane) + ' ', base)]
        if agent:
            cells.append((fit(row.agent or '—', agent) + ' ', ('dim' if not sel else 'selected') + sel))
        cells.append((fit(status_cell(row), STATUS_W), (row.status or 'dim') + sel))
        if path:
            cells.append((' ' + fit_left(short_path(row.path), path), 'dim' + sel))
        body.append(boxed([('▸ ' if sel else '  ', base or '')] + cells, width))
    return [header] + (body + [boxed([], width)] * body_height)[:body_height]


def preview_lines(rows, state, width, count, preview):
    label = rows[selected_index(rows, state)].label if rows else ''
    used = cell_width('├─ preview · ' + label + ' ┤') + 1
    title = [('├─ ', 'frame'), ('preview', 'title'), (' · ' + label + ' ', 'dim')]
    separator = fit_line(title, width - 1) + [('┤', 'frame')] if used > width else title + [('─' * (width - used + 1), 'frame'), ('┤', 'frame')]
    shown = list(preview)[-count:] if count else []
    shown += [''] * (count - len(shown))
    return [separator] + [boxed([(line, 'dim')], width) for line in shown]


def footer(state, width):
    now = time.time()
    if state.message and now < state.message_until:
        content = [(state.message, 'error')]
    elif state.mode == 'filter':
        content = [('/', 'key'), (state.text + '█', '')]
    elif state.mode == 'rename':
        content = [('rename: ', 'key'), (state.text + '█', '')]
    elif state.mode == 'confirm-kill':
        content = [('close this pane? ', 'error'), ('y', 'key'), (' / ', 'dim'), ('n', 'key')]
    else:
        content = []
        if state.query or state.agents_only:
            active = ' '.join(filter(None, (state.query and '/' + state.query, state.agents_only and 'agents only')))
            content += [(active, 'title'), (' · esc clears · ', 'dim')]
        room = width - 4 - sum(cell_width(t) for t, _ in content)
        kept = set()
        for key in HINT_PRIORITY:  # Whole hints only: drop the least useful ones first.
            trial = kept | {key}
            if sum(cell_width(f'{k} {w}') + 3 for k, w in HINTS if k in trial) - 3 <= room:
                kept = trial
        for i, (key, what) in enumerate((k, w) for k, w in HINTS if k in kept):
            content += ([(' · ', 'dim')] if i else []) + [(key, 'key'), (' ' + what, 'dim')]
    room = width - 4
    content = fit_line(content, room) if sum(cell_width(t) for t, _ in content) > room else content
    used = sum(cell_width(t) for t, _ in content)
    return [('└ ', 'frame')] + content + [(' ' + '─' * (room - used) + '┘', 'frame')]


def render(rows, state, width, height, preview):
    """Exactly height lines of exactly width cells."""
    if width < 20 or height < 4:
        return [fit_line([('tmux-workspace: make the window bigger', 'dim')], width) for _ in range(height)]
    count = min(10, height // 3) if height >= 20 else 0
    table_height = height - 2 - (count + 1 if count else 0)
    lines = [top_border(rows, width)] + table_lines(rows, state, width, table_height)
    if count:
        lines += preview_lines(rows, state, width, count, preview)
    lines.append(footer(state, width))
    return [fit_line(line, width) for line in lines]


def sgr(style):
    names = style.split()
    fg, bg, bold = STYLES.get(names[0] if names and names[0] != 'sel' else '', STYLES[''])
    if 'sel' in names:
        bg = SELECTION
    codes = ['1'] if bold else []
    codes.append('38;2;%d;%d;%d' % tuple(int(fg[i:i + 2], 16) for i in (1, 3, 5)))
    if bg:
        codes.append('48;2;%d;%d;%d' % tuple(int(bg[i:i + 2], 16) for i in (1, 3, 5)))
    return '\x1b[0;' + ';'.join(codes) + 'm'


def frame_text(lines, color):
    if not color:
        return '\n'.join(''.join(text for text, _ in line) for line in lines)
    return '\n'.join(''.join(sgr(style) + text for text, style in line) + '\x1b[0m' for line in lines)
