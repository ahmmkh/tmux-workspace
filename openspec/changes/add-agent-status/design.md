# Design

## Context

See `proposal.md` for the motivation and the specs for the required behaviour. This
section lists only the current state and the constraints that shape the approach.

- `tmux.conf` sets the pane border format, the left side of the status line and the two
  tab formats on lines 82, 87, 90 and 91. None of them knows about agents.
- omp loads `integrations/tmux-workspace.ts` each time it starts. The extension publishes the
  pane's identity through the Python helper and already runs a timer once a second.
- Claude Code runs the helper's `claude-status` call once a second for each Claude pane
  through its status line. That call already knows the pid of the Claude process. The
  settings already hold a `SessionEnd` entry that clears the identity.
- The pickers live in `bin/tmux-workspace`. The workspace picker reads the name back from
  the text of the row it displays, and it returns when fzf exits with "no match".
- Every edited file is live through a symlink. A broken file would stop autosave or the
  next start of the tmux server.
- Two interpreters run the helper. The hook entries and the omp extension call
  `/usr/bin/python3`, which is Python 3.9.6. `snapshot()` executes the helper file under
  `python3`, which is Python 3.14.
- The tmux server that runs now is detached, carries herdr and Claude variables and holds
  17 omp agents. Any tmux server that loads `~/.tmux.conf` restores every workspace and
  starts the saved agents again. No test may touch that server or load that file.
- The status line has no spare width. `odoo-project` already overflows its 179 columns by 8.
- In omp an unhandled promise rejection exits the process.
- In Claude Code a `PreToolUse` hook that exits with status 2 blocks the tool call.
- tmux parses a config file completely before it runs any of it.

Five read-only reviewers checked these mechanics against the tmux 3.7c source, the omp
18.4.2 source and the Claude Code hook reference. The first task of each group then proved
its part on a scratch tmux server. Only the live walk-through of task group 5 has not run.

## Goals / Non-Goals

**Goals:**

- One value per pane is the only stored state, so a roll-up can never go stale.
- Reporting status can never take an agent down and can never block a tool call.
- The two writers of a Claude pane cannot overwrite each other.
- A mistake in the new tmux formats cannot disable the prefix or the bindings.
- Status text in a picker row cannot reach the name that tmux-workspace opens or creates.
- No test executes the real `tmux.conf` or touches the running tmux server. The parse test
  reads it with `source-file -n`, which runs none of it.

**Non-Goals:**

- Status for Codex and other agents.
- Notifications. The change `add-agent-status-sound` covers sounds.
- An always-on list of other workspaces.
- Marks in the prefix+W tree. The change `add-tree-picker-agent-status` covers them.
- The identity publishing fix. The change `fix-agent-identity-publish` covers it.
- The unattended restore guard. The change `guard-unattended-restore` covers it.

## Decisions

### 1. One pane-scoped option holds the status and tmux rolls it up at draw time

One pane-scoped tmux option, `@workspace_agent`, holds `working`, `blocked`, `done` or
`idle`. An unset option means no agent. tmux rolls tabs and workspaces up each time it
draws them. It stores nothing per tab or workspace, so nothing needs updating when panes
move, park or close.

A pane whose foreground program is the shell shows no mark. That hides the status of an
agent that died without sending its shutdown event.

A parked pane lives in `__parked` and carries `@workspace_home`. The workspace format
counts it toward that home workspace.

The alternative was to store a status on each tab and each workspace and to update it on
every change. It lost because every move, park and close of a pane would need its own
update, and one missed update would leave a wrong mark in place.

### 2. Visible and turn over have one definition, as tmux formats

**Visible** means the pane's tab is the current tab of a workspace that a terminal with
focus is showing. A terminal that reports no focus events counts as focused, because tmux
marks a client focused when it attaches.

**Turn over** is the rule both agents use when a turn ends. The pane becomes idle if it is
visible. If it is not visible and it was working, blocked or done, it becomes done. In any
other case it becomes idle.

Both rules live once, as formats in the new tmux file. Each writer applies turn over with
a single command:

```sh
tmux set -pF -t <pane> @workspace_agent '#{?#{@agent_turn_over},#{E:@agent_turn_over},idle}'
```

