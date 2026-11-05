"""Agent and workspace dashboard: one full-screen table of every pane, with live agent status.

render() is pure: rows and state in, styled lines out. The curses loop only paints them, so the
layout is tested without a terminal.
"""
from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from typing import List, Optional, Tuple
import unicodedata

from workspace_model import AGENT_ORDER, PANE_FIELDS, build_rows, filter_rows, summary

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
    target: str = ''                # the pane a rename or close question is about, fixed when it is asked
    target_label: str = ''
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
    """~/code for a path under the home folder, also when one of them is reached through a symlink."""
    home = Path.home()
    for prefix in dict.fromkeys((str(home), str(home.resolve()))):
        if path == prefix or path.startswith(prefix + '/'):
            return '~' + path[len(prefix):]
    return path


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
    cap = min(PANE_MAX, max([8] + [cell_width(f'{r.pane_index}:{r.label}') for r in rows]))
    if pane > cap + 1 + PATH_MIN:
        pane, path = cap, pane - cap - 1
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
        content = [(f'close {state.target_label}? ', 'error'), ('y', 'key'), (' / ', 'dim'), ('n', 'key')]
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


# ---------------------------------------------------------------------------------------------
# Keys and actions. handle_key only calls the controller functions it gets in ws.

MOVES = {'j': 1, 'KEY_DOWN': 1, 'k': -1, 'KEY_UP': -1}
ENTER = ('\n', '\r', 'KEY_ENTER')
BACKSPACE = ('KEY_BACKSPACE', '\x7f', '\b')
ESCAPE = '\x1b'
MESSAGE_SECONDS = 3


def say(state, text):
    state.message, state.message_until = text, time.time() + MESSAGE_SECONDS
    return state


def error_text(exc):
    detail = getattr(exc, 'stderr', None)
    if isinstance(detail, bytes):
        detail = detail.decode(errors='replace')
    return (detail or str(exc)).strip()


def handle_key(key, state, rows, ws):
    """Apply one key to the state. rows are the visible rows, in table order."""
    if state.mode == 'help':
        state.mode = 'normal'
        return state
    if state.mode in ('filter', 'rename'):
        return typing(key, state, rows, ws)
    if state.mode == 'confirm-kill':
        state.mode = 'normal'
        if key in ('y', 'Y'):
            if not any(r.pane_id == state.target for r in rows):
                say(state, f'{state.target_label} is gone already')
            else:
                target = state.target
                act(state, ws, lambda: ws['tm']('kill-pane', '-t', target))
        return state
    if key in ('q',):
        state.quit = True
    elif key == ESCAPE:
        if state.query or state.agents_only:
            state.query, state.agents_only = '', False
        else:
            state.quit = True
    elif key == '/':
        state.mode, state.text = 'filter', state.query
    elif key == 'a':
        state.agents_only = not state.agents_only
    elif key == '?':
        state.mode = 'help'
    elif not rows:
        pass
    elif key in MOVES:
        index = min(max(selected_index(rows, state) + MOVES[key], 0), len(rows) - 1)
        state.selected = rows[index].pane_id
    elif key in ('g', 'KEY_HOME'):
        state.selected = rows[0].pane_id
    elif key in ('G', 'KEY_END'):
        state.selected = rows[-1].pane_id
    elif key in ENTER:
        jump(state, rows, ws)
    elif key == 'p':
        row = current(rows, state)
        if row.pane_id == state.origin:
            say(state, 'Cannot park the pane this dashboard was opened from')
        elif row.parked:
            say(state, 'That pane is already parked')
        else:
            act(state, ws, lambda: ws['park'](row.pane_id))
    elif key == 'u':
        row = current(rows, state)
        if not row.parked:
            say(state, 'That pane is not parked')
        else:
            bring_back(state, row, ws)
    elif key == 'r':
        row = current(rows, state)
        state.mode, state.text, state.target, state.target_label = 'rename', row.label, row.pane_id, row.label
    elif key == 'x':
        row = current(rows, state)
        state.mode, state.target, state.target_label = 'confirm-kill', row.pane_id, row.label
    return state


