//! The help topics, generated from the Python controller's HELP_TOPICS by scripts/gen_help.py.
//! Do not edit by hand: change the topics there and run the script again.

use crate::help::Topic;

pub const TOPICS: &[Topic] = &[
    Topic {
        id: r##"dashboard"##,
        display: r##"Ctrl+a A             Agent and workspace dashboard"##,
        title: r##"Dashboard"##,
        text: r##"SHORTCUT: Ctrl+a then A (capital A)

DESCRIPTION:
  One table of every pane in every workspace, with its agent and live status.
  Workspaces with a blocked agent come first, then done, working and idle.
  Parked panes appear under the workspace they came from.

KEYS:
  j k ↓ ↑     move            enter   jump to the pane
  /           filter          a       only panes with an agent
  p / u       park / unpark   r       rename the label
  x           close the pane  ?       help, q or esc closes

  From a shell: tmux-workspace dash, or tmux-workspace dash --once for one frame."##,
    },
    Topic {
        id: r##"cleanup"##,
        display: r##"Ctrl+a C             Review inactive terminals and close selected panes"##,
        title: r##"Terminal cleanup"##,
        text: r##"Tab selects panes; Enter reviews; type close to terminate the selected terminals.
Working and blocked agents are excluded. Unverified panes may still run jobs.
Memory is estimated process RSS, excluding shared detached daemons.
Closing a terminal can lose unsaved work; nothing closes automatically."##,
    },
    Topic {
        id: r##"ai-tools"##,
        display: r##"Ctrl+a a             Jev Live Finder, Attention, Skills & Tab Names"##,
        title: r##"Fast Jev Workspace Tools"##,
        text: r##"SHORTCUT: Ctrl+a then a

Choose find, attention, search-history, suggest, or name-tab.
Find reranks as you type after a 250ms pause; empty query stays local.
Search and suggestions select existing sources. One Choice selects a safe existing
phrase for a tab name; confirmation is required before renaming.
Jev supplies fast typed decisions through OpenRouter; code owns every action.

PRIVACY:
Metadata-only by default. The share menu explicitly permits recent output AND
retained history for one pane. Revoke there or with:
  tmux-workspace ai share %PANE --off
Permission resets when tmux restarts. Redaction is not a guarantee.

UNIFIED HOME:
  ~/Projects/tmux-workspace
  work tmux-workspace
Edit config.json for model and Doppler settings; no secret is stored there.
Run tmux-workspace ai check to verify credentials and structured output."##,
    },
    Topic {
        id: r##"split-v"##,
        display: r##"Ctrl+a | (or %)       Split Pane Vertically (Left / Right)"##,
        title: r##"Vertical Split — Left / Right"##,
        text: r##"SHORTCUT: Ctrl+a then | (or %)

DESCRIPTION:
  Splits the active pane vertically into two side-by-side panes (left and right).
  The new pane automatically opens in the current directory of the active pane.

PROMPT:
  As soon as the split is created, a prompt appears:
    Pane label [shell]: 
  Type a label (e.g. 'editor', 'server', 'logs') or hit Enter for 'shell'.
  This label is displayed on the top border of the pane.

RELATED COMMANDS:
  Ctrl+a -        Horizontal split (top / bottom)
  Ctrl+a t        Rename active pane label
  Ctrl+a h/l      Move focus left or right
  Ctrl+a H/L      Resize split boundary left or right"##,
    },
    Topic {
        id: r##"split-h"##,
        display: r##"Ctrl+a - (or ")       Split Pane Horizontally (Top / Bottom)"##,
        title: r##"Horizontal Split — Top / Bottom"##,
        text: r##"SHORTCUT: Ctrl+a then - (or ")

DESCRIPTION:
  Splits the active pane horizontally into stacked panes (top and bottom).
  The new pane inherits the current working directory and prompts for a label.

PROMPT:
  Pane label [shell]: 
  Type a label or hit Enter.

RELATED COMMANDS:
  Ctrl+a |        Vertical split (left / right)
  Ctrl+a j/k      Move focus down or up
  Ctrl+a J/K      Resize split boundary down or up"##,
    },
    Topic {
        id: r##"park"##,
        display: r##"Ctrl+a !              Park Pane (Hide split, keep running in background)"##,
        title: r##"Park Pane — Background Execution"##,
        text: r##"SHORTCUT: Ctrl+a then !

DESCRIPTION:
  Temporarily hides the active pane to free up screen space, while keeping its
  process RUNNING uninterrupted in a hidden background session (__parked).
  Sibling panes immediately expand to reclaim the freed space.

USE CASES:
  - Running a development server or long build without cluttering your view
  - Watching logs that you only need to inspect periodically
  - Parking a test watcher while coding full-screen

RESTORING:
  Ctrl+a u        Restore most recently parked pane to the RIGHT
  Ctrl+a b        Restore most recently parked pane BELOW
  Ctrl+a U        Interactive popup chooser: pick which parked pane to restore RIGHT
  Ctrl+a B        Interactive popup chooser: pick which parked pane to restore BELOW

AGENT STATUS:
  A parked agent counts toward the status of its home workspace. A parked
  agent that is done keeps showing done until the pane comes back."##,
    },
    Topic {
        id: r##"unpark-latest-right"##,
        display: r##"Ctrl+a u              Restore Latest Parked Pane (to Right)"##,
        title: r##"Restore Latest Parked Pane to the Right"##,
        text: r##"SHORTCUT: Ctrl+a then u

DESCRIPTION:
  Takes the most recently parked pane from background storage and splits the
  current window to restore it to the RIGHT of your focused pane.
  The running process is preserved exactly as it was."##,
    },
    Topic {
        id: r##"unpark-latest-below"##,
        display: r##"Ctrl+a b              Restore Latest Parked Pane (Below)"##,
        title: r##"Restore Latest Parked Pane Below"##,
        text: r##"SHORTCUT: Ctrl+a then b

DESCRIPTION:
  Takes the most recently parked pane and restores it BELOW your focused pane.
  Process state, working directory, and label are fully preserved."##,
    },
    Topic {
        id: r##"choose-park-right"##,
        display: r##"Ctrl+a U              Choose & Restore Parked Pane (to Right)"##,
        title: r##"Choose & Restore Parked Pane (Right)"##,
        text: r##"SHORTCUT: Ctrl+a then U (capital U)

DESCRIPTION:
  Opens a popup menu displaying all currently parked panes:
    1. logs  [my-project]
    2. agent  [my-project]  done
  A parked pane that runs an agent shows its status word after the workspace.
  A parked pane without an agent shows no status word.
  Type the number of the pane you wish to restore and hit Enter.
  The selected pane is attached to the RIGHT of your active pane."##,
    },
    Topic {
        id: r##"workspace-picker"##,
        display: r##"Ctrl+a s (or work)       Workspace Picker (Switch or Create New)"##,
        title: r##"Workspace Picker & Creator"##,
        text: r##"SHORTCUT: Ctrl+a then s, or `work` from your shell

DESCRIPTION:
  Opens an interactive fuzzy selector listing:
  - Active tmux sessions (marked with ●)
  - Existing project folders in ~/Projects/ (marked with 📁)
  - Recent zoxide paths (marked with ⚡)

AGENT STATUS:
  The row of an active workspace shows the most urgent status among its
  agents as a coloured word. The words are blocked, done, working and idle.
  Parked agents count toward their home workspace. A workspace without an
  agent shows no status word. The ● still marks an active workspace.
  The search matches only the name. It never matches the status word.

SWITCHING & CREATING:
  - Select any item to switch/connect to it.
  - Or type a BRAND NEW project name (e.g. 'my-api') and hit Enter:
    Automatically creates `~/Projects/<name>`, sets up the
    session, and connects immediately!
    Enter creates the workspace only when the typed name matches no row.
    The status word never becomes part of a workspace name.

SHELL USAGE:
  work                  Fuzzy picker (switch or type new name to create)
  work my-project       Directly connect to or create my-project
  work .                Open workspace for current directory
  work rm [name]        Delete / close a workspace"##,
    },
    Topic {
        id: r##"workspace-last"##,
        display: r##"Ctrl+a Tab            Bounce to Previous Workspace"##,
        title: r##"Jump to Previous Workspace"##,
        text: r##"SHORTCUT: Ctrl+a then Tab

DESCRIPTION:
  Seamlessly bounces back and forth between your two most recently used workspaces.
  Powered by `sesh last`."##,
    },
    Topic {
        id: r##"nav-vim"##,
        display: r##"Ctrl+a h/j/k/l        Move Focus (Left / Down / Up / Right)"##,
        title: r##"Directional Movement (Vim keys)"##,
        text: r##"SHORTCUT: Ctrl+a then h / j / k / l

DESCRIPTION:
  Moves focus to the adjacent pane:
    h: Left
    j: Down
    k: Up
    l: Right

REPEATABLE:
  You only need to press Ctrl+a once; you can tap h, j, k, l repeatedly
  within 800ms to jump across multiple splits quickly."##,
    },
    Topic {
        id: r##"nav-arrows"##,
        display: r##"Ctrl+a Arrows         Move Focus (Arrow keys)"##,
        title: r##"Directional Movement (Arrow keys)"##,
        text: r##"SHORTCUT: Ctrl+a then Left / Down / Up / Right

DESCRIPTION:
  Moves focus using standard arrow keys. Also repeatable."##,
    },
    Topic {
        id: r##"resize-keys"##,
        display: r##"Ctrl+a H/J/K/L        Resize Active Pane (Repeatable)"##,
        title: r##"Pane Resizing"##,
        text: r##"SHORTCUT: Ctrl+a then H / J / K / L (or Shift+Arrows)

DESCRIPTION:
  Grows or shrinks the active pane:
    H: Grow Left (5 cells)
    J: Grow Down (3 cells)
    K: Grow Up (3 cells)
    L: Grow Right (5 cells)

REPEATABLE & MOUSE:
  Hold or tap repeatedly to adjust size quickly.
  You can also simply click and drag any pane border line with your mouse."##,
    },
    Topic {
        id: r##"zoom"##,
        display: r##"Ctrl+a z              Toggle Zoom (Full Screen Split)"##,
        title: r##"Zoom Pane"##,
        text: r##"SHORTCUT: Ctrl+a then z

DESCRIPTION:
  Maximizes the focused pane to take up the entire window.
  Other panes remain running in the background.
  Press Ctrl+a then z again to restore the exact original split layout.

LARGER TEXT IN GHOSTTY:
  Cmd + plus (or equals): enlarge text. Cmd + minus: shrink. Cmd + 0: reset.
  Font size affects the entire terminal. Zoom the pane first for focused reading."##,
    },
    Topic {
        id: r##"label-pane"##,
        display: r##"Ctrl+a t              Rename Active Pane Label"##,
        title: r##"Rename Pane Label"##,
        text: r##"SHORTCUT: Ctrl+a then t

DESCRIPTION:
  Opens an on-screen popup asking for a new pane label:
    Pane label [current]:
  The label is displayed in the top border of the pane and persists across
  parking, unparking, and reboot recovery snapshots.
  The pane border shows the agent status mark after the label."##,
    },
    Topic {
        id: r##"rotate-layout"##,
        display: r##"Ctrl+a Space          Rotate Through Split Layouts"##,
        title: r##"Rotate Layouts"##,
        text: r##"SHORTCUT: Ctrl+a then Space

DESCRIPTION:
  Cycles the current window through preset split geometries:
  - even-horizontal
  - even-vertical
  - main-horizontal
  - main-vertical
  - tiled"##,
    },
    Topic {
        id: r##"new-window"##,
        display: r##"Ctrl+a c              New Window (Tab/View)"##,
        title: r##"Create New Window (Tab)"##,
        text: r##"SHORTCUT: Ctrl+a then c

DESCRIPTION:
  Prompts you to name the tab first in the status bar:
    Tab name: <type name>
  Creates a new full-screen window (tab) inside the current workspace session
  with that name.
  Inherits the current pane's working directory, and automatically sets the
  initial pane's label to match your tab name so you can get to work immediately.
  Pressing Esc cancels creation; pressing Enter with no text defaults to 'tab'."##,
    },
    Topic {
        id: r##"switch-window"##,
        display: r##"Ctrl+a 1..9 (or n/p)   Switch Windows"##,
        title: r##"Switch Windows"##,
        text: r##"SHORTCUT: Ctrl+a then 1..9, or n (next) / p (previous)

DESCRIPTION:
  Jumps directly to window 1 through 9, or moves forward/backward between windows."##,
    },
    Topic {
        id: r##"pick-window"##,
        display: r##"Ctrl+a w              Window Fuzzy Picker (Current Project)"##,
        title: r##"Window Fuzzy Picker (Tabs within Project)"##,
        text: r##"SHORTCUT: Ctrl+a then w

DESCRIPTION:
  Opens an interactive fuzzy search popup listing all windows (tabs) in your
  CURRENT project workspace.

FEATURES:
  - Lists window index, tab name, pane count, and running pane labels
  - Shows the agent status word of each tab after the tab name. A tab
    without an agent shows no status word.
  - Live preview pane shows the layout, panes, and active screen of each tab
  - Type to filter by index, window name, or pane label
  - Press Enter to jump immediately to the selected window
  - Press Esc to cancel

RELATED:
  Ctrl+a W (capital W)   Open global tmux session & window tree
  Ctrl+a s               Open project workspace picker
  Ctrl+a 1..9            Jump directly to window 1..9"##,
    },
    Topic {
        id: r##"detach"##,
        display: r##"Ctrl+a d              Detach From Workspace (Keep running)"##,
        title: r##"Detach From Tmux"##,
        text: r##"SHORTCUT: Ctrl+a then d

DESCRIPTION:
  Disconnects from the tmux server and returns you to your standard terminal.
  All your workspaces, windows, background processes, AI sessions, and Neovim
  editors CONTINUE RUNNING untouched.
  Re-attach anytime using the `work` command."##,
    },
    Topic {
        id: r##"close-workspace"##,
        display: r##"Ctrl+a X (or work rm)   Close / Delete Workspace (With confirmation)"##,
        title: r##"Close / Delete Workspace"##,
        text: r##"SHORTCUT: Ctrl+a then X (capital X), or `work rm [name]` from shell

DESCRIPTION:
  Closes and terminates a project workspace:
  - From inside tmux: Press Ctrl+a then X. Prompts confirmation:
      Close workspace <name>? (y/n)
    Automatically switches you to another active workspace before closing so you
    remain inside tmux.
  - From your shell:
      work rm             Interactive fuzzy picker of active workspaces to delete
      work rm <name>      Closes the specified workspace directly"##,
    },
    Topic {
        id: r##"save-snapshot"##,
        display: r##"Ctrl+a Ctrl+s         Save Workspace Snapshot Now"##,
        title: r##"Save Snapshot Manually"##,
        text: r##"SHORTCUT: Ctrl+a then Ctrl+s

DESCRIPTION:
  Immediately captures a full snapshot of all workspaces, windows, split layouts,
  pane labels, sanitized terminal history, and active AI / Neovim session IDs.
  Snapshots also run automatically every 5 minutes while tmux is running."##,
    },
    Topic {
        id: r##"restore-snapshot"##,
        display: r##"Ctrl+a Ctrl+r         Restore Workspaces From Snapshot"##,
        title: r##"Restore Missing Workspaces"##,
        text: r##"SHORTCUT: Ctrl+a then Ctrl+r

DESCRIPTION:
  Reads the latest saved snapshot and reconstructs any missing workspaces,
  windows, splits, and terminal scrollbacks, reconnecting active AI tools and Neovim."##,
    },
    Topic {
        id: r##"view-history"##,
        display: r##"Ctrl+a P              View Saved Terminal Scrollback (Pager)"##,
        title: r##"View Saved Terminal Scrollback"##,
        text: r##"SHORTCUT: Ctrl+a then P (capital P)

DESCRIPTION:
  Opens the saved terminal scrollback for the current pane in `less`.
  Lets you search and read historical terminal text captured in the last snapshot."##,
    },
    Topic {
        id: r##"copy-mode"##,
        display: r##"Ctrl+a [ (or Enter)   Copy Mode & System Clipboard"##,
        title: r##"Copy Mode & Clipboard"##,
        text: r##"SHORTCUT: Ctrl+a then [ or Enter (or scroll trackpad/mouse)

DESCRIPTION:
  Enters scrollback copy mode:
  - Scroll with j/k, arrow keys, or mouse wheel (3 lines per notch)
  - Search with / or ? (press n for next, N for previous)
  - v selects characters, V whole lines, Ctrl+v toggles a rectangle
  - H / L jump to the start / end of the line
  - y copies the selection, Y copies to the end of the line (system clipboard)
  - Mouse: drag to copy; double-click copies a word, triple-click a line
  - Press q or Esc to exit copy mode
  - Press Ctrl+a then ] to paste the system clipboard into the pane
  - tw-clip picks pbcopy, wl-copy, xclip or xsel, and falls back to OSC 52 over SSH"##,
    },
    Topic {
        id: r##"popup-tools"##,
        display: r##"Ctrl+a g / e          Lazygit / Scratch Shell Popup (Pane Directory)"##,
        title: r##"Floating Tools"##,
        text: r##"SHORTCUTS: Ctrl+a then g (lazygit), Ctrl+a then e (scratch shell)

DESCRIPTION:
  Opens a floating popup in the focused pane's directory:
  - g runs lazygit for the repository you are in
  - e opens a throwaway zsh for a quick command without splitting the tab
  The popup closes when its program exits (q in lazygit, exit or Ctrl+d in the shell).
  Popups are not panes: they never join a snapshot and are not restored."##,
    },
    Topic {
        id: r##"sync-panes"##,
        display: r##"Ctrl+a y              Toggle Typing Into All Panes of the Tab"##,
        title: r##"Synchronize Panes"##,
        text: r##"SHORTCUT: Ctrl+a then y

DESCRIPTION:
  Sends every keystroke to all panes of the current tab at once, e.g. to run the
  same command on several servers. The status bar shows SYNC while it is on.
  Press Ctrl+a then y again to turn it off."##,
    },
    Topic {
        id: r##"move-tab"##,
        display: r##"Ctrl+a < / >          Move Current Tab Left / Right (Repeatable)"##,
        title: r##"Reorder Tabs"##,
        text: r##"SHORTCUT: Ctrl+a then < or > (hold to repeat)

DESCRIPTION:
  Swaps the current tab with its neighbour and keeps it selected, so tab numbers
  (Ctrl+a 1..9) follow the order you want."##,
    },
    Topic {
        id: r##"agent-status"##,
        display: r##"Topic: Agent Status   What the Marks on Panes, Tabs and the Workspace Name Mean"##,
        title: r##"Agent Status Marks"##,
        text: r##"STATUSES:
  Every pane that runs an agent has one of four statuses.
  working   The agent is running a turn.
  blocked   The agent cannot continue without you. It waits for an approval
            or an answer, or its turn failed.
  done      The agent finished a turn while you were not looking at its tab.
  idle      The agent waits for a prompt and you have seen it.
  A pane without an agent has no status and shows no mark.

SOUNDS:
  A pane that becomes blocked plays one sound, even in the tab you are looking at.
  A pane that becomes done plays a different sound.
  Working and idle are silent.
  Ctrl+a then M turns the sounds off and on. The status line shows muted while they are off.

MARKS:
  blocked   red dot
  done      cyan dot
  working   yellow dot
  idle      green ring
  The pane border shows the mark of its pane after the label. Each tab shows
  its mark in front of its index and keeps its width. The workspace mark
  follows the workspace name on the left of the status line, and a separator
  stands between it and the first tab.
  The tab name and the pane label take the colour of the status.
  An omp agent that was already running shows no mark until it restarts.

ROLL-UP:
  A tab shows the most urgent status among its panes. A workspace shows the
  most urgent status among its tabs and its parked panes. The order is
  blocked, done, working, idle.

DONE AND SEEN:
  A turn that finishes in a tab you are looking at makes the pane idle. A
  turn that finishes out of sight makes the pane done. You are looking at a
  tab when it is the current tab of a workspace that a terminal with focus
  is showing. Viewing a tab makes every done agent in it seen, and each of
  those panes becomes idle. Viewing never clears blocked.

PICKERS:
  The pickers show the status as a coloured word. The workspace picker on
  Ctrl+a s shows the status of each active workspace. The tab picker on
  Ctrl+a w shows the status of each tab after the tab name. The parked
  chooser on Ctrl+a U and Ctrl+a B shows the status of each parked pane.
  A row without an agent shows no status word. If a picker cannot read the
  status, it lists its rows without status and works as before.

OMP:
  omp reports its status from its extension events. A prompt makes the pane
  working. An approval, a question, a plan that awaits approval and a failed
  turn make it blocked. The pane stays working while a background job or a
  subagent of omp is still running.
  An omp agent that is already running must restart before it uses the new
  extension.
  Some omp prompts fire no event and show working. They are the approval of a
  settings write, consent popups and dialogs that other extensions open.
  A long-lived background job keeps its pane working while it runs.
  A plan review that you dismiss without choosing stays blocked until the
  next prompt. An Esc during a provider retry does the same.
  A plan that omp submits from inside its eval tool has not been observed.
  The pane may show idle while that plan review is open.

CODEX:
  Native hooks report prompts as working, approvals/questions as blocked,
  hidden completions as done, and visible completions or interrupts as idle.
  Review the installed agent-hook.py codex entries in /hooks, then restart
  existing Codex CLIs. Ctrl+a D opens diagnostics and shows the last event.
  Codex's separate computer-use notify command is unchanged.

CLAUDE CODE:
  Claude Code reports its status from the state file that it writes for each
  interactive session. Its status line calls the helper once a second, and
  the helper reads that file. A running turn makes the pane working. A
  permission prompt, a question and a plan approval make it blocked at once
  through three hooks. A failed turn stays blocked until the next prompt.
  The status is unset when Claude Code exits.
  Claude Code does not document the state file. If an update changes it,
  Claude Code panes keep their last mark or show none until the helper is
  updated.
  A turn shorter than a second can pass without a working mark."##,
    },
    Topic {
        id: r##"topic-ai"##,
        display: r##"Topic: AI Recovery    How Claude, Codex, & OMP Session Recovery Works"##,
        title: r##"AI Conversation Recovery Architecture"##,
        text: r##"AI SESSION PERSISTENCE RULES:

Ordinary Shell Commands:
  You do not need a special wrapper. You can launch `claude`, `codex`, or `omp`
  directly in any shell pane, and switch agents within the same shell.

Supported Tools:
  1. Claude Code:
     - Tracked via native hooks and statusLine.
     - Resumed via: claude --resume <exact-session-id>
  2. OpenAI Codex:
     - Tracked via native title thread-id and open rollout/sqlite databases.
     - Resumed via: codex resume <exact-thread-id>
  3. Oh My Pi (omp):
     - Tracked via global extension on session_start/switch/branch/tree.
     - Resumed via: omp --resume <exact-session-file>

Key Guarantees:
  - No prompt replay: Reopens conversation and waits for your input.
  - Per-pane isolation: Two panes in the same project directory track distinct IDs.
  - Exited agents: Deliberately exiting an agent to the shell restores as a shell."##,
    },
    Topic {
        id: r##"topic-neovim"##,
        display: r##"Topic: Neovim         How Neovim Auto-Session Persistence Works"##,
        title: r##"Neovim Session Persistence Rules"##,
        text: r##"NEOVIM SESSION PERSISTENCE:

Module:
  ~/.config/nvim/lua/custom/tmux_workspace.lua
  Loaded automatically in your init.lua.

Features:
  - Native mksession: Saves window layout, splits, tabs, and open files.
  - Zero auto-writing: Never blindly writes dirty buffer contents to disk.
  - Swapfile Safety: If a reboot occurs while you have unsaved edits,
    Neovim's native swapfile recovery prompt ([O]pen, (E)dit, (R)ecover...)
    appears on restore to protect your changes.
  - Private storage: Session files stored mode 0600 in ~/.local/state/tmux-workspace/."##,
    },
    Topic {
        id: r##"topic-guide"##,
        display: r##"Topic: Full Manual    Open Complete Offline Guide (~/.config/tmux/WORKSPACE_GUIDE.md)"##,
        title: r##"Full Offline Reference Manual"##,
        text: r##"The complete 12-section manual is saved at:
  ~/.config/tmux/WORKSPACE_GUIDE.md

Select this item and press Enter to read the complete guide in a full pager."##,
    },
];
