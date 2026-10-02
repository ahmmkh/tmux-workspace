# Design

## Context

Three writers set the pane option `@workspace_agent`: the omp extension, the Claude Code helper and the seen hooks in `agent-status.conf`. tmux computes the turn-over result, so no writer alone knows both the old status and whether the new one is done or idle.

## Goals / Non-Goals

**Goals:**
- One sound for blocked and one for done, once per change.
- No change to any writer.
- A mute key and a visible muted state.

**Non-Goals:**
- Toasts or desktop notifications.
- Sounds on a remote client. The sound plays on the machine that runs the tmux server.
- A delay that drops a sound when the status moves on, as herdr has.

## Decisions

### 1. One tmux hook decides

An `after-set-option` hook in `agent-status.conf` compares the pane's status with a shadow pane option, `@agent_heard`, and rings when they differ and the status is blocked or done. It then copies the status into the shadow. A scratch server confirmed that the hook fires for a write from an outside tmux client, for a write nested in `if-shell -F`, and that its target is the written pane.

The alternative was to play the sound from each writer. It lost because that needs the same rule in TypeScript, Python and tmux, and the omp writer cannot know the turn-over result.

### 2. The hook reads `@agent_state`, not the raw option

A pane with the shell in front shows no mark, so it also plays no sound. The alternative, reading `@workspace_agent` directly, would ring for an agent that already exited.

### 3. The player and both files are options

`@agent_sound_cmd`, `@agent_sound_blocked` and `@agent_sound_done` hold the player and the two files. The tests point the player at a stub that records its argument. The alternative, a fixed `afplay` call, cannot be tested without noise.

### 4. `run-shell -b` starts the player

tmux does not wait for the sound. The alternative without `-b` holds the tmux command queue for the length of the sound.

### 5. Mute is one global option

`@agent_sound` is `on` or `off`. It is set with `-o`, so a reload keeps the choice. prefix+M toggles it and replaces the default binding that clears the marked pane. The status line shows `muted` while it is off. The alternative, a config file setting, needs an edit and a reload to silence a noisy moment.

## Risks / Trade-offs

- The hook runs after every `set-option` on the server → it is one format comparison inside tmux and starts no process unless a status changed.
- The seen hooks write from inside a hook, where tmux runs no after hook → the shadow lags until the next write, and that write is done to idle, which is silent anyway.
- `afplay` takes under a second to start → the sound is late, tmux is not.
- A second blocked prompt on an already blocked pane does not ring again.
- macOS only → another system sets `@agent_sound_cmd` to its own player.
