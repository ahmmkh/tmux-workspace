//! The call-count contract: how many tmux and ps processes each scenario starts. A child process
//! start is one line of the `TMUX_WORKSPACE_TRACE` file, `<program> <arguments>`.

use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use tmux_workspace::testing::{ScratchTmux, TempDir};

const EXE: &str = env!("CARGO_BIN_EXE_tmux-workspace");

/// How many processes of this program the trace shows. A line that is just the program counts too.
fn started(trace: &Path, program: &str) -> usize {
    let text = std::fs::read_to_string(trace).unwrap_or_default();
    text.lines()
        .filter(|line| *line == program || line.starts_with(&format!("{program} ")))
        .count()
}

#[test]
fn hook_pre_tool_use_for_bash_starts_no_process() {
    let scratch = ScratchTmux::new();
    let pane = scratch.expand("alpha:one", "#{pane_id}");
    let dir = TempDir::new();
    let trace = dir.path().join("trace");
    let payload = json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "session_id": "s", "cwd": "/tmp"});
    let mut child = scratch
        .bin(EXE)
        .args(["agent-hook", "claude"])
        .env("TMUX_PANE", &pane)
        .env("TMUX_WORKSPACE_TRACE", &trace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert_eq!(started(&trace, "tmux"), 0);
    assert_eq!(started(&trace, "ps"), 0);
    let text = std::fs::read_to_string(&trace).unwrap_or_default();
    assert_eq!(text, "", "no child process of any kind");
}

#[test]
fn the_counter_matches_by_program_name() {
    let dir = TempDir::new();
    let trace = dir.path().join("trace");
    std::fs::write(
        &trace,
        "tmux\ntmux list-panes\nps -p 1\npsql x\ntmuxinator\n",
    )
    .unwrap();
    assert_eq!(started(&trace, "tmux"), 2);
    assert_eq!(started(&trace, "ps"), 1);
    assert_eq!(started(&dir.path().join("absent"), "tmux"), 0);
}

/// A shell that does nothing and exits, so that `pane-start` ends at once and never starts the
/// developer's own shell.
fn quiet_shell(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("quiet-shell");
    tmux_workspace::testing::write_script(&path, "#!/bin/sh\nexit 0\n");
    path
}

/// Run `pane-start` in `pane` with a trace; give the trace file.
fn pane_start(
    scratch: &ScratchTmux,
    pane: &str,
    dir: &Path,
    configure: impl FnOnce(&mut std::process::Command),
) -> std::path::PathBuf {
    let trace = dir.join(format!("trace-{}", std::process::id()));
    let _ = std::fs::remove_file(&trace);
    let mut command = scratch.bin(EXE);
    command
        .arg("pane-start")
        .env("TMUX_PANE", pane)
        .env("TMUX_WORKSPACE_TRACE", &trace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(&mut command);
    let out = command.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "it must not prompt"
    );
    trace
}

#[test]
fn pane_start_with_override_shell_makes_at_most_two_tmux_calls() {
    let scratch = ScratchTmux::new();
    let pane = scratch.expand("alpha:one", "#{pane_id}");
    scratch.tmux(&["set-option", "-p", "-t", &pane, "@workspace_label", "main"]);
    let dir = TempDir::new();
    let shell = quiet_shell(dir.path());
    let run = || {
        pane_start(&scratch, &pane, dir.path(), |command| {
            command.env("TMUX_WORKSPACE_SHELL", &shell);
        })
    };
    // A new pane: one read, one write of its identity.
    let trace = run();
    assert!(
        started(&trace, "tmux") <= 2,
        "{}",
        std::fs::read_to_string(&trace).unwrap()
    );
    assert_eq!(started(&trace, "ps"), 0);
    // A pane that has its identity: one read.
    let trace = run();
    assert_eq!(
        started(&trace, "tmux"),
        1,
        "{}",
        std::fs::read_to_string(&trace).unwrap()
    );
    assert_eq!(started(&trace, "ps"), 0);
}

#[test]
fn pane_start_with_server_shell_makes_at_most_two_tmux_calls() {
    let scratch = ScratchTmux::new();
    let pane = scratch.expand("alpha:one", "#{pane_id}");
    scratch.tmux(&["set-option", "-p", "-t", &pane, "@workspace_label", "main"]);
    let dir = TempDir::new();
    let shell = quiet_shell(dir.path());
    scratch.tmux(&["set-environment", "-g", "SHELL", shell.to_str().unwrap()]);
    let run = || {
        pane_start(&scratch, &pane, dir.path(), |command| {
            command.env("SHELL", "/bin/sh");
        })
    };
    let trace = run();
    assert!(
        started(&trace, "tmux") <= 2,
        "{}",
        std::fs::read_to_string(&trace).unwrap()
    );
    assert_eq!(started(&trace, "ps"), 0);
    let trace = run();
    assert_eq!(
        started(&trace, "tmux"),
        1,
        "{}",
        std::fs::read_to_string(&trace).unwrap()
    );
    assert_eq!(started(&trace, "ps"), 0);
    // It ran the server's shell: the last child process of the trace.
    let text = std::fs::read_to_string(&trace).unwrap();
    assert_eq!(text.lines().last(), Some("quiet-shell"), "{text}");
}

