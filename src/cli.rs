//! The command line: which command runs, and what a failure looks like.

use crate::doctor;
use crate::error::{Error, Result, fail};
use crate::help;
use crate::hook;
use crate::host::RealHost;
use crate::integrations;
use crate::onboarding;
use crate::pane;
use crate::park;
use crate::paths::share_root;
use crate::pick;
use crate::process::{self, Spawn};
use crate::snapshot;
use crate::tmux::Tmux;
use std::io::Write;
use std::time::Duration;

/// How long the check of the Python version may take.
const PYTHON_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// The legacy Python is tested on 3.9, which is what macOS ships as `/usr/bin/python3`.
const PYTHON_CHECK: &str = "import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)";

/// Run the command named by the arguments and return the process exit status.
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if makes_private_files(command(&args)) {
        // SAFETY: `umask` only sets this process's file-creation mask; it cannot fail.
        unsafe { libc::umask(0o077) };
    }
    exit_code(run(&args))
}

/// Whether a command runs with umask 077, so that what it creates, and what its children create,
/// is private: the state, the config and the folders `setup` makes. The agent hook is the exception.
/// It was always a separate program that set no umask, and it writes no files.
fn makes_private_files(command: &str) -> bool {
    command != "agent-hook"
}

/// Whether this runs inside tmux, where a message goes on the status line.
pub fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some_and(|value| !value.is_empty())
}

/// Tell the user something: on the tmux status line inside tmux, else on stdout.
pub fn message(text: &str) {
    message_via(&Tmux::new(), text);
}

/// `message`, through this tmux client.
pub fn message_via(tmux: &Tmux, text: &str) {
    if in_tmux() {
        tmux.quiet(&["display-message", text]);
    } else {
        let _ = writeln!(std::io::stdout(), "{text}");
    }
}

/// The command to run: the first argument, or `open` without one.
fn command(args: &[String]) -> &str {
    args.first().map_or("open", String::as_str)
}

fn run(args: &[String]) -> Result<i32> {
    dispatch(command(args), args.get(1..).unwrap_or_default())
}

/// Run one command and give its exit status.
fn dispatch(command: &str, args: &[String]) -> Result<i32> {
    // Each command is one arm here, added as it is ported.
    match command {
        // The hook never fails, and nothing else may write: it is not subject to the error contract.
        "agent-hook" => Ok(hook::main(args)),
        "setup" => onboarding::setup(args),
        "integrations" => integrations::main(args),
        "doctor" => doctor::doctor(&Tmux::new()),
        // Runs in every new pane before the user's shell, and ends by becoming that shell.
        "pane-start" => pane::pane_start(
            &Tmux::new(),
            args.first().map(String::as_str),
            args.get(1).map(String::as_str),
        )
        .map(|()| 0),
        "exec-shell" => Err(pane::exec_shell(&Tmux::new())),
        "label" => pane::label(&Tmux::new(), args.first().map(String::as_str)).map(|()| 0),
        // At server start (from tmux.conf): bring back the missing sessions and start the autosave.
        "init" => snapshot::initialize(&Tmux::new()).map(|()| 0),
        "tick" => snapshot::tick(&Tmux::new(), &RealHost).map(|()| 0),
        "save" => snapshot::save(&Tmux::new(), &RealHost, true).map(|()| 0),
        "restore" => {
            let restored = snapshot::restore(&Tmux::new(), false)?;
            message(if restored {
                "Missing workspaces restored"
            } else {
                "No missing saved workspaces to restore"
            });
            Ok(0)
        }
        "history" => {
            let tmux = Tmux::new();
            let pane = pane_or_current(&tmux, args.first())?;
            snapshot::history(&tmux, &pane).map(|()| 0)
        }
        "park" => {
            let tmux = Tmux::new();
            let pane = pane_or_current(&tmux, args.first())?;
            park::park(&tmux, &pane).map(|()| 0)
        }
        "unpark" | "choose-park" => {
            let tmux = Tmux::new();
            let (target, direction) = park::unpark_args(args);
            let target = pane_or_current(&tmux, target.as_ref())?;
            park::unpark(&tmux, &target, &direction, command == "choose-park").map(|()| 0)
        }
        // `work NAME` and `prefix s`; without a name, the workspace picker.
        "open" => pick::open_workspace(&Tmux::new(), args.first().map(String::as_str)).map(|()| 0),
        "pick-window" => pick::pick_window(&Tmux::new()).map(|()| 0),
        // The preview of the tab picker: it reads the index from its argument.
        "window-preview" => {
            pick::window_preview(&Tmux::new(), args.first().map(String::as_str));
            Ok(0)
        }
        "new-tab" => pick::new_tab(
            &Tmux::new(),
            args.first().map(String::as_str),
            args.get(1).map(String::as_str),
        )
        .map(|()| 0),
        "kill-workspace" | "delete" | "rm" => {
            pick::delete_workspace(&Tmux::new(), args.first().map(String::as_str)).map(|()| 0)
        }
        // Reads no pane and asks tmux nothing: fzf shows this program's own topics.
        "help" => help::help_interactive().map(|()| 0),
        "help-topic" => {
            help::print_topic(args.first().map_or("", String::as_str));
            Ok(0)
        }
        // Still Python: the stage 2 and 3 ports take these over.
        "dash" | "dashboard" | "ai" | "cleanup" => Err(legacy(command, args)),
        _ => fail(format!("Unknown workspace command: {command}")),
    }
}

