# tmux-workspace for fish: the work command. The installer links this file into
# ~/.config/fish/functions/, so fish loads it on first use.
contains -- $HOME/.local/bin $PATH; or set -gx PATH $HOME/.local/bin $PATH

function work --description 'Open, create or close a tmux workspace, or update tmux-workspace'
    switch "$argv[1]"
        case rm delete kill
            $HOME/.local/bin/tmux-workspace kill-workspace $argv[2..-1]
        case update
            $HOME/.local/bin/tmux-workspace update $argv[2..-1]
        case ./update
            # A workspace named update.
            $HOME/.local/bin/tmux-workspace open update $argv[2..-1]
        case '*'
            $HOME/.local/bin/tmux-workspace open $argv
    end
end