The fallback makes the pane idle when `agent-status.conf` is not loaded, so turn over never
writes an empty status. The Claude helper runs this command inside its compare-and-set, so
tmux decides at the moment of the write.

The alternative was to let each writer work out visibility itself, once in TypeScript and
once in Python. It lost because the two copies and the hooks would drift apart, and
because separate queries leave a gap between the check and the write. One `set -pF`
command reads and writes inside tmux in one step.

### 3. Four tmux hooks make done agents seen

Viewing a tab turns done into idle for every pane in it. Four hooks run the same guarded
command. They cover switching tabs, switching workspaces, attaching, the terminal
regaining focus and a parked pane joining the tab. The guard runs the command only when
the tab is visible and holds a done pane. The command touches only panes that are done, so
viewing never clears blocked.

The alternative was a timer that polls for visible tabs. It lost because the hooks fire at
the moment of viewing and cost nothing between events. The reviewer measured the new
status line at 1.9 ms per redraw against a 100 ms limit, so it needs no tuning.

### 4. The formats and hooks live in their own tmux file

The formats and hooks go in `agent-status.conf`. `tmux.conf` loses the four format lines
that this file replaces, on lines 82, 87, 90 and 91. It gains one line after its status
settings:

```tmux
source-file "/Users/kh/untitled folder/Projects/tmux-workspace/agent-status.conf"
```

This is the content of `agent-status.conf`:

```tmux
# Agent status. Each pane carries @workspace_agent = working | blocked | done | idle.
set -g @agent_state '#{?#{==:#{pane_current_command},zsh},,#{@workspace_agent}}'
set -g @agent_pane '#{?#{==:#{E:@agent_state},blocked},#[fg=#f7768e]● ,#{==:#{E:@agent_state},done},#[fg=#7dcfff]● ,#{==:#{E:@agent_state},working},#[fg=#e0af68]● ,#{==:#{E:@agent_state},idle},#[fg=#9ece6a]○ ,}'
set -g @agent_color '#{?#{==:#{E:@agent_state},blocked},#[fg=#f7768e#,bold],#{==:#{E:@agent_state},done},#[fg=#7dcfff],#{==:#{E:@agent_state},working},#[fg=#e0af68],#{==:#{E:@agent_state},idle},#[fg=#9ece6a],}'
set -g @agent_tab_states '#{P:#{E:@agent_state} }'
set -g @agent_tab '#{?#{m:*blocked*,#{E:@agent_tab_states}},#[fg=#f7768e]●,#{m:*done*,#{E:@agent_tab_states}},#[fg=#7dcfff]●,#{m:*working*,#{E:@agent_tab_states}},#[fg=#e0af68]●,#{m:*idle*,#{E:@agent_tab_states}},#[fg=#9ece6a]○, }'
set -g @agent_own '#{W:#{P:#{E:@agent_state} }}'
set -g @agent_parked '#{S:#{?#{==:#{session_name},__parked},#{W:#{P:|#{@workspace_home}:#{E:@agent_state}|}},}}'
set -g @agent_workspace '#{?#{||:#{m:*blocked*,#{E:@agent_own}},#{m:*|#{session_name}:blocked|*,#{E:@agent_parked}}},#[fg=#f7768e]●,#{||:#{m:*done*,#{E:@agent_own}},#{m:*|#{session_name}:done|*,#{E:@agent_parked}}},#[fg=#7dcfff]●,#{||:#{m:*working*,#{E:@agent_own}},#{m:*|#{session_name}:working|*,#{E:@agent_parked}}},#[fg=#e0af68]●,#{||:#{m:*idle*,#{E:@agent_own}},#{m:*|#{session_name}:idle|*,#{E:@agent_parked}}},#[fg=#9ece6a]○, }'
set -g status-left '#[bold,fg=colour75] #S #[default]#{E:@agent_workspace}#[default] │ '
setw -g window-status-format '#{E:@agent_tab}#I:#W#[default] '
setw -g window-status-current-format '#{E:@agent_tab}#[bold,fg=colour234,bg=colour75]#I:#W#[default] '
set -g pane-border-format ' #{E:@agent_color}#{pane_index}: #{?@workspace_label,#{@workspace_label},name this pane} #{E:@agent_pane}#[default]'

# Visible means this tab is current in a workspace that a terminal with focus is showing.
set -g @agent_visible '#{&&:#{window_active},#{m:*1*,#{L:#{&&:#{m:*focused*,#{client_flags}},#{==:#{client_session},#{session_name}}}}}}'
# Turn over. A visible pane becomes idle. A hidden pane that was active becomes done.
set -g @agent_turn_over '#{?#{E:@agent_visible},idle,#{?#{&&:#{@workspace_agent},#{!=:#{@workspace_agent},idle}},done,idle}}'
# Viewing a tab acknowledges its finished agents. Done becomes idle for every pane in it.
set -g @agent_seen '#{P:#{?#{==:#{@workspace_agent},done},set -p -t #{pane_id} @workspace_agent idle ; ,}}'
set -g @agent_seen_if '#{&&:#{E:@agent_visible},#{m:*done*,#{P:#{@workspace_agent} }}}'
set-hook -g session-window-changed { if -F '#{E:@agent_seen_if}' { run-shell -C '#{E:@agent_seen}' } }
set-hook -g client-session-changed { if -F '#{E:@agent_seen_if}' { run-shell -C '#{E:@agent_seen}' } }
set-hook -g client-focus-in        { if -F '#{E:@agent_seen_if}' { run-shell -C '#{E:@agent_seen}' } }
set-hook -g window-layout-changed  { if -F '#{E:@agent_seen_if}' { run-shell -C '#{E:@agent_seen}' } }
```

