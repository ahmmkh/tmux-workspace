"""Where tmux-workspace finds its files, its config and the tools it needs."""

# tmux 3.2 lacks display-popup -B/-e/-T, popup-border-* and the client-focus-in hook.
# 3.3 and 3.4 work; tmux.conf sets their missing cosmetic options with -q.
TMUX_MIN = (3, 3)
