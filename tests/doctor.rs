//! `doctor`, ported from `tests/test_integrations.py::Diagnostics`. The binary runs in a world of its
//! own: a temporary `HOME`, a `PATH` that holds fake programs only, and a share folder with fake
//! `tw-clip` and `tw-sound`. The live checks run against a scratch tmux server.

mod common;

use common::{EXE, Fakes, text};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use tmux_workspace::integrations::{Target, install_on};
use tmux_workspace::testing::{ScratchTmux, TempDir};

/// The checkout, whose `integrations/` the fake share folder borrows.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap()
}

/// A machine with a tmux that runs no server, and a share folder.
struct Machine {
    fakes: Fakes,
    share: TempDir,
    home: TempDir,
}

impl Machine {
    fn new() -> Machine {
        let machine = Machine {
            fakes: Fakes::new(),
            share: TempDir::new(),
            home: TempDir::new(),
        };
        machine.tmux("tmux 3.5a");
        machine.tools("pbcopy", "afplay");
        symlink(
            root().join("integrations"),
            machine.share().join("integrations"),
        )
        .unwrap();
        symlink(EXE, machine.share().join("bin/tmux-workspace")).unwrap();
        machine
    }

    fn share(&self) -> &Path {
        self.share.path()
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.home().join(relative)
    }

    /// A tmux that reports `version` and has no server.
    fn tmux(&self, version: &str) {
        self.fakes.stand_in_tmux(&format!(
            r#"case "$1" in
  -V) echo '{version}';;
  *) echo 'no server running on /tmp/tmux-501/default' >&2; exit 1;;
esac"#
        ));
    }

    /// `tw-clip which` and `tw-sound which` in the share folder name these tools.
    fn tools(&self, clip: &str, sound: &str) {
        let bin = self.share().join("bin");
        fs::create_dir_all(&bin).unwrap();
        for (name, answer) in [("tw-clip", clip), ("tw-sound", sound)] {
            let path = bin.join(name);
            tmux_workspace::testing::write_script(&path, &format!("#!/bin/sh\necho '{answer}'\n"));
        }
    }

    /// `doctor` with only the fakes on `PATH`. Its status and what it printed.
    fn doctor(&self) -> (i32, String) {
        self.doctor_with(None)
    }

    fn doctor_with(&self, codex_home: Option<&Path>) -> (i32, String) {
        let mut command = Command::new(EXE);
        command
            .env_clear()
            .env("HOME", self.home())
            .env("PATH", self.fakes.path())
            .env("TMUX_WORKSPACE_SHARE", self.share())
            .arg("doctor");
        if let Some(codex) = codex_home {
            command.env("CODEX_HOME", codex);
        }
        let out = common::quietly(command);
        assert_eq!(text(&out.stderr), "", "doctor writes nothing to stderr");
        (out.status.code().unwrap(), text(&out.stdout))
    }

    /// Register the integrations with the tools found in the home, Codex in `codex_home` when given.
    fn install_agents(&self, codex_home: Option<PathBuf>) {
        install_on(&Target {
            home: self.home().to_path_buf(),
            share: root(),
            codex_home,
            system: "Linux".into(),
        })
        .unwrap();
    }
}

#[test]
fn missing_server_reports_warning_without_starting_a_server() {
    let machine = Machine::new();
    let (code, out) = machine.doctor();
    assert_eq!(code, 1);
    assert!(
        out.contains("WARN  tmux server: no server running"),
        "{out}"
    );
    assert!(
        out.ends_with("\nStart or attach with work, then run tmux-workspace doctor again.\n"),
        "{out}"
    );
    let calls = machine.fakes.all_args("tmux");
    assert_eq!(
        calls.last().unwrap(),
        &["-N", "display-message", "-p", "#{socket_path}"],
        "the server check never starts a server"
    );
    assert!(
        calls.iter().flatten().all(|arg| arg != "capture-pane"),
        "doctor reads no terminal output"
    );
    assert_eq!(calls.len(), 2, "{calls:?}");
}

