# tmux-workspace public v1: dashboard, portability, installer, tests

Date: 2026-11-05 · Status: approved in conversation, awaiting spec review · Branch: `feat/public-v1`

## Intent

The repository is public on GitHub but only its author can use it. It has no README and no
LICENSE. It hardcodes `/Users/<user>` and macOS tools, and the author's Doppler project sits in
the tracked `config.json`. This change makes it a project that a stranger on macOS or Linux
can install with one command, understand from the README, and trust because CI tests it on
every supported environment. It also adds the first full-screen interface: an agent and
workspace dashboard.

Success means:

1. `curl -fsSL https://raw.githubusercontent.com/ahmmkh/tmux-workspace/main/install.sh | bash`
   works on macOS, Ubuntu 24.04, Debian 12, Fedora, Arch and Alpine. CI proves it.
2. `Ctrl+a A` opens a dashboard that matches the approved mockup (below).
3. The README shows real screenshots generated from the code, and the repo carries an MIT
   license.
4. The author's existing setup at `~/Projects/tmux-workspace` keeps working: same symlinks,
   same keybindings, same snapshots.
5. The full test suite runs green locally with `make test` and in GitHub Actions.

Out of scope: rewriting existing features, removing the agent-tool folders (`.agents`,
`.claude`, `.hermes`, `.omp`, `.opencode`, `openspec`), Windows/WSL-specific work, and
pip or third-party Python dependencies.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| UI | Terminal dashboard in a tmux popup | Lives where the user already works; no server or browser |
| UI toolkit | Python stdlib `curses` | Zero dependencies keeps the installer reliable on every distro |
| Platforms | macOS first-class, Linux supported | Most tmux users run Linux |
| License | MIT, © 2026 Ahmed Khatab | Common for dotfiles and tmux tooling |
| Dashboard key | `Ctrl+a A` | Free today; "A" for agents |
| Installer | Bash, compatible with bash 3.2 | macOS ships bash 3.2 |
| Screenshots | Generated SVG from `capture-pane -e` | Reproducible, crisp, renders on GitHub, no extra tools |

## 1. Dashboard

### Behaviour

```
┌ tmux-workspace ─────────────── 3 blocked · 2 done · 5 working ┐
│ WORKSPACE        TAB         PANE        AGENT   STATUS       │
│ ▸ api            1:code      1:claude    claude  ● blocked    │
│   api            2:servers   1:logs      —       —            │
│   frontend       1:code      2:codex     codex   ● done       │
│   infra          1:main      1:omp       omp     ● working    │
│   ⇣ parked       api         3:tests     —       —            │
├───────────────────────────────────────────────────────────────┤
│ preview of selected pane …                                    │
└ enter jump · p park · u unpark · r rename · x kill · / filter ┘
```

- `tmux-workspace dash` (alias `dashboard`) runs the curses UI. `tmux.conf` binds
  `Ctrl+a A` to open it in a `display-popup -E -w 90% -h 85%`.
- Header: product name, then the counts of each non-zero status in roll-up order
  (`blocked · done · working · idle`), then `N panes · M workspaces`.
- One row per pane across every workspace. Parked panes (session `__parked`) appear under
  their home workspace with the `⇣ parked` marker in the workspace column.
- Ordering: workspaces by their roll-up urgency (blocked, done, working, idle, none), then by
  name; within a workspace by tab index then pane index; parked panes last in their group.
- Status column: the same four words and colours as `agent-status.conf` (blocked red
  `#f7768e` bold, done cyan `#7dcfff`, working yellow `#e0af68`, idle green `#9ece6a`), with
  `●` for blocked/done/working and `○` for idle. A pane whose foreground command is a shell
  shows no status, matching the status line.
- Agent column: the `kind` from `@workspace_app` when present, else the foreground command if
  it is a known agent (`claude`, `codex`, `omp`), else `—`.
- Preview: the last lines of the selected pane from `capture-pane -p`, control sequences
  stripped, drawn dim. Hidden when the terminal has fewer than 20 rows.
- Refresh: once per second, from one `list-panes -a` call. The selection follows the same
  pane id across refreshes.
- Width: below 80 columns the TAB column truncates first, then PANE; below 50 columns the
  AGENT column hides.
- Colours: curses 256-colour pairs approximating the Tokyo Night palette; falls back to the
  terminal's 8 colours when 256 are not available.

