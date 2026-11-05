//! The command line: which command runs, and what a failure looks like.

use crate::error::{Error, Result, fail};
use crate::hook;
use crate::onboarding;
use crate::pane;
use crate::tmux::Tmux;
use std::io::Write;

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

/// Tell the user something: on the tmux status line inside tmux, else on stdout.
pub fn message(text: &str) {
    if std::env::var_os("TMUX").is_some_and(|value| !value.is_empty()) {
        Tmux::new().quiet(&["display-message", text]);
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
        // Runs in every new pane before the user's shell, and ends by becoming that shell.
        "pane-start" => pane::pane_start(
            &Tmux::new(),
            args.first().map(String::as_str),
            args.get(1).map(String::as_str),
        )
        .map(|()| 0),
        "exec-shell" => Err(pane::exec_shell(&Tmux::new())),
        "label" => pane::label(&Tmux::new(), args.first().map(String::as_str)).map(|()| 0),
        _ => fail(format!("Unknown workspace command: {command}")),
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
