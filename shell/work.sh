# shellcheck shell=sh
# tmux-workspace for bash and zsh: the work command. Sourced from ~/.bashrc or ~/.zshrc.
#   work            pick a workspace
#   work NAME       open or create a workspace
#   work rm [NAME]  close a workspace
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
    *)
        "$HOME/.local/bin/tmux-workspace" open "$@"
        ;;
    esac
}
