# Proposal

## Why

After the restore on 2026-10-01, 15 of the 17 panes running omp never published their identity option, `@workspace_app`. All three retained snapshots save those panes as plain shells, so the next restore would open 15 shells where agents ran. Three faults combine. The omp extension publishes the identity once and never retries. The helper's 2 second sub-timeouts expire when 17 agents start together. The snapshot cannot recognise an agent that a restore started, so nothing records the gap. Recovery of omp agents therefore works for one restore and fails on the next.

## What Changes

- The omp extension keeps publishing the identity until the helper confirms success. Today `update` in `integrations/tmux-workspace.ts` assigns `current` on line 31 and sends on line 32. The comparison on line 30 then treats the identity as published, so the 1 second timer never sends again. `send` discards the result of `spawnSync`.
- The helper tells its caller whether it wrote the identity. Today `run` in `integrations/agent-hook.py` gives each `ps` and `tmux` call 2 seconds on lines 18 to 20. Lines 246 to 250 swallow every failure, so the helper ends the same way whether it wrote the option or not. One publish makes seven or more of those calls, and the extension stops the helper after 3 seconds. When 17 agents started together the helper timed out.
- The snapshot recognises an agent that a restore started. Today the fallback on line 193 of `bin/tmux-workspace` compares the pane's foreground command with the agent names. `pane_start` runs a restored agent as a child of the Python wrapper on line 277, so the foreground command is the wrapper and the fallback never matches. `snapshot` then saves the pane with no agent, and the next `pane_start` opens a plain shell.
- A recognised agent with no published identity gets the record that the fallback already writes for an agent started by hand. The next restore then prints "Recovery needs attention" in that pane. It no longer opens a shell without a word.
- Publishing for Claude Code stays as it is. Its status line calls the helper once a second, so a failed attempt repeats by itself.

## Capabilities

### New Capabilities

- `workspace-recovery`: Saving workspaces, tabs, panes and the agents that run in them, and restoring them later. This change contributes two requirements. An agent's identity reaches its pane even when the first attempt fails. A snapshot records a restored agent as an agent, never as a plain shell.

The change guard-unattended-restore introduces the same capability. Whichever of the two the user archives first creates it, and the other then lists it as modified.

### Modified Capabilities

None.

## Impact

- `integrations/tmux-workspace.ts` gains the retry. `send` uses `spawnSync`, which blocks omp for up to 3 seconds on each attempt, so the retry must not freeze omp while the machine is busy. An unhandled promise rejection exits omp, so the retry swallows every failure.
- `integrations/agent-hook.py` gains a success signal for the omp call. The helper stays valid on Python 3.9.6 and 3.14, because it runs under both. It runs no code at module top level, because `snapshot` executes the file on line 177 of `bin/tmux-workspace`.
- `bin/tmux-workspace` changes the fallback in `snapshot`.
- No test covers `snapshot`, `pane_start` or the helper today. This change adds those tests.
- All three files are live through symlinks. A broken helper stops autosave, and a broken extension stops identity publishing for every omp agent that starts afterwards.
- omp loads the extension when it starts. The 15 agents that run now keep the old extension, and their panes stay unpublished until those agents restart.
- The change add-agent-status edits the same two integration files. The change that the user applies second builds on the first.
- `WORKSPACE_GUIDE.md` section 10 and the help topic on AI recovery describe how omp is tracked. Both gain the retry and the warning.