### Keys

| Key | Action |
|---|---|
| `j` `k` `↓` `↑` `g` `G` | Move; top; bottom |
| `Enter` | Jump: `switch-client` to the workspace, select the tab and pane, then exit. For a parked pane, unpark it to the right of the pane the popup was opened from |
| `/` | Filter by workspace, tab, label or agent (case-insensitive substring); `Esc` clears |
| `a` | Toggle "agents only" |
| `p` | Park the selected pane (not allowed for the pane that opened the popup) |
| `u` | Unpark the selected parked pane to the right of the invoking pane |
| `r` | Rename the label (inline prompt in the footer) |
| `x` | Kill the selected pane after a `y/n` confirmation in the footer |
| `?` | Toggle the key help overlay |
| `q` `Esc` | Quit |

Errors from an action show in the footer for 3 seconds; the dashboard never crashes on a
failed tmux call. A pane that vanished between refreshes is skipped.

### Units

- `lib/workspace_model.py` (pure, no tmux, no curses):
  `AGENT_ORDER`, `SHELLS`, `roll_up(rows)` (moved here from `bin/tmux-workspace`, which then
  imports it), `build_rows(pane_rows) -> list[Row]`, `summary(rows)`, `filter_rows(rows, query,
  agents_only)`, `sort_rows(rows)`. `Row` is a dataclass with the fields the table shows plus
  `pane_id`, `session_name`, `window_index`, `parked`.
- `lib/dashboard.py`:
  - `render(rows, state, width, height) -> list[Line]` is pure: a `Line` is a list of
    `(text, style)` segments. All layout, truncation and the header/footer live here.
  - `handle_key(key, state, rows, ws) -> state` maps keys to state changes and calls the
    actions through the `ws` dict of controller functions, the pattern `cleanup(globals())`
    already uses.
  - `run(ws)` is the thin curses loop: read rows, render, paint, read keys with a 1 s
    timeout.
  - `frame_text(ws, width, height, color)` renders one frame without curses, for
    `dash --once`.
- `bin/tmux-workspace`: commands `dash`/`dashboard`, with `--once [--width W --height H]
  [--color]` printing a single frame to stdout. Tests and the screenshot script use it.

## 2. Portability

| Today | After |
|---|---|
| `tmux.conf` calls `/Users/<user>/.local/bin/tmux-workspace` (21 places) | Calls `$HOME/.local/bin/tmux-workspace` inside single-quoted shell commands, so `sh` expands it at run time |
| `source-file "/Users/<user>/Projects/tmux-workspace/agent-status.conf"` | `source-file ~/.config/tmux-workspace/agent-status.conf`, a symlink the installer creates |
| `integrations/codex-hooks.json` hardcodes paths (8 places) | Uses a `{{AGENT_HOOK}}` placeholder that `bin/install-terminal-settings` fills in |
| `snapshot` loads `~/.local/lib/tmux-workspace/agent-hook.py` | Loads `integrations/agent-hook.py` relative to the resolved script, with the old path as fallback |
| `pbcopy` in five copy bindings | `bin/tw-clip`: `pbcopy`, else `wl-copy` (Wayland), else `xclip`, else `xsel`, else OSC 52 to the client tty |
| `afplay` and `/System/Library/Sounds` | `bin/tw-sound <blocked|done>`: `afplay` with system sounds on macOS; `paplay`/`pw-play` with freedesktop sounds on Linux; silent when neither exists |
| Shell detection is `== 'zsh'` in Python and `agent-status.conf` | `SHELLS = {zsh, bash, fish, sh, dash, ksh, tcsh}` in Python; the tmux format matches `#{m/r:^(zsh\|bash\|fish\|sh\|dash\|ksh\|tcsh)$,...}` |
| `set -g default-shell /bin/zsh`; `os.execv('/bin/zsh')` | Line removed (tmux uses `$SHELL`); `os.execv` uses `$SHELL` with `/bin/sh` fallback |
| `~/Projects` as the projects root | `projects_dir` in the user config, `TMUX_WORKSPACE_PROJECTS` env override, default `~/Projects` |
| Tracked `config.json` with the author's Doppler project and model | `config.example.json` is tracked; the real config lives at `~/.config/tmux-workspace/config.json` (`$XDG_CONFIG_HOME` respected); the old repo `config.json` is read as a fallback, migrated by the installer, and git-ignored |
| `shell.zsh` (zsh only, requires zoxide) | `shell/work.sh` for bash and zsh, `shell/work.fish` for fish; zoxide initialises only when installed; `shell.zsh` stays as a one-line shim that sources `shell/work.sh` so the author's `.zshrc` keeps working |
| `diagnostics.py` checks `pbcopy`, `afplay` | Checks the platform's clipboard and sound tools, the tmux version floor, and the symlinks the installer manages |

