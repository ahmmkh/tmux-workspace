# TMUX WORKSPACE COMPLETE GUIDE

This document is your permanent offline reference for your personalized tmux workspace setup.
It covers core concepts, everyday shortcuts, advanced parking workflows, sesh workspace management, and how reboot recovery works with AI tools and Neovim.

## Start here

All workspace-specific source now lives in:
`~/untitled folder/Projects/tmux-workspace/`

- Open the project: `work tmux-workspace`
- Open all AI tools: **`Ctrl+a`, release, then `a`**
- Read this guide: `less "$HOME/untitled folder/Projects/tmux-workspace/WORKSPACE_GUIDE.md"`
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
- **Sesh ranking**: `sesh.toml` prioritizes zoxide at the top: `sort_order = ["zoxide", "tmux", "config"]`.

### Switching & Deleting Workspaces from Inside tmux:
- **`Ctrl+a` then `s`**: Opens the workspace picker popup over your screen. Select an existing workspace or type a new name to create one on the fly.
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

---

## 7. Selective Parking (Hide & Restore Splits)

Parking is a feature built specifically for hiding splits (like long-running server logs or watchers) while keeping their processes running in the background.

### Parking a Pane
- Focus the pane you want to hide.
- Press **`Ctrl+a` then `!`**.
- The pane disappears from your visible window and its space is reclaimed by sibling panes.
- The process in that pane **continues running** uninterrupted in a hidden holding session (`__parked`).

### Restoring Parked Panes
- **Restore the most recently parked pane:**
  - **`Ctrl+a` then `u`**: Restores the pane to the **right** of your current pane.
  - **`Ctrl+a` then `b`**: Restores the pane **below** your current pane.

- **Interactive Chooser (when multiple panes are parked):**
  - **`Ctrl+a` then `U`**: Opens a popup listing all parked panes with their labels and origin workspaces. Type the number and hit Enter to restore that pane to the **right**.
  - **`Ctrl+a` then `B`**: Opens the popup chooser and restores the chosen pane **below**.

---

## 8. Windows (Tabs/Views within a Project)

Windows let you organize different concerns inside the same project workspace:

- **`Ctrl+a` then `c`**: Create a new window / tab. Prompts you to name the tab first in the status bar (`Tab name: `), sets the tab name, and automatically labels the initial pane with that name so you can start working immediately (defaults to `tab` if left blank; press `Esc` to cancel).
- **`Ctrl+a` then `,`**: Rename the current window.
- **`Ctrl+a` then `1` .. `9`**: Jump directly to window 1 through 9.
- **`Ctrl+a` then `w`**: **Window Fuzzy Picker**. Opens an interactive search popup listing all windows (tabs) in your current workspace with live terminal screen previews and running pane summaries. Filter by index, name, or pane label and press Enter to switch immediately.
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
| `bin/tmux-workspace` | `~/.local/bin/tmux-workspace` symlink; existing controller and AI entrypoint |
| `lib/ai.py`, `lib/ai_core.py` | AI popup, permission controls and shared OpenRouter client |
| `lib/panes_ai.py` | Semantic pane finder and evidence-backed attention list |
| `lib/history_ai.py` | Opt-in snapshot search |
| `lib/suggest_ai.py` | Source-backed shortcut and installed-skill suggestions |
| `config.json` | Model and Doppler project/config/scope; no API key |
| `shell.zsh` | Sourced by `~/.zshrc`; `work` and zoxide integration |
| `sesh.toml` | `~/.config/sesh/sesh.toml` symlink |
| `integrations/agent-hook.py` | `~/.local/lib/tmux-workspace/agent-hook.py` symlink |
| `integrations/tmux-workspace.ts` | `~/.omp/agent/extensions/tmux-workspace.ts` symlink |
| `integrations/tmux_workspace.lua` | `~/.config/nvim/lua/custom/tmux_workspace.lua` symlink |
| `WORKSPACE_GUIDE.md` | `~/.config/tmux/WORKSPACE_GUIDE.md` symlink; this guide |

Shared application registrations stay in their owning configurations:
`~/.claude/settings.json` invokes the agent-hook path above for session hooks/status;
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

### Repeatable verification

From the project root, run the deterministic suite without provider access:

```sh
python3 -W error::ResourceWarning -m unittest discover -s tests -q
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