#[test]
fn pane_start_reads_label_with_dollar() {
    let scratch = ScratchTmux::new();
    let pane = scratch.expand("alpha:one", "#{pane_id}");
    scratch.tmux(&["set-option", "-p", "-t", &pane, "@workspace_label", "a$b"]);
    let dir = TempDir::new();
    let shell = quiet_shell(dir.path());
    // Input is closed: a prompt would end at once and replace the label with "shell".
    pane_start(&scratch, &pane, dir.path(), |command| {
        command.env("TMUX_WORKSPACE_SHELL", &shell);
    });
    // Exactly what was set, as this tmux prints it (3.4 escapes the $: see ScratchTmux::as_printed).
    assert_eq!(
        scratch.tmux(&["show-options", "-pqv", "-t", &pane, "@workspace_label"]),
        scratch.as_printed("a$b")
    );
}

/// Two sessions, four tabs, five panes.
fn five_panes() -> ScratchTmux {
    let scratch = ScratchTmux::new();
    scratch.pane(&["split-window", "-t", "alpha:one"]);
    scratch.pane(&["new-window", "-t", "alpha:", "-n", "two"]);
    scratch.tmux(&[
        "new-session",
        "-d",
        "-s",
        "beta",
        "-n",
        "code",
        "exec sleep 300",
    ]);
    scratch.pane(&["new-window", "-t", "beta:", "-n", "db"]);
    assert_eq!(scratch.tmux(&["list-panes", "-a"]).lines().count(), 5);
    scratch
}

/// Run `command` with a trace and give the trace file.
fn traced(scratch: &ScratchTmux, dir: &Path, command: &str) -> std::path::PathBuf {
    let trace = dir.join(format!("trace-{command}"));
    let out = scratch
        .bin(EXE)
        .arg(command)
        .env("TMUX_WORKSPACE_TRACE", &trace)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    trace
}

#[test]
fn save_with_five_panes_makes_at_most_nine_tmux_calls_and_two_ps_calls() {
    let scratch = five_panes();
    let dir = TempDir::new();
    // New panes: their identities are written too, which is the most calls a save makes.
    let trace = traced(&scratch, dir.path(), "save");
    let text = std::fs::read_to_string(&trace).unwrap();
    assert!(started(&trace, "tmux") <= 9, "{text}");
    assert!(started(&trace, "ps") <= 2, "{text}");
    assert_eq!(
        started(&trace, "ps"),
        2,
        "the process table and the Codex scan\n{text}"
    );
    // The two ps calls read the processes on the ttys of the panes, not every process: `ps -ax` takes
    // 140 ms on a Mac with a thousand processes, and the save has 60 ms.
    let mut ttys: Vec<String> = scratch
        .tmux(&["list-panes", "-a", "-F", "#{pane_tty}"])
        .lines()
        .map(|tty| tty.trim_start_matches("/dev/").to_string())
        .collect();
    ttys.sort();
    ttys.dedup();
    assert_eq!(ttys.len(), 5, "a tty for each pane");
    let scans: Vec<&str> = text.lines().filter(|l| l.starts_with("ps ")).collect();
    for (line, columns) in scans
        .iter()
        .zip(["pid=,ppid=,pgid=,tpgid=,lstart=", "pid=,ppid=,tty=,comm="])
    {
        let words: Vec<&str> = line.split(' ').collect();
        assert_eq!(&words[..2], ["ps", "-t"], "{text}");
        assert_eq!(&words[3..], ["-o", columns], "{text}");
        let mut listed: Vec<&str> = words[2].split(',').collect();
        listed.sort();
        assert_eq!(listed, ttys, "{text}");
    }
    // One capture per pane, and nothing else per pane.
    let captures = text
        .lines()
        .filter(|line| line.contains(" capture-pane "))
        .count();
    assert_eq!(captures, 5, "{text}");
    // Saved again, every pane has its identity: one call fewer.
    let again = dir.path().join("again");
    let out = scratch
        .bin(EXE)
        .arg("save")
        .env("TMUX_WORKSPACE_TRACE", &again)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        started(&again, "tmux") <= 8,
        "{}",
        std::fs::read_to_string(&again).unwrap()
    );
    assert!(started(&again, "ps") <= 2);
}

#[test]
fn a_tick_soon_after_a_save_makes_two_tmux_calls_and_no_ps() {
    let scratch = five_panes();
    let dir = TempDir::new();
    traced(&scratch, dir.path(), "save");
    // One read tells that nothing is to be done; the other schedules the next tick.
    let trace = traced(&scratch, dir.path(), "tick");
    assert_eq!(
        started(&trace, "tmux"),
        2,
        "{}",
        std::fs::read_to_string(&trace).unwrap()
    );
    assert_eq!(started(&trace, "ps"), 0);
}

#[test]
fn help_without_a_terminal_starts_no_process() {
    let scratch = ScratchTmux::new();
    let dir = TempDir::new();
    let trace = dir.path().join("trace");
    // The scratch home has no guide, and a pipe is not a terminal: help prints every topic.
    let out = scratch
        .bin(EXE)
        .arg("help")
        .env("TMUX_WORKSPACE_TRACE", &trace)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("=== Dashboard ===\n\n"));
    assert_eq!(started(&trace, "tmux"), 0);
    assert_eq!(started(&trace, "ps"), 0);
    let text = std::fs::read_to_string(&trace).unwrap_or_default();
    assert_eq!(text, "", "no child process of any kind");
}