The tmux version floor is verified on a scratch server in the first implementation task. The
expected floor is 3.3 (popups with `-e`/`-T`, `#{P:}` loops, `set -p`).

## 3. Installer

`install.sh` at the repository root, bash 3.2 compatible, `set -euo pipefail`, passes
shellcheck.

### Modes

- Remote: piped from curl, it clones (or `git pull --ff-only` updates) the repo into
  `${TMUX_WORKSPACE_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/tmux-workspace}`, then runs the
  local mode from there.
- Local: run from a clone, it links to that clone. The author's `~/Projects/tmux-workspace`
  therefore stays the live source.

### Steps

1. Detect the OS (`uname`), the distro (`/etc/os-release`) and the package manager (brew,
   apt-get, dnf, pacman, apk, zypper).
2. Check requirements: tmux ≥ floor, python3 ≥ 3.9, git. Check recommended tools: fzf,
   zoxide, sesh, lazygit. Print a table of found/missing with versions.
3. With `--deps`, install what is missing through the package manager (`sudo` when not
   root). Without it, missing requirements stop the install with the exact command to run.
4. Link `~/.local/bin/tmux-workspace`, `~/.config/tmux-workspace/agent-status.conf`,
   `~/.tmux.conf` (skip with `--no-tmux-conf`), and the sesh config when sesh is present.
5. Add a marked block (`# >>> tmux-workspace >>>` … `# <<< tmux-workspace <<<`) to `~/.zshrc`
   and/or `~/.bashrc` for the shells that exist, and link `work.fish` into
   `~/.config/fish/functions/` when fish exists. Skip with `--no-shell`.
6. Create `~/.config/tmux-workspace/config.json` from the example, or migrate the repo's
   legacy `config.json` when it exists and the target does not.
