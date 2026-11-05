# tmux-workspace in Rust, stage 1: core, hooks, releases

Date: 2026-11-05 · Status: approved in conversation, awaiting spec review · Branch: `rust-core`

## Intent

Every hot path runs Python, and Python's fixed cost (interpreter start plus imports) is about
40 ms per run. A new pane waits on it before the user's shell starts, and Claude Code waits on
it for every tool call. This change rewrites the core of tmux-workspace in Rust, so those paths
cost a few milliseconds, and ships it as a prebuilt binary, so users no longer need Python for
the core.

This is stage 1 of three:

1. **Stage 1 (this spec), v2.0.0:** core commands, agent hooks, integration installer,
   `doctor`, `setup`, the release pipeline and the new `install.sh`.
2. **Stage 2:** the dashboard (`dash`) in Rust, against the same golden frames.
3. **Stage 3:** the AI commands (`ai`, `cleanup`) in Rust. Delete `legacy/` and every
   runtime use of Python.

Each stage gets its own spec, plan and review.

Measured on the author's Mac (Apple Silicon, tmux 3.8, Python 3.15, hyperfine, 30 runs, scratch
server with 5 panes):

| Path | Runs when | Python today | Stage-1 target |
|---|---|---|---|
| Agent hook with nothing to do | Every Claude tool call | 42 ms | ≤ 5 ms |
| `pane-start` until it execs the shell | Every new pane and tab | 60 ms | ≤ 15 ms |
| `agent-hook claude-status` | Every Claude status-line refresh | 66 ms | ≤ 20 ms |
| `help` | `prefix ?` | 42 ms | ≤ 5 ms |
| `save` | Autosave every 5 min, `prefix C-s` | 264 ms | ≤ 60 ms |

One tmux client call costs about 4.7 ms and one `ps` about 2 ms in any language. That is why
the stage-1 work batches tmux calls as well as removing the interpreter.

Success means:

1. Every command listed under "Native in stage 1" runs in Rust and behaves as the Python version
   at commit `30b1401` does: same output, exit status, tmux writes and files. The only
   exceptions are listed under "Deliberate differences".
2. The five targets above hold under `make bench` on the author's Mac.
3. Pushing a `v2.0.0` tag publishes a GitHub Release with four platform archives and
   `SHA256SUMS`, and `curl … | bash` installs from it on macOS, Ubuntu 24.04, Debian 12,
   Fedora, Arch and Alpine. CI proves it.
4. The author's existing install upgrades in place: same snapshots, options, keybindings
   and config, and the agent hooks are rewritten to the binary.
5. `make test` (cargo tests, the Python suites still covering `legacy/`, and the comparison tests) is
   green locally and in GitHub Actions.

Out of scope: porting `dash`, `ai` or `cleanup` (stages 2 and 3), changing any behaviour on
purpose, Windows, Homebrew, native process APIs instead of `ps`, and the README screenshot
tooling in `scripts/`, which stays Python as a development tool.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Scope | Full rewrite in three stages; this is stage 1 | Ends Python-free; each stage ships on its own |
| Reference | The Python code at `30b1401` is the behavioural spec | Precise, executable, already tested |
| Crate | One package at the repo root, one binary `tmux-workspace`, edition 2024 | One thing to build, install and version |
| Dependencies | `serde`, `serde_json`, `libc`; nothing else | Fast builds, small audit surface, zero startup cost |
| Arguments | Hand-written parser over the existing positional CLI | The CLI has no flags worth a framework; keeps startup at zero |
| Process facts | Keep calling `ps` exactly as Python does | Stored `birth` strings stay identical across versions |
| Distribution | GitHub Releases, four static targets, SHA-256 checked by `install.sh` | Users need no toolchain |
| Version | 2.0.0 | The install layout changes |

## 1. Shape

