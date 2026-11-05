"""Where tmux-workspace finds its files, its config and the tools it needs."""
import json
import os
from pathlib import Path
import shlex

# tmux 3.2 lacks display-popup -B/-e/-T, popup-border-* and the client-focus-in hook.
# 3.3 and 3.4 work; tmux.conf sets their missing cosmetic options with -q.
TMUX_MIN = (3, 3)

REPO = Path(__file__).resolve().parent.parent


def config_dir():
    base = os.environ.get('XDG_CONFIG_HOME') or str(Path.home() / '.config')
    return Path(base) / 'tmux-workspace'


def user_config_path():
    return config_dir() / 'config.json'


def config_path():
    """The user's config, then the legacy config inside the clone, then the shipped example."""
    for path in (user_config_path(), REPO / 'config.json'):
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


def needs_onboarding():
    """True until the user has chosen a projects folder. A broken config is reported, never replaced."""
    try:
        data = json.loads(user_config_path().read_text())
    except FileNotFoundError:
        return True
    except (OSError, ValueError):
        return False
    return isinstance(data, dict) and 'projects_dir' not in data


def save_config(updates):
    """Merge updates into the user's config, starting from the legacy or example config the first time."""
    data = load_config()
    data.update(updates)
    path = user_config_path()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = path.with_name('.config.json.' + str(os.getpid()))
    temporary.write_text(json.dumps(data, indent=2) + '\n')
    temporary.chmod(0o600)
    temporary.replace(path)
    return path


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


def render_hooks(template, home):
    """An integrations/*-hooks.json template with {{BIN}} (and the older {{PYTHON}} and {{AGENT_HOOK}}) filled in."""
    python = '/usr/bin/python3' if Path('/usr/bin/python3').exists() else '/usr/bin/env python3'
    hook = Path(home) / '.local/lib/tmux-workspace/agent-hook.py'
    # Quoted for the shell, then escaped to sit inside a JSON string.
    binary = json.dumps(shlex.quote(str(Path(home) / '.local/bin/tmux-workspace')))[1:-1]
    text = Path(template).read_text().replace('{{BIN}}', binary)
    text = text.replace('{{PYTHON}}', python).replace('{{AGENT_HOOK}}', str(hook))
    return json.loads(text)
