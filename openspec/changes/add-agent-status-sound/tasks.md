# Tasks

## 1. Sound hook

- [x] 1.1 Prove on a scratch server that an `after-set-option` hook fires for an outside `set-option` and for one nested in `if-shell -F`, and that it targets the written pane. Verified with a stub player that logged BLOCKED once and DONE once.
- [x] 1.2 Add the four options and the hook to `agent-status.conf`. Verified by the `Sound` tests.
- [x] 1.3 Add the `Sound` tests to `tests/test_agent_status_tmux.py` with a stub in place of the player. Verified by `python3 -W error::ResourceWarning -m unittest discover -s tests -q`.

## 2. Mute

- [x] 2.1 Add the prefix+M binding and the muted note in `status-right` to `tmux.conf`. Verified by the parse test, which loads `tmux.conf` with `source-file -n`.
- [x] 2.2 Describe the sounds and the mute key in the `agent-status` help topic and in the agent status section of `WORKSPACE_GUIDE.md`. Verified by reading `tmux-workspace help-topic agent-status`.

## 3. Live check

- [ ] 3.1 On a tmux server started from a normal terminal, hear the done sound when an agent finishes in another Tab and the blocked sound when an agent asks a question.
- [ ] 3.2 Press prefix+M. Verify that the status line shows `muted` and that no sound plays.
