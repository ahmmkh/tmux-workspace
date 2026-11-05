# shellcheck shell=sh
# tmux-workspace for bash and zsh: the work command. Sourced from ~/.bashrc or ~/.zshrc.
#   work            pick a workspace
#   work NAME       open or create a workspace
#   work rm [NAME]  close a workspace
#   work update     update tmux-workspace (--check only says whether there is an update)
#   work ./update   open a workspace that is named update
# Set TMUX_WORKSPACE_ZOXIDE_CD=1 before sourcing to let zoxide take over cd.

if [ "${TMUX_WORKSPACE_ZOXIDE_CD:-0}" = 1 ] && command -v zoxide >/dev/null 2>&1; then
    if [ -n "${ZSH_VERSION:-}" ]; then
        eval "$(zoxide init zsh --cmd cd)"
    elif [ -n "${BASH_VERSION:-}" ]; then
        eval "$(zoxide init bash --cmd cd)"
    fi
fi

# tmux-workspace and its helpers live in ~/.local/bin.
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) PATH="$HOME/.local/bin:$PATH"; export PATH ;;
esac

work() {
    case "${1:-}" in
    rm|delete|kill)
        shift
        "$HOME/.local/bin/tmux-workspace" kill-workspace "$@"
        ;;
    update)
        shift
        "$HOME/.local/bin/tmux-workspace" update "$@"
        ;;
    ./update)
        # A workspace named update. After --, open takes the name as it is: a bare `open update` is
        # what the work function of 1.x runs for `work update`, and open refuses that.
        shift
        "$HOME/.local/bin/tmux-workspace" open -- update "$@"
        ;;
    *)
        "$HOME/.local/bin/tmux-workspace" open "$@"
        ;;
    esac
}
