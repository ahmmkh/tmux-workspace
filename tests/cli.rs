//! The binary's top level: dispatch, the error contract and the process trace.

use tmux_workspace::testing::{ScratchTmux, TempDir};

const EXE: &str = env!("CARGO_BIN_EXE_tmux-workspace");

#[test]
fn unknown_command_reports_and_exits_1() {
    let scratch = ScratchTmux::new();
    let out = scratch.bin(EXE).arg("nope").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Workspace error: Unknown workspace command: nope\n"
    );
    assert_eq!(
        out.stdout, b"",
        "inside tmux the message goes to display-message, not stdout"
    );
    assert_eq!(out.status.code(), Some(1));
    // The display-message call is asserted by the trace test below: no client is attached to read it back.
}

#[test]
fn trace_records_each_child_process() {
    let scratch = ScratchTmux::new();
    let dir = TempDir::new();
    let trace = dir.path().join("trace");
    let out = scratch
        .bin(EXE)
        .arg("nope")
        .env("TMUX_WORKSPACE_TRACE", &trace)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        std::fs::read_to_string(&trace).unwrap(),
        "tmux display-message Workspace: Unknown workspace command: nope\n"
    );
}

#[test]
fn outside_tmux_the_message_goes_to_stdout_and_no_tmux_runs() {
    let scratch = ScratchTmux::new();
    let dir = TempDir::new();
    let trace = dir.path().join("trace");
    let out = scratch
        .bin(EXE)
        .arg("nope")
        .env_remove("TMUX")
        .env("TMUX_WORKSPACE_TRACE", &trace)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Workspace error: Unknown workspace command: nope\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Workspace: Unknown workspace command: nope\n"
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(!trace.exists(), "no child process was started");
}

#[test]
fn no_argument_dispatches_open() {
    // `open` is ported in a later task; until then the dispatcher reports it as unknown.
    let scratch = ScratchTmux::new();
    let out = scratch.bin(EXE).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Workspace error: Unknown workspace command: open\n"
    );
}
