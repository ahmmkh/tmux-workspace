"""Read local settings and agent metadata without reading terminal output."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import time


def doctor(ws):
    warnings = []

    def check(label, ok, detail):
        print(f"{'OK' if ok else 'WARN'}  {label}: {detail}")
        if not ok:
            warnings.append(label)

    print('Tmux workspace diagnostics\n')
    for name in ('tmux', 'fzf', 'sesh', 'zoxide', 'pbcopy', 'afplay'):
        path = shutil.which(name)
        check(name, bool(path), path or 'not found on PATH')
    project = Path(ws['SELF']).parent.parent
    for live, source in ((Path.home() / '.tmux.conf', project / 'tmux.conf'),
                         (Path.home() / '.local/lib/tmux-workspace/agent-hook.py', project / 'integrations/agent-hook.py')):
        check(str(live), live.resolve() == source, str(live.resolve()))

    codex = Path(os.environ.get('CODEX_HOME', Path.home() / '.codex'))
    try:
        hooks = json.loads((codex / 'hooks.json').read_text()).get('hooks', {})
        expected = json.loads((project / 'integrations/codex-hooks.json').read_text())['hooks']
        missing = [event for event, groups in expected.items() if groups[0] not in hooks.get(event, [])]
        check('Codex hooks', not missing, 'missing: ' + ', '.join(missing) if missing else 'all workspace hooks installed')
        print('    Codex /hooks must show these definitions as trusted. New or changed hooks require review.')
    except (OSError, ValueError, AttributeError) as exc:
        check('Codex hooks', False, str(exc))
    result = subprocess.run([ws['TMUX'], '-N', 'display-message', '-p', '#{socket_path}'],
                            capture_output=True, text=True)
    check('tmux server', result.returncode == 0, result.stdout.strip() or result.stderr.strip())
    if result.returncode:
        print('\nStart or attach with work, then run tmux-workspace doctor again.')
        return 1
    tm, option = ws['tm'], ws['option']
    print('\nLive settings')
    for name, expected in (('mouse', 'on'), ('focus-events', 'on'), ('set-clipboard', 'external'),
                           ('default-terminal', 'tmux-256color')):
        value = option(name)
        check(name, value == expected, value or 'unset')
    print('    Sounds: ' + (option('@agent_sound') or 'unconfigured'))
    for name in ('blocked', 'done'):
        path = option('@agent_sound_' + name)
        check('sound ' + name, Path(path).is_file(), path or 'unconfigured')
    error = option('@workspace_save_error')
    check('autosave', not error, error or ('last saved ' + (option('@workspace_saved') or 'not yet')))
    try:
        latest = json.loads((ws['ROOT'] / 'latest.json').read_text())
        age = int(time.time() - latest['saved_at'])
        check('snapshot age', age < 900, f'{age // 60} minutes')
    except (OSError, ValueError, KeyError, TypeError):
        check('snapshot', False, 'no readable latest snapshot')

    print('\nClients (focus controls whether completion is done or idle)')
    print(tm('list-clients', '-F', '#{client_name}  #{client_termname}  #{client_flags}\n  #{client_termfeatures}') or 'No attached clients')
    for client in ws['fields']('list-clients', ['client_termname', 'client_termfeatures']):
        if client['client_termname'] == 'xterm-ghostty':
            features = set(client['client_termfeatures'].split(','))
            needed = {'RGB', 'clipboard', 'focus', 'sync'}
            missing = needed - features
            check('Ghostty capabilities', not missing, 'missing: ' + ', '.join(sorted(missing)) if missing
                  else 'true color, clipboard, focus and synchronized redraws enabled')
    print('\nPanes (no prompts or terminal content are read)')
    names = ['session_name', 'window_index', 'pane_index', 'pane_id', 'pane_current_command',
             '@workspace_agent', '@workspace_codex']
    for row in ws['fields']('list-panes', names, '-a'):
        location = f"{row['session_name']}:{row['window_index']}.{row['pane_index']}"
        command, status = row['pane_current_command'], row['@workspace_agent']
        detail = ''
        try:
            event = json.loads(row['@workspace_codex'] or '{}')
            if event:
                detail = f"  {event['event']} {int(time.time() - event['at'])}s ago"
        except (ValueError, KeyError, TypeError):
            detail = '  unreadable Codex event metadata'
        print(f"{location:32} {row['pane_id']:5} {command:14} {status or '-':8}{detail}")
        if command == 'codex' and not status:
            warnings.append(location)
            print('    WARN  No Codex status. Review /hooks, then restart/resume this CLI.')
    print(f'\n{len(warnings)} warning(s). Ctrl+a r reloads tmux; Ctrl+a M toggles agent sounds.')
    return 1 if warnings else 0