The tab name and the pane label take the colour of the status through `@agent_color`. A
separator follows the workspace mark, so the mark does not read as a pair with the mark of
the first tab. The file also holds the sound lines that the change
`add-agent-status-sound` adds after this block.

A separate file matters because tmux parses a config file completely before it runs any of
it. A mistake inside `tmux.conf` would stop every line of it from loading, including the
prefix and the bindings. A mistake in a sourced file fails only that file. The test and the
Claude experiment also load exactly this file, so neither ever reads the real `tmux.conf`.

The alternative was to edit the formats in place inside `tmux.conf`. It lost for both
reasons above.

### 5. Marks sit in existing columns and use dots in tokyo-night colours

A mark is a coloured dot, or a ring for idle. Blocked is `#f7768e`, done is `#7dcfff`,
working is `#e0af68` and idle is `#9ece6a`. This is herdr's dots style in tokyo-night
colours.

The tab mark sits in the padding column each tab already has, so tabs do not get wider.
The current tab's highlight covers only the index and the name, and its mark sits outside
the highlight so the colour stays readable. The workspace mark adds two columns. On a pane
border the mark follows the label.

The alternative was a new column or a status word for each tab. It lost because the
status line has no spare width.

### 6. omp status comes from extension events and goes straight to tmux

The omp extension gains status tracking. It reports from omp's events, as herdr's
extension does, and it writes the pane option itself.

The alternative was the terminal title. omp already encodes working, waiting and idle in
its title, and a single tmux hook could have read it with no omp-side code and no restart
of running agents. It lost because the title misreports status in four cases that events
handle. It says idle while background jobs or subagents are still running. omp does not
restore it when omp is killed or crashes. It keeps the waiting mark until an approved tool
finishes. It follows whichever subagent is in view. Extension events are also omp's
explicit interface. `docs/adr/0001-omp-status-from-extension-events.md` records this
decision and its cost.

### 7. Only the root session of a pane handles status events

Every status handler returns at once unless the session is the pane's root. The root
session meets three conditions: `ctx.hasUI`, `ctx.mode === "tui"` and
`ctx.agent.kind === "main"`.

The alternative was to let every loaded copy of the extension report. It lost because
subagents get their own copy of the extension, and a finishing subagent would clear the
pane.

### 8. The omp event table, the status rule and the fallback timer

