# Proposal

## Why

Several agents run at once in the panes of this tmux setup, spread over many tabs and
workspaces. Nothing shows which agent is working, which one is blocked on the user and
which one finished out of sight, so the user has to visit each pane to find out. herdr
shows exactly that status, and it is the one thing the user wants from herdr while staying
on this setup.

The workspace picker also breaks a promise. Its header and the guide say that typing a new
name creates a workspace, and today typing a name that matches nothing creates nothing.

## What Changes

- Every pane that runs an agent gets a status. The four statuses are working, blocked, done
  and idle. A pane without an agent has no status.
- Blocked covers an approval, a question, a plan that awaits approval and a failed turn. A
  failed turn stays blocked until the next prompt.
- Done appears only when a turn finishes out of sight. Viewing the tab makes every done
  agent in it seen and idle. Viewing never clears blocked.
- Tabs and workspaces roll up the status of their agents in the order blocked, done,
  working, idle.
- A mark shows the status on each pane border, on each tab and beside the workspace name.
  A mark is a coloured dot, or a ring for idle. Tabs keep their width.
- A parked agent counts toward its home workspace and keeps showing done until the user
  brings the pane back.
- omp and Claude Code report status in the first version. omp reports from its extension
  events. Claude Code reports from the state file it writes while it runs, backed by
  three hooks.
- The workspace picker shows each workspace's rolled-up status. The tab picker shows each
  tab's status. The parked chooser shows each parked pane's status.
- The status text in a picker row can never change which workspace the picker opens or
  creates, and the search ignores it.
- Typing a name that matches nothing in the workspace picker creates that workspace. This
  fixes the existing bug.
- The status formats and hooks live in a new tmux file that `tmux.conf` loads. Four format
  lines move out of `tmux.conf` into that file.
- The change appends three hook entries to the Claude Code settings. The entries of other
  tools stay as they are.
- The guide and the built-in help describe the marks, the statuses and the picker rows.

This change removes no behaviour and has no breaking change.

## Capabilities

### New Capabilities

- `agent-status`: The status of each agent, how a pane gets and loses it, how tabs and
  workspaces roll it up, and where the marks appear.
- `workspace-picker`: The workspace picker, the tab picker and the parked chooser. It
  covers the status they show, the rule that status text never affects what the picker opens
  or creates, and the creation of a workspace from a typed name.

### Modified Capabilities

None. The project has no specs yet.

## Impact

- The project gains `agent-status.conf`, `tests/test_agent_status.py` and
  `tests/test_agent_status_tmux.py`. It changes `tmux.conf`,
  `integrations/tmux-workspace.ts`, `integrations/agent-hook.py`, `bin/tmux-workspace` and
  `WORKSPACE_GUIDE.md`.
- Outside the project, `~/.claude/settings.json` gains three hook entries. Claude Code
  runs them wherever it runs on this machine, so each entry always exits with status 0.
- Every edited project file is live through a symlink. A file that fails to parse would
  stop autosave or the next start of the tmux server, so a parse check follows each edit.
- An omp agent that is already running uses the new extension only after it restarts.
- The change adds no dependency. The pickers rely on fzf options that the installed fzf
  0.74.4 provides. The Claude Code path depends on a state file whose format Claude Code
  does not document.
- Status stays on this machine and never goes to a model provider.
- Out of scope are Codex and other agents, notifications and sounds, an always-on list of
  other workspaces, marks in the prefix+W tree, the identity publishing fix and the
  unattended restore guard. The last three have their own follow-up changes.
