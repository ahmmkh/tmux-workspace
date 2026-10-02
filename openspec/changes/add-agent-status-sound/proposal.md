# Proposal

## Why

Agent status is visible as marks but silent. A blocked agent in another Workspace goes unnoticed until you look at the status line or a picker. herdr played a sound for this, and that is the part of it still missing here.

## What Changes

- A pane that becomes blocked plays a sound, even in the Tab on screen.
- A pane that becomes done plays a different sound.
- Working, idle, a starting agent and a restored agent are silent.
- A prefix key turns the sounds off and on for the whole tmux server, and the status line shows when they are off.

## Capabilities

### New Capabilities
- `agent-sound`: the sounds that announce a change of agent status, and how to silence them.

### Modified Capabilities

None.

## Impact

- `agent-status.conf` gains four options and one tmux hook.
- `tmux.conf` gains one key binding and a muted note in the status line.
- `tests/test_agent_status_tmux.py` gains tests that replace the player with a silent stub.
- The `agent-status` help topic and the agent status section of the guide describe the sounds.
- It depends on the change `add-agent-status`. It needs `afplay`, which macOS ships.