| omp event | Effect |
|---|---|
| `agent_start` | Forget pending prompts, a pending plan and any failure. Cancel the settle timer. The pane is working. |
| `tool_approval_requested` | Add that `toolCallId` to the pending set. |
| `tool_approval_resolved` | Remove that `toolCallId` from the pending set. |
| `tool_execution_start` for the `ask` tool | Add that `toolCallId` to the pending set. |
| `tool_execution_end` for the `ask` tool | Remove that `toolCallId` from the pending set. |
| `tool_execution_end` for a `write` whose result is a plan proposal | Mark a plan as pending. It counts as blocked until the next `agent_start`. |
| `auto_retry_start`, `auto_retry_end` | Mark omp as retrying, then not. |
| `agent_end` with `willContinue` | No change. omp will continue, so the pane stays working. |
| `agent_end` without `willContinue` | Clear the pending set. Mark the turn failed if the last assistant message has `stopReason` `error`. Recompute one second later. |
| `session_start`, `session_switch`, `session_branch`, `session_tree` | Reset, forget the last value written, then recompute one second later. After `session_start` an idle pane is written as plain idle until the first `agent_start`, so a new session never becomes done. |
| `session_compact` | Forget the last value written, then recompute. A failed turn and a pending plan stay. |
| The extension's existing 1 second timer | Recompute as a fallback. Settle to idle only after three consecutive idle readings. |
| `session_shutdown` | Stop handling events, then unset the option after the queued writes. |

The extension always computes status the same way. The pane is blocked if the pending set
is not empty, a plan is pending or the turn failed. Otherwise it is working if omp is busy,
which means `ctx.isIdle()` is false, omp is retrying, or
`ctx.getAsyncJobSnapshot().running` is not empty. Otherwise it is idle, and the writer
applies turn over.

Three rules in the table need a reason.

- A plan that omp submits for approval counts as blocked until the next `agent_start`.
  This matches Claude Code's plan approval and follows from the definition of blocked.
- A retry counts as working, because omp is still busy on the user's behalf.
- The timer corrects the status within a few seconds after a missed event, after a `/new`
  during a running turn, and after background work that is cancelled and never resumes
  omp. It settles to idle only after three consecutive idle readings.

The alternative was to trust events alone, with no timer. It lost because each of the
three cases above would leave a wrong status in place until the next prompt.

The table was checked on 2026-10-01 with omp 18.4.2 on a scratch tmux server, with a
logging extension loaded through `omp -e`. Every event in the table fired with the fields
the table relies on. The root session reported `hasUI` true, `mode` `tui` and
`agent.kind` `main`. A task subagent reported `hasUI` false, `mode` `print` and
`agent.kind` `sub`. Four details came out of the run.

- A background job ends the turn with `agent_end` and `willContinue` true, and
  `getAsyncJobSnapshot().running` holds the job until it finishes.
- A provider failure gave ten retries. Each retry sent `auto_retry_start` and an
  `agent_end` with `willContinue` true. One `auto_retry_end` came at the end, followed by
  an `agent_end` with `stopReason` `error`. The pane stayed working for the whole time and
  then became blocked.
- `/new` during a turn sends `session_switch` and no `agent_end`. Esc during a turn sends
  an `agent_end` with `stopReason` `aborted`, which is not a failure.
- A tool call inside a task subagent raised no approval prompt and no approval event,
  even with `--approval-mode=always-ask`. A subagent therefore never blocks the pane on an
  approval, and the root-session gate loses nothing.

### 9. The omp writer cannot reject and is bounded

In omp an unhandled promise rejection exits the process, so the writer must never
reject. Every write is this call, with both outcomes swallowed:

```ts
pi.exec("tmux", ["-N", …], { cwd: "/", timeout: 2000 })
```

Handlers never wait for it. The writer keeps only the latest wanted value, so a burst of
events produces one write.

The alternative was the path that identity publishing uses, a synchronous call to the
Python helper. It lost because that path blocks omp while it runs and because Python's
start-up timeouts broke identity publishing. No Python runs on the status path, so those
timeouts cannot affect status.

### 10. Claude Code status comes from its session state file and three hooks

Under tmux, Claude Code keeps a static title. It does write the live state of each
interactive session to `~/.claude/sessions/<pid>.json`. The helper's `claude-status` call
gains one step that reads that file and corrects the pane.

| File status | Effect |
|---|---|
| `busy` | The pane is working. |
| `waiting` | The pane is blocked. |
| `idle`, `shell` | The turn-over rule applies, unless the turn failed. |

Three hooks cover what the file cannot.

