# Changelog

## 1.0.0 — 2026-11-05

The first release for anyone to install.

### Added

- `Ctrl+a A` dashboard: every pane of every workspace with its agent, status, folder and a
  preview; jump, filter, park, unpark, rename and close from the keyboard. `tmux-workspace dash
  --once` prints one frame.
- `install.sh` for macOS and Linux (brew, apt, dnf, pacman, apk, zypper): dependency checks and
  `--deps`, backups, `--dry-run`, `--uninstall`, and `curl | bash` installs.
- First-run onboarding that asks for your projects folder (`tmux-workspace setup`).
- Linux support: clipboard through `tw-clip` (pbcopy, wl-copy, xclip, xsel, OSC 52) and sounds
  through `tw-sound` (afplay, pw-play, paplay).
- Agent status for bash, fish and other shells, not only zsh.
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
