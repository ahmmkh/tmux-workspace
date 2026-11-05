"""First-run setup: ask where the user keeps projects and save it in their config."""
from pathlib import Path
import sys

import paths

WELCOME = """
Welcome to tmux-workspace.

Each workspace is a folder in your projects folder: `work api` opens ~/Projects/api,
creating it the first time. You can change this later in {config}.
"""
NEXT = """
tmux-workspace is set up.
  Projects folder  {projects}
  Config           {config}

Try:
  work my-project   open or create a workspace in {projects}/my-project
  Ctrl+a A          dashboard of every pane and agent
  Ctrl+a ?          every shortcut
  tmux-workspace doctor   check the install
"""


def display(path):
    """~/code rather than /home/me/code, so the config moves between machines."""
    home = Path.home()
    try:
        return '~/' + str(path.relative_to(home)) if path != home else '~'
    except ValueError:
        return str(path)


def resolve(answer):
    path = Path(answer.strip()).expanduser()
    return path if path.is_absolute() else Path.home() / path


def problem(path):
    if path.exists() and not path.is_dir():
        return f'{display(path)} is a file, not a folder.'
    return ''


def setup(args):
    """tmux-workspace setup [--projects DIR] [--yes]. Returns an exit status."""
    default = paths.projects_dir()
    config = display(paths.user_config_path())
    if '--projects' in args:
        index = args.index('--projects') + 1
        if index >= len(args):
            print('usage: tmux-workspace setup [--projects DIR] [--yes]', file=sys.stderr)
            return 2
        chosen = resolve(args[index])
        if problem(chosen):
            print(problem(chosen), file=sys.stderr)
            return 2
    elif '--yes' in args or not sys.stdin.isatty():
        chosen = default
    else:
        print(WELCOME.format(config=config))
        while True:
            try:
                answer = input(f'Where do you keep your projects? [{display(default)}]: ')
            except (EOFError, KeyboardInterrupt):
                print('\nSetup cancelled; nothing was saved. Run tmux-workspace setup to finish.')
                return 1
            chosen = resolve(answer) if answer.strip() else default
            if not problem(chosen):
                break
            print(problem(chosen) + ' Choose another folder.')
    if not chosen.exists():
        chosen.mkdir(parents=True)
        print(f'Created {display(chosen)}')
    paths.save_config({'projects_dir': display(chosen)})
    print(NEXT.format(projects=display(chosen), config=config))
    return 0
