"""Pane rows from `list-panes -a` for four demo workspaces, shared by the model and dashboard tests."""
import json


def pane(pane_id, session, window, index, name, pane_index, label, command, status='', app='', home='', path=''):
    return {'session_name': session, 'window_id': '@' + str(window), 'window_index': str(index), 'window_name': name,
            'pane_id': pane_id, 'pane_index': str(pane_index), 'pane_current_command': command, 'pane_current_path': path or '/srv/projects/' + (home or session),
            '@workspace_label': label, '@workspace_agent': status,
            '@workspace_app': json.dumps({'kind': app, 'id': 'x'}) if app else '', '@workspace_home': home}


FIXTURE = [
    pane('%4', 'docs', 4, 1, 'write', 1, 'guide', 'omp', 'idle', app='omp'),
    pane('%2', 'api', 2, 2, 'servers', 1, 'logs', 'zsh'),
    pane('%3', 'frontend', 3, 1, 'code', 2, 'ui', 'codex', 'done'),
    pane('%1', 'api', 1, 1, 'code', 1, 'claude', 'node', 'blocked', app='claude'),
    pane('%5', 'infra', 5, 1, 'deploy', 1, 'terraform', 'omp', 'working'),
    pane('%6', '__parked', 6, 1, 'tests', 1, 'tests', 'zsh', home='api'),
]

PREVIEW = ['$ make test', 'Ran 235 tests in 21.8s', '', 'OK']
