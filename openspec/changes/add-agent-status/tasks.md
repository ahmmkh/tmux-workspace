# Tasks

These rules hold for every task.

- Never test on the running tmux server and never execute the real `tmux.conf` in a test.
  The parse check reads it with `source-file -n`, which runs none of it. Every scratch
  server follows the rule of design decision 16.
- Every edited project file is live through a symlink. After each edit to
  `bin/tmux-workspace` or `integrations/agent-hook.py` run the parse check for the Python
  files. After each edit to `tmux.conf` or `agent-status.conf` run the parse check for the
  tmux files. A task is not complete until its parse check passes. Run the safety checks
  of a group before moving to the next group.
- The parse check for the Python files is these two commands, run from the project root:

  ```sh
  python3 -c 'import ast,sys; [ast.parse(open(f).read(), f) for f in sys.argv[1:]]' bin/tmux-workspace integrations/agent-hook.py
  /usr/bin/python3 -c 'import ast,sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])' integrations/agent-hook.py
  ```

- The parse check for the tmux files is the test from task 1.2, which runs
  `source-file -n` on `tmux.conf` and `agent-status.conf` on the scratch server:

  ```sh
  python3 -m unittest discover -s tests -p 'test_agent_status_tmux.py' -k parse
  ```

## 1. Status marks in tmux

- [x] 1.1 Create `agent-status.conf` with the exact content of design decision 4 and do not
  load it from `tmux.conf` yet. Prove it on a scratch server under the rule of design
  decision 16. Load the file with `source-file`, set `@workspace_agent` by hand on panes
  that run `sleep`, and expand `@agent_pane`, `@agent_tab`, `@agent_workspace` and
  `@agent_turn_over` with `display-message -p -t`. Verify that the file loads without an
  error and that each expansion gives the mark or the value that design decisions 1 to 5
  describe. If tmux 3.7c rejects or misreads a format, fix `agent-status.conf`, record the
  fix in `design.md` and repeat this task.
- [x] 1.2 Create `tests/test_agent_status_tmux.py` with the one wrapper that every tmux call
  goes through. The wrapper adds `-L <unique name> -f /dev/null`, removes `TMUX` and
  `TMUX_PANE` from the environment, points `TMUX_WORKSPACE_STATE_DIR` at an empty temporary
  directory and always passes `-t`. Panes run `sleep`, and the test never calls
  `tmux-workspace` commands. Add a parse test that runs `source-file -n` on `tmux.conf` and
  on `agent-status.conf`. Verify that the parse test passes, that it fails on a temporary
  file with an unclosed brace, and that the teardown leaves no scratch server behind.
- [x] 1.3 Add tests for the roll-up order blocked, done, working, idle on a tab and on a
  workspace, for a parked pane that counts toward `@workspace_home`, for a pane at the
  shell that shows no mark, and for a tab that keeps its width with and without a mark.
  Verify that `python3 -m unittest discover -s tests -p 'test_agent_status_tmux.py'` passes.
- [x] 1.4 Add tests for turn over and for the four seen hooks. Hold a control-mode client
  attached for the cases that need a visible tab and poll for each expected value. Cover a
  visible pane that becomes idle, a pane that is not visible and becomes done, a tab on
  screen without focus that counts as not visible, a viewed tab whose done panes all
  become idle, a blocked pane that viewing does not change and a parked pane that becomes
  idle when it joins a visible tab. Verify that the same test command passes.
- [x] 1.5 Edit `tmux.conf`. Remove the four format lines on lines 82, 87, 90 and 91 and add
  the `source-file` line of design decision 4 after the status settings. Verify that the
  parse check for the tmux files passes and that `git diff tmux.conf` shows four removed
  lines and one added line.
- [x] 1.6 Add the `agent-status` topic to `HELP_TOPICS` in `bin/tmux-workspace`. It names
  the four statuses, the marks and their colours, the roll-up order and the rule for done
  and seen. Update the `label-pane` topic to say that the pane border shows the mark after
  the label. Update the `park` topic to say that a parked agent counts toward its home
  workspace and keeps showing done until the pane comes back. Verify that
  `tmux-workspace help-topic agent-status` prints the new text.
- [x] 1.7 Update `WORKSPACE_GUIDE.md`. Add an agent status section before the change record
  and add it to the table of contents. Update the border display passage of section 6, the
  parking passage of section 7, the tab passage of section 8, the integration table of
  section 11 with a row for `agent-status.conf`, and the test instructions under
  "Repeatable verification" with the scratch-server test. Add a change record entry.
  State the three limits of the marks from the risks in `design.md`: a pane behind a
  zoomed sibling counts as seen, `swap-window` does not make the done agents of a tab seen
  until the next tab switch or focus change, and an old mark reappears when a non-shell
  program runs in the pane of a killed agent. Verify that every statement in the new
  section matches the `agent-status` spec.
- [x] 1.8 Run the safety checks. Verify that both parse checks pass and that
  `python3 -W error::ResourceWarning -m unittest discover -s tests -q` passes.

## 2. omp status

This group has no automated test, because the project has no test runner for TypeScript.
Its checks are observations on a scratch server.

