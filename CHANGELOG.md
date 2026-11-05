# Changelog

## 2.0.0 — 2026-11-05

The parts that run all the time are Rust now, and installing no longer needs Python or a clone.

Python spends about 35 ms starting before it does anything, and the agent hook, `pane-start`, the
status line, `help` and autosave each paid for it. They run in a Rust binary now. `make bench`, on an
Apple M4 Pro (macOS 27.2, tmux 3.8, Python 3.15.2), hyperfine, mean of 30 runs on a scratch server
with 5 panes:

| Path | 1.0.0 (Python) | 2.0.0 (Rust) |
|---|---|---|
| Agent hook with nothing to do (every Claude Code tool call) | 35.8 ms | 1.9 ms |
| `pane-start` until it hands over to your shell (every new pane) | 50.4 ms | 6.9 ms |
| `agent-hook claude-status` (every status line refresh) | 56.0 ms | 14.9 ms |
| `help` (`Ctrl+a ?`) | 36.9 ms | 2.0 ms |
| `save` of 5 panes (autosave, `Ctrl+a Ctrl+s`) | 284.1 ms | 30.9 ms |

### Install

- `install.sh` downloads a prebuilt binary for macOS (arm64, x86_64) or Linux (x86_64, arm64; static
  musl, so any distribution), checks it against the release's `SHA256SUMS` before it writes anything,
  and unpacks it to `~/.local/share/tmux-workspace/<version>`. It no longer clones the repository.
  `--version vX.Y.Z` installs another release.
- From a clone, it builds the binary with cargo (Rust 1.89 or newer), or downloads the release of the
  clone's version when there is no cargo.
- Python 3.9 or newer is needed only for `dash`, `ai` and `cleanup` until they are ported. Without
  it the installer warns and goes on; `--deps` no longer installs Python.

### Added

- `work update` and `tmux-workspace update [--check]`: update an install from a release (checked
  against `SHA256SUMS`, the version before is kept) or a clone (a fast-forward `git pull`). They keep
  the choices you made at install time.
- `tmux-workspace integrations install|uninstall|status`, which replaces `bin/install-integrations`.
  `install.sh --agents` and `--uninstall` call it.
- `make bench` (hyperfine against the Python of 1.0.0, in `legacy/`), `make dist` (the release archive of
  this machine) and, for maintainers, `make stats` (downloads, clones, views and stars; see
  `docs/RELEASING.md`).
- `TMUX_WORKSPACE_TRACE=<file>` appends a line for every tmux, ps or other process the program starts.

### Fixed

- The agent hook finds `tmux` and `lsof` on `PATH`. It only looked in `/opt/homebrew/bin/tmux` and
  `/usr/sbin/lsof`, so on Linux and on Intel Macs agent status never worked.
- Hook commands in the Claude Code and Codex settings are shell-quoted, so a home folder with a space
  in its name works.
- `integrations` keeps the order of keys in your settings files and treats only this project's hook
  entries as its own.

### Upgrading

- Installed with the one-line command: run it again once. A 1.x install made by that command (a clone
  in `~/.local/share/tmux-workspace`) is updated in place. Your snapshots, options, key bindings and
  config stay as they are. After that, `work update` updates.
- Hooks registered by 1.x call `agent-hook.py`; they keep working through a small shim until
  `install.sh --agents` (or a later `work update`) rewrites them to `tmux-workspace agent-hook`.
  Review the Codex entries with `/hooks` afterwards, and restart running agents.
- Using a clone and updating it with `git pull`: run `./install.sh` (or `make build`) right after
  pulling 2.0. `bin/tmux-workspace` is a build product now, and tmux starts it in every new pane.

### Differences from 1.0.0

- Error text for failures of the operating system (a file that cannot be read, a folder that cannot
  be made) uses Rust's wording instead of Python's.
- Some inputs that crashed the Python give a clear error: `No such pane: <id>`; `park` with no
  argument parks the current pane.
- Autosave records an error for one pane (a Codex lookup that fails, a Neovim that does not save)
  instead of giving up on the whole save.
- Ctrl-C at the label prompt of a new pane keeps the pane, and Ctrl-C while an app is being reopened
  goes to the app.

## 1.0.0 — 2026-11-05

The first release for anyone to install.

### Upgrading an existing clone

After `git pull`, run `./install.sh` from the clone before you reload or restart tmux. The new
`tmux.conf` sources `~/.config/tmux-workspace/agent-status.conf` and calls `tw-clip` and
`tw-sound`, which only the installer links. Move a tracked `config.json` you edited out of the clone
first (`git pull` refuses to overwrite it): the installer copies it to
`~/.config/tmux-workspace/config.json` when that file does not exist yet.

### Added

- `Ctrl+a A` dashboard: every pane of every workspace with its agent, status, folder and a
  preview; jump, filter, park, unpark, rename and close from the keyboard. `tmux-workspace dash
  --once` prints one frame.
- `install.sh` for macOS and Linux (brew, apt, dnf, pacman, apk, zypper): dependency checks and
  `--deps`, backups, `--dry-run`, `--uninstall` (which also removes `--agents` integrations), and
  `curl | bash` installs.
- First-run onboarding that asks for your projects folder (`tmux-workspace setup`).
- Linux support: clipboard through `tw-clip` (pbcopy, wl-copy, xclip, xsel, OSC 52) and sounds
  through `tw-sound` (afplay, pw-play, paplay).
- Agent status for bash, fish and other shells, not only zsh. tmux runs its commands with `/bin/sh`,
  so any login shell (nu, xonsh, pwsh, …) works in panes; `TMUX_WORKSPACE_SHELL` picks another one.
- Support for tmux 3.3 and 3.4 and fzf 0.38+; the picker falls back to a numbered list without fzf.
- CI on macOS and Ubuntu, an install matrix for Ubuntu, Debian, Fedora, Arch and Alpine, and
  generated screenshots.
- MIT license and README.

### Changed

- The config moves to `~/.config/tmux-workspace/config.json`; the clone ships
  `config.example.json`, and a `config.json` in the clone is still read.
- `bin/install-terminal-settings` is now `bin/install-integrations` and also registers Claude Code,
  omp and Neovim.
- Agent sounds call `tw-sound blocked|done`; the `@agent_sound_blocked` and `@agent_sound_done`
  options are gone.
- zoxide only takes over `cd` when `TMUX_WORKSPACE_ZOXIDE_CD=1` (the old `shell.zsh` sets it).
