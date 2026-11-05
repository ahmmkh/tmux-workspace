//! The command line: which command runs, and what a failure looks like.

use crate::error::{Error, Result, fail};
use crate::tmux::Tmux;
use std::io::Write;

/// Run the command named by the arguments and return the process exit status.
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    exit_code(run(&args))
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

fn run(args: &[String]) -> Result<()> {
    dispatch(command(args), args.get(1..).unwrap_or_default())
}

fn dispatch(command: &str, _args: &[String]) -> Result<()> {
    // Each command is one arm here, added as it is ported.
    fail(format!("Unknown workspace command: {command}"))
}

/// The error contract: end of input is not a failure; any other error is reported and exits 1.
fn exit_code(result: Result<()>) -> i32 {
    match result {
        Ok(()) | Err(Error::Eof) => 0,
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
        assert_eq!(exit_code(Ok(())), 0);
        assert_eq!(exit_code(Err(Error::Eof)), 0);
    }

    #[test]
    fn an_unknown_command_is_an_error() {
        let e = run(&args(&["nope", "x"])).unwrap_err();
        assert_eq!(e.to_string(), "Unknown workspace command: nope");
    }
}
