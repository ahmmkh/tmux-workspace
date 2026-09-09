# TMUX WORKSPACE COMPLETE GUIDE

This document is your permanent offline reference for your personalized tmux workspace setup.
It covers core concepts, everyday shortcuts, advanced parking workflows, sesh workspace management, and how reboot recovery works with AI tools and Neovim.

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
- **`Ctrl+a` then `w`**: Opens tmux's interactive tree view of sessions and windows.
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

### Active Configuration Files
| File Path | Description |
|---|---|
| `~/.tmux.conf` | Core tmux options, prefix, keybindings, status line, hooks |
| `~/.local/bin/tmux-workspace` | Python controller managing snapshots, labels, parking, and recovery |
| `~/.local/lib/tmux-workspace/agent-hook.py` | Native hooks tracking Claude, Codex, and OMP session IDs |
| `~/.config/nvim/lua/custom/tmux_workspace.lua` | Neovim auto-session persistence module |
| `~/.omp/agent/extensions/tmux-workspace.ts` | Oh My Pi global agent extension for session tracking |
| `~/.config/sesh/sesh.toml` | sesh workspace discovery configuration |
| `~/.zshrc` | Shell integration (`work` function and zoxide init) |

### Backups Location
Your original configuration files before this setup were preserved in:
`~/.local/state/tmux-workspace/backups/`

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
| `Ctrl+a` `X` | Close current workspace (with confirmation) |
| `Ctrl+a` `d` | Detach session (leaves everything running) |
| `Ctrl+a` `Ctrl+s` | Save snapshot now |
| `Ctrl+a` `Ctrl+r` | Restore missing workspaces |
| `Ctrl+a` `P` | View saved scrollback history for pane |
| `Ctrl+a` `[` | Enter scroll/copy mode (`q` exits) |
| `Ctrl+a` `]` | Paste buffer |
| `Ctrl+a` `r` | Reload tmux configuration |
| `Ctrl+a` `Ctrl+a` | Send raw Ctrl+a to shell/editor |
