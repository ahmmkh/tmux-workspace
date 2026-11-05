//! `bug` and `idea`, and what they read: errors.log, the doctor, the opener. The binary runs on a scratch
//! tmux server with a home, a state folder and fake programs of its own, and no run reaches a real
//! browser, clipboard or terminal.

mod common;

use common::{EXE, text};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use tmux_workspace::testing::ScratchTmux;

/// The lines of errors.log in a state folder.
fn log_lines(state: &Path) -> Vec<String> {
    fs::read_to_string(state.join("errors.log"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// Whether `line` starts with a UTC stamp, `2026-11-06 01:12`, and what follows it.
fn after_stamp(line: &str) -> &str {
    let (stamp, rest) = line.split_at(16);
    let digits = |range: std::ops::Range<usize>| stamp[range].bytes().all(|b| b.is_ascii_digit());
    assert!(
        digits(0..4)
            && &stamp[4..5] == "-"
            && digits(5..7)
            && &stamp[7..8] == "-"
            && digits(8..10)
            && &stamp[10..11] == " "
            && digits(11..13)
            && &stamp[13..14] == ":"
            && digits(14..16),
        "no stamp in {line:?}"
    );
    rest
}

// ---------------------------------------------------------------------------------------------
// errors.log and the line that offers a report

#[test]
fn a_failing_foreground_command_logs_one_line_and_offers_a_report() {
    let scratch = ScratchTmux::new();
    let out = scratch.bin(EXE).args(["update", "--now"]).output().unwrap();
    assert_eq!(
        text(&out.stderr),
        "Workspace error: Usage: tmux-workspace update [--check]\nReport it: work bug\n"
    );
    assert_eq!(out.status.code(), Some(1));
    let lines = log_lines(scratch.state_dir());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        after_stamp(&lines[0]),
        " update: Usage: tmux-workspace update [--check]"
    );
    assert!(
        !lines[0].contains("--now"),
        "never the arguments: {lines:?}"
    );
}

#[test]
fn the_log_and_its_folder_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = ScratchTmux::new();
    scratch.bin(EXE).args(["update", "--now"]).output().unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(scratch.state_dir()), 0o700);
    assert_eq!(mode(&scratch.state_dir().join("errors.log")), 0o600);
}

#[test]
fn a_command_that_ends_with_status_2_is_quiet_and_logs_nothing() {
    let scratch = ScratchTmux::new();
    let out = scratch
        .bin(EXE)
        .args(["open", "--update"])
        .output()
        .unwrap();
    assert_eq!(
        text(&out.stderr),
        "Workspace error: A workspace name cannot start with -: --update\n"
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(log_lines(scratch.state_dir()), Vec::<String>::new());
}

#[test]
fn a_background_command_logs_its_failure_but_never_offers_a_report() {
    let scratch = ScratchTmux::new();
    for command in ["save", "tick", "init"] {
        let out = scratch
            .bin(EXE)
            .arg(command)
            .env("TMUX", "/nonexistent/socket,1,0")
            .output()
            .unwrap();
        let stderr = text(&out.stderr);
        assert!(
            stderr.starts_with("Workspace error: "),
            "{command}: {stderr}"
        );
        assert!(!stderr.contains("Report it"), "{command}: {stderr}");
        assert_eq!(out.status.code(), Some(1), "{command}");
    }
    let lines = log_lines(scratch.state_dir());
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(
        after_stamp(&lines[0]),
        " save: tmux list-panes exited with status 1"
    );
    assert_eq!(
        after_stamp(&lines[1]),
        " tick: tmux set-option exited with status 1"
    );
    assert_eq!(
        after_stamp(&lines[2]),
        " init: tmux set-option exited with status 1"
    );
}

#[test]
fn the_hook_stays_silent_when_it_fails_and_the_log_keeps_the_failure() {
    let scratch = ScratchTmux::new();
    let pane = scratch.expand("alpha:one", "#{pane_id}");
    let mut command = scratch.bin(EXE);
    command
        .args(["agent-hook", "claude"])
        // A server that is gone: every tmux call of the hook fails.
        .env("TMUX", "/nonexistent/socket,1,0")
        .env("TMUX_PANE", &pane)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/tmp"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(text(&out.stderr), "");
    let lines = log_lines(scratch.state_dir());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        after_stamp(&lines[0]).starts_with(" agent-hook: tmux "),
        "{lines:?}"
    );
    assert!(!lines[0].contains(&pane), "never an argument: {lines:?}");
}

#[test]
fn a_failure_of_bug_itself_offers_no_report() {
    let scratch = ScratchTmux::new();
    let out = scratch.bin(EXE).args(["bug", "--nope"]).output().unwrap();
    assert!(
        !text(&out.stderr).contains("Report it"),
        "{}",
        text(&out.stderr)
    );
}