#[test]
fn doctor_checks_the_tmux_floor() {
    let machine = Machine::new();
    machine.tmux("tmux 3.2a");
    let (_, out) = machine.doctor();
    assert!(
        out.contains("WARN  tmux version: tmux 3.2a (needs 3.3 or newer)"),
        "{out}"
    );
    machine.tmux("tmux next-3.6");
    let (_, out) = machine.doctor();
    assert!(out.contains("OK  tmux version: tmux next-3.6"), "{out}");
    machine.tmux("tmux 3.3");
    assert!(machine.doctor().1.contains("OK  tmux version: tmux 3.3\n"));
    machine.tmux("tmux 10.0");
    assert!(machine.doctor().1.contains("OK  tmux version: tmux 10.0\n"));
}

#[test]
fn doctor_names_clipboard_and_sound_tools() {
    let machine = Machine::new();
    machine.tools("OSC 52", "none");
    let (_, out) = machine.doctor();
    assert!(out.contains("OK  clipboard: OSC 52\n"), "{out}");
    assert!(
        out.contains("      sounds: none (agent sounds stay silent)\n"),
        "{out}"
    );
    assert!(!out.replace("clipboard", "").contains("pbcopy"));
    machine.tools("pbcopy", "afplay");
    let (_, out) = machine.doctor();
    assert!(out.contains("OK  clipboard: pbcopy\n"), "{out}");
    assert!(out.contains("      sounds: afplay\n"), "{out}");
}

#[test]
fn doctor_checks_the_installer_links() {
    let machine = Machine::new();
    fs::create_dir_all(machine.path(".local/bin")).unwrap();
    symlink(
        machine.share().join("bin/tmux-workspace"),
        machine.path(".local/bin/tmux-workspace"),
    )
    .unwrap();
    let (_, out) = machine.doctor();
    assert!(
        out.contains(&format!(
            "OK  {}",
            machine.path(".local/bin/tmux-workspace").display()
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "WARN  {}: missing; run install.sh",
            machine
                .path(".config/tmux-workspace/agent-status.conf")
                .display()
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!("WARN  {}", machine.path(".tmux.conf").display())),
        "{out}"
    );
}

#[test]
fn a_link_to_another_clone_is_a_warning_that_names_where_it_leads() {
    let machine = Machine::new();
    fs::create_dir_all(machine.path(".local/bin")).unwrap();
    let other = machine.path("other-clone/bin");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("tmux-workspace"), "").unwrap();
    symlink(
        other.join("tmux-workspace"),
        machine.path(".local/bin/tmux-workspace"),
    )
    .unwrap();
    let (_, out) = machine.doctor();
    let leads = other.join("tmux-workspace").canonicalize().unwrap();
    assert!(
        out.contains(&format!(
            "WARN  {}: {}",
            machine.path(".local/bin/tmux-workspace").display(),
            leads.display()
        )),
        "{out}"
    );
}

#[test]
fn doctor_does_not_warn_about_agents_that_were_never_installed() {
    let machine = Machine::new();
    let (_, out) = machine.doctor();
    assert!(!out.contains("agent-hook.py"), "{out}");
    assert!(
        out.contains("      agent integrations: not installed (install.sh --agents)\n"),
        "{out}"
    );
    assert!(!out.contains("WARN  agent integrations"), "{out}");
}

#[test]
fn doctor_skips_codex_when_it_is_not_installed() {
    let machine = Machine::new();
    let (_, out) = machine.doctor();
    assert!(!out.contains("WARN  Codex hooks"), "{out}");
    assert!(out.contains("      Codex: not installed\n"), "{out}");
}

#[test]
fn doctor_accepts_rendered_codex_hooks() {
    let machine = Machine::new();
    fs::create_dir(machine.path(".codex")).unwrap();
    machine.install_agents(None);
    let (_, out) = machine.doctor();
    assert!(
        out.contains("OK  Codex hooks: all workspace hooks installed\n"),
        "{out}"
    );
    assert!(out.contains("    Codex /hooks must show these definitions as trusted."));
    assert!(out.contains("OK  agent integrations: Codex\n"), "{out}");
}