```
Cargo.toml             package tmux-workspace 2.0.0, edition 2024, rust-version 1.89
src/main.rs            dispatch, the error contract, legacy hand-off
src/tmux.rs            client: run, batch (\; chains), fields (\x1f and the 3.4 \037/\$ forms), timeouts
src/paths.rs           state dir, config dir/path, projects dir, config load/save   (lib/paths.py)
src/model.rs           rows, statuses, roll-up, option_json                         (lib/workspace_model.py)
src/pane.rs            pane-start, exec-shell, login shell, labels, identity
src/snapshot.rs        init, tick, save, restore, lock, atomic writes
src/park.rs            park, unpark, choose-park, rejoin
src/pick.rs            open, pick-window, window-preview, new-tab, kill-workspace (fzf)
src/help.rs            help topics (text moved verbatim), help, help-topic
src/hook/mod.rs        agent-hook entry: payload checks, the silent contract   (integrations/agent-hook.py)
src/hook/process.rs    owner walk, process kind, birth via ps
src/hook/{claude,codex,omp}.rs
src/integrations.rs    install/uninstall agent integrations              (bin/install-integrations)
src/doctor.rs          doctor                                             (lib/diagnostics.py)
src/onboarding.rs      setup                                              (lib/onboarding.py)
src/legacy.rs          hand-off of unported commands to legacy/
legacy/                today's Python (bin/tmux-workspace, lib/), moved with one fix: paths.REPO
                       still names the share root, so config.json/config.example.json resolve as before
tests/*.rs             integration tests (ScratchTmux)
```

Release profile: `opt-level = 3`, `lto = "thin"`, `codegen-units = 1`, `strip = true`,
`panic = "unwind"` (the hook catches panics).

### Native in stage 1

`pane-start`, `exec-shell`, `open` (and the no-argument default), `init`, `tick`, `save`,
`restore`, `park`, `unpark`, `choose-park`, `label`, `history`, `help`, `help-topic`,
`new-tab`, `pick-window`, `window-preview`, `kill-workspace`/`delete`/`rm`, `setup`,
`doctor`, `agent-hook <claude|claude-status|codex|omp>`, and
`integrations <install|uninstall>`, which replaces `bin/install-integrations`.

### Legacy hand-off

`dash`/`dashboard`, `ai` and `cleanup` exec `python3 <share>/legacy/tmux-workspace <command>
<args…>`. `<share>` is the directory holding the binary's companion files (section 6). If
`python3` is missing or older than 3.9, the command prints
`tmux-workspace <command> needs python3 3.9+ until it is ported (stage 2/3)` to stderr and exits 1.
Inside `legacy/`, the Python's `SELF` path points at the Python script. Its
background `tick` timer therefore only starts if the Python `init` runs, and that never happens
once tmux.conf calls the binary.

## 2. Compatibility contract

These must hold, with tests:

- **CLI:** same command names, aliases, arguments, defaults, stdout/stderr text and exit
  codes.
- **Errors:** a failing command writes `Workspace error: <message>` to stderr, shows
  `Workspace: <message>` with `display-message`, and exits 1. EOF or Ctrl-C exits 0.
- **State:** the same `TMUX_WORKSPACE_STATE_DIR` (default `~/.local/state/tmux-workspace`),
  file names, JSON keys, file modes (umask 077, config 600) and lock. A snapshot written by either
  version restores with the other.
- **tmux options:** the same `@workspace_*` and `@agent_*` names and values, and the same
  compare-and-set guards in the hook, so Rust and Python writers can interleave safely.
- **Config and environment:** `~/.config/tmux-workspace/config.json` (XDG), and the variables
  `TMUX_WORKSPACE_STATE_DIR`, `TMUX_WORKSPACE_PROJECTS`, `TMUX_WORKSPACE_SHELL`,
  `TMUX_WORKSPACE_ZOXIDE_CD`.
- **Floors:** tmux 3.3; macOS and Linux (glibc and musl); fzf 0.38; bash 3.2 for `install.sh`.

### Deliberate differences

- Hook commands in Claude and Codex settings become `<bin> agent-hook claude` and
  `<bin> agent-hook codex`, where `<bin>` is the absolute path of the installed binary.
  `integrations install` replaces our old `agent-hook.py` entries; it already drops them.
  The ownership mark becomes "the command runs `tmux-workspace agent-hook`" and the old mark
  still counts as ours. A user's own hooks are never touched.
