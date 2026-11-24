# Pure-Rust Architecture, `agy` Support, Bug Fixes, Auto-Update & Python Retirement — Implementation Plan

> **For agentic workers:** Follow TDD (`test` first where practical), keep every module clean and modular, and run `make lint && make test` at every milestone gate (remember: NEVER overwrite `bin/tmux-workspace` during `make test`).

**Goal:** Deliver `tmux-workspace` `v2.2.0` as a 100% pure-Rust single binary with a central `src/agent/` registry, modularized submodules, full Antigravity CLI (`agy`) support, 5 concrete bug fixes (Issue #4, Codex `0.171.0`, snapshot `pane_title` control-char safety & error collapsing, restored pane `tcsetpgrp`, recovery test PATH isolation), native Rust `dash`, `cleanup`, and `ai` (`ureq` + `rustls`), opt-in background auto-update (`work update --auto on|off`), zero runtime Python, and a tagged `v2.2.0` GitHub Release downloadable via `work update`.

**Spec:** [docs/superpowers/specs/2026-11-24-agy-fixes-architecture-design.md](../specs/2026-11-24-agy-fixes-architecture-design.md)

---

## Milestone 1: Modular Core, `agy` Support & 5 Bug Fixes

### Task 1: Shared Helper Deduplication, `src/agent/` Registry & Module Splits
- Add `text::truthy(value: &Value) -> bool`, `paths::codex_home(home)`, `paths::agy_dir(home)` (and `Locations::agy_dir`).
- Remove duplicate `truthy` (in `pane.rs`, `hook/mod.rs`, `integrations.rs`, `doctor.rs`), duplicate `py_splitlines` (in `tmux.rs`), and duplicate `codex_home` helpers.
- Create `src/agent/{mod.rs, claude.rs, codex.rs, omp.rs, agy.rs, nvim.rs}` defining `AgentKind`, `AgentSpec`, `ALL`, lookup helpers, and moving Claude/Codex logic cleanly into `agent::{claude, codex}` (re-exported from `hook::{claude, codex}` for backwards-compatible crate paths).
- Split monoliths with full crate re-exports:
  - `src/snapshot.rs` → `src/snapshot/{mod.rs, store.rs, apps.rs, save.rs, restore.rs}`
  - `src/integrations.rs` → `src/integrations/{mod.rs, json.rs, target.rs}`
  - `src/pick.rs` → `src/pick/{mod.rs, workspace.rs, window.rs}`
- Add `"agy"` to `src/anonymize.rs` known program names list and `model::AGENTS` (from `agent::AI_AGENT_IDS`).
- Verify `cargo test --features testing`.

### Task 2: Antigravity CLI (`agy`) Full Support
- Create `integrations/agy-settings.json` (`{"title": {"type": "command", "command": "{{BIN}} agent-hook agy"}}`).
- Implement `src/agent/agy.rs` (`agy_event`, payload parsing, title formatting, `agent_state` + `tool_confirmation_pending` mapping with `"initializing"` → `"idle"`, reset on `id`/`pid`/`birth` change, empty `conversation_id` allowed on blank startup/`/new`).
- Wire `hook/mod.rs` (`"agy"` mode) and `hook/process.rs` (`agy` process kind + non-interactive flags/subcommands from `AgentSpec`).
- Update `snapshot/apps.rs::live_app`: return `None` when `kind == "agy"` and `id == ""`.
- Add post-exit staleness guard in `agent-status.conf`, `src/model.rs::pane_status`, `src/pick/mod.rs::agent_status` (include `@workspace_app`), and `legacy/lib/workspace_model.py` (until Milestone 3 deletes it).
- Update `src/integrations/{mod.rs, json.rs, target.rs}`: merge/remove `"title"` in `~/.gemini/antigravity-cli/settings.json` with normalization idempotence guard, preserve custom user `"title"`, add `("agy", hooked_agy(...))` in `Target::each()`.
- Update `src/doctor.rs` (`agent_integrations` includes `"Antigravity CLI"` without standalone failure), `src/bug.rs` / `src/report.rs` (`agy ✓/✗`), and `install.sh` (`agent_names()`, `survey()` checklist row, `act agents`).
- Add unit + live scratch-tmux tests in `src/agent/agy.rs`, `tests/hook_tmux.rs`, `tests/recovery.rs`, `tests/integrations.rs`, `tests/doctor.rs`, `tests/report.rs`.

### Task 3: Fix 1 — GitHub Issue #4 (Classification Gate, Feedback Template & Status Bar)
- Create `.github/ISSUE_TEMPLATE/feedback.yml`.
- Add `Kind::Feedback` in `src/report.rs`.
- Add `work feedback [--print]` and interactive `work report [--print]` classification gate in `src/bug.rs` and `src/cli.rs` (`dispatch`, `KNOWN`, `UNWATCHED`, `COMMAND_NAMES` with `"2.2"`).
- Update `shell/work.sh`, `shell/work.fish`, and `tmux.conf` (`bind F` → `report`, `status-right` → `| C-a D health · F feedback | %a %H:%M `, `status-right-length 120`).
- Add tests in `src/report.rs`, `src/bug.rs`, `tests/report.rs`, `tests/test_portability.py`.

### Task 4: Fix 2 — Codex `0.171.0` Status & Identity Recovery
- Remove `[ -n "$TMUX_PANE" ] && ` prefix from `integrations/codex-hooks.json`.
- Update `src/hook/mod.rs`: when `mode == "codex"` and `TMUX`/`TMUX_PANE` unset, stat default socket `${TMUX_TMPDIR:-/tmp}/tmux-<euid>/default` (0 child procs if absent) and route to `detached_codex`.
- Update `src/agent/codex.rs`: `strip_codex_spinner`, shared contradiction check, SQLite schema probe (`threads.name` ONLY, never `threads.title`), modern title + CLI `resume <uuid>` matching in both `detached_codex` and `resolve_codex`, and mid-turn adoption when `routed_detached && !other_session_on_same_process`.
- Update `snapshot/apps.rs::shows_current_conversation`: return `false` for non-legacy titles so `save` resolves against `state_*.sqlite`.
- Add unit + live tmux tests in `src/agent/codex.rs`, `tests/hook_tmux.rs`, `tests/recovery.rs`, `tests/calls.rs`.

### Task 5: Fixes 3, 4 & 5 — Snapshot `pane_title`, Error Collapsing, Restored Pane `tcsetpgrp`, Recovery Test Isolation
- Remove `"pane_title"` from bulk `PANE_FIELDS` in `src/snapshot/save.rs`; query on-demand via single `display-message` only for Codex candidate panes in `src/snapshot/apps.rs`.
- Collapse consecutive identical errors (`(×N)` over last 50 lines → newest 10) in `src/bug.rs::diagnostics`.
- Give resumed children their own foreground process group (`process_group(0)` + `tcsetpgrp` with `SIGTTOU`/`SIGTTIN` ignored) in `src/pane.rs::reopen_app` when `isatty(0) == 1`.
- Isolate `PATH` in `tests/recovery.rs::an_app_that_is_not_installed_needs_attention_and_the_shell_still_starts`.
- **Milestone 1 Gate:** Run `make lint && make test` (all green!).

---

## Milestone 2: Native Rust `dash` & `cleanup`

### Task 6: Add Cargo Dependencies & Extend `src/model.rs`
- Add `unicode-width = "0.2"`, `unicode-normalization = "0.1"`, `regex = "1"`, `ureq = { version = "3", default-features = false, features = ["rustls", "gzip"] }` to `Cargo.toml` and update `Cargo.lock`. Verify `cargo check --all-targets --features testing`.
- Extend `src/model.rs` with `DASH_PANE_FIELDS`, `Row`, `Summary`, `agent()`, `build_rows()`, `sort_rows()`, `filter_rows()`, `summary()`, applying the `agy` staleness guard in both `pane_status()` and `agent()`.
- Port all unit tests from `tests/test_workspace_model.py` into `src/model.rs`.

### Task 7: Native Rust `src/dash/` (`mod.rs`, `render.rs`, `terminal.rs`)
- Implement pure `render.rs` matching `tests/golden/dashboard-{120x40,80x24,50x20}.txt` byte-for-byte and matching `dashboard.py::sgr` 24-bit color formatting.
- Implement `mod.rs` (`State`, `Mode`, `Key`, `DashActions` trait, `handle_key`, `read_rows`, `read_preview`, `main` with `--once`, `--width`, `--height`, `--color`).
- Implement `terminal.rs` (RAII raw termios + alt screen + cursor restore, `SIGWINCH` + `TIOCGWINSZ`, `poll(1000ms)` + 25ms Esc/CSI/SS3/UTF-8 decoder, buffered flicker-free ANSI paint).
- Wire `"dash" | "dashboard"` in `src/cli.rs` directly to `dash::main`.
- Create `tests/dash.rs` porting all tests from `tests/test_dashboard.py` (including golden frames) and `tests/test_dashboard_tmux.py` (live scratch tmux `--once`, `j`+`Enter`, `p` park).

### Task 8: Native Rust `src/cleanup.rs`
- Implement `src/cleanup.rs` (`candidates`, `CleanupHost` trait, `cleanup`, atomic `if-shell` `kill-pane` guard).
- Wire `"cleanup"` in `src/cli.rs` directly to `cleanup::main`.
- Create `tests/cleanup.rs` porting all tests from `tests/test_cleanup.py`.
- **Milestone 2 Gate:** Run `make lint && make test` (all green!).

---

## Milestone 3: Native Rust `ai`, Auto-Update, Python Retirement & `v2.2.0` Release

### Task 9: Native Rust `src/ai/` (`core.rs`, `panes.rs`, `history.rs`, `suggest.rs`, `mod.rs`)
- Implement `src/ai/core.rs`: `AiError`, `Transport` trait + `UreqTransport` (`ureq` 3 + `rustls` + `webpki-roots`, 15s timeout, 180 KiB request cap, 256 KiB response cap, zero body/key leakage, exact `ai_core.py` headers/provider/instructions/tolerances), `clean()`, `config()`, `api_key()` (exact `OPENROUTER_API_KEY` + Doppler `project`/`config`/`scope` `--attempts 1 --no-read-env` 15s timeout), `judge()`, `rank()` (batching + `std::thread::scope` max 4 workers + local fallback), `collect_panes()` (pane-only `@workspace_ai_share`, `crate::sha256::hex` fingerprint, post-capture consent re-check).
- Implement `src/ai/panes.rs`: `find()`, `attention()`, `name_tab()` (NFC normalization via `unicode-normalization`, candidate safety filters, pre-request consent check).
- Implement `src/ai/history.rs`: `search()` (`libc::openat` with `O_NOFOLLOW` + `fstat` checking `uid`, `mode & 0o022 == 0`, `nlink == 1`, newest 3 numeric snapshots, byte budgets, UTF-8 boundary drop without `\u{FFFD}`, raw locations preserved across redaction, double-checked consent).
- Implement `src/ai/suggest.rs`: `suggest()` reading `crate::help_topics::TOPICS` directly + installed `SKILL.md` roots, frontmatter scalar parser, Jev verification.
- Implement `src/ai/mod.rs`: CLI parser, `home`, `choose`, live `fzf` finder (`--_find-reload`, `--_find-preview`, private `0700` dir + immutable `0400` `<32-hex>.json` records via `openat`/`fstat`), `navigate`, `rename_tab`, `sharing`, `check`.
- Wire `"ai"` in `src/cli.rs` directly to `ai::main`.
- Create `tests/ai.rs` porting 100% of `test_ai_boundaries.py`, `test_transport.py`, `test_pane_decisions.py`, `test_history_ai.py`, `test_ai_actions.py`, plus `suggest.rs` tests and `#[ignore]` live-Jev eval test.

### Task 10: Opt-in Background Auto-Update (`src/update.rs`)
- Create `src/update.rs` implementing:
  - `main(args)` handling `update`, `update --check`, `update --auto [on|off|status]`.
  - `maybe_auto_update(tmux)` called at end of `snapshot::tick` (24h rate-limit via `<state_dir>/last-update-check`, non-blocking `flock` `<state_dir>/update.lock`, resets child `umask 022` before running `bash <share>/install.sh --update`, status-line notice only on actual version update, quiet `errors.log` on failure).
- Wire `src/cli.rs` `"update"` → `update::main(args)` and `src/snapshot/restore.rs::tick` → `update::maybe_auto_update(tmux)`.
- Show `auto-update: on (daily)` / `off (work update --auto on)` in `src/doctor.rs`.
- Add unit & integration tests in `src/update.rs` and `tests/cli.rs`.

### Task 11: Full Python Runtime Retirement, `tests/agent_status.rs`, Installer & Docs
- Port `tests/test_agent_status_tmux.py` to native Rust `tests/agent_status.rs` (including new `agy` staleness guard & `Ctrl+a F` `report` popup tests!).
- Delete `legacy/`, `integrations/agent-hook.py`, `scripts/gen_help.py`, `tests/compare.rs`, `tests/compare/`, and all retired Python test files (`test_workspace_model.py`, `test_dashboard.py`, `test_dashboard_tmux.py`, `test_cleanup.py`, `test_ai_actions.py`, `test_ai_boundaries.py`, `test_history_ai.py`, `test_pane_decisions.py`, `test_transport.py`, `test_agent_status_tmux.py`, `fixtures.py`, `support.py`, `benchmark_workspace.py`, `live_jev.py`).
- Remove `cli::legacy()`, `cli::python3_9()`, `doctor::python3_version()`, `doctor::developer_tools()`, and `python3 <ver> · ` from `bug.rs`/`report.rs`/`anonymize.rs`; update `tests/{cli,doctor,report}.rs`.
- Update `install.sh` (remove all Python checks/rows/warnings; auto-migrate ALREADY-installed v1 integrations during piped upgrade `[ -n "$UPDATE_FROM" ] && integrations_installed`).
- Update `tests/test_install.py` (drop `legacy/lib/paths.py` import, verify zero `.py` files in release archive, include `integrations/agy-settings.json`, verify piped v1 upgrade migrates hooks to Rust binary, remove Python checklist tests) and `tests/test_portability.py` (drop `legacy/lib` imports, add `work feedback`/`work report`).
- Update `scripts/package.sh`, `scripts/bench.sh`, `Makefile`, `.github/workflows/ci.yml` (remove `python39` job), `tests/install/smoke.sh` (run `dash --once` unconditionally without Python).
- Update `src/help_topics.rs`, `README.md`, `WORKSPACE_GUIDE.md`, and `CHANGELOG.md`.
- **Milestone 3 Gate:** Run `make lint && make test` and `scripts/package.sh` dry-run verification!

### Task 12: Cut & Publish Release `v2.2.0` (so `work update` downloads it!)
- Bump `Cargo.toml` version to `2.2.0`, refresh `Cargo.lock`, finalize `CHANGELOG.md` `## 2.2.0 — 2026-11-24` section.
- Run final `make lint && make test` and `scripts/package.sh` check.
- Commit cleanly, push `main`, tag `v2.2.0`, push tag `v2.2.0`, and monitor GitHub Actions `CI` & `Release` workflows until all 4 archives + `SHA256SUMS` + `VERSION` are published on GitHub Releases!
- Rebuild local live install (`./install.sh --yes --agents` or rebuild `bin/tmux-workspace`) if requested / keep local checkout clean.
