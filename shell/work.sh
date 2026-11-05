# shellcheck shell=sh
# tmux-workspace for bash and zsh: the work command. Sourced from ~/.bashrc or ~/.zshrc.
#   work            pick a workspace
#   work NAME       open or create a workspace
#   work rm [NAME]  close a workspace
#   work update     update tmux-workspace (--check only says whether there is an update)
#   work bug        report a bug (work idea: an idea); --print only shows the report
#   work ./update   open a workspace that is named update (work ./bug, work ./idea: likewise)
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
    bug|idea)
        "$HOME/.local/bin/tmux-workspace" "$@"
        ;;
    ./update)
        # A workspace named update. After --, open takes the name as it is: a bare `open update` is
        # what the work function of 1.x runs for `work update`, and open refuses that.
        shift
        "$HOME/.local/bin/tmux-workspace" open -- update "$@"
        ;;
    ./bug)
        # Likewise a workspace named bug, which a shell from before 2.1 gets from a bare `work bug`.
        shift
        "$HOME/.local/bin/tmux-workspace" open -- bug "$@"
        ;;
    ./idea)
        shift
        "$HOME/.local/bin/tmux-workspace" open -- idea "$@"
        ;;
    *)
        "$HOME/.local/bin/tmux-workspace" open "$@"
        ;;
    esac
}
