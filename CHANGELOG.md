# Changelog

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
