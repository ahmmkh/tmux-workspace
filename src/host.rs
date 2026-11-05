//! The seam between the hook and the machine it runs on: running commands and finding its parent.

use crate::error::{Error, Result};
use crate::process::{Spawn, spawn};
use crate::text::py_strip;
use std::time::Duration;

/// How long a child process may run before the hook gives up on it and kills it.
pub const CHILD_TIMEOUT: Duration = Duration::from_secs(2);

/// Everything the hook asks of the system, so a test can stand in for it.
pub trait Host {
    /// Run a command: checked, with a 2 s timeout, stderr discarded, stdout stripped at both ends as Python's
    /// `str.strip()` does.
    fn run(&self, argv: &[String]) -> Result<String>;
    /// The pid of the process that started this one.
    fn parent_pid(&self) -> i32;
}

/// The machine we are on.
pub struct RealHost;

impl Host for RealHost {
    fn run(&self, argv: &[String]) -> Result<String> {
        let out = spawn(&Spawn {
            argv: argv.to_vec(),
            timeout: Some(CHILD_TIMEOUT),
            ..Spawn::default()
        })?;
        if out.status != 0 {
            return Err(Error::Command {
                argv: argv.to_vec(),
                status: out.status,
            });
        }
        Ok(py_strip(&out.stdout).to_string())
    }

    fn parent_pid(&self) -> i32 {
        unsafe { libc::getppid() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn run_strips_the_output_at_both_ends() {
        let out = RealHost.run(&argv(&["printf", "\n  a b \n\n"])).unwrap();
        assert_eq!(out, "a b");
    }

    #[test]
    fn run_strips_like_python_including_the_information_separators() {
        let out = RealHost
            .run(&argv(&["printf", "\\037 a\\034b \\037\\n"]))
            .unwrap();
        assert_eq!(out, "a\x1cb");
    }

    #[test]
    fn run_is_checked() {
        let e = RealHost.run(&argv(&["sh", "-c", "exit 3"])).unwrap_err();
        assert!(
            matches!(e, Error::Command { status: 3, ref argv } if argv[0] == "sh"),
            "{e:?}"
        );
    }

    #[test]
    fn run_discards_stderr() {
        let out = RealHost
            .run(&argv(&["sh", "-c", "echo out; echo err >&2"]))
            .unwrap();
        assert_eq!(out, "out");
    }

    #[test]
    fn run_kills_a_child_after_two_seconds() {
        let started = std::time::Instant::now();
        let e = RealHost.run(&argv(&["sleep", "30"])).unwrap_err();
        assert!(
            matches!(e, Error::Msg(ref m) if m == "timed out: sleep"),
            "{e:?}"
        );
        let took = started.elapsed();
        assert!(
            took >= CHILD_TIMEOUT && took < std::time::Duration::from_secs(10),
            "{took:?}"
        );
    }

    #[test]
    fn parent_pid_is_the_real_one() {
        assert_eq!(RealHost.parent_pid(), unsafe { libc::getppid() });
        assert!(RealHost.parent_pid() > 0);
    }
}
