# Proposal

## Why

Any tmux server that loads `~/.tmux.conf` runs `tmux-workspace init`, which restores every saved workspace and resumes the saved agents. At 08:58 on 2026-10-01 a Claude Code session inside herdr started the current server with a command that begins `tmux new-session -d -s fruit`. That command restored every saved workspace and resumed 17 omp agents with nobody attached. The server also carries the herdr and Claude variables of that session, so every restored pane and agent runs with them. A background command must not start agents that nobody asked for.

## What Changes

- **BREAKING** Starting a tmux server no longer restores workspaces. Today line 96 of `tmux.conf` runs `tmux-workspace init` when the config loads, and line 95 runs it again from the `session-created` hook. `initialize` on lines 341 to 349 of `bin/tmux-workspace` calls `restore` on its first run in each server. Nothing checks whether a terminal is attached.
- Restore runs when a person asks for it or attaches. A person asks by running `work` or by pressing prefix Ctrl+r. A person attaches by connecting a terminal to the server.
- The automatic restore runs at most once per server, as it does today. The option `@workspace_initialized` gives that guarantee, so a config reload does not bring back a workspace that the person closed.
- Autosave keeps running on every server, and a server that has not restored the snapshot does not replace it. Today `snapshot` saves only the panes on the running server and keeps three snapshots. Without this rule the first autosave, 5 minutes after a background start, becomes the snapshot that `restore` reads. The third autosave deletes the last snapshot that held the saved workspaces.

## Capabilities

### New Capabilities

- `workspace-recovery`: Saving workspaces, tabs, panes and the agents that run in them, and restoring them later. This change contributes two requirements. A restore starts only when a person asks for it or attaches, never because a server started. A server that has not restored the snapshot does not overwrite it.

The change fix-agent-identity-publish introduces the same capability. Whichever of the two the user archives first creates it, and the other then lists it as modified.

### Modified Capabilities

None.

## Impact

- `tmux.conf` changes lines 95 and 96. The file is live through the `~/.tmux.conf` symlink, and tmux parses a config file completely before it runs any of it. A mistake there stops the prefix and every binding from loading.
- `bin/tmux-workspace` changes `initialize`, `restore` and the autosave in `tick` and `snapshot`.
- `open_workspace` and `choose_or_create_workspace` start a server with the placeholder `__workspace_bootstrap` and then call `initialize`, on lines 433 to 440 and 501 to 508. That path is a person asking, so it keeps restoring.
- `WORKSPACE_GUIDE.md` section 10 already says that the restore happens when you run `work` after a reboot. The change makes the code match that text. The section and the help topic for prefix Ctrl+r gain the attach rule.
- No test covers `initialize`, `restore` or `snapshot` today. This change adds those tests. They run on a scratch server with an empty state directory and never touch the real server or the real snapshots.
- This change does not clean the environment of a server that a tool started. A person who attaches to such a server restores into that tool's environment variables. The design states what restore does in that case.
- The change add-agent-status edits other lines of `tmux.conf`. The two changes do not overlap.