def typing(key, state, rows, ws):
    if key == ESCAPE:
        if state.mode == 'filter':
            state.query = ''
        state.mode, state.text = 'normal', ''
    elif key in ENTER:
        if state.mode == 'rename' and state.target:
            target, label = state.target, state.text
            act(state, ws, lambda: ws['set_label'](target, label))
        state.mode, state.text = 'normal', ''
    elif key in BACKSPACE:
        state.text = state.text[:-1]
    elif len(key) == 1 and key.isprintable():
        state.text += key
    if state.mode == 'filter':
        state.query = state.text
    return state


def current(rows, state):
    return rows[selected_index(rows, state)]


def act(state, ws, action):
    """Run a tmux action; a failure becomes a footer message instead of a crash."""
    try:
        action()
        return True
    except Exception as exc:  # noqa: BLE001 - tmux, OSError and ValueError all end up here
        say(state, error_text(exc))
        return False


def jump(state, rows, ws):
    row = current(rows, state)
    if row.parked:
        bring_back(state, row, ws)
        return

    def go():
        ws['tm']('select-window', '-t', row.pane_id)
        ws['tm']('select-pane', '-t', row.pane_id)
        ws['tm']('switch-client', '-t', row.pane_id, check=False)  # No client when run outside a popup.
    state.quit = act(state, ws, go)


def bring_back(state, row, ws):
    if not state.origin:
        say(state, 'Open the dashboard from a pane to bring parked panes back')
        return
    state.quit = act(state, ws, lambda: ws['rejoin'](row.pane_id, state.origin))


# ---------------------------------------------------------------------------------------------
# Reading tmux, the curses loop and the command line.

CSI = re.compile(r'\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)')


def read_rows(ws):
    return build_rows(ws['fields']('list-panes', PANE_FIELDS, '-a'))


def read_preview(ws, pane_id, lines):
    try:
        text = ws['tm']('capture-pane', '-p', '-J', '-t', pane_id)
    except Exception:  # noqa: BLE001 - the pane may be gone already
        return []
    out = [''.join(c for c in CSI.sub('', line) if c.isprintable() or c == '\t').expandtabs()
           for line in text.splitlines()]
    while out and not out[-1].strip():
        out.pop()
    return out[-lines:]


def server_running(ws):
    try:
        ws['tm']('display-message', '-p', '#{socket_path}')
        return True
    except (OSError, subprocess.SubprocessError):
        return False


def visible_rows(rows, state):
    return filter_rows(rows, state.query, state.agents_only)


def keep_selection(rows, state, previous_index):
    """Follow the same pane across refreshes; if it is gone, stay at the same height."""
    if rows and not any(r.pane_id == state.selected for r in rows):
        state.selected = rows[min(previous_index, len(rows) - 1)].pane_id


def main(ws, args):
    if not server_running(ws):
        sys.stderr.write('tmux-workspace dash: no tmux server is running (start one with: work)\n')
        return 1
    if '--once' in args:
        def number(flag, default):
            try:
                return int(args[args.index(flag) + 1])
            except (ValueError, IndexError):
                return default
        width = number('--width', 100) if '--width' in args else 100
        height = number('--height', 30) if '--height' in args else 30
        state = State(origin=os.environ.get('TMUX_PANE', ''))
        rows = read_rows(ws)
        preview = read_preview(ws, rows[0].pane_id, 10) if rows and height >= 20 else []
        print(frame_text(render(rows, state, width, height, preview), color='--color' in args))
        return 0
    return run(ws)


def usable_terminal():
    """macOS's system curses lacks tmux-256color; fall back to a terminal it knows."""
    import curses
    for term in (os.environ.get('TERM', ''), 'xterm-256color', 'xterm'):
        try:
            curses.setupterm(term or None, sys.stdout.fileno())
            os.environ['TERM'] = term
            return
        except curses.error:
            continue


def run(ws):
    import curses
    os.environ.setdefault('ESCDELAY', '25')
    usable_terminal()
    return curses.wrapper(lambda screen: Screen(screen, ws).loop())


