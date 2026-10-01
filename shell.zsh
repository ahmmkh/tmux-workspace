# Project sessions stay opt-in; work . connects the current directory.
eval "$(zoxide init zsh --cmd cd)"
work() {
  if [[ "$1" == "rm" || "$1" == "delete" || "$1" == "kill" ]]; then
    shift
    "$HOME/.local/bin/tmux-workspace" kill-workspace "$@"
  else
    "$HOME/.local/bin/tmux-workspace" open "$@"
  fi
}