| Event | Effect |
|---|---|
| `PermissionRequest`, and `PreToolUse` with matcher `AskUserQuestion\|ExitPlanMode` | The pane is blocked at once. Claude hides the status line while a prompt is open, so the once-a-second call may pause. |
| `StopFailure` | The pane is blocked and marked failed. It stays blocked until the file reports busy again. |
| `SessionEnd` | The helper clears the status together with the identity. This entry exists already. |

If the file is missing or has no `status`, the helper changes nothing.

The alternative was hooks alone. It lost because it has two gaps. The pane shows working
after Esc until the next prompt, and it shows blocked through an approved long command.

Nobody had observed this file under tmux, so the first Claude task was an experiment. It
ran Claude Code in a scratch tmux server and logged the file through a permission prompt
that the user approves with a long command, one that the user denies, an Esc during a
turn, a question, background subagents and `/compact`. It also checked whether the once-a-second
call keeps running while a prompt is open. Hooks alone were the fallback if the file did
not track those cases, with the two gaps that the user accepted earlier.

The experiment ran on 2026-10-01 with Claude Code 2.1.286 on a scratch tmux server. The
file tracks every case, so the decision stands. Each `statusUpdatedAt` is a time in
milliseconds and moved at the moment the status changed.

| Case | Observed `status` | `statusUpdatedAt` |
|---|---|---|
| A prompt starts a turn | `idle` to `busy` | 1790875443026 |
| A permission prompt opens | `busy` to `waiting`, with `waitingFor` set to `permission prompt` | 1790875445610 |
| The user approves and `sleep 12` runs | `waiting` to `busy` for the whole command, then `idle` | 1790875461009, then 1790875475586 |
| The user denies a permission prompt | `waiting` to `idle` | 1790875484985, then 1790875492564 |
| Esc during a turn | `busy` to `idle` within one second | 1790875509750, then 1790875514812 |
| A question opens | `busy` to `waiting`, with `waitingFor` set to `input needed` | 1790875523290 |
| The user answers the question | `waiting` to `busy`, then `idle` | 1790875538564, then 1790875539942 |
| A subagent runs | `busy` until the subagent's result arrived, then `idle` | 1790875546095, then 1790875570906 |
| `/compact` | `busy` while it compacts | 1790875598862 |
| Claude Code exits | The file is removed. | |

The once-a-second `claude-status` call stopped while a permission prompt or a question was
open and resumed when it closed. The hooks for blocked are therefore needed. The run could
not tell whether the subagent ran in the background.

Task 3.6 repeated the cases on 2026-10-01 with Claude Code 2.1.287, the new helper and the
three hook entries. A short prompt gave working and then done without a client, and idle
with one. A permission prompt and a question gave blocked at once. An approved command
gave working until it ended. A denial and an Esc gave idle. A failed turn stayed blocked
until the next prompt. The exit unset all four pane options. The run did not repeat the
subagent and `/compact` cases.

### 11. Compare-and-set writes and the blocked timestamp keep the two writers apart

The helper reads the pane's current status in the tmux call it already makes. It writes
only when the value changes, and the write is a compare-and-set inside tmux, so a hook and
the once-a-second call cannot overwrite each other. The identity write gets the same
treatment. Today it rewrites the option every second, and each write redraws every
terminal.

A hook that sets blocked records the time its process started in `@workspace_agent_wait`.
The once-a-second call returns the pane to working only when the file's `statusUpdatedAt`
is newer.

Three details came out of the run on the scratch server.

- The helper reads the turn-over value in the one tmux call it already makes and writes
  that value under the compare-and-set. It does not run `set -pF`, because a write that
  happens only on a change needs the value before the write.
- `StopFailure` marks the failed turn in a third pane option, `@workspace_agent_failed`,
  which holds the session id. Any status that the file gives later removes it.
- The once-a-second call can miss a turn that is shorter than a second. A failed pane
  therefore also leaves blocked when the file is idle and its `statusUpdatedAt` is more
  than five seconds newer than the failure. A missed turn that ends inside those five
  seconds keeps the pane blocked until the next prompt.

The alternative was an unconditional write from each writer, with the last one winning.
It lost because a once-a-second call that read the file shortly before a prompt opened would
put the pane back to working right after the hook set blocked.