- [x] 2.1 Prove the omp events before the tracking code lands. Start a scratch
  server with `-L <unique name> -f /dev/null`, load `agent-status.conf` into it and run
  omp in one of its panes. Use a temporary logging handler for the events of the table in
  design decision 8. Verify that the log shows each event with the fields the table relies
  on, and that `ctx.hasUI`, `ctx.mode` and `ctx.agent.kind` tell the root session apart
  from a subagent. Remove the logging handler afterwards. If omp 18.4.2 differs from the
  table, correct `design.md` before task 2.2.
- [x] 2.2 Add the writer of design decision 9 to `integrations/tmux-workspace.ts`. It calls
  `pi.exec` with `cwd: "/"` and `timeout: 2000`, swallows both outcomes, keeps only the
  latest wanted value and never makes a handler wait. Verify with a temporary target pane
  that does not exist that omp keeps running and reports no unhandled rejection.
- [x] 2.3 Add the root-session gate of design decision 7, the handlers for `agent_start`,
  `agent_end`, the five `session_` reset events and `session_shutdown`, the status rule and
  the fallback on the existing timer with three consecutive idle readings. Verify on the
  scratch server with `show-options -pqv -t <pane> @workspace_agent` that a prompt gives
  working, that a finished turn on a server without an attached terminal gives done, that a
  background job keeps the pane working, that a finishing subagent does not change the
  pane and that the option is unset after omp exits.
- [x] 2.4 Add the pending set for approvals and for the `ask` tool, the pending plan, the
  retry flag and the failed turn. Verify on the scratch server that an approval, an `ask`
  and a submitted plan each give blocked, that an answer gives working again, and that a
  failed turn stays blocked until the next prompt.
- [x] 2.5 Add the omp paragraph to the agent status section of `WORKSPACE_GUIDE.md` and to
  the `agent-status` help topic. It says what omp reports, that a running omp agent must
  restart to use the new extension, and the omp limits from the risks in `design.md`.
  Update the row for `integrations/tmux-workspace.ts` in the integration table of section
  11. Verify that `tmux-workspace help-topic agent-status` prints the omp text.
- [x] 2.6 Run the safety checks. Verify that both parse checks pass and that the full test
  command of task 1.8 passes.

## 3. Claude Code status

- [x] 3.1 Run the Claude experiment before any helper code changes. Start a scratch
  server with `-L <unique name> -f /dev/null`, load `agent-status.conf` and nothing else
  into it and run Claude Code in one of its panes. Log `~/.claude/sessions/<pid>.json`
  once a second through a permission prompt that the user approves with a long command,
  one that the user denies, an Esc during a turn, a question, background subagents and `/compact`. Check whether the once-a-second
  `claude-status` call keeps running while a prompt is open. Verify the task by recording
  the observed `status` and `statusUpdatedAt` for each case under design decision 10. The
  fallback is hooks alone, with the two gaps that the user accepted. If the file does not
  track these cases, stop and revise design decision 10, the affected spec scenarios and
  tasks 3.2 to 3.6 with `/opsx:update` before going on.
- [x] 3.2 Create `tests/test_agent_status.py` with tests for the helper's mapping. Cover
  `busy` to working, `waiting` to blocked, `idle` and `shell` to turn over unless the turn
  failed, a missing file or a missing `status` that changes nothing, the blocked timestamp
  rule and the identity rule of design decision 12. Use no unittest feature newer than
  Python 3.9. Verify that the tests fail before task 3.3 and that the file runs under both
  interpreters.
- [x] 3.3 Add the session file read to the `claude-status` path of
  `integrations/agent-hook.py` and make the status write and the identity write
  compare-and-set writes that run only when the value changes. Keep the file free of code
  at module top level. Verify that the mapping tests pass under `python3` and under
  `/usr/bin/python3`, and that a test proves the helper issues no tmux write when the value
  is unchanged.
- [x] 3.4 Add the hook events to the helper. `PermissionRequest` and `PreToolUse` set
  blocked at once and record the start time of the hook process in `@workspace_agent_wait`.
  `StopFailure` sets blocked and marks the turn failed. `SessionEnd` clears the status
  together with the identity. The helper accepts an event only when `@workspace_app` belongs
  to this Claude session, counts subagent events for blocked and prints nothing for hook
  events. Verify with tests that feed each event on standard input, including an event
  from a nested `claude -p` that changes nothing.
- [x] 3.5 Append the three hook entries to `~/.claude/settings.json` with the
  `update-config` skill. The entries are for `PermissionRequest`, for `PreToolUse` with the
  matcher `AskUserQuestion|ExitPlanMode` and for `StopFailure`. Each uses the always-exit-0
  shape of design decision 13 with `"timeout": 3`. Leave the supacode, herdr and graphify
  hooks and every other entry untouched. Verify that the file still parses as JSON, that a
  comparison with the previous file shows three added entries and no other difference,
  that the command exits 0 with `TMUX_PANE` unset, and that it exits 0 when the helper path
  does not exist.
