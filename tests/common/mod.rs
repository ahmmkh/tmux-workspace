//! What the integration tests of the pickers and of `help` share: fake programs on a `PATH` of their
//! own, and a pseudo-terminal to run a command on.

#![allow(dead_code)]

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use tmux_workspace::testing::TempDir;

pub const EXE: &str = env!("CARGO_BIN_EXE_tmux-workspace");

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Executable programs in a folder that records how they ran: the arguments of call N in
/// `<name>.args.N` (one per line), what call N read in `<name>.input.N` when its input was a pipe,
/// and the number of calls in `<name>.count`.
pub struct Fakes {
    pub dir: TempDir,
}

impl Fakes {
    pub fn new() -> Fakes {
        Fakes {
            dir: TempDir::new(),
        }
    }

    /// A program `name` that records itself, then runs `after`.
    pub fn add(&self, name: &str, after: &str) {
        let script = format!(
            r#"#!/bin/sh
dir="${{0%/*}}"
n=0
[ -f "$dir/{name}.count" ] && read -r n < "$dir/{name}.count"
n=$((n + 1))
echo "$n" > "$dir/{name}.count"
printf '%s\n' "$@" > "$dir/{name}.args.$n"
[ -t 0 ] || /bin/cat > "$dir/{name}.input.$n"
{after}
"#
        );
        let path = self.dir.path().join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// An fzf that picks `$PICK` the first time and is cancelled (status 130) after that.
    pub fn fzf(&self) {
        self.add(
            "fzf",
            r#"if [ "$n" = 1 ]; then printf '%s\n' "$PICK"; exit 0; fi
exit 130"#,
        );
    }

    pub fn less(&self) {
        self.add("less", "exit 0");
    }

    pub fn read(&self, file: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(file)).ok()
    }

    pub fn calls(&self, name: &str) -> usize {
        self.read(&format!("{name}.count"))
            .map_or(0, |count| count.trim().parse().unwrap())
    }

    pub fn args(&self, name: &str, call: usize) -> Vec<String> {
        self.read(&format!("{name}.args.{call}"))
            .unwrap_or_else(|| panic!("{name} call {call} left no arguments"))
            .lines()
            .map(String::from)
            .collect()
    }

    pub fn input(&self, name: &str, call: usize) -> Option<String> {
        self.read(&format!("{name}.input.{call}"))
    }
}

/// Run `command` with a pseudo-terminal for its input and output, after typing `typed` on it. Give
/// what it wrote to its error output and to the terminal, and its exit status.
pub fn on_a_terminal(mut command: Command, typed: &str) -> (i32, String, String) {
    let (mut master, mut slave) = (0, 0);
    // SAFETY: fills in two file descriptors; the null pointers ask for no name, mode or size.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0, "openpty");
    // SAFETY: both descriptors are new and owned by nobody else.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    // Raw: what is typed is not echoed, and the output's line ends stay as they are.
    // SAFETY: `termios` is plain data that `tcgetattr` fills in before it is used.
    unsafe {
        let mut mode: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(slave.as_raw_fd(), &mut mode), 0);
        libc::cfmakeraw(&mut mode);
        assert_eq!(libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &mode), 0);
    }
    let written = unsafe { libc::write(master.as_raw_fd(), typed.as_ptr().cast(), typed.len()) };
    assert_eq!(written, typed.len() as isize, "type on the terminal");
    command
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    drop(command);
    drop(slave);

    // Everything the child writes must be read, or a full terminal holds it up.
    let reader = thread::spawn(move || {
        let mut shown = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            // SAFETY: reads into `buffer`, which is that long.
            let count = unsafe { libc::read(master.as_raw_fd(), buffer.as_mut_ptr().cast(), 4096) };
            if count <= 0 {
                return shown;
            }
            shown.extend_from_slice(&buffer[..count as usize]);
        }
    });
    let (pid, finished) = (child.id() as i32, Arc::new(AtomicBool::new(false)));
    let watchdog = Arc::clone(&finished);
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(30));
        if !watchdog.load(Ordering::Relaxed) {
            // SAFETY: ends a child that does not finish.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    });
    let out = child.wait_with_output().unwrap();
    finished.store(true, Ordering::Relaxed);
    let shown = text(&reader.join().unwrap());
    (out.status.code().unwrap_or(-1), text(&out.stderr), shown)
}

/// The path the binary shows for itself in previews: the binary, as the home has no link to it.
pub fn self_path() -> String {
    std::fs::canonicalize(EXE).unwrap().to_str().unwrap().into()
}

pub fn shell_quote(text: &str) -> String {
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r#"'"'"'"#))
    }
}

impl Fakes {
    /// An fzf that prints `$PICK` and exits with `status` the first time, and is cancelled (status 130)
    /// after that.
    pub fn fzf_exiting(&self, status: i32) {
        self.add(
            "fzf",
            &format!(
                r#"if [ "$n" = 1 ]; then printf '%s\n' "$PICK"; exit {status}; fi
exit 130"#
            ),
        );
    }

    /// The folder the fakes are in, which is what `PATH` is.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The arguments of every call to the fake `name`, in order.
    pub fn all_args(&self, name: &str) -> Vec<Vec<String>> {
        (1..=self.calls(name))
            .map(|call| self.args(name, call))
            .collect()
    }

    /// A zoxide that lists `list` for `query -l`, knows the answers `(query, folder)` for `query QUERY`
    /// and knows no other. `add` does nothing. It records every call, `add` included.
    pub fn zoxide(&self, list: &str, answers: &[(&str, &str)]) {
        std::fs::write(self.path().join("zoxide.list"), list).unwrap();
        let mut cases = String::new();
        for (query, folder) in answers {
            cases += &format!("  query\\ {query}) printf '%s\\n' '{folder}'; exit 0;;\n");
        }
        self.add(
            "zoxide",
            &format!(
                r#"case "$1 $2" in
  "query -l") /bin/cat "$dir/zoxide.list"; exit 0;;
{cases}  "query "*) exit 1;;
esac
exit 0"#
            ),
        );
    }

    /// The real `tmux` under this name, so that the program under test finds one and no other program
    /// of the developer's.
    pub fn real_tmux(&self) {
        std::os::unix::fs::symlink(find_in_path("tmux"), self.path().join("tmux")).unwrap();
    }

    /// A `tmux` for a run outside tmux: every command goes to the server on `socket`, except
    /// `attach-session`, which is recorded in `attach.args` (one argument per line) and does nothing. So a
    /// test sees where `work` would have attached without a terminal being taken over.
    pub fn tmux_outside(&self, socket: &str) {
        let real = find_in_path("tmux");
        let script = format!(
            r#"#!/bin/sh
dir="${{0%/*}}"
case "$1" in
  attach-session) printf '%s\n' "$@" > "$dir/attach.args"; exit 0;;
esac
exec '{}' -u -S '{socket}' "$@"
"#,
            real.display()
        );
        let path = self.path().join("tmux");
        // Not a write through a link to the real tmux.
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The first `name` on the real `PATH`.
pub fn find_in_path(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{name} is not installed"))
}

/// The number of children of `program` that the trace file shows.
pub fn started(trace: &Path, program: &str) -> usize {
    let text = std::fs::read_to_string(trace).unwrap_or_default();
    text.lines()
        .filter(|line| *line == program || line.starts_with(&format!("{program} ")))
        .count()
}

/// Run `command` with no terminal: a closed input, and its output and error captured.
pub fn quietly(mut command: Command) -> std::process::Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.output().unwrap()
}
