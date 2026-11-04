"""Where tmux-workspace finds its files, its config and the tools it needs."""
import json
import os
from pathlib import Path

# tmux 3.2 lacks display-popup -B/-e/-T, popup-border-* and the client-focus-in hook.
# 3.3 and 3.4 work; tmux.conf sets their missing cosmetic options with -q.
TMUX_MIN = (3, 3)

REPO = Path(__file__).resolve().parent.parent


def config_dir():
    base = os.environ.get('XDG_CONFIG_HOME') or str(Path.home() / '.config')
    return Path(base) / 'tmux-workspace'


def config_path():
    """The user's config, then the legacy config inside the clone, then the shipped example."""
    for path in (config_dir() / 'config.json', REPO / 'config.json'):
        if path.is_file():
            return path
    return REPO / 'config.example.json'


def load_config():
    path = config_path()
    try:
        value = json.loads(path.read_text())
    except (OSError, ValueError) as exc:
        raise ValueError(f'Cannot read {path}: {exc}') from exc
    if not isinstance(value, dict):
        raise ValueError(f'Cannot read {path}: the top level must be an object')
    return value


def projects_dir():
    """Folder that holds one directory per project. A broken config never blocks a workspace."""
    override = os.environ.get('TMUX_WORKSPACE_PROJECTS')
    if override:
        return Path(override).expanduser()
    try:
        configured = load_config().get('projects_dir')
    except ValueError:
        configured = None
    if isinstance(configured, str) and configured.strip():
        return Path(configured.strip()).expanduser()
    return Path.home() / 'Projects'


def agent_hook_path():
    return REPO / 'integrations' / 'agent-hook.py'
