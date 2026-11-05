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