class Screen:
    """Paints rendered lines with curses colours and turns key presses into handle_key calls."""

    def __init__(self, screen, ws):
        import curses
        self.curses, self.screen, self.ws = curses, screen, ws
        self.pairs = {}
        curses.curs_set(0)
        screen.timeout(1000)
        screen.keypad(True)
        curses.start_color()
        try:
            curses.use_default_colors()
        except curses.error:
            pass

    def colour(self, hex_colour):
        if self.curses.COLORS < 256:
            return {'#f7768e': 1, '#9ece6a': 2, '#e0af68': 3, '#7aa2f7': 4, '#7dcfff': 6}.get(hex_colour, 7)
        r, g, b = (int(hex_colour[i:i + 2], 16) for i in (1, 3, 5))
        cube = lambda v: 0 if v < 48 else 1 if v < 115 else (v - 35) // 40
        index = 16 + 36 * cube(r) + 6 * cube(g) + cube(b)
        grey = 232 + min(23, max(0, (r + g + b) // 3 - 8) // 10)
        level = lambda i: (0, 95, 135, 175, 215, 255)[i]
        cube_rgb = (level(cube(r)), level(cube(g)), level(cube(b)))
        grey_v = 8 + 10 * (grey - 232)
        distance = lambda rgb: sum((a - b) ** 2 for a, b in zip(rgb, (r, g, b)))
        return index if distance(cube_rgb) <= distance((grey_v,) * 3) else grey

    def attr(self, style):
        if style not in self.pairs:
            names = style.split()
            fg, bg, bold = STYLES.get(names[0] if names and names[0] != 'sel' else '', STYLES[''])
            selected = 'sel' in names
            number = len(self.pairs) + 1
            extra = 0
            try:
                if selected and self.curses.COLORS < 256:
                    self.curses.init_pair(number, self.colour(fg), -1)
                    extra = self.curses.A_REVERSE
                else:
                    self.curses.init_pair(number, self.colour(fg), self.colour(SELECTION) if selected else -1)
                value = self.curses.color_pair(number)
            except self.curses.error:
                value = 0
            if style.startswith(('dim', 'frame', 'header')) and self.curses.COLORS < 256:
                extra |= self.curses.A_DIM
            self.pairs[style] = value | extra | (self.curses.A_BOLD if bold else 0)
        return self.pairs[style]

    def paint(self, lines):
        self.screen.erase()
        for y, line in enumerate(lines):
            x = 0
            for text, style in line:
                try:
                    self.screen.addstr(y, x, text, self.attr(style))
                except self.curses.error:
                    pass  # The bottom-right cell cannot be written without scrolling.
                x += cell_width(text)
        self.screen.refresh()

    def key(self):
        try:
            key = self.screen.get_wch()
        except self.curses.error:
            return None
        if isinstance(key, int):
            return self.curses.keyname(key).decode(errors='replace')
        return key

    def loop(self):
        state = State(origin=os.environ.get('TMUX_PANE', ''))
        rows, read_at, index = [], 0.0, 0
        while not state.quit:
            if time.time() - read_at >= 1:
                try:
                    rows = read_rows(self.ws)
                except Exception as exc:  # noqa: BLE001 - keep the last rows and say why
                    say(state, error_text(exc))
                read_at = time.time()
            shown = visible_rows(rows, state)
            keep_selection(shown, state, index)
            index = selected_index(shown, state) if shown else 0
            height, width = self.screen.getmaxyx()
            preview = read_preview(self.ws, shown[index].pane_id, 10) if shown and height >= 20 else []
            self.paint(render(shown, state, width, height, preview))
            key = self.key()
            if key is None or key == 'KEY_RESIZE':
                continue
            before = (state.mode, state.message)
            state = handle_key(key, state, shown, self.ws)
            if (state.mode, state.message) != before or key in ENTER or key in ('p', 'u', 'y'):
                read_at = 0.0  # An action changed tmux; read it again now.
        return 0
