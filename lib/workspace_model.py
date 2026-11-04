"""Pane rows and agent status, with the rules of agent-status.conf. No tmux calls and no curses."""
from dataclasses import dataclass
import json
from pathlib import PurePath

AGENT_ORDER = ('blocked', 'done', 'working', 'idle')  # Most urgent first.
SHELLS = frozenset({'zsh', 'bash', 'fish', 'sh', 'dash', 'ksh', 'tcsh'})
AGENTS = ('claude', 'codex', 'omp')
PARKING = '__parked'
BOOTSTRAP = '__workspace_bootstrap'  # Placeholder session that `work` starts a server with.
PANE_FIELDS = ['session_name', 'window_id', 'window_index', 'window_name', 'pane_id', 'pane_index',
               'pane_current_command', 'pane_current_path', '@workspace_label', '@workspace_agent',
               '@workspace_app', '@workspace_home']


def is_shell(command):
    """A login shell shows as -zsh; a full path can appear too."""
    name = PurePath(command or '').name
    return (name[1:] if name.startswith('-') else name) in SHELLS


def pane_status(row):
    """A pane at its shell prompt has no agent status, whatever its option still says."""
    status = row.get('@workspace_agent', '')
    if is_shell(row.get('pane_current_command', '')) or status not in AGENT_ORDER:
        return ''
    return status


def roll_up(rows):
    """Roll pane status up per tab and per workspace. A parked pane counts toward its home."""
    by_pane, by_tab, by_workspace = {}, {}, {}
    rank = AGENT_ORDER.index
    for row in rows:
        status = pane_status(row)
        if not status:
            continue
        by_pane[row['pane_id']] = status
        workspace = row['@workspace_home'] if row['session_name'] == PARKING else row['session_name']
        for group, key in ((by_tab, row['window_id']), (by_workspace, workspace)):
            group[key] = min(group.get(key, status), status, key=rank)
    return by_pane, by_tab, by_workspace


@dataclass(frozen=True)
class Row:
    """One pane as the dashboard shows it. A parked pane belongs to its home workspace and has no tab."""
    pane_id: str
    workspace: str
    window_index: int
    tab: str
    pane_index: int
    label: str
    agent: str
    status: str
    parked: bool
    path: str


def _number(value):
    try:
        return int(value)
    except (TypeError, ValueError):
        return 0


def _agent(row):
    try:
        kind = json.loads(row.get('@workspace_app') or '{}').get('kind')
    except (ValueError, AttributeError):
        kind = None
    if isinstance(kind, str) and kind:
        return kind
    command = PurePath(row.get('pane_current_command') or '').name
    return command if command in AGENTS else ''


def build_rows(pane_rows):
    rows = []
    for row in pane_rows:
        session = row.get('session_name', '')
        if session == BOOTSTRAP:
            continue
        parked = session == PARKING
        rows.append(Row(
            pane_id=row.get('pane_id', ''),
            workspace=(row.get('@workspace_home') or PARKING) if parked else session,
            window_index=_number(row.get('window_index')),
            tab='' if parked else row.get('window_name', ''),
            pane_index=_number(row.get('pane_index')),
            label=row.get('@workspace_label') or row.get('pane_current_command', ''),
            agent=_agent(row),
            status=pane_status(row),
            parked=parked,
            path=row.get('pane_current_path', ''),
        ))
    return sort_rows(rows)


def sort_rows(rows):
    """Workspaces by their most urgent agent, then by name; parked panes last in their workspace."""
    none = len(AGENT_ORDER)
    urgency = {}
    for row in rows:
        rank = AGENT_ORDER.index(row.status) if row.status else none
        urgency[row.workspace] = min(urgency.get(row.workspace, none), rank)
    return sorted(rows, key=lambda r: (urgency[r.workspace], r.workspace, r.parked, r.window_index, r.pane_index))


def filter_rows(rows, query='', agents_only=False):
    needle = query.strip().lower()
    return [r for r in rows
            if (not agents_only or r.agent)
            and (not needle or any(needle in field.lower() for field in (r.workspace, r.tab, r.label, r.agent)))]


def summary(rows):
    counts = {status: 0 for status in AGENT_ORDER}
    for row in rows:
        if row.status:
            counts[row.status] += 1
    counts['panes'] = len(rows)
    counts['workspaces'] = len({r.workspace for r in rows})
    return counts