#[test]
fn doctor_finds_codex_where_codex_home_says() {
    let machine = Machine::new();
    let custom = machine.path("custom-codex");
    fs::create_dir(&custom).unwrap();
    machine.install_agents(Some(custom.clone()));
    let (_, out) = machine.doctor_with(Some(&custom));
    assert!(
        out.contains("OK  Codex hooks: all workspace hooks installed\n"),
        "{out}"
    );
    // Without CODEX_HOME the same home has no Codex.
    let (_, out) = machine.doctor();
    assert!(out.contains("      Codex: not installed\n"), "{out}");
}

#[test]
fn doctor_reports_a_codex_folder_without_hooks() {
    let machine = Machine::new();
    fs::create_dir(machine.path(".codex")).unwrap();
    let (code, out) = machine.doctor();
    assert_eq!(code, 1);
    assert!(
        out.contains(&format!(
            "WARN  Codex hooks: Cannot read {}",
            machine.path(".codex/hooks.json").display()
        )),
        "{out}"
    );
    fs::write(machine.path(".codex/hooks.json"), "{not json").unwrap();
    let (_, out) = machine.doctor();
    assert!(out.contains("WARN  Codex hooks: Cannot read "), "{out}");
    fs::write(machine.path(".codex/hooks.json"), "[]").unwrap();
    let (_, out) = machine.doctor();
    assert!(out.contains("WARN  Codex hooks: "), "{out}");
    assert!(!out.contains("all workspace hooks installed"), "{out}");
}

#[test]
fn doctor_names_the_missing_codex_hooks() {
    let machine = Machine::new();
    fs::create_dir(machine.path(".codex")).unwrap();
    fs::write(
        machine.path(".codex/hooks.json"),
        json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "mine"}]}]}})
            .to_string(),
    )
    .unwrap();
    let (code, out) = machine.doctor();
    assert_eq!(code, 1);
    assert!(
        out.contains("WARN  Codex hooks: missing: SessionStart, SessionEnd, UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, Stop, Interrupt\n"),
        "{out}"
    );
}

#[test]
fn doctor_reports_agent_hooks_that_are_installed() {
    let machine = Machine::new();
    fs::create_dir(machine.path(".claude")).unwrap();
    fs::create_dir(machine.path(".codex")).unwrap();
    machine.install_agents(None);
    let (_, out) = machine.doctor();
    assert!(
        out.contains("OK  agent integrations: Claude, Codex\n"),
        "{out}"
    );
}

fn write_settings(machine: &Machine, relative: &str, settings: &Value) {
    fs::create_dir_all(machine.path(relative).parent().unwrap()).unwrap();
    fs::write(machine.path(relative), settings.to_string()).unwrap();
}

#[test]
fn doctor_warns_about_old_python_hooks() {
    let machine = Machine::new();
    write_settings(
        &machine,
        ".claude/settings.json",
        &json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command",
            "command": "/usr/bin/python3 /h/.local/lib/tmux-workspace/agent-hook.py claude"}]}]}}),
    );
    let (code, out) = machine.doctor();
    assert_eq!(code, 1);
    let line = out
        .lines()
        .find(|line| line.contains("agent integrations"))
        .unwrap();
    assert_eq!(
        line,
        "WARN  agent integrations: old Python hooks; run install.sh --agents"
    );
    assert!(
        !out.contains("not installed (install.sh --agents)"),
        "{out}"
    );
}

#[test]
fn a_users_own_agent_hook_py_is_not_an_old_python_hook() {
    let machine = Machine::new();
    write_settings(
        &machine,
        ".claude/settings.json",
        &json!({"hooks": {"Stop": [{"hooks": [
            {"type": "command", "command": "python3 /home/u/bin/my-agent-hook.py stop"},
            {"type": "command", "command": "/usr/bin/python3 /home/u/bin/agent-hook.py claude"}]}]}}),
    );
    let (_, out) = machine.doctor();
    assert!(!out.contains("old Python hooks"), "{out}");
    assert!(
        out.contains("      agent integrations: not installed (install.sh --agents)\n"),
        "{out}"
    );
}