7. Onboarding (added 2026-11-05 at the author's request): `tmux-workspace setup` asks
   "Where do you keep your projects? [~/Projects]", creates the folder, saves `projects_dir` and
   prints the first keys to try. The installer runs it on `/dev/tty` when one exists, so
   `curl | bash` can ask too; `--projects DIR` answers it without a prompt. An unattended
   install leaves it pending, and the first interactive `work` asks once.
8. With `--agents`: register the Claude Code, Codex, omp and Neovim integrations, and on macOS
   run `bin/install-terminal-settings`.
9. Reload a running tmux server (`tmux source-file ~/.tmux.conf`) when one exists.
10. Run `tmux-workspace doctor` and print next steps.

### Safety

- Every file or link it replaces moves to
  `~/.local/state/tmux-workspace/backups/install-<timestamp>/` and is recorded in a
  `manifest` (original path, backup path, kind).
- `--uninstall` removes what the installer created, removes the shell blocks, and restores
  the newest manifest's backups byte for byte. It leaves snapshots and the user config.
- `--dry-run` prints every action and changes nothing.
- A second run with the same flags changes nothing and prints "already installed".
- `--yes` answers prompts; without a tty, prompts take their safe default (do not replace an
  existing `~/.tmux.conf` that is not ours unless `--yes`).
- Flags: `--deps`, `--agents`, `--projects DIR`, `--no-shell`, `--no-tmux-conf`, `--dry-run`, `--yes`,
  `--uninstall`, `--help`.

## 4. Tests and CI

All Python tests stay on stdlib `unittest`, the existing style.

- `tests/test_workspace_model.py`: roll-up order, parked panes counted under home, shell
  detection for each shell, filter, agents-only, sort, summary.
- `tests/test_dashboard.py`: golden frames at 120×40, 80×24 and 50×20 (plain text, stored in
  `tests/golden/`, regenerated with `UPDATE_GOLDEN=1`); key handling with a mocked `ws`
  (jump, park refuses the invoking pane, rename, kill confirm/cancel, filter); action errors
  reach the footer.
- `tests/test_dashboard_tmux.py`: on a scratch tmux server (the `ScratchTmux` pattern),
  `dash --once` lists the panes; the curses UI in a pane draws the table (checked with
  `capture-pane`); `Enter` selects the pane; `p` parks it.
- `tests/test_recovery.py`: on a scratch server with a temporary state directory,
  `snapshot` → kill server → `restore` reproduces sessions, tab names, labels and layouts;
  `initialize` restores at most once per server; `pane_start` builds the right command for
  each app kind and refuses invalid identities.
- `tests/test_portability.py`: `tw-clip` and `tw-sound` choose the right tool given a fake
  `PATH` and `uname`; OSC 52 output is well-formed; config lookup order (XDG, legacy,
  example); `projects_dir` override.
- `tests/test_install.py`: with a temporary `HOME` and stub tools on `PATH`: fresh install,
  idempotent second run, backup of an existing `~/.tmux.conf`, uninstall restores it byte for
  byte, `--dry-run` writes nothing, missing tmux fails with the install hint, shell blocks are
  added once, `bash -n install.sh`, shellcheck when available.
- `tests/install/matrix.sh`: for each image (`ubuntu:24.04`, `debian:12`, `fedora:latest`,
  `archlinux:latest`, `alpine:3.20`), copy the repo in, run `install.sh --deps --yes` as a
  non-root user with sudo, then smoke-test: `tmux -f ~/.tmux.conf new -d`, check that
  `Ctrl+a A` is bound, `tmux-workspace doctor`, `tmux-workspace dash --once`, and
  `work demo` creates a session. `make test-install` runs it locally through Docker.
- `.github/workflows/ci.yml`: unit and integration tests on `macos-latest` and
  `ubuntu-latest` with Python 3.9 and 3.13; shellcheck; the distro matrix as a separate job.
- `Makefile`: `test`, `test-install`, `lint`, `screenshots`.

Opt-in tests that cost money (`tests/live_jev.py`) stay opt-in and out of CI.

## 5. Docs and screenshots

- `scripts/demo.py` starts a scratch tmux server (its own `-L` socket and temporary state
  directory), creates the demo workspaces `api`, `frontend`, `docs` and `infra` with labelled
  panes, sets agent states through `@workspace_agent`/`@workspace_app`, parks one pane, and
  captures views with `capture-pane -e -p`: the dashboard, the status line with marks, the
  workspace picker and the window picker.
- `scripts/ansi2svg.py` (stdlib) converts the captured ANSI into SVG: monospace text, SGR
  colours (16, 256 and truecolor), bold/dim, the Tokyo Night background and a rounded
  window frame. Output goes to `docs/images/*.svg`.
- `make screenshots` runs both. Each image is checked in a browser before it is committed.
- `README.md`: hero dashboard image; what it is and why; features with images; install
  (one-liner, from a clone, flags); requirements table per platform; keybinding cheatsheet;
  agent integrations; configuration; uninstall; troubleshooting with `doctor`; contributing
  (`make test`); license.
- `LICENSE` (MIT), `CHANGELOG.md` (v1.0.0), `config.example.json`.
- `WORKSPACE_GUIDE.md` stays the in-depth guide. It drops author-specific paths, documents
  the dashboard and the Linux tools, and the README links to it.

## Delivery

On the branch `feat/public-v1`, one commit per workstream, in this order: portability →
dashboard → recovery tests → installer and its tests → CI → screenshots and docs. Pushing and
opening the pull request wait for the author's go-ahead.

## Risks

- The author's live config is symlinked into this repo, so every `tmux.conf` change takes
  effect on their next reload. Each change to `tmux.conf` is first loaded on a scratch server
  (`tmux -L test -f tmux.conf`), and the test suite asserts that it parses.
- The `agent-status.conf` shell regex replaces an exact match. A tmux format mistake there
  silently hides every status mark, so a scratch-server test checks all four marks for bash
  and zsh panes.
- After the portability commit, `tmux.conf` sources `~/.config/tmux-workspace/agent-status.conf`,
  which only the installer creates. The author's machine runs `./install.sh` from the clone
  in the same step (with backups), and `doctor` reports a missing link.
- Distro images change. The matrix pins major versions where the distro has them, and a
  failing image fails CI loudly instead of being skipped.
