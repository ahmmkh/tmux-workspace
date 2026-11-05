//! Child processes: spawn with a timeout, exec, `which`, and the `TMUX_WORKSPACE_TRACE` log.

use crate::error::{Error, Result, fail};
use std::ffi::{CString, OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Names a file to which every child process start is appended, one line each.
const TRACE_ENV: &str = "TMUX_WORKSPACE_TRACE";

/// What to run and how.
#[derive(Clone, Debug, Default)]
pub struct Spawn {
    /// Program and arguments. The program is looked up on `PATH`.
    pub argv: Vec<String>,
    /// Text for the child's stdin. Without it the child reads an empty stdin, unless
    /// `inherit_stdio` hands it ours.
    pub stdin: Option<String>,
    /// Kill the child, and fail with `timed out: <program>`, when it takes longer.
    pub timeout: Option<Duration>,
    pub cwd: Option<PathBuf>,
    /// Give the child our stdout and stderr (and our stdin, when `stdin` is not set), for
    /// programs that talk to the terminal. Nothing is captured then.
    pub inherit_stdio: bool,
}

/// What a finished child left behind. `status` is the exit code, or minus the signal number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

enum Stream {
    Out,
    Err,
}

/// Run a child to completion. The start is traced (see `TMUX_WORKSPACE_TRACE`).
pub fn spawn(spec: &Spawn) -> Result<Output> {
    let Some((program, args)) = spec.argv.split_first() else {
        return fail("empty command");
    };
    trace(&spec.argv);
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = &spec.cwd {
        command.current_dir(dir);
    }
    command.stdin(match (&spec.stdin, spec.inherit_stdio) {
        (Some(_), _) => Stdio::piped(),
        (None, true) => Stdio::inherit(),
        (None, false) => Stdio::null(),
    });
    let capture = if spec.inherit_stdio {
        Stdio::inherit
    } else {
        Stdio::piped
    };
    command.stdout(capture()).stderr(capture());
    let mut child = command.spawn()?;
    let deadline = spec.timeout.map(|limit| Instant::now() + limit);
    let timed_out = || Error::Msg(format!("timed out: {program}"));

    if let (Some(text), Some(mut pipe)) = (&spec.stdin, child.stdin.take()) {
        let text = text.clone().into_bytes();
        // A child that exits without reading closes the pipe; that is not an error.
        thread::spawn(move || pipe.write_all(&text));
    }
    let (sender, receiver) = mpsc::channel();
    let mut streams = 0;
    if let Some(mut pipe) = child.stdout.take() {
        let sender = sender.clone();
        streams += 1;
        thread::spawn(move || sender.send((Stream::Out, drain(&mut pipe))));
    }
    if let Some(mut pipe) = child.stderr.take() {
        streams += 1;
        thread::spawn(move || sender.send((Stream::Err, drain(&mut pipe))));
    }

    let status = match deadline {
        None => child.wait()?,
        Some(deadline) => match wait_until(&mut child, deadline)? {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(timed_out());
            }
        },
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    for _ in 0..streams {
        // The pipes close with the child, unless a grandchild still holds them open.
        let received = match deadline {
            None => receiver.recv().ok(),
            Some(deadline) => receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .ok(),
        };
        match received {
            Some((Stream::Out, bytes)) => stdout = bytes,
            Some((Stream::Err, bytes)) => stderr = bytes,
            None => return Err(timed_out()),
        }
    }
    Ok(Output {
        status: status
            .code()
            .unwrap_or_else(|| -status.signal().unwrap_or(0)),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Run a command and capture its output, with no timeout.
pub fn run(argv: &[&str]) -> Result<Output> {
    spawn(&Spawn {
        argv: argv.iter().map(|a| a.to_string()).collect(),
        ..Spawn::default()
    })
}

/// Replace this process with the command. Returns only when that fails.
pub fn exec(argv: &[String]) -> Error {
    let Some((program, args)) = argv.split_first() else {
        return Error::Msg("empty command".into());
    };
    trace(argv);
    Error::Io(Command::new(program).args(args).exec())
}

/// Find an executable the way `shutil.which` does: a name with a slash is checked as it is, any
/// other is searched for along `PATH` (`/usr/bin:/bin` when unset). Empty `PATH` entries are skipped.
pub fn which(name: &str) -> Option<PathBuf> {
    which_in(
        name,
        &std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin")),
    )
}

fn which_in(name: &str, path: &OsStr) -> Option<PathBuf> {
    if name.contains('/') {
        return executable(Path::new(name)).then(|| PathBuf::from(name));
    }
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| executable(candidate))
}

fn executable(path: &Path) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    path.is_file() && unsafe { libc::access(c_path.as_ptr(), libc::X_OK) } == 0
}

fn drain(pipe: &mut impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes);
    bytes
}

/// Wait for the child until `deadline`; `None` when it is still running by then.
fn wait_until(child: &mut Child, deadline: Instant) -> Result<Option<ExitStatus>> {
    let mut nap = Duration::from_micros(200);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        thread::sleep(nap.min(left));
        nap = (nap * 2).min(Duration::from_millis(5));
    }
}