- The Claude status line, when it is ours, becomes `<bin> agent-hook claude-status`.
- The omp extension calls `tmux-workspace agent-hook omp` (found on `PATH`, falling back to
  `~/.local/bin/tmux-workspace`) instead of `python3 ~/.local/lib/tmux-workspace/agent-hook.py`.
- `integrations install` removes the old `~/.local/lib/tmux-workspace/agent-hook.py` link
  when it points into this project.

## 3. Speed

- **Batch tmux reads.** Several values of one target come from one `display-message -p` with a
  `\x1f`-separated format. Commands with no read dependency run as one `\;` chain.
- **Parallelise per-pane captures.** `save` reads the pane list in one call and starts the
  `capture-pane` calls on at most 8 threads. Writes stay in the Python order.
- **No work before the early exit.** The hook reads stdin and checks the event and payload before
  any process call. A `PreToolUse` for a tool other than `AskUserQuestion` or `ExitPlanMode`
  makes no tmux or `ps` call, as today.

Call counts are deterministic, so tests enforce them. `TMUX_WORKSPACE_TRACE=<file>` makes the
binary append one line per child process it starts (`tmux …`, `ps …`, `fzf …`). It costs
nothing when unset.

| Scenario | tmux calls | ps calls |
|---|---|---|
| Hook, `PreToolUse` for `Bash` | 0 | 0 |
| `pane-start`, labelled pane, `TMUX_WORKSPACE_SHELL` set | ≤ 2 | 0 |
| `pane-start`, labelled pane, shell from the server | ≤ 2 | 0 |
| `help` (no fzf, non-interactive) | 0 | 0 |
| `save` with N panes | ≤ 4 + N | ≤ 1 |

`make bench` builds the release binary and runs hyperfine for the five targets on a scratch
server. Its numbers go in the README. CI runs `make bench` and reports its numbers without
gating on them, because runner load varies.

## 4. Agent hook

`agent-hook` must never change an agent's behaviour:

- It never writes to stdout, except the status-line text for `claude-status`, and never to
  stderr. It always exits 0.
- It catches all errors and panics, and a panic hook prints nothing.
- Every child process has a 2-second timeout and is killed when it runs out.
- It ignores stdin larger than 1 MiB.
- The payload checks are unchanged: session id length ≤ 4096, no control characters,
  absolute cwd, pane id `%N`, absolute socket from `TMUX`.

## 5. Tests

- **Unit tests** (`cargo test`) port the Python tests of pure logic: `test_workspace_model`,
  `test_agent_status` (decisions, with an injected runner for tmux and `ps`),
  `test_pane_decisions` for the non-AI parts, `test_integrations` (merging and uninstall on
  temporary homes), `test_onboarding`, and the `paths`/config cases of `test_portability`.
- **Integration tests** (`tests/*.rs`) use a `ScratchTmux` helper: a unique `-L` socket,
  `-f /dev/null`, `-u`, a temporary `TMUX_WORKSPACE_STATE_DIR` and home, and `exec sleep 300`
  for panes. They port `test_recovery`, `test_agent_status_tmux`, the tmux parts of
  `test_portability` and the call-count table above.
- **Comparison tests** (stages 1–2) run one scenario through `legacy/tmux-workspace` and through
  the binary on two scratch servers built the same way, then compare four things:
  - snapshot JSON, with ids and timestamps normalised
  - the tmux options written
  - stdout
  - exit codes

  They cover save → restore round trips in both directions, park/unpark, and each hook
  event. Stage 3 removes them with the Python.
- **The Python suites for `dash`, `ai` and `cleanup`** keep running against `legacy/`. Python
  tests for code that moved to Rust are deleted once their Rust ports pass.
