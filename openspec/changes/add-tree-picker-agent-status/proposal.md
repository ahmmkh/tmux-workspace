# Proposal

## Why

The tree on prefix+W lists every workspace, tab and pane in one view. The change add-agent-status puts a mark on each pane border, each tab and the workspace name, and a status word in the pickers on prefix+s and prefix+w. The tree stays without marks, because its binding passes no row format and tmux draws its default rows. A person who opens the tree to find a blocked agent cannot see it there.

## What Changes

- The tree on prefix+W shows a mark on each workspace row, each tab row and each pane row. Today line 64 of `tmux.conf` binds `choose-tree -Zw` with a filter that hides `__parked` and with no `-F` format.
- The marks are the ones that add-agent-status defines. They use the same four statuses, the same colours and the same roll-up order, which is blocked, done, working, idle. The tree reuses the formats `@agent_pane`, `@agent_tab` and `@agent_workspace` from `agent-status.conf` and defines no second set of rules.
- A parked agent counts toward its home workspace in the tree, as it does in the workspace mark. The tree keeps hiding `__parked`.
- A row with no agent shows no mark.
- This change depends on add-agent-status. The option `@workspace_agent` and the three formats do not exist until the user applies that change.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `workspace-picker`: Gains one requirement. The tree on prefix+W shows the status of each workspace, tab and pane with the same marks as the status line.

The change add-agent-status creates `workspace-picker`. The capability appears under `openspec/specs/` when the user archives that change, and this change modifies it from then on.

## Impact

- The binding on line 64 of `tmux.conf` gains a row format. The design decides whether the binding moves into `agent-status.conf`, because a mistake in a sourced file fails only that file.
- The fallback tree that prefix+w opens when fzf is missing, on line 655 of `bin/tmux-workspace`, stays as it is.
- The help topic for prefix+w and lines 83, 186 and 297 of `WORKSPACE_GUIDE.md` describe the tree. They gain the marks.
- No tmux command has run for this proposal. The first task proves on a scratch server that the tree draws the colours and that one format tells a workspace row, a tab row and a pane row apart.
- `@agent_workspace` walks every pane on the server for each workspace row. The first task also measures how long the tree takes to open with every saved workspace running.