/// Append one line for this child to the trace file, if `TMUX_WORKSPACE_TRACE` names one.
fn trace(argv: &[String]) {
    if let Some(file) = std::env::var_os(TRACE_ENV).filter(|f| !f.is_empty()) {
        append_trace(Path::new(&file), argv);
    }
}

/// `<basename of argv[0]> <arguments joined by a space>`, newline-terminated.
fn trace_line(argv: &[String]) -> String {
    let mut words = argv.iter().map(String::as_str);
    let program = words.next().unwrap_or_default();
    let mut line = Path::new(program)
        .file_name()
        .map_or(program, |name| name.to_str().unwrap_or(program))
        .to_string();
    for word in words {
        line.push(' ');
        line.push_str(word);
    }
    line.push('\n');
    line
}

/// One `write` on an `O_APPEND` file, so concurrent writers do not interleave. Tracing never fails the command.
fn append_trace(file: &Path, argv: &[String]) {
    if let Ok(mut out) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
    {
        let _ = out.write_all(trace_line(argv).as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    fn sh(script: &str) -> Spawn {
        Spawn {
            argv: argv(&["sh", "-c", script]),
            ..Spawn::default()
        }
    }

    #[test]
    fn spawn_captures_stdout_stderr_and_status() {
        let out = spawn(&sh("echo out; echo err >&2; exit 3")).unwrap();
        assert_eq!(
            out,
            Output {
                status: 3,
                stdout: "out\n".into(),
                stderr: "err\n".into()
            }
        );
    }

    #[test]
    fn spawn_reports_a_signal_as_a_negative_status() {
        let out = spawn(&sh("kill -9 $$")).unwrap();
        assert_eq!(out.status, -9);
    }

    #[test]
    fn spawn_feeds_stdin_even_when_it_is_larger_than_a_pipe() {
        let text = "x".repeat(1 << 20);
        let out = spawn(&Spawn {
            argv: argv(&["cat"]),
            stdin: Some(text.clone()),
            ..Spawn::default()
        })
        .unwrap();
        assert_eq!(out.stdout.len(), text.len());
    }

    #[test]
    fn spawn_gives_a_child_without_stdin_an_empty_one() {
        let out = spawn(&Spawn {
            argv: argv(&["cat"]),
            ..Spawn::default()
        })
        .unwrap();
        assert_eq!(out.stdout, "");
    }

    #[test]
    fn spawn_runs_in_the_given_directory() {
        let dir = TempDir::new();
        let out = spawn(&Spawn {
            argv: argv(&["pwd", "-P"]),
            cwd: Some(dir.path().to_path_buf()),
            ..Spawn::default()
        })
        .unwrap();
        assert_eq!(
            out.stdout.trim_end(),
            dir.path().canonicalize().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn spawn_with_inherited_stdio_captures_nothing() {
        let out = spawn(&Spawn {
            argv: argv(&["true"]),
            inherit_stdio: true,
            ..Spawn::default()
        })
        .unwrap();
        assert_eq!(
            out,
            Output {
                status: 0,
                stdout: String::new(),
                stderr: String::new()
            }
        );
    }

    #[test]
    fn spawn_reports_a_missing_program_as_an_io_error() {
        match spawn(&Spawn {
            argv: argv(&["tw-no-such-program"]),
            ..Spawn::default()
        }) {
            Err(Error::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn spawn_rejects_an_empty_command() {
        assert!(matches!(spawn(&Spawn::default()), Err(Error::Msg(_))));
    }

    #[test]
    fn spawn_kills_a_child_that_outlives_its_timeout() {
        let dir = TempDir::new();
        let pidfile = dir.path().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 30", pidfile.display());
        let started = Instant::now();
        let r = spawn(&Spawn {
            timeout: Some(Duration::from_millis(300)),
            ..sh(&script)
        });
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            matches!(r, Err(Error::Msg(ref m)) if m == "timed out: sh"),
            "{r:?}"
        );
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The child is reaped: signalling its pid fails with ESRCH.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[test]
    fn spawn_gives_up_on_output_held_open_by_a_grandchild() {
        let started = Instant::now();
        let r = spawn(&Spawn {
            timeout: Some(Duration::from_millis(300)),
            ..sh("sleep 3 & exit 0")
        });
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            matches!(r, Err(Error::Msg(ref m)) if m == "timed out: sh"),
            "{r:?}"
        );
    }

    #[test]
    fn spawn_returns_promptly_when_the_child_beats_its_timeout() {
        let started = Instant::now();
        let out = spawn(&Spawn {
            timeout: Some(Duration::from_secs(10)),
            ..sh("echo hi")
        })
        .unwrap();
        assert_eq!(out.stdout, "hi\n");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn run_captures_without_a_timeout() {
        let out = run(&["sh", "-c", "echo $0", "zero"]).unwrap();
        assert_eq!((out.status, out.stdout.as_str()), (0, "zero\n"));
    }

    #[test]
    fn exec_returns_the_error_when_the_program_cannot_start() {
        assert!(
            matches!(exec(&argv(&["tw-no-such-program"])), Error::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(matches!(exec(&[]), Error::Msg(_)));
    }

    #[test]
    fn trace_line_is_the_basename_and_the_arguments() {
        assert_eq!(
            trace_line(&argv(&["/opt/homebrew/bin/tmux", "display-message", "a b"])),
            "tmux display-message a b\n"
        );
        assert_eq!(trace_line(&argv(&["tmux"])), "tmux\n");
    }

    #[test]
    fn trace_appends_one_line_per_call() {
        let dir = TempDir::new();
        let file = dir.path().join("trace");
        append_trace(&file, &argv(&["/usr/bin/ps", "-axo", "pid="]));
        append_trace(&file, &argv(&["fzf"]));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "ps -axo pid=\nfzf\n"
        );
    }

    #[test]
    fn trace_into_an_unwritable_path_is_silent() {
        append_trace(Path::new("/nonexistent-dir/trace"), &argv(&["tmux"]));
    }

    fn executable(dir: &TempDir, name: &str, mode: u32) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn which_finds_the_first_executable_on_the_path() {
        let first = TempDir::new();
        let second = TempDir::new();
        executable(&first, "plain", 0o644);
        std::fs::create_dir(first.path().join("tool")).unwrap();
        let tool = executable(&second, "tool", 0o755);
        let path = std::env::join_paths([first.path(), second.path()]).unwrap();
        assert_eq!(which_in("tool", &path), Some(tool));
        assert_eq!(which_in("plain", &path), None, "not executable");
        assert_eq!(which_in("absent", &path), None);
    }

    #[test]
    fn which_checks_a_name_with_a_slash_directly() {
        let dir = TempDir::new();
        let tool = executable(&dir, "tool", 0o755);
        assert_eq!(
            which_in(tool.to_str().unwrap(), std::ffi::OsStr::new("")),
            Some(tool)
        );
        assert_eq!(
            which_in("/tw-no-such-dir/tool", std::ffi::OsStr::new("")),
            None
        );
    }

    #[test]
    fn which_finds_sh_on_the_real_path() {
        assert!(which("sh").is_some_and(|p| p.is_absolute()));
        assert_eq!(which("tw-no-such-program"), None);
    }
}
