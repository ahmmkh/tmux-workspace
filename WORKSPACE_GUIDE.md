# TMUX WORKSPACE COMPLETE GUIDE

This document is your permanent offline reference for your personalized tmux workspace setup.
It covers core concepts, everyday shortcuts, advanced parking workflows, sesh workspace management, and how reboot recovery works with AI tools and Neovim.

## Start here

All workspace-specific source now lives in:
`~/untitled folder/Projects/tmux-workspace/`

- Open the project: `work tmux-workspace`
- Open all AI tools: **`Ctrl+a`, release, then `a`**
- Read this guide: `less "$HOME/untitled folder/Projects/tmux-workspace/WORKSPACE_GUIDE.md"`
- Check terminal, hooks and saves: `tmux-workspace doctor` or **`Ctrl+a D`**
- Check provider access: `tmux-workspace ai check`
- Existing keybindings, `work`, recovery, agent hooks and Neovim integration remain active through symlinks to this project.

---

## Table of Contents
1. [Core Concepts & Terminology](#1-core-concepts--terminology)
2. [Starting & Switching Workspaces (`work` & sesh)](#2-starting--switching-workspaces-work--sesh)
3. [Prefix & Key Combinations](#3-prefix--key-combinations)
4. [Splitting, Rotating & Zooming Panes](#4-splitting-rotating--zooming-panes)
5. [Moving & Resizing](#5-moving--resizing)
6. [Pane Labels & Naming](#6-pane-labels--naming)
7. [Selective Parking (Hide & Restore Splits)](#7-selective-parking-hide--restore-splits)
8. [Windows (Tabs/Views within a Project)](#8-windows-tabviews-within-a-project)
9. [Copy Mode & macOS Clipboard](#9-copy-mode--macos-clipboard)
10. [Reboot Recovery & AI Session Persistence](#10-reboot-recovery--ai-session-persistence)
11. [Configuration Files & Backups](#11-configuration-files--backups)
12. [Quick Reference Cheatsheet](#12-quick-reference-cheatsheet)
13. [AI Workflow Tools & Privacy](#13-ai-workflow-tools--privacy)
    - [Agent status](#agent-status)

---

## 1. Core Concepts & Terminology

- **Session (Project Workspace)**: A persistent collection of windows representing a single project or task (e.g. `main`, `ecommerce-api`, `frontend`). Sessions remain alive even when you detach or close terminal windows.
- **Window (Tab / View)**: A full-screen view inside a session containing one or more splits. You can have a `code` window, a `servers` window, and a `db` window in the same session.
- **Pane (Split)**: A rectangular terminal division running an interactive shell or program.
- **Prefix Key (`Ctrl+a`)**: Tells tmux the next keystroke is a command for tmux rather than the active shell. Press `Ctrl+a`, release it, then press the shortcut key.
- **Passthrough (`Ctrl+a Ctrl+a`)**: Sends an actual `Ctrl+a` to your shell (e.g. to jump to the beginning of the command line in bash/zsh) or into an editor.

---

## 2. Starting, Creating & Deleting Workspaces (`work`)

Your shell is integrated with **tmux-workspace**, **sesh**, and **zoxide** through the `work` command:

```bash
# Open the interactive fuzzy workspace picker (select existing, or type a NEW name to create)
work

# Connect to an existing workspace, or automatically CREATE a new one
work my-api

# Connect to or create a workspace named after the current folder
work .

# Close / delete an active workspace
work rm my-api

# Interactive fuzzy picker to choose which workspace to close/delete
work rm
```

### Automatic Project Folder Creation:
When you run `work <name>` (or type a new name in the `work` picker):
1. It checks if `~/untitled folder/Projects/<name>` exists; if not, it **automatically creates that folder**.
2. It creates the tmux session rooted in `~/untitled folder/Projects/<name>`.
3. All subsequent windows and splits inside that workspace **default to this project folder**.
4. It registers the project path with `zoxide` for fast navigation.

### Zoxide as Default Navigation & Resolution:
- **`cd` powered by zoxide**: Your shell uses `eval "$(zoxide init zsh --cmd cd)"`. Running `cd <partial-name>` (e.g. `cd asda` or `cd cafe`) jumps directly to your most used directories and projects.
- **`work` uses zoxide first**: Running `work <name>` checks zoxide's smart index first to find existing matching projects before falling back to creating a new project folder.
- **Workspace picker ranking**: Running workspaces appear most recently accessed first, followed by zoxide directories and unopened project folders. Typing ranks fuzzy matches by relevance, with recency breaking ties. Recency comes from tmux and resets when its server restarts. The fallback `sesh` picker uses its own ranking.

### Switching & Deleting Workspaces from Inside tmux:
- **`Ctrl+a` then `s`**: Opens the workspace picker popup over your screen. Select an existing workspace or type a new name to create one on the fly. Enter creates the workspace when the typed name matches no row. The row of an active workspace shows the most urgent agent status among its agents as a coloured word. The search matches only the name, and the status word never becomes part of a workspace name.
- **`Ctrl+a` then `Tab`**: Bounces back to your previous workspace.
- **`Ctrl+a` then `X`** *(capital X)*: Closes and terminates the current workspace session (asks confirmation first, and automatically switches you to another active workspace so you remain inside tmux).
- **`Ctrl+a` then `w`**: Opens the current project's fuzzy window picker with pane previews. **`Ctrl+a W`** opens the global session/window tree.
- **`Ctrl+a` then `$`**: Rename the current session.

---

## 3. Prefix & Key Combinations

The prefix is **`Ctrl+a`**.
All commands below assume you:
1. Press **`Ctrl+a`**
2. Release both keys
3. Press the corresponding command key

*Tip: For movement and resizing, bindings are **repeatable**. You can press `Ctrl+a`, then tap direction keys multiple times within 800ms without pressing `Ctrl+a` again.*

---

## 4. Splitting, Rotating & Zooming Panes

### Creating Splits
- **`Ctrl+a` then `|`** *(or `%`)*: Split **vertically** (left and right).
- **`Ctrl+a` then `-`** *(or `"`)*: Split **horizontally** (top and bottom).
*(New splits always open in the current pane's working directory and prompt you for a label).*

### Rotating & Layouts
- **`Ctrl+a` then `Space`**: Cycles through preset split arrangements:
  - Even horizontal splits
  - Even vertical splits
  - Main vertical (large pane on left, stacked splits on right)
  - Main horizontal (large pane on top, stacked splits on bottom)
  - Tiled grid

### Zoom (Focus View)
- **`Ctrl+a` then `z`**: Toggles **Zoom** on the active pane. When zoomed, the pane temporarily fills the entire window. Press `Ctrl+a` then `z` again to return to the previous multi-pane layout.
- For larger text in Ghostty, press **`⌘` + `+`** (or **`⌘` + `=`**) repeatedly. **`⌘` + `−`** shrinks it; **`⌘` + `0`** resets the font size.
- Font size applies to the whole Ghostty terminal, including all its tmux panes. For focused reading, zoom the pane first, then enlarge the font. The status bar shows **ZOOM** while the pane is maximized.

### Closing
- **`Ctrl+a` then `x`**: Asks for confirmation (`Close pane <label>? (y/n)`) before closing the active pane.
- Or simply type `exit` in the shell.

---

## 5. Moving & Resizing

### Moving Focus
- **`Ctrl+a` then `h` / `j` / `k` / `l`**: Move focus Left, Down, Up, Right (vim keys, repeatable).
- **`Ctrl+a` then `Left` / `Down` / `Up` / `Right`**: Move focus using arrow keys (repeatable).
- **`Ctrl+a` then `o`**: Cycle focus to the next pane in order.
- **`Ctrl+a` then `;`**: Jump back to the previously active pane.
- **Mouse**: Click on any pane to focus it.

### Resizing Panes
- **`Ctrl+a` then `H` / `J` / `K` / `L`**: Grow/shrink the active pane Left, Down, Up, Right in 5/3 cell steps (repeatable).
- **`Ctrl+a` then `Shift + Arrow keys`**: Directional resize (repeatable).
- **Mouse**: Click and drag any border line between panes to adjust sizes smoothly.

---

## 6. Pane Labels & Naming

Every pane maintains an independent label saved in tmux user options (`@workspace_label`), so running programs cannot overwrite it:

- **At Pane Creation**: Whenever you create a split or window, a popup prompt asks:
  ```text
  Pane label [shell]: 
  ```
  Type a descriptive name (e.g. `api`, `logs`, `tests`, `db`) or press Enter to keep `shell`.
- **Rename at Any Time**: Press **`Ctrl+a` then `t`** to open the label popup and enter a new name.
- **Border Display**: Labels are rendered on the top border of each pane:
  ```text
  ┌ 1: api ──────────────────────────┬ 2: logs ──────────────────────────┐
  ```
  When the pane runs an agent, the border shows the agent status mark after the label:
  ```text
  ┌ 1: api ● ────────────────────────┬ 2: logs ──────────────────────────┐
  ```
  The section [Agent status](#agent-status) explains the marks.

---

## 7. Selective Parking (Hide & Restore Splits)

Parking is a feature built specifically for hiding splits (like long-running server logs or watchers) while keeping their processes running in the background.

### Parking a Pane
- Focus the pane you want to hide.
- Press **`Ctrl+a` then `!`**.
- The pane disappears from your visible window and its space is reclaimed by sibling panes.
- The process in that pane **continues running** uninterrupted in a hidden holding session (`__parked`).
- A parked agent counts toward the status of its home workspace. A parked agent that is done keeps showing done until you bring the pane back.

### Restoring Parked Panes
- **Restore the most recently parked pane:**
  - **`Ctrl+a` then `u`**: Restores the pane to the **right** of your current pane.
  - **`Ctrl+a` then `b`**: Restores the pane **below** your current pane.

- **Interactive Chooser (when multiple panes are parked):**
  - **`Ctrl+a` then `U`**: Opens a popup listing all parked panes with their labels and origin workspaces. Type the number and hit Enter to restore that pane to the **right**. The row of a parked pane that runs an agent also shows its status word.
  - **`Ctrl+a` then `B`**: Opens the popup chooser and restores the chosen pane **below**.

---

## 8. Windows (Tabs/Views within a Project)

Windows let you organize different concerns inside the same project workspace.
Each tab in the status line shows the mark of the most urgent agent status among its panes. The mark sits in front of the tab index, and a tab keeps its width with and without a mark.

- **`Ctrl+a` then `c`**: Create a new window / tab. Prompts you to name the tab first in the status bar (`Tab name: `), sets the tab name, and automatically labels the initial pane with that name so you can start working immediately (defaults to `tab` if left blank; press `Esc` to cancel).
- **`Ctrl+a` then `,`**: Rename the current window.
- **`Ctrl+a` then `1` .. `9`**: Jump directly to window 1 through 9.
- **`Ctrl+a` then `w`**: **Window Fuzzy Picker**. Opens an interactive search popup listing all windows (tabs) in your current workspace with live terminal screen previews and running pane summaries. Filter by index, name, or pane label and press Enter to switch immediately. Each row shows the agent status word of its tab after the tab name.
- **`Ctrl+a` then `W`** *(capital W)*: Opens tmux's global interactive tree view across all sessions and windows.
- **`Ctrl+a` then `n`**: Next window.
- **`Ctrl+a` then `p`**: Previous window.
- **`Ctrl+a` then `&`**: Close the entire active window and all its panes.
- **`Ctrl+a` then `d`**: **Detach** from tmux. Leaves all workspaces, panes, background tasks, and editors running on the server. Reconnect anytime with `work`.

---

## 9. Copy Mode & macOS Clipboard

- **Mouse Scroll**: Scrolling with your trackpad or mouse wheel automatically enters scrollback mode.
- **Enter Copy Mode manually**: Press **`Ctrl+a` then `[`** (press `q` or `Esc` to exit).
- **Navigate Scrollback**: Use `h, j, k, l`, arrow keys, or `Ctrl+u` / `Ctrl+d` (half page up/down).
- **Search Scrollback**: Press `/` in copy mode, type your search term, and press Enter (`n` for next match, `N` for previous).
- **Text Selection & Copying**:
  - In copy mode, press `v` to begin text selection.
  - Move cursor to highlight text.
  - Press `y` to copy the selected text directly to your macOS system clipboard (`pbcopy`).
  - Or simply click and drag with the mouse; releasing the drag automatically copies to your macOS clipboard.
- **Paste in tmux**: Press **`Ctrl+a` then `]`** (or standard `Cmd+v` in your terminal app).

---

## 10. Reboot Recovery & AI Session Persistence

### How Snapshots Work
- **Automatic Snapshots**: Saved every 5 minutes while tmux runs.
- **Manual Snapshot**: Press **`Ctrl+a` then `Ctrl+s`** to take an immediate snapshot before a planned restart.
- **Privacy & Security**: Snapshots are saved in `~/.local/state/tmux-workspace/snapshots/` with strict user permissions (`0700` directory, `0600` files).
- **Terminal Scrollback**: Sanitized text from all panes is saved. Press **`Ctrl+a` then `P`** to view the saved scrollback history for the current pane in a pager.

### What Happens When You Restart tmux
When you run `work` after a reboot:
1. tmux reads the latest snapshot.
2. All workspaces, windows, split geometries, and pane labels are reconstructed.
3. Each pane displays its previous terminal text header so you can see where you left off.
4. Active applications are restored according to exact rules:
   - **Claude Code**: Reopens via `claude --resume <exact-id>`.
   - **OpenAI Codex**: Reopens via `codex resume <exact-id>`.
   - **Oh My Pi**: Reopens via `omp --resume <session-file>`.
   - **Neovim**: Restores open files, splits, tabs, and cursor positions via `nvim -S <session>`. Swapfile recovery protects unsaved edits; source files are **never** auto-written.
   - **Deliberately Exited Agent**: Restores as a clean interactive shell; does not resurrect an exited tool.
   - **Other Programs** (e.g. servers, logs, scripts): Restored as clean shells in their original directory with their label and scrollback; programs are not blindly re-executed.

### Ordinary Shell Commands
You do not need a special launcher. You can type `claude`, `codex`, `omp`, or `nvim` directly in any shell pane. Built-in lifecycle hooks and native process tracking detect tool startup, thread changes, and exits automatically.

---

## 11. Configuration Files & Backups

### One canonical project

Edit these files in `~/untitled folder/Projects/tmux-workspace/`; do not maintain separate copies.

| Project path | Live integration |
|---|---|
| `tmux.conf` | `~/.tmux.conf` symlink; options, keybindings and status |
| `agent-status.conf` | Loaded by `tmux.conf` with `source-file`. It holds the formats and hooks of the agent status marks. |
| `bin/tmux-workspace` | `~/.local/bin/tmux-workspace` symlink; existing controller and AI entrypoint |
| `lib/ai.py`, `lib/ai_core.py` | AI popup, permission controls and shared OpenRouter client |
| `lib/panes_ai.py` | Semantic pane finder and evidence-backed attention list |
| `lib/history_ai.py` | Opt-in snapshot search |
| `lib/suggest_ai.py` | Source-backed shortcut and installed-skill suggestions |
| `config.json` | Model and Doppler project/config/scope; no API key |
| `shell.zsh` | Sourced by `~/.zshrc`; `work` and zoxide integration |
| `sesh.toml` | `~/.config/sesh/sesh.toml` symlink |
| `integrations/agent-hook.py` | `~/.local/lib/tmux-workspace/agent-hook.py` symlink. The helper publishes pane identities and reports Claude Code and Codex status. |
| `integrations/tmux-workspace.ts` | `~/.omp/agent/extensions/tmux-workspace.ts` symlink. The omp extension publishes the pane identity and reports the agent status of omp. |
| `integrations/tmux_workspace.lua` | `~/.config/nvim/lua/custom/tmux_workspace.lua` symlink |
| `WORKSPACE_GUIDE.md` | `~/.config/tmux/WORKSPACE_GUIDE.md` symlink; this guide |

Shared application registrations stay in their owning configurations:
`~/.claude/settings.json` invokes the agent-hook path above for the session hooks, for the
status line and for the three agent status hooks `PermissionRequest`, `PreToolUse` and
`StopFailure`. Each status hook entry always exits with status 0.
`~/.config/nvim/init.lua` calls `require('custom.tmux_workspace').setup()`.
Those shared files are not copied into this project.

### Private state and backups stay separate

`~/.local/state/tmux-workspace/` retains snapshots, editor sessions and original
configuration backups (`backups/`). Agent conversation history stays with its
original harness. None of that private state was moved into the source project.

The setup is personal to this machine: source and live symlinks use absolute paths.
Moving the project again requires updating those symlinks and the shell source path.

---

## 12. Quick Reference Cheatsheet

| Key | Action |
|---|---|
| `Ctrl+a` `?` | **Open built-in comprehensive help card** |
| `Ctrl+a` `s` | Open sesh workspace picker popup |
| `Ctrl+a` `Tab` | Bounce to previous workspace |
| `Ctrl+a` `\|` or `%` | Split vertically (left/right) |
| `Ctrl+a` `-` or `"` | Split horizontally (top/bottom) |
| `Ctrl+a` `h` / `j` / `k` / `l` | Move focus (repeatable) |
| `Ctrl+a` `H` / `J` / `K` / `L` | Resize pane (repeatable) |
| `Ctrl+a` `Shift+arrows` | Directional resize (repeatable) |
| `Ctrl+a` `Space` | Rotate split layouts |
| `Ctrl+a` `z` | Toggle zoom active pane |
| `Ctrl+a` `t` | Rename active pane label |
| `Ctrl+a` `x` | Close active pane (with confirmation) |
| `Ctrl+a` `!` | **Park (hide) focused pane in background** |
| `Ctrl+a` `u` | Restore latest parked pane (right) |
| `Ctrl+a` `b` | Restore latest parked pane (below) |
| `Ctrl+a` `U` | Choose and restore parked pane (right) |
| `Ctrl+a` `B` | Choose and restore parked pane (below) |
| `Ctrl+a` `c` | Name & create new window (tab) |
| `Ctrl+a` `1`..`9` | Switch to window 1..9 |
| `Ctrl+a` `w` | **Fuzzy search & switch windows (in project)** |
| `Ctrl+a` `W` | Global session & window tree view |
| `Ctrl+a` `X` | Close current workspace (with confirmation) |
| `Ctrl+a` `d` | Detach session (leaves everything running) |
| `Ctrl+a` `Ctrl+s` | Save snapshot now |
| `Ctrl+a` `Ctrl+r` | Restore missing workspaces |
| `Ctrl+a` `P` | View saved scrollback history for pane |
| `Ctrl+a` `[` | Enter scroll/copy mode (`q` exits) |
| `Ctrl+a` `]` | Paste buffer |
| `Ctrl+a` `r` | Reload tmux configuration |
| `Ctrl+a` `D` | Terminal, hooks, agent and autosave diagnostics |
| `Ctrl+a` `M` | Toggle agent sounds |
| `Ctrl+a` `Ctrl+a` | Send raw Ctrl+a to shell/editor |

---

## 13. AI Workflow Tools & Privacy

### Five on-demand tools

Press **`Ctrl+a a`** for one menu:

- **find**: type a task to rerank live and parked panes across all workspaces.
  The local catalog appears first; Jev reranks after a 250 ms typing pause.
  Obsolete refreshes are cancelled, but requests already received by the provider
  cannot be recalled. Clearing the query restores the local catalog without inference.
  Metadata works without output sharing. Enter inspects evidence; `g` navigates.
  Bringing back a parked pane requires another confirmation. Source and destination
  are checked again after confirmation; changed identities or locations abort the move.
- **attention**: inspect opted-in output for independent conditions: user input
  needed, unresolved blocker, or completed work awaiting review. Independent Noul
  predicates and speculative source-line Choices run together over shared state.
  Each finding includes P(yes), an exact source line and context. P(yes) > 0.5 plus
  supporting evidence routes a review item, never an action. Unknowns stay unclassified.
  Capture time is not message time, and silence does not mean an agent is stuck or done.
  Routine approval or input alone is not a technical blocker. Asking to review
  finished work alone is not blocking input; a separate deployment blocker does not
  cancel an explicit request to review finished code.
- **search-history**: search the retained snapshots for currently opted-in pane
  identities. Results contain source quotes, saved timestamps and locations,
  clearly marked historical. At most three snapshots are searched; source reads
  are bounded, and a local shortlist of up to 32 chunks is ranked. Notices report
  truncation or incomplete coverage. Archived output is not evidence of current state.
- **suggest**: ask which existing shortcut or installed skill fits. All discovered
  catalog descriptions are ranked; up to five candidates are checked against
  source excerpts. Suggestions show real shortcut text or `/skill-name`, never
  generated commands. No skill executes and nothing is typed into an agent.
- **name-tab**: Jev selects a short existing phrase from an optional task hint,
  meaningful pane/project labels, or opted-in source spans. Code bounds and checks
  candidates; one Choice selects a candidate or none. No free-form text generation.
  Preview the suggestion, press `r`, then confirm. Existing names are never overwritten automatically; concurrent
  manual renames, changed task context, or changed window membership invalidate the suggestion,
  including changes during confirmation. Pane labels
  are unchanged. `Ctrl+a ,` remains the direct manual rename shortcut.

The menu also provides **share** (per-pane permission) and **check** (provider test).
The menu's **check** keeps success or failure visible until you press Enter.
Direct `tmux-workspace ai check` prints its result and exits without a dismissal prompt.
Esc cancels a picker. Enter selects evidence, not permission to execute commands.
Existing `Ctrl+a s`, `Ctrl+a w`, `Ctrl+a U` and `Ctrl+a ?` remain available offline.

### Command-line access

Run inside tmux for interactive navigation. `--json` prints results without navigation:

```sh
tmux-workspace ai find "client onboarding"
tmux-workspace ai attention
tmux-workspace ai search-history "database authentication failed"
tmux-workspace ai suggest "hide this pane without stopping it"
tmux-workspace ai name-tab "fix billing database authentication"
tmux-workspace ai find "billing migration" --json
tmux-workspace ai home
```

### Permission and data handling

**Metadata-only by default.** Semantic pane search sends labels, project/directory
metadata and process identity metadata. Skill suggestions send installed skill
descriptions and selected excerpts. No terminal output is read for AI without
explicit per-pane sharing.

The share menu warns before enabling access to both recent terminal output and
retained snapshot text for that pane. Permission applies to the pane, including
future programs started there, until revoked or tmux restarts. Restored panes
start private. No background AI polling or automatic history uploads run.

```sh
# Prompts for permission before enabling:
tmux-workspace ai share %12
# Revoke explicitly:
tmux-workspace ai share %12 --off
```

Use an actual pane ID shown by the tool, not the example `%12`.
`--allow` is the explicit noninteractive consent flag. Avoid it unless you intend
to share that pane's current and retained output.

Sharing and pane identity are rechecked after credential lookup and before each
request carrying terminal evidence, including each batch. A detected revocation
aborts that evidence instead of returning stale excerpt-bearing fallback results.

Terminal output can contain credentials or client data. Common-secret redaction
is best-effort, not a guarantee. Requests go to OpenRouter and the selected model
provider, with `data_collection: deny` and `zdr: true` routing constraints.
Already-sent requests cannot be recalled. Do not opt in sensitive panes.

### Provider and failure behavior

The model is **TypeSafe Jev 1.13**, reached through **OpenRouter** with the existing
OpenRouter key: `typesafe/jev-1.13`, using `POST /api/v1/systemone`.
No chat-completions endpoint, GPT path, or automatic model substitution remains.
Edit `config.json` for another Jev version or Doppler source.
The client reads `OPENROUTER_API_KEY` from the environment first, otherwise from
the configured Doppler project/config. Secrets stay in memory; none are written
to this source project or printed by the helper. The live finder resolves the key
once and passes it only in its child processes' environment, never command arguments.

Jev acts as a semantic condition in ordinary code: Noul for attention predicates,
Score for graded relevance, Choice for source and tab-name selection. Independent
questions share one request when bounds allow; overflow batches run concurrently.
Scores sort by expected relevance (0–2); a highest-probability no-match level excludes
the candidate. Raw probabilities/confidence remain available in `--json` results.
These review/ranking policies were checked on synthetic examples, not calibrated
against a labeled history of this workspace. Confidence is not workflow correctness
or permission to act. Invalid or failed responses produce clearly labeled local
catalogs or unclassified output, never a replacement model. No model response
becomes a shell command, approval, recovery identity or destructive action.

API contract: [OpenRouter TypeSafe integration](https://openrouter.ai/docs/guides/community/typesafe-sdk).
For model semantics and limits, see [TypeSafe documentation](https://docs.typesafe.ai/llms.txt).

Validate a provider/config change with `tmux-workspace ai check`. The check sends
only a synthetic readiness sentence, not terminal output.

### Agent status

Every pane that runs an agent has one status at a time. A pane without an agent has no
status and shows no mark.

| Status | Meaning | Mark |
|---|---|---|
| blocked | The agent cannot continue without you. It waits for an approval or an answer, or its turn failed. | red dot |
| done | The agent finished a turn while you were not looking at its tab. | cyan dot |
| working | The agent is running a turn. | yellow dot |
| idle | The agent waits for a prompt and you have seen it. | green ring |

A pane that becomes blocked plays a sound, even in the tab you are looking at. A pane that
becomes done plays a different sound. Working and idle are silent. **`Ctrl+a` then `M`**
turns the sounds off and on, and the status line shows `muted` while they are off. The two
sound files and the player are the options `@agent_sound_blocked`, `@agent_sound_done` and
`@agent_sound_cmd` in `agent-status.conf`. The sound plays on the machine that runs tmux.

The marks appear in three places. The pane border shows the mark of its pane after the
label. Each tab shows its mark in front of its index and keeps its width. The workspace
mark follows the workspace name on the left of the status line, and a separator stands
between it and the first tab. The tab name and the pane label take the colour of the
status. An omp agent that was already running shows no mark until it restarts.

A tab shows the most urgent status among its panes. A workspace shows the most urgent
status among its tabs and its parked panes. The order is blocked, done, working, idle. A
tab or workspace without an agent shows no mark.

Done appears only when a turn finishes out of sight. A pane is in sight when its tab is the
current tab of a workspace that a terminal with focus is showing. A turn that finishes in
sight makes the pane idle. A turn that finishes out of sight makes the pane done. A tab on
screen in a terminal without focus counts as out of sight. A terminal that cannot report
focus counts as having focus.

Viewing a tab makes every done agent in it seen, and each of those panes becomes idle.
This holds for every pane of the tab and not only for the active pane. Viewing never clears
blocked and does not change working.

A parked agent counts toward its home workspace and is always out of sight. A parked agent
that is done keeps showing done until you bring its pane back into a tab.

A pane whose agent has exited shows no mark and no longer counts toward its tab or its
workspace. Snapshots do not save status. Status stays on this machine and never goes to a
model provider.

The pickers show the status as a coloured word. The workspace picker on `Ctrl+a` `s` shows
the status of each active workspace, and its parked agents count. The tab picker on
`Ctrl+a` `w` shows the status of each tab after the tab name. The parked chooser on
`Ctrl+a` `U` and `Ctrl+a` `B` shows the status of each parked pane. A row without an agent
shows no status word. The workspace picker searches only the name, so the status word
never changes which workspace it opens or creates. If a picker cannot read the status, it
lists its rows without status and works as before.

The marks have three limits.

- A pane hidden behind a zoomed pane of the same tab counts as seen, because seeing is per tab.
- Swapping tabs with `swap-window` does not make the done agents of a tab seen until the
  next tab switch or focus change.
- If an agent is killed and you then run a program other than the shell in the same pane,
  the old mark reappears until the next agent starts there.

omp reports its status from its extension events. A prompt makes the pane working. An
approval, a question from the `ask` tool, a plan that awaits approval and a failed turn
make it blocked. A provider retry counts as working. The pane stays working while a
background job or a subagent of omp is still running, and the turn counts as finished only
when all of them have stopped. A subagent never changes the status of the pane by itself.
An omp agent that is already running must restart before it uses the new extension.

The omp status has five limits.

- Some omp prompts fire no event and show working. They are the approval of a settings
  write, consent popups and dialogs that other extensions open.
- A long-lived background job, such as a dev server, keeps its pane working while it runs.
- If you dismiss a plan review without choosing, the pane stays blocked until the next
  prompt.
- An Esc during a provider retry also shows blocked until the next prompt.
- A plan that omp submits from inside its `eval` tool has not been observed. The pane may
  show idle while that plan review is open.

Claude Code reports its status from the state file that it writes for each interactive
session under `~/.claude/sessions`. Its status line calls the helper once a second, and the
helper reads that file. A running turn makes the pane working. A permission prompt and a
question make it blocked. A finished or interrupted turn follows the rule for done and
seen. Three hook entries cover what the file cannot. A permission prompt, a question and a
plan approval make the pane blocked at once, because Claude Code pauses its status line
while a prompt is open. A failed turn makes the pane blocked until the next prompt. A
subagent that needs your approval counts as blocked, and a nested `claude -p` never changes
the pane. The status is unset when Claude Code exits.

The Claude Code status has two limits.

- Claude Code does not document the state file. If an update changes it, Claude Code panes
  keep their last mark or show none until the helper is updated.
- The helper reads the file once a second, so a turn shorter than a second can pass without
  a working mark.

### Repeatable verification

From the project root, run the deterministic suite without provider access:

```sh
python3 -W error::ResourceWarning -m unittest discover -s tests -q
```

The suite includes `tests/test_agent_status_tmux.py`, which checks the agent status marks
on a scratch tmux server. That server uses its own socket and never loads `~/.tmux.conf`,
so the test cannot touch the running workspaces. Run it alone with this command:

```sh
python3 -m unittest discover -s tests -p 'test_agent_status_tmux.py'
```

`tests/test_agent_status.py` checks how the helper maps the state of Claude Code to a
status. It also checks the roll-up and the rows of the pickers. The hooks run the helper under `/usr/bin/python3`, so run that test under it too:

```sh
/usr/bin/python3 -B -m unittest discover -s tests -p 'test_agent_status.py'
```

The opt-in live suite uses the configured OpenRouter key and consumes API credit.
It sends synthetic fixtures only, not current pane output or real snapshot history:

```sh
RUN_LIVE_JEV=1 python3 tests/live_jev.py -v
```

The live suite covers 32 scenarios: independent attention conditions and negative
cases, English/Arabic input, prompt injection, semantic ranking, source-backed
shortcut/skill selection, and context-based names or abstention. These are
regression examples, not a guarantee of model accuracy on arbitrary terminal logs.
The deterministic suite covers malformed/provider failures, request bounds,
permission changes, history filesystem boundaries, and stale confirmed actions.
Interactive behavior still requires real tmux/fzf checks.

### Change record

- 2026-09-21: Consolidated workspace source and guide into one project; retained
  live paths as symlinks; added four opt-in Jev workflow tools through OpenRouter,
  confirmed context-based tab-name selection, and `Ctrl+a a`. Replaced the initial
  GPT implementation with native Jev primitives, batched decisions, and live typing-driven search.
- 2026-09-21: Expanded verification to 92 deterministic tests and an opt-in
  32-scenario live Jev suite. Fixed confirmation races, consent changes during
  credential lookup, boolean snapshot versions, oversized numeric decisions,
  HTTP error resource cleanup, and hidden finder status messages. Refined attention
  condition boundaries using live cases. Verified repeated real picker cancellation,
  confirmed rename/restore paths, source exclusion, and real HTTP 401 local fallback.
- 2026-09-22: Verified all five workflows through the installed `Ctrl+a a` popup,
  including origin-window routing and per-pane consent/revocation. Fixed the
  provider-check popup closing before its result could be read; success and failure
  now remain visible until Enter, while direct CLI checks remain nonblocking.
  Added menu/direct-command lifecycle coverage; all 93 deterministic tests pass.
- 2026-10-01: Added agent status marks. Each pane that runs an agent shows working,
  blocked, done or idle on its border, and tabs and the workspace name show the most
  urgent status among their agents. The formats and hooks moved from `tmux.conf` into
  `agent-status.conf`. Added a scratch-server test for the marks. The workspace picker,
  the tab picker and the parked chooser show the status as a word. Typing a name that
  matches no row in the workspace picker now creates that workspace.


## iTerm2 keyboard and scrolling setup

Run `bin/install-terminal-settings` from this project to install the **tmux workspace**
dynamic profile and merge the Codex hooks. The installer preserves unrelated hooks,
keeps backups under `~/.local/state/tmux-workspace/backups/terminal-*`, and can be rerun.
The profile inherits the appearance of **Default**. Select **Profiles > tmux workspace**
in iTerm2 to open a tab with it, then run `work`. Existing sessions keep their profile;
you do not need to stop their tmux processes. Set the profile as default in iTerm2's
profile settings if you want every new terminal to use it.

| Setting | Behavior |
|---|---|
| Left Option | Esc+, so Alt shortcuts reach zsh, tmux and editors |
| Right Option | Normal, for typing special characters |
| Option+Left / Right | Backward / forward word in shells with standard Emacs bindings |
| Shift+arrows | Sends distinct escape sequences; `Ctrl+a` then Shift+arrow resizes a pane |
| Mouse and wheel reporting | Enabled; tmux receives scrolling and border dragging |
| Terminal type | `xterm-256color` outside tmux; tmux provides `tmux-256color` inside |
| Terminal scrollback | 10,000 lines outside tmux; tmux retains 50,000 per pane |
| Alternate-screen scrollback | Off, so terminal history does not accumulate tmux screen redraws |
| Terminal bell | Visual indicator, without a second audible bell; tmux agent sounds remain enabled |

Scroll in a tmux pane to use its history. `Ctrl+a [` enters copy mode, `/` searches,
`v` begins a selection, `y` copies to macOS and `q` returns to live output.
Mouse-aware programs such as Neovim can consume the wheel themselves; use `Ctrl+a [`
to inspect tmux history explicitly. Do not use iTerm2's own scrollback to navigate tmux
history. `Cmd+v` pastes through the terminal. Hold Option while selecting if you want
iTerm2's native selection instead of tmux's mouse handling.

Keep iTerm2's **Natural Text Editing** preset off for this profile. Its remappings
can consume keys before tmux sees them. This profile supplies only word-navigation
and Shift+arrow mappings. macOS may still reserve Control+arrow shortcuts for Mission
Control; the prefix plus `h/j/k/l` and uppercase resize keys avoid those shortcuts.
This setup uses ordinary `tmux`, not `tmux -CC` control mode. Keep splits and workspace
tabs inside tmux so its labels, parking, recovery and shortcuts describe the same layout.

Reload with `Ctrl+a r`. The terminal title now shows the workspace and window.
The tmux status line shows `COPY`, `ZOOM`, sound mute and `save ERROR` when applicable.
Repeated reloads do not append duplicate terminal capability entries.

## Codex notifications and diagnostics

### Ghostty and performance

Ghostty can use its default configuration. The tmux configuration enables true color,
clipboard, focus events, hyperlinks, styled underlines and synchronized redraws for
`xterm-ghostty`. After reloading tmux, detach and reattach once so the existing client
negotiates the additional capabilities. `tmux-workspace doctor` checks the active
Ghostty client. Escape-key disambiguation is 10 ms. Ghostty VSync remains at its default;
disabling it can increase CPU use under load.

The performance pass on 2026-10-12 measured these warm picker preparation times on the
live 41-pane server. These exclude Python process startup, drawing and human input.
Each picker measurement used 15 samples; p95 uses the nearest-rank method.

| Operation | Before median / p95 | After median / p95 |
| --- | --- | --- |
| Workspace picker | 65.3 / 97.0 ms | 43.2 / 54.0 ms |
| Tab picker | 170.6 / 193.9 ms | 25.1 / 40.9 ms |
| Shell initialization, inherited Node version | 1,038 ms median | 437 ms median |

Run `python3 tests/benchmark_workspace.py` from this project to repeat the picker
measurement without displaying a picker or switching workspaces. Measurements vary
with other workloads. The structural regression tests do not depend on wall-clock time.

For W tabs, tab-picker subprocess cost changed from W + 3 tmux reads to 2. The data
still takes O(P) work for P panes; it is fetched together rather than through W child
processes. Workspace-picker subprocesses dropped from 5 to 3. Snapshot metadata is
also fetched together, and fallback identity resolution shares one process table.
High-frequency Claude hooks inspect only their ancestor chain, O(depth), instead of
all system processes. Detached Codex hooks inspect only the matched terminal's processes.
Native executables no longer trigger extra argument lookups just to identify their type.

Autosave runs at lower CPU priority. An agent identity lookup failure is recorded as
an explicit recovery error for that pane while layout and history can still be saved.
A failed history capture leaves the previous complete snapshot selected. A real save
of 40 panes completed in 3.3 seconds with no recovery errors after this change.
Codex recovery resolves its current native thread title against a read-only state
database, including versions where the shared daemon owns that database. It never
chooses a thread by recency and rejects ambiguous title prefixes.

The local `~/.zshrc` loads NVM with `--no-use` only when its selected Node executable
is already inherited correctly. Fresh shells without that selection retain NVM's
normal initialization. The prior file is backed up under
`~/.local/state/tmux-workspace/backups/shell-1791754909830145000/zshrc`.
New shells pick up this change automatically.

At measurement time the host was heavily loaded, with simulator, virtual-machine and
agent processes running. Workspace changes do not stop those jobs. Use `Ctrl+a C` to
review terminals before closing them; a high memory estimate is not proof that a job
is disposable.

### Notifications and cleanup

`Ctrl+a C` opens terminal cleanup. It lists inactive or unverified panes, including
parked panes, sorted by estimated process RSS. Tab selects panes, Enter reviews the
selection, and typing `close` terminates them. Working and blocked agents are excluded.
An idle or unverified status is a review candidate, not proof that a job is safe to kill.
Processes and status are checked again before closure. RSS can count shared pages more
than once and excludes detached shared daemons; it is not a promise of memory reclaimed.
The command `tmux-workspace cleanup` prints the list when run without a terminal.

Detached Codex app-server hooks are routed by the native `codex | <thread-id>` title
and a verified foreground Codex process. Keep the thread ID in the Codex terminal title.
Missing or ambiguous titles are ignored rather than updating an unrelated pane.

Codex now uses native `SessionStart`, `UserPromptSubmit`, `PreToolUse`,
`PermissionRequest`, `PostToolUse`, `Stop`, `Interrupt` and `SessionEnd` hooks.
The registrations are in `integrations/codex-hooks.json`; the shared helper is
`integrations/agent-hook.py`. The existing Codex `notify` computer-use integration
is preserved.

After installation, open **`/hooks` in Codex and review/trust the new `agent-hook.py codex`
entries**. Codex requires review of new hook definitions before running them. The
installer does not change trusted hashes or bypass that review. Restart/resume existing
Codex CLIs to load the registrations. See the [official hook documentation](https://learn.chatgpt.com/docs/hooks#review-and-trust-hooks).

A new prompt reports working. An approval or `request_user_input` question reports
blocked and plays the existing blocked sound. Completion reports done and plays the
completion sound when the tab is out of focus; completion in a focused tab reports idle.
Interrupt reports idle, and session exit clears the status. Compaction keeps the current
turn. Unrelated tool completions cannot clear an outstanding approval. A newer turn
ignores late events from the previous turn. Nested CLI processes and hooks with an
`agent_id` cannot claim the pane's identity.

Hooks never print approval decisions or read transcripts. The pane stores only native
event metadata and, where needed, hashes of tool arguments to match an approval to its
completion. No prompt, response, terminal content or raw tool arguments are stored.
A `Stop` event means the stop hooks are running; another stop hook may request another
turn. Working resumes on the next prompt or tool event. Codex errors that emit no Stop
or Interrupt event can leave the last status until the next event. These are native
hook limits, not inferred completion from silence.

`Ctrl+a D` opens local diagnostics; press `q` to close it. The same report is available
with `tmux-workspace doctor` and exits nonzero when it finds warnings. It checks tool
paths, live settings, hook registrations, Ghostty capabilities, sound files and snapshot
age. It lists terminal focus and each pane's command/status, with the last Codex event
and its age. It does not read terminal output or invoke an AI provider. Hook installation
is not proof of trust; `/hooks` remains the authority for whether Codex runs them.

To undo the iTerm2 changes, select your original **Default** profile and remove the
`~/Library/Application Support/iTerm2/DynamicProfiles/tmux-workspace.json` symlink.
To undo Codex registration, remove only the `agent-hook.py codex` entries from
`~/.codex/hooks.json`. A full backup restore is appropriate only if you made no later
hook edits. The installer prints the backup directory it created.
