#!/usr/bin/env python3
"""Compatibility shim for the hook entries of tmux-workspace 1.x.

Those entries run `/usr/bin/python3 ~/.local/lib/tmux-workspace/agent-hook.py <kind>`, a link to this
file. The hook is part of the tmux-workspace binary now, so this hands the call over:
`~/.local/bin/tmux-workspace agent-hook <kind>`, with the same input and output. `install.sh --agents`
replaces the old entries and removes the link, and this file with them.

It must never affect the agent it runs under: without the binary it prints nothing and exits 0.
"""
import os
import sys

try:
    binary = os.path.join(os.path.expanduser('~'), '.local/bin/tmux-workspace')
    os.execv(binary, [binary, 'agent-hook', *sys.argv[1:]])
except BaseException:
    pass
sys.exit(0)
