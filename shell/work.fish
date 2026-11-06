# tmux-workspace for fish: the work command. The installer links this file into
# ~/.config/fish/functions/, so fish loads it on first use.
contains -- $HOME/.local/bin $PATH; or set -gx PATH $HOME/.local/bin $PATH

function work --description 'Open, create or close a tmux workspace, update tmux-workspace, or report a bug or an idea'
    switch "$argv[1]"
        case rm delete kill
            $HOME/.local/bin/tmux-workspace kill-workspace $argv[2..-1]
        case update
            $HOME/.local/bin/tmux-workspace update $argv[2..-1]
        case bug idea
            $HOME/.local/bin/tmux-workspace $argv
        case ./update
            # A workspace named update. After --, open takes the name as it is: a bare `open update` is
            # what the work function of 1.x runs for `work update`, and open refuses that.
            $HOME/.local/bin/tmux-workspace open -- update $argv[2..-1]
        case ./bug
            # Likewise a workspace named bug, which a shell from before 2.1 gets from a bare `work bug`.
            $HOME/.local/bin/tmux-workspace open -- bug $argv[2..-1]
        case ./idea
            $HOME/.local/bin/tmux-workspace open -- idea $argv[2..-1]
        case '*'
            $HOME/.local/bin/tmux-workspace open $argv
    end
end