### 12. The helper accepts a hook event only from the pane's own Claude session

The helper accepts a hook event only when the pane's published identity, `@workspace_app`,
belongs to this Claude session. A nested `claude -p` therefore cannot overwrite the pane.
Subagent events count for blocked.

The alternative was to accept every hook event that arrives with the pane's `TMUX_PANE`.
It lost because a headless Claude that an agent starts inside the pane inherits that
variable.

### 13. Each settings entry always exits 0

The change appends three entries to `~/.claude/settings.json`, for `PermissionRequest`,
`PreToolUse` and `StopFailure`. Each has this shape with `"timeout": 3`:

```sh
[ -n "$TMUX_PANE" ] && /usr/bin/python3 /Users/kh/.local/lib/tmux-workspace/agent-hook.py claude; true
```

It always exits 0 and starts no Python outside tmux. The supacode, herdr and graphify hooks
stay as they are.

The alternative was a bare call to the helper, the shape of the existing `SessionEnd`
entry. It lost because Python exits with status 2 when the helper file is missing, and
status 2 on `PreToolUse` blocks the tool call in every Claude Code process on the machine.

### 14. The helper stays valid on Python 3.9.6 and 3.14

The helper must stay valid on Python 3.9.6 and 3.14, because it runs under both. It prints
nothing for hook events and runs no code at module top level, because `snapshot()`
executes the file.

`tests/test_agent_status.py` uses no unittest feature newer than Python 3.9, so it also
runs under `/usr/bin/python3`.

The alternative was to target one interpreter and use newer syntax. It lost because a
helper that fails to parse under 3.9.6 breaks every hook, and one that fails under 3.14
breaks autosave.

### 15. Picker rows carry a hidden name field

One helper in `bin/tmux-workspace` reads `list-panes -a` once and rolls status up per tab
and per workspace with the same rules as the tmux formats. A pane whose foreground program
is the shell has no status. A parked pane counts toward `@workspace_home`. If the read
fails, the pickers show their rows without status.

In prefix+s each row becomes `name<TAB>label<TAB>status`, and fzf runs with these flags:

- `--delimiter` set to the tab character
- `--with-nth=2..`
- `--nth=1`
- `--accept-nth=1`
- `--ansi`
- `--bind=enter:accept-or-print-query`

fzf then searches only the label and prints only the name, so the status text can never
become part of the name that tmux-workspace opens or creates. The parse becomes one rule,
the last non-empty line of fzf's output, in a pure function with a unit test. The `enter`
binding makes typing a new name create the workspace, as the header and the guide already
promise. The status appears as a coloured word, and the existing `●` stays the marker for
a live workspace.

In prefix+w each row shows the tab's status word after the name. The rows come from the
same single read, keyed by `window_id`. The index stays the first `:` field. The parked
chooser shows each pane's status word.

The first alternative was to append the status word to the visible row and to keep reading
the name back from the row text. It lost because the status word could then end up in the
name of a workspace. The second alternative was to ask tmux to expand the roll-up formats
for each row. It lost because it needs one tmux call per row, while one read feeds all
three pickers and a pure function that a unit test covers.

### 16. Tests run only against a scratch tmux server

The scratch-server test follows one rule. Every tmux call goes through one wrapper that
adds `-L <unique name> -f /dev/null`, removes `TMUX` and `TMUX_PANE` from the environment,
points `TMUX_WORKSPACE_STATE_DIR` at an empty temporary directory and always passes `-t`.
Panes run `sleep`, so a leftover server ends by itself. The test never calls
`tmux-workspace` commands. For the cases that need a visible tab it holds a control-mode
client attached and polls for each expected value.

`tests/test_agent_status_tmux.py` loads `agent-status.conf` into that server and checks
roll-ups, parked credit, shell hiding, tab width, turn over and the seen hooks.

The alternative was to test on the running server, or on a server that loads the real
`tmux.conf`. It lost because any server that loads `~/.tmux.conf` restores every workspace
and starts the saved agents again, and the running server holds live agents.

## Risks / Trade-offs

This section lists every limit of the first version with what softens it.

- Claude status depends on a file whose format Claude Code does not document. If an update
  changes it, Claude panes show no mark until the helper is updated. The helper changes
  nothing when the file is missing or has no `status`, and hooks alone remain the fallback.
