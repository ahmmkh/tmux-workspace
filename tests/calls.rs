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
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("quiet-shell");
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
    assert_eq!(
        scratch.tmux(&["show-options", "-pqv", "-t", &pane, "@workspace_label"]),
        "a$b"
    );
}