- [x] 3.6 Repeat the cases of task 3.1 on a scratch server that has loaded
  `agent-status.conf`, with the new helper and the new entries. Verify with `show-options -pqv -t <pane> @workspace_agent` that a turn gives
  working, that a permission prompt and a question give blocked at once, that an approved
  prompt returns to working, that an Esc ends in the turn-over value, that a failed turn
  stays blocked until the next prompt and that the option is unset after Claude Code exits.
- [x] 3.7 Add the Claude Code paragraph to the agent status section of `WORKSPACE_GUIDE.md`
  and to the `agent-status` help topic. It says what Claude Code reports and that the
  status depends on a state file that Claude Code does not document. Update the row for
  `integrations/agent-hook.py` in the integration table of section 11, the sentence below
  that table about `~/.claude/settings.json`, and the test instructions with the
  `/usr/bin/python3` command. Verify that the documented test commands run as written.
- [x] 3.8 Run the safety checks. Verify that both parse checks pass, that
  `/usr/bin/python3 -B -m unittest discover -s tests -p 'test_agent_status.py'` passes and
  that the full test command of task 1.8 passes.

## 4. Pickers

- [x] 4.1 Prove the picker mechanics before the picker code changes. Feed two rows of the
  form `name<TAB>label<TAB>status` to fzf with `--delimiter`, `--with-nth=2..`, `--nth=1`,
  `--accept-nth=1`, `--ansi` and `--filter`. On a scratch server run the single
  `list-panes -a` read with the fields the roll-up needs. Verify that a filter on the label
  prints only the name, that a filter on the status word prints nothing, and that the read
  returns one line per pane with the status, the foreground program, `window_id` and
  `@workspace_home`.
- [x] 4.2 Add tests to `tests/test_agent_status.py` for the roll-up per tab and per
  workspace, for a pane at the shell that has no status, for a parked pane that counts
  toward its home workspace, for the row format and for the parse rule that takes the last
  non-empty line of fzf's output. Verify that the tests fail before task 4.3 and that the
  file still runs under `/usr/bin/python3`.
- [x] 4.3 Add the status helper to `bin/tmux-workspace`. It reads `list-panes -a` once and
  rolls status up with the same rules as the tmux formats. It returns no status when the
  read fails. Verify that the roll-up tests pass and that a test with a failing read gets
  rows without status.
- [x] 4.4 Change the workspace picker. Each row becomes `name<TAB>label<TAB>status`, fzf
  runs with the flags of design decision 15 including
  `--bind=enter:accept-or-print-query`, and the parse becomes the pure function from task
  4.2. Verify that the row and parse tests pass, including a typed name that matches
  nothing and a row whose status word never reaches the returned name.
- [x] 4.5 Change the tab picker. Each row shows the tab's status word after the name, the
  rows come from the same single read keyed by `window_id`, and the index stays the first
  `:` field. Verify with a test that the index parse returns the same index with and
  without a status word.
- [x] 4.6 Change the parked chooser so each row shows the pane's status word. Verify with a
  test that a row for a pane without an agent has no status word and that the numbering of
  the rows does not change.
- [x] 4.7 Update the help topics `workspace-picker`, `pick-window` and `choose-park-right`
  and add the pickers to the `agent-status` topic. Update `WORKSPACE_GUIDE.md` in the
  passage "Switching & Deleting Workspaces from Inside tmux" of section 2, the chooser
  passage of section 7, the `Ctrl+a` `w` entry of section 8 and the agent status section.
  Extend the change record entry. Verify that `tmux-workspace help-topic workspace-picker`
  prints the new text and that the guide still promises that a typed new name creates a
  workspace.
- [x] 4.8 Run the safety checks. Verify that both parse checks pass, that the command of
  task 3.8 for `/usr/bin/python3` passes and that the full test command of task 1.8 passes.

## 5. Live walk-through

This group checks the whole change on a tmux server that the user started from a normal
terminal. The detached server that runs now carries herdr and Claude variables, so it
cannot serve this check.

- [ ] 5.1 Ask the user to start a tmux server from a normal terminal and to attach it. No
  agent starts that server, because a server that loads `~/.tmux.conf` restores every
  workspace. Verify with `tmux show-environment -g` that the server carries no herdr and
  no Claude variables.
- [x] 5.2 Check autosave after the last edit. Verify that
  `tmux show-options -gqv @workspace_saved` advances within five minutes.
- [ ] 5.3 Run omp in a tab and prompt it. Verify that the mark goes from working to done
  after a switch to another tab, that it turns idle on return, and that an `ask` turns it
  blocked.
- [ ] 5.4 Repeat the walk-through with Claude Code, including a permission prompt and an
  Esc. Verify that the marks on the pane border, the tab and the workspace name follow each
  step.
- [ ] 5.5 Park an agent. Verify that the mark of its home workspace still counts it and
  that a done parked agent turns idle only after the pane comes back.
- [ ] 5.6 Open prefix+s and prefix+w. Verify that each row shows the right status word.
- [ ] 5.7 Type a new workspace name in prefix+s and press Enter. Verify that tmux-workspace
  creates a workspace with exactly that name and creates nothing else.
- [x] 5.8 Run `openspec validate "add-agent-status" --strict`. Verify that it reports no
  error and no warning.