- An omp agent that is already running uses the new extension only after it restarts. The
  live walk-through starts fresh agents.
- Some omp prompts fire no event and show working. They are the approval of a settings
  write, consent popups and dialogs that other extensions open. The first version accepts
  this.
- A long-lived background job in omp, such as a dev server, keeps its pane working while
  it runs. This follows from the rule that background work keeps a pane working.
- If the user dismisses an omp plan review without choosing, the pane stays blocked until
  the next prompt. An Esc during a provider retry also shows blocked. The next prompt
  clears both.
- A plan that omp submits from inside its `eval` tool has not been observed. The event
  then names `eval` and not `write`, so the pane may show idle while the plan review is
  open. A plan that omp submits with the `write` tool directly gives blocked.
- A pane hidden behind a zoomed sibling counts as seen, because seeing is per tab.
- Swapping tabs with `swap-window` does not make the tab's done agents seen until the next
  tab switch or focus change. The AI tool's jump to another workspace also makes seen the
  done agents of the tab that was current there.
- If an agent is killed and the user then runs a non-shell program in the same pane, the
  old mark reappears until the next agent starts there.
- The Claude experiment passed, so the state file is in use. If the file stops working,
  hooks alone leave two gaps. The pane shows working after Esc until the next prompt, and
  it shows blocked through an approved long command. The user accepted both.
- The scratch-server runs of task groups 1 to 4 are the only proof so far. The live
  walk-through of task group 5 has not run, apart from the autosave check and the
  validation.
- Every edited file is live through a symlink. A parse check follows each edit, and a
  final check confirms that autosave still runs.
- The three settings entries run in every Claude Code process on the machine. Each entry always
  exits 0, has a timeout of 3 seconds and starts no Python outside tmux.

## Migration Plan

The change touches these files.

| File | Change |
|---|---|
| `agent-status.conf` | New. It holds the formats and hooks of decision 4. |
| `tmux.conf` | Remove the four format lines that the new file replaces, on lines 82, 87, 90 and 91. Add the `source-file` line. |
| `integrations/tmux-workspace.ts` | Add omp status tracking. |
| `integrations/agent-hook.py` | Add the session file read, the three hook events and the compare-and-set writes. |
| `bin/tmux-workspace` | Add the status helper, the picker rows, the fzf flags and a help topic. |
| `tests/test_agent_status.py` | New. It tests the helper's mapping, the roll-up and the row parsing. |
| `tests/test_agent_status_tmux.py` | New. It follows the scratch-server rule of decision 16. |
| `WORKSPACE_GUIDE.md` | Add an agent status section before the change record. Update the contents and the passages on pane labels, parking, tabs, the pickers, the integration table and the test instructions. |
| `~/.claude/settings.json` | Append three hook entries with the `update-config` skill. |

The work lands in the order of the task groups. Each group is usable without the next one.

1. The tmux file and the `tmux.conf` edit. Marks appear for any pane whose option is set.
2. The omp extension. omp panes report after each omp agent restarts.
3. The Claude experiment, the helper and the settings entries. Claude panes report.
4. The pickers.
5. The live walk-through on a tmux server started from a normal terminal. The detached
   server that runs now carries herdr and Claude variables, so it cannot serve this check.

These checks guard every step.

- `python3 -W error::ResourceWarning -m unittest discover -s tests -q` passes with the new
  tests.
- `/usr/bin/python3 -B -m unittest discover -s tests -p 'test_agent_status.py'` passes, so
  the helper works under Python 3.9.
- After each edit both Python files still parse, and the scratch server parses `tmux.conf`
  and `agent-status.conf` with `source-file -n`.
- After the last edit `tmux show-options -gqv @workspace_saved` advances within five
  minutes, which shows that autosave still runs.

Rollback has three steps.

1. Restore `tmux.conf` to its four format lines and remove the `source-file` line, then
   reload the configuration. The reload restores the old formats.
2. Remove the four hooks from the running server with `set-hook -gu`, or restart the
   server. Leftover `@workspace_agent` values are harmless, because no format reads them.
3. Remove the three entries from `~/.claude/settings.json` and restore the other project
   files from git. Running omp agents keep the new extension until they restart.
