"""Pane rows and agent status, with the rules of agent-status.conf. No tmux calls and no curses."""
from pathlib import PurePath

AGENT_ORDER = ('blocked', 'done', 'working', 'idle')  # Most urgent first.
SHELLS = frozenset({'zsh', 'bash', 'fish', 'sh', 'dash', 'ksh', 'tcsh'})
AGENTS = ('claude', 'codex', 'omp')
PARKING = '__parked'


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