/// Hand a command that is not ported yet to `legacy/bin/tmux-workspace`: become `python3` running it.
/// Returns only when that fails. Without a python3 of version 3.9 or later, it says so on stderr and
/// exits 1: the error contract does not apply, as the command is not ours yet.
pub fn legacy(command: &str, args: &[String]) -> Error {
    let Some(python) = python3_9() else {
        let _ = writeln!(
            std::io::stderr(),
            "tmux-workspace {command} needs python3 3.9+ until it is ported (stage 2/3)"
        );
        std::process::exit(1);
    };
    let script = share_root().join("legacy/bin/tmux-workspace");
    if !script.is_file() {
        return Error::Msg(format!(
            "{command} needs {}, which is missing",
            script.display()
        ));
    }
    let mut argv = vec![
        python.to_string_lossy().into_owned(),
        script.to_string_lossy().into_owned(),
        command.to_string(),
    ];
    argv.extend(args.iter().cloned());
    process::exec(&argv)
}

/// The `python3` on `PATH`, when it is version 3.9 or later. One `python3 -c` call.
fn python3_9() -> Option<std::path::PathBuf> {
    let python = process::which("python3")?;
    let check = process::spawn(&Spawn {
        argv: vec![
            python.to_string_lossy().into_owned(),
            "-c".into(),
            PYTHON_CHECK.into(),
        ],
        timeout: Some(PYTHON_CHECK_TIMEOUT),
        ..Spawn::default()
    });
    check.is_ok_and(|out| out.status == 0).then_some(python)
}

/// The pane named, or else the one the command runs in.
fn pane_or_current(tmux: &Tmux, pane: Option<&String>) -> Result<String> {
    match pane {
        Some(pane) => Ok(pane.clone()),
        None => tmux.run(&["display-message", "-p", "#{pane_id}"]),
    }
}

/// The error contract: a command's own status is passed on, end of input is not a failure, and any
/// other error is reported and exits 1.
fn exit_code(result: Result<i32>) -> i32 {
    match result {
        Ok(status) => status,
        Err(Error::Eof) => 0,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "Workspace error: {error}");
            message(&format!("Workspace: {error}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn no_argument_means_open() {
        assert_eq!(command(&args(&[])), "open");
        assert_eq!(command(&args(&["save"])), "save");
        assert_eq!(command(&args(&["park", "right"])), "park");
    }

    #[test]
    fn eof_and_success_exit_zero_without_output() {
        assert_eq!(exit_code(Ok(0)), 0);
        assert_eq!(exit_code(Err(Error::Eof)), 0);
    }

    #[test]
    fn every_command_but_the_hook_runs_with_a_private_umask() {
        for command in ["open", "setup", "save", "pane-start", "exec-shell", "nope"] {
            assert!(makes_private_files(command), "{command}");
        }
        assert!(!makes_private_files("agent-hook"));
    }

    #[test]
    fn a_commands_own_status_is_the_exit_status() {
        assert_eq!(exit_code(Ok(2)), 2);
    }

    #[test]
    fn an_unknown_command_is_an_error() {
        let e = run(&args(&["nope", "x"])).unwrap_err();
        assert_eq!(e.to_string(), "Unknown workspace command: nope");
    }
}
