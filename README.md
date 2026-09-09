# tmux-workspace

> A resilient, full-featured tmux workspace setup with smart project management (**sesh** + **zoxide**), selective pane parking, interactive fuzzy help (**fzf**), and automatic reboot recovery with exact conversation tracking for **Claude Code**, **OpenAI Codex**, **Oh My Pi**, and **Neovim**.

---

## Quick Install (New Machine)

Clone this repository and run the automated installer:

```bash
git clone https://github.com/ahmmkh/tmux-workspace.git ~/.config/tmux-workspace
~/.config/tmux-workspace/install.sh
```

The installer is idempotent and will:
1. Check for prerequisites (`tmux`, `fzf`, `zoxide`, `sesh`, `python3`) and offer to install missing tools via Homebrew.
2. Back up any existing configuration files into `~/.local/state/tmux-workspace/backups/`.
3. Install the configuration files, CLI controller, and lifecycle hooks.
4. Set up shell navigation (`work`, zoxide integration) in `~/.zshrc`.
5. Connect Neovim and terminal AI session recovery hooks.

---

## Core Features

### 1. Smart Project Workspaces (`work`)
- **Fuzzy Picker**: Run `work` (or press `Ctrl+a` then `s` inside tmux) to fuzzy-find and switch between live sessions, project directories, and frequent zoxide paths.
- **Auto Project Folder**: Run `work my-new-project`. If `~/untitled folder/Projects/my-new-project` does not exist, it is **automatically created**, the tmux session is started in that directory, and all subsequent panes/splits default to it.
- **Zoxide First**: Directory resolution is powered by zoxide (`cd <partial>`, `work <partial>`).
- **Close & Delete**: Run `work rm [name]` or press `Ctrl+a X` inside tmux to close a workspace with confirmation (automatically switches to another session before terminating).

### 2. Ergonomic Splits & Rotation
- **Prefix Key**: **`Ctrl+a`** (press `Ctrl+a Ctrl+a` to send raw `Ctrl+a` through).
- **Vertical Split**: `Ctrl+a |` (or `%`).
- **Horizontal Split**: `Ctrl+a -` (or `"`).
- **Name Tabs First**: Pressing `Ctrl+a c` prompts for a tab name in the status line, then opens the tab with that name and automatically labels the initial pane.
- **Repeatable Navigation**: Move between splits using `Ctrl+a` then `h/j/k/l` or arrows (tap repeatedly within 800ms without re-pressing `Ctrl+a`).
- **Fast Resizing**: `Ctrl+a` then `H/J/K/L` (repeatable) or `Shift + Arrows`, plus mouse border dragging.
- **Zoom Toggle**: `Ctrl+a z` expands the active pane to full screen; repeat to restore splits.
- **Rotate Layouts**: `Ctrl+a Space` cycles through preset split arrangements.

### 3. Selective Pane Parking (Hide Splits)
Need to hide a running server or log watcher without killing it?
- **Park Pane**: Press **`Ctrl+a !`** on any focused split. It disappears from your window and continues running silently in a hidden holding session (`__parked`).
- **Restore Latest Right**: Press **`Ctrl+a u`**.
- **Restore Latest Below**: Press **`Ctrl+a b`**.
- **Interactive Chooser**: Press **`Ctrl+a U`** (restore right) or **`Ctrl+a B`** (restore below) to open a popup menu of all parked panes and select which one to restore.

### 4. Interactive Fuzzy Search Help (`Ctrl+a ?`)
- Press **`Ctrl+a ?`** anywhere in tmux to open an interactive fuzzy search dropdown powered by `fzf`.
- As you type, a side panel displays live in-depth documentation, syntax, prompt behaviors, and related shortcuts.
- Press **Enter** to open the full topic guide in a scrollable pager (`less -R`).
- Press **Esc** or **q** to close and return immediately to your work.

### 5. Automatic Reboot Recovery
- **5-Minute Snapshots**: Automatically saves all workspaces, windows, split geometries, pane labels, and sanitized scrollbacks every 5 minutes (or manually via `Ctrl+a Ctrl+s`).
- **Private Storage**: Saved locally in `~/.local/state/tmux-workspace/` with strict `0700` directory and `0600` file permissions.
- **Claude Code**: Resumes exact active conversation via `claude --resume <exact-id>` without replaying prompts.
- **OpenAI Codex**: Resumes exact active thread via `codex resume <exact-id>` without replaying prompts.
- **Oh My Pi**: Resumes exact active session file via `omp --resume <exact-file>`.
- **Neovim**: Resumes tabs, splits, open buffers, and cursor positions via native `mksession`. Source files on disk are **never** auto-written; swapfile recovery protects unsaved edits.
- **Other Programs**: Servers, logs, and background scripts restore as clean interactive shells in their directory with their label and previous terminal text displayed.
- **Ordinary Commands**: Works when launching tools directly from ordinary shell prompts, including switching agents in the same pane.

---

## Keybindings Cheatsheet

| Key | Action |
|---|---|
| `Ctrl+a` `?` | **Open interactive fuzzy search help card** |
| `Ctrl+a` `s` | Open workspace fuzzy picker (switch or create new) |
| `Ctrl+a` `Tab` | Bounce to previous workspace |
| `Ctrl+a` `X` | Close / delete current workspace (with confirmation) |
| `Ctrl+a` `\|` or `%` | Split vertically (left / right) |
| `Ctrl+a` `-` or `"` | Split horizontally (top / bottom) |
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
| `Ctrl+a` `d` | Detach session (leaves everything running) |
| `Ctrl+a` `Ctrl+s` | Save snapshot now |
| `Ctrl+a` `Ctrl+r` | Restore missing workspaces |
| `Ctrl+a` `P` | View saved scrollback history for pane |
| `Ctrl+a` `[` | Enter scroll/copy mode (`q` exits) |
| `Ctrl+a` `]` | Paste buffer |
| `Ctrl+a` `r` | Reload tmux configuration |
| `Ctrl+a` `Ctrl+a` | Send raw Ctrl+a to shell/editor |

---

## File Structure

```text
tmux-workspace/
├── README.md                  # This guide
├── install.sh                 # Idempotent automated installer
├── WORKSPACE_GUIDE.md         # Full 12-section offline manual
├── tmux/
│   └── tmux.conf             # Core tmux options, bindings, status line
├── bin/
│   └── tmux-workspace        # Main CLI controller (python3)
├── lib/
│   └── agent-hook.py         # AI session tracking hooks
├── nvim/
│   └── tmux_workspace.lua    # Neovim auto-session persistence
├── omp/
│   └── tmux-workspace.ts     # Oh My Pi agent extension
└── sesh/
    └── sesh.toml             # sesh workspace discovery configuration
```

---

## License

MIT