- **Linters:** `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
  shellcheck, actionlint.
- **The install matrix** (ubuntu:24.04, debian:12, fedora, archlinux, alpine:3.20, and macOS)
  installs from a release-format archive built earlier in the same CI run. It checks
  `doctor`, the hook, `pane-start` and uninstall.
- **tmux versions:** the integration tests run on 3.3 (debian:12), 3.4 (ubuntu:24.04) and the
  current version (macOS), as today.

## 6. Distribution and install

### Release

`.github/workflows/release.yml` runs on tags `v*`:

| Target | Runner |
|---|---|
| `aarch64-apple-darwin` | `macos-latest` |
| `x86_64-apple-darwin` | `macos-latest`, cross target (Apple's SDK builds both) |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` with `musl-tools` |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` with `musl-tools` |

Each target produces `tmux-workspace-<version>-<target>.tar.gz` with this layout:

```
tmux-workspace-<version>/
  bin/tmux-workspace  bin/tw-clip  bin/tw-sound
  tmux.conf  agent-status.conf  sesh.toml  shell/  shell.zsh
  integrations/  legacy/  config.example.json
  README.md  LICENSE  CHANGELOG.md  WORKSPACE_GUIDE.md
```

The release also carries `SHA256SUMS` and the version's CHANGELOG section as its notes. The
workflow fails if the tag differs from the `Cargo.toml` version.

### install.sh

- **From a git checkout:**
  - With cargo: `cargo build --release --locked`, then link `target/release/tmux-workspace`.
  - Without cargo: download the release whose version matches `Cargo.toml` into
    `~/.local/share/tmux-workspace/bin/`.
  - The other files link from the checkout, as today.
- **Remote (`curl … | bash`):**
  - Download the archive for `uname -s`/`uname -m` and the latest release, or `--version vX.Y.Z`.
  - Check it against `SHA256SUMS` with `shasum -a 256` or `sha256sum`. A mismatch aborts
    before anything is written.
  - Extract to `~/.local/share/tmux-workspace/` (a version directory plus a `current` link), then link as today.
  - It no longer clones the repository.
- **Unsupported platform without cargo:** exit 3 with
  `no prebuilt binary for <os>/<arch>; install Rust (https://rustup.rs) and rerun`.
- **Python:** no longer required. Without `python3` 3.9+, it prints a warning that `dash`,
  `ai` and `cleanup` need it until stages 2 and 3, and continues. `--deps` stops installing
  Python.
- **Integrations:** `install.sh --agents` and `--uninstall` call `tmux-workspace integrations
  install|uninstall`.
- **Unchanged:** the backups, manifest, consent prompts, shell block, onboarding and every other flag.

### Upgrade from v1

Rerunning `install.sh` in the author's checkout builds the binary and repoints
`~/.local/bin/tmux-workspace` from the Python script to the binary. With `--agents`, it also
rewrites the hooks. The running server picks up the binary on the next `pane-start`, because
tmux.conf names `$HOME/.local/bin/tmux-workspace`, so no reload is needed. CHANGELOG
carries the upgrade note.

## Delivery

One branch, `rust-core`, merged after the final review. The tasks are ordered so that every
commit builds and `make test` stays green:

1. Crate, error contract, tmux client
2. paths/model
3. Hook
4. pane-start and exec-shell
5. Snapshot
6. park/pick/help
7. Integrations, doctor, setup
8. Legacy hand-off and move
9. Comparison tests
10. install.sh
11. Release workflow
12. Docs and bench

## Risks

| Risk | Mitigation |
|---|---|
| Behaviour drifts from Python in a case no test covers | Comparison tests on the same scenarios; the Python stays the reference until stage 3 |
| tmux 3.3/3.4 quirks (`\037`, `\$`, missing `-t` on `source-file`) | The existing tests that found them are ported first; the matrix runs 3.3 and 3.4 |
| Hook prints or fails and so affects Claude/Codex | Silent contract with panic catching, and a test that feeds malformed payloads and checks empty output and exit 0 |
| Musl or cross builds break late | The release workflow also runs on pull requests as a dry run, without publishing |
| An old hook entry points at a removed `agent-hook.py` | `integrations install` rewrites it; `doctor` reports stale entries |
| Two binaries on PATH after upgrade | `doctor` warns when `command -v tmux-workspace` is not the installed binary |
