"""Review inactive terminals before explicitly closing their processes."""
import os
import shlex
import shutil
import subprocess
import sys


def candidates(ws):
    rows = ws['fields']('list-panes', [
        'pane_id', 'pane_pid', 'pane_current_command', 'pane_dead', 'pane_tty',
        'session_name', 'window_index', 'pane_index', '@workspace_label',
        '@workspace_agent', '@workspace_home',
    ], '-a')
    output = subprocess.run(['ps', '-axo', 'pid=,ppid=,rss=,comm='],
                            capture_output=True, text=True, check=True).stdout
    processes = {}
    for line in output.splitlines():
        parts = line.split(None, 3)
        if len(parts) == 4:
            processes[int(parts[0])] = (int(parts[1]), int(parts[2]), parts[3])
    for row in rows:
        members = {int(row['pane_pid'])}
        while True:
            children = {pid for pid, (parent, _rss, _cmd) in processes.items() if parent in members}
            if children <= members:
                break
            members |= children
        row['processes'] = tuple(sorted((pid, processes[pid]) for pid in members if pid in processes))
        row['memory'] = sum(proc[1] for _pid, proc in row['processes']) / 1024
        # RSS changes constantly; process membership and commands define the review identity.
        row['identity'] = (row['pane_pid'], row['pane_current_command'], row['@workspace_agent'],
                           tuple((pid, proc[0], proc[2]) for pid, proc in row['processes']))
    return [r for r in rows if r['pane_id'] != os.environ.get('TMUX_PANE')
            and r['session_name'] != '__workspace_bootstrap'
            and r['@workspace_agent'] not in ('working', 'blocked')]


def cleanup(ws):
    rows = sorted(candidates(ws), key=lambda r: -r['memory'])
    if not rows:
        print('No inactive terminal candidates.')
        return
    lines = []
    for r in rows:
        status = 'dead' if r['pane_dead'] == '1' else r['@workspace_agent'] or 'unverified'
        location = f"{r['session_name']}:{r['window_index']}.{r['pane_index']}"
        label = r['@workspace_label'] or r['pane_current_command']
        text = f"{location}  {label}  [{status}]  {r['memory']:.0f} MiB / {len(r['processes'])} processes"
        if r['@workspace_home']:
            text += '  home: ' + r['@workspace_home']
        lines.append(r['pane_id'] + '\t' + ''.join(c for c in text if c.isprintable()))
    if not sys.stdin.isatty() or not shutil.which('fzf'):
        print('\n'.join(lines))
        print('Run tmux-workspace cleanup in a terminal with fzf to select and close panes.')
        return
    selected = subprocess.run([
        'fzf', '--multi', '--delimiter=\t', '--with-nth=2..', '--layout=reverse',
        '--header=Review candidates, not proven stale | Tab: select | Enter: review closure | Esc: cancel',
        '--prompt=Clean terminals > ',
    ], input='\n'.join(lines), text=True, capture_output=True)
    if selected.returncode or not selected.stdout.strip():
        return
    ids = {line.split('\t', 1)[0] for line in selected.stdout.splitlines()}
    chosen = [r for r in rows if r['pane_id'] in ids]
    if not chosen:
        return
    print('\nClose these terminals:\n' + '\n'.join(line for line in lines if line.split('\t')[0] in ids))
    print('This terminates their jobs and may lose unsaved work. Shared daemon memory is not included.')
    if input('Type close to confirm: ').strip() != 'close':
        return
    current = {r['pane_id']: r for r in candidates(ws)}
    for row in chosen:
        pane = row['pane_id']
        if pane not in current or current[pane]['identity'] != row['identity']:
            print(f'Skipped {pane}: processes or status changed; review again.')
            continue
        # Check status again in the same tmux command queue as the close.
        guard = '#{&&:#{==:#{pane_pid},' + row['pane_pid'] + '},#{&&:#{!=:#{@workspace_agent},working},#{!=:#{@workspace_agent},blocked}}}'
        ws['tm']('if-shell', '-F', '-t', pane, guard, 'kill-pane -t ' + shlex.quote(pane))
