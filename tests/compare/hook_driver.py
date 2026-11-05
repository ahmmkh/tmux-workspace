"""Run the Python agent hook (the reference) the way the old HelperWrites tests did.

    hook_driver.py <mode> <pid> <birth> <sessions_dir>

The payload comes on stdin. The process lookup is the stand-in that the old tests used: `owner`
answers (pid, birth) whatever the process table says, and `SESSIONS` is the folder of the state
files. `TMUX` (the program) is the tmux on PATH; the server is the scratch one that the TMUX and
TMUX_PANE variables name. Whatever main() prints goes to stdout, and an error ends it silently, as
the hook's own entry point does.
"""
import importlib.util
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('agent_hook', ROOT / 'legacy' / 'integrations' / 'agent-hook.py')
assert SPEC is not None and SPEC.loader is not None
hook = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(hook)


def main():
    mode, pid, birth, sessions = sys.argv[1:5]
    hook.TMUX = shutil.which('tmux')
    hook.owner = lambda *_: (int(pid), birth)
    hook.SESSIONS = sessions
    sys.argv = ['agent-hook.py', mode]
    try:
        hook.main()
    except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
        pass


if __name__ == '__main__':
    main()