#[test]
fn both_real_old_forms_are_old_python_hooks() {
    for command in [
        "/usr/bin/python3 /Users/u/.local/lib/tmux-workspace/agent-hook.py claude",
        "/usr/bin/python3 /Users/u/Projects/tmux-workspace/integrations/agent-hook.py claude",
    ] {
        let machine = Machine::new();
        write_settings(
            &machine,
            ".claude/settings.json",
            &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": command}]}]}}),
        );
        let (_, out) = machine.doctor();
        assert!(
            out.contains("WARN  agent integrations: old Python hooks; run install.sh --agents\n"),
            "{command}\n{out}"
        );
    }
}

#[test]
fn old_python_hooks_win_over_new_ones() {
    let machine = Machine::new();
    fs::create_dir(machine.path(".codex")).unwrap();
    machine.install_agents(None);
    write_settings(
        &machine,
        ".claude/settings.json",
        &json!({"statusLine": {"type": "command",
            "command": "/usr/bin/python3 /h/.local/lib/tmux-workspace/agent-hook.py claude-status"}}),
    );
    let (_, out) = machine.doctor();
    assert!(
        out.contains("WARN  agent integrations: old Python hooks; run install.sh --agents\n"),
        "{out}"
    );
}

#[test]
fn a_users_own_hook_is_not_an_agent_integration() {
    let machine = Machine::new();
    write_settings(
        &machine,
        ".claude/settings.json",
        &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "my-agent-hook run"}]}]}}),
    );
    let (_, out) = machine.doctor();
    assert!(
        out.contains("      agent integrations: not installed (install.sh --agents)\n"),
        "{out}"
    );
}

#[test]
fn doctor_warns_when_another_binary_is_first_on_path() {
    let machine = Machine::new();
    machine.fakes.add("tmux-workspace", "exit 0");
    let (_, out) = machine.doctor();
    let other = machine.fakes.path().join("tmux-workspace");
    assert!(
        out.contains(&format!(
            "WARN  tmux-workspace on PATH: {} is not the installed binary\n",
            other.display()
        )),
        "{out}"
    );
}

#[test]
fn doctor_accepts_the_binary_that_is_first_on_path() {
    let machine = Machine::new();
    let link = machine.fakes.path().join("tmux-workspace");
    symlink(EXE, &link).unwrap();
    let (_, out) = machine.doctor();
    assert!(
        out.contains(&format!("OK  tmux-workspace on PATH: {}\n", link.display())),
        "{out}"
    );
}

#[test]
fn doctor_warns_when_the_binary_is_not_on_path_at_all() {
    let machine = Machine::new();
    let (_, out) = machine.doctor();
    assert!(
        out.contains("WARN  tmux-workspace on PATH: not found on PATH\n"),
        "{out}"
    );
}

#[test]
fn doctor_says_which_python3_the_helpers_would_use() {
    let machine = Machine::new();
    let (_, out) = machine.doctor();
    assert!(
        out.contains("      python3: not found (dash, ai and cleanup)\n"),
        "{out}"
    );
    machine.fakes.add("python3", "echo 'Python 3.11.2'");
    let (_, out) = machine.doctor();
    assert!(
        out.contains("      python3: 3.11.2 (dash, ai and cleanup)\n"),
        "{out}"
    );
}

#[test]
fn the_report_starts_with_the_tools_it_needs() {
    let machine = Machine::new();
    machine.fakes.add("fzf", "exit 0");
    let (_, out) = machine.doctor();
    assert!(out.starts_with("Tmux workspace diagnostics\n\n"), "{out}");
    assert!(
        out.contains(&format!(
            "OK  fzf: {}\n",
            machine.fakes.path().join("fzf").display()
        )),
        "{out}"
    );
    assert!(out.contains("WARN  sesh: not found on PATH\n"), "{out}");
    assert!(out.contains("WARN  zoxide: not found on PATH\n"), "{out}");
}

/// The binary against the scratch server, which has one session `alpha` and no clients.
fn live(scratch: &ScratchTmux) -> (i32, String) {
    let mut command = scratch.bin(EXE);
    command.arg("doctor");
    let out = common::quietly(command);
    assert_eq!(text(&out.stderr), "");
    (out.status.code().unwrap(), text(&out.stdout))
}

#[test]
fn doctor_checks_the_live_settings() {
    let scratch = ScratchTmux::new();
    for (name, value) in [
        ("mouse", "on"),
        ("focus-events", "on"),
        ("set-clipboard", "external"),
        ("default-terminal", "tmux-256color"),
    ] {
        scratch.tmux(&["set-option", "-g", name, value]);
    }
    scratch.tmux(&["set-option", "-g", "@agent_sound", "on"]);
    let (_, out) = live(&scratch);
    assert!(out.contains("\nLive settings\n"), "{out}");
    for line in [
        "OK  tmux server: ",
        "OK  mouse: on",
        "OK  focus-events: on",
        "OK  set-clipboard: external",
        "OK  default-terminal: tmux-256color",
        "    Sounds: on",
        "OK  autosave: last saved not yet",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }

    scratch.tmux(&["set-option", "-g", "mouse", "off"]);
    scratch.tmux(&["set-option", "-gu", "@agent_sound"]);
    scratch.tmux(&["set-option", "-g", "@workspace_save_error", "disk full"]);
    let (code, out) = live(&scratch);
    assert_eq!(code, 1);
    assert!(out.contains("WARN  mouse: off\n"), "{out}");
    assert!(out.contains("    Sounds: unconfigured\n"), "{out}");
    assert!(out.contains("WARN  autosave: disk full\n"), "{out}");
}

#[test]
fn doctor_reads_the_age_of_the_latest_snapshot() {
    let scratch = ScratchTmux::new();
    let (_, out) = live(&scratch);
    assert!(
        out.contains("WARN  snapshot: no readable latest snapshot\n"),
        "{out}"
    );
    let latest = scratch.state_dir().join("latest.json");
    let saved = |ago: f64| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        fs::write(&latest, json!({"saved_at": now - ago}).to_string()).unwrap();
    };
    saved(125.0);
    let (_, out) = live(&scratch);
    assert!(out.contains("OK  snapshot age: 2 minutes\n"), "{out}");
    saved(2000.0);
    let (_, out) = live(&scratch);
    assert!(out.contains("WARN  snapshot age: 33 minutes\n"), "{out}");
    fs::write(&latest, r#"{"saved_at": "yesterday"}"#).unwrap();
    let (_, out) = live(&scratch);
    assert!(
        out.contains("WARN  snapshot: no readable latest snapshot\n"),
        "{out}"
    );
}

#[test]
fn doctor_lists_the_panes_without_reading_them() {
    let scratch = ScratchTmux::new();
    let working = scratch.pane(&["split-window", "-t", "alpha:one"]);
    let silent = scratch.pane(&["split-window", "-t", "alpha:one"]);
    let broken = scratch.pane(&["split-window", "-t", "alpha:one"]);
    scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &working,
        "@workspace_agent",
        "working",
    ]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &working,
        "@workspace_codex",
        &json!({"event": "Stop", "at": now - 5}).to_string(),
    ]);
    scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &broken,
        "@workspace_codex",
        "not json",
    ]);

    let (code, out) = live(&scratch);

    assert_eq!(code, 1);
    assert!(
        out.contains("\nPanes (no prompts or terminal content are read)\n"),
        "{out}"
    );
    let line_of = |pane: &str| {
        out.lines()
            .find(|line| line[line.len().min(33)..].starts_with(&format!("{pane:5} ")))
            .unwrap_or_else(|| panic!("no line for {pane}\n{out}"))
            .to_string()
    };
    let working_line = line_of(&working);
    assert!(working_line.starts_with("alpha:0."), "{working_line}");
    assert!(working_line.contains(" sleep"), "{working_line}");
    assert!(working_line.contains(" working "), "{working_line}");
    assert!(
        working_line.contains("  Stop 5s ago") || working_line.contains("  Stop 6s ago"),
        "{working_line}"
    );
    assert!(
        line_of(&silent).contains(" -       "),
        "{}",
        line_of(&silent)
    );
    assert!(
        line_of(&broken).ends_with("  unreadable Codex event metadata"),
        "{}",
        line_of(&broken)
    );
    assert!(
        out.contains(" warning(s). Ctrl+a r reloads tmux; Ctrl+a M toggles agent sounds.\n"),
        "{out}"
    );
    assert!(
        out.contains("\nNo attached clients\n"),
        "the scratch server has no client: {out}"
    );
}
