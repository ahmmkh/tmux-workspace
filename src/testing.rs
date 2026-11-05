//! Test support: a private tmux server and temporary directories. Behind the `testing` feature.
//!
//! Nothing here touches the user's tmux server: every tmux call names a scratch socket with `-L`.

use crate::error::{Error, Result};
use crate::host::{Host, RealHost};
use crate::text::py_strip;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Panes end by themselves, so a leftover server does too. `exec`, because dash (Debian's sh) does
/// not hand the pane over to its last command, and the pane would show sh instead of sleep.
const SLEEP: &str = "exec sleep 300";

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A name no other test, process or earlier run is using.
fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!(
        "{}-{nanos:x}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Which of the caller's variables a scratch command must not see: the tmux server and pane it may be
/// running in, the config and Codex folders that would escape its temporary `HOME`, and every workspace
/// setting. `TMUX`, `TMUX_PANE`, `XDG_CONFIG_HOME` and `CODEX_HOME` are always on the list.
fn inherited_to_remove(keys: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut remove: Vec<OsString> = vec![
        "TMUX".into(),
        "TMUX_PANE".into(),
        "XDG_CONFIG_HOME".into(),
        "CODEX_HOME".into(),
    ];
    remove.extend(
        keys.into_iter()
            .filter(|key| key.to_string_lossy().starts_with("TMUX_WORKSPACE_")),
    );
    remove
}

/// Write a runnable script (mode 0755) that is a file of its own. Whatever is at `path` is unlinked
/// first, a link included, and the new file is created exclusively (`set -C`), so a write can never
/// follow a link to a real program (a Homebrew `tmux` or a developer's `fzf`). Only a missing file is
/// no error.
///
/// A `/bin/sh` of its own writes the file, never this process: a child that another test thread forks
/// takes every descriptor of this process along until it execs, and a script that is open for writing
/// anywhere fails to run with "Text file busy".
pub fn write_script(path: &Path, body: &str) {
    use std::io::Write;
    use std::process::Stdio;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("unlink {}: {e}", path.display()),
    }
    let mut writer = Command::new("/bin/sh")
        .args([
            "-c",
            r#"set -C && umask 077 && cat >"$1" && chmod 755 "$1""#,
            "sh",
        ])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("start the writer of {}: {e}", path.display()));
    let stdin = writer.stdin.take().expect("the writer's input");
    // A writer that failed has closed its input; its own message says why.
    let _ = { stdin }.write_all(body.as_bytes());
    let out = writer
        .wait_with_output()
        .unwrap_or_else(|e| panic!("wait for the writer of {}: {e}", path.display()));
    assert!(
        out.status.success(),
        "write {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

/// A directory that is removed, with everything in it, when this is dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> TempDir {
        let path = std::env::temp_dir().join(format!("tw-test-{}", unique()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Default for TempDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A tmux server on its own socket that never loads the user's config or state: session `alpha`,
/// window `one`, 120x40, running `sleep`. Dropping it kills the server.
pub struct ScratchTmux {
    name: String,
    socket: String,
    state: TempDir,
    home: TempDir,
}

impl ScratchTmux {
    pub fn new() -> ScratchTmux {
        let mut scratch = ScratchTmux {
            name: format!("tw-test-{}", unique()),
            socket: String::new(),
            state: TempDir::new(),
            home: TempDir::new(),
        };
        scratch.tmux(&[
            "new-session",
            "-d",
            "-s",
            "alpha",
            "-n",
            "one",
            "-x",
            "120",
            "-y",
            "40",
            SLEEP,
        ]);
        scratch.socket = scratch.expand("alpha", "#{socket_path}");
        scratch
    }

    /// A command that runs in this scratch world: no inherited tmux or workspace variables, state in a
    /// temporary directory, and a temporary `HOME`.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        for key in inherited_to_remove(std::env::vars_os().map(|(key, _)| key)) {
            command.env_remove(key);
        }
        command
            .env("TMUX_WORKSPACE_STATE_DIR", self.state.path())
            .env("HOME", self.home.path())
            .env("TERM", "xterm-256color");
        command
    }

    /// Run tmux on the scratch server and return what it did, whether or not it succeeded.
    pub fn run(&self, args: &[&str]) -> Output {
        // -u: UTF-8 output even without a UTF-8 locale; tmux 3.3 prints ● as _ otherwise.
        self.command("tmux")
            .args(["-u", "-L", &self.name, "-f", "/dev/null"])
            .args(args)
            .output()
            .expect("run tmux")
    }

    /// Run tmux on the scratch server. It must succeed; the output has no trailing newlines.
    pub fn tmux(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string()
    }

    /// Start a pane running `sleep` and return its id. `args` is a tmux command with its target, for
    /// example `["split-window", "-t", "alpha:one"]`. Returns once the pane runs `sleep` rather than the shell
    /// that started it, because the agent notifications of a newborn shell pane are deliberately suppressed.
    pub fn pane(&self, args: &[&str]) -> String {
        let (command, rest) = args.split_first().expect("pane needs a tmux command");
        let mut full = vec![*command, "-d", "-P", "-F", "#{pane_id}"];
        full.extend(rest);
        full.push(SLEEP);
        let pane = self.tmux(&full);
        wait_for(|| self.expand(&pane, "#{pane_current_command}"), "sleep");
        pane
    }

    /// The server's socket path.
    pub fn socket(&self) -> &str {
        &self.socket
    }

    /// Expand a tmux format for a target.
    pub fn expand(&self, target: &str, format: &str) -> String {
        self.tmux(&["display-message", "-t", target, "-p", format])
    }

    /// `value` as this server's tmux prints it with `-p`, `show-options -v` or `display-message`: byte for
    /// byte on every version but 3.4, which prints each `$` as `\$`. That is a known issue: the product
    /// reads a stored `a$b` as `a\$b` there. A test of what is stored compares with this, so it is exact on
    /// every version and fails on one that corrupts the value.
    pub fn as_printed(&self, value: &str) -> String {
        if prints_dollar_escaped(&self.expand("alpha:one", "#{version}")) {
            value.replace('$', "\\$")
        } else {
            value.to_string()
        }
    }

    /// The state directory the binary under test gets as `TMUX_WORKSPACE_STATE_DIR`.
    pub fn state_dir(&self) -> &Path {
        self.state.path()
    }

    /// The `HOME` the binary under test gets.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// The binary under test (`env!("CARGO_BIN_EXE_tmux-workspace")` in an integration test), set up to talk to
    /// this server: `TMUX=<socket>,1,0`, the state directory, `HOME`, `TERM`, and no `TMUX_PANE`.
    pub fn bin(&self, exe: &str) -> Command {
        let mut command = self.command(exe);
        command.env("TMUX", format!("{},1,0", self.socket));
        command
    }
}

impl Default for ScratchTmux {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScratchTmux {
    fn drop(&mut self) {
        self.run(&["kill-server"]);
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.run(&["list-sessions"]).status.success() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        // tmux leaves the socket file of a dead server.
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// A `tmux` that is a script: it records every call and prints a fixed answer. For the tests of what the
/// client makes of tmux's output, and of how often it asks.
pub struct ScriptedTmux {
    dir: TempDir,
}

impl ScriptedTmux {
    /// A tmux that prints `stdout` and exits with `status`, whatever it is asked.
    pub fn new(stdout: &str, status: i32) -> ScriptedTmux {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("stdout"), stdout).expect("write the answer");
        let script = format!(
            "#!/bin/sh\ndir=\"${{0%/*}}\"\nfor a in \"$@\"; do printf '%s\\037' \"$a\"; done >> \"$dir/calls\"\n\
             printf '\\n' >> \"$dir/calls\"\n/bin/cat \"$dir/stdout\"\nexit {status}\n"
        );
        write_script(&dir.path().join("tmux"), &script);
        ScriptedTmux { dir }
    }

    /// A client that runs the script.
    pub fn client(&self) -> crate::tmux::Tmux {
        crate::tmux::Tmux::new().program(&self.dir.path().join("tmux").to_string_lossy())
    }

    /// The arguments of every call so far, in order.
    pub fn calls(&self) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(self.dir.path().join("calls")).unwrap_or_default();
        text.lines()
            .map(|line| line.split_terminator('\x1f').map(str::to_string).collect())
            .collect()
    }
}

/// One process of a [`FakeHost`]: what `ps` would say about it.
#[derive(Clone, Debug, Default)]
pub struct Proc {
    pub ppid: i32,
    /// As `ps -o tty=` prints it: `ttys001`, no `/dev/`.
    pub tty: String,
    pub comm: String,
    pub args: String,
    pub pgid: i32,
    pub tpgid: i32,
    pub lstart: String,
}

impl Proc {
    /// A process that runs `comm` with no arguments, in no foreground group, with no start time.
    pub fn new(ppid: i32, tty: &str, comm: &str) -> Proc {
        Proc {
            ppid,
            tty: tty.to_string(),
            comm: comm.to_string(),
            args: comm.to_string(),
            ..Proc::default()
        }
    }
}

/// A stand-in for tmux: it sees the whole command line, tmux prefix included.
pub type TmuxScript = Box<dyn Fn(&[String]) -> Result<String>>;

/// How a [`FakeHost`] answers a `tmux` command.
pub enum TmuxMode {
    /// Run the real tmux. Aim it at a scratch server with `-S`.
    Real,
    /// Answer from the test: it sees the whole command line, tmux prefix included.
    Scripted(TmuxScript),
}

/// A machine for the hook: a table of processes that `ps` and `lsof` read, a parent pid, and either
/// the real tmux or a script standing in for it. It records every command it is asked to run.
pub struct FakeHost {
    pub procs: BTreeMap<i32, Proc>,
    pub parent: i32,
    /// What `lsof -a -p <pid> -Ffan` prints, by pid.
    pub lsof: BTreeMap<i32, String>,
    /// Every command line run, in order, tmux and ps included.
    pub calls: RefCell<Vec<Vec<String>>>,
    pub tmux: TmuxMode,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeHost {
    /// No processes, parent pid 0, the real tmux.
    pub fn new() -> FakeHost {
        FakeHost {
            procs: BTreeMap::new(),
            parent: 0,
            lsof: BTreeMap::new(),
            calls: RefCell::new(Vec::new()),
            tmux: TmuxMode::Real,
        }
    }

    /// No processes, parent pid 0, and `script` in place of tmux.
    pub fn scripted(script: impl Fn(&[String]) -> Result<String> + 'static) -> FakeHost {
        FakeHost {
            tmux: TmuxMode::Scripted(Box::new(script)),
            ..FakeHost::new()
        }
    }

    /// The recorded commands of one program, by its name (`tmux`, `ps`, `lsof`).
    pub fn calls_to(&self, program: &str) -> Vec<Vec<String>> {
        self.calls
            .borrow()
            .iter()
            .filter(|call| {
                call.first()
                    .is_some_and(|first| program_of(first) == program)
            })
            .cloned()
            .collect()
    }

    /// The pid's process, or the error `ps -p` ends in.
    fn proc(&self, pid: &str) -> Result<(i32, &Proc)> {
        let found = pid
            .parse::<i32>()
            .ok()
            .and_then(|pid| self.procs.get(&pid).map(|proc| (pid, proc)));
        found.ok_or_else(|| no_such_process(pid))
    }

    /// The output of `ps` for the forms the hook and `save` use. Like the real one, it fails when
    /// nothing matches, and for any form it does not know.
    fn ps(&self, args: &[&str]) -> Result<String> {
        let table = |proc: (i32, &Proc)| {
            let (pid, p) = proc;
            format!("{pid} {} {} {}", p.ppid, p.tty, p.comm)
        };
        let lines: Vec<String> = match args {
            ["-p", pid, "-o", "pid=,ppid=,tty=,comm="] => vec![table(self.proc(pid)?)],
            ["-axo", "pid=,ppid=,tty=,comm="] => {
                self.procs.iter().map(|(&pid, p)| table((pid, p))).collect()
            }
            // One tty, or several separated by commas.
            ["-t", ttys, "-o", "pid=,ppid=,tty=,comm="] => self
                .procs
                .iter()
                .filter(|(_, p)| ttys.split(',').any(|tty| p.tty == tty))
                .map(|(&pid, p)| table((pid, p)))
                .collect(),
            ["-ww", "-p", pid, "-o", "args="] => vec![self.proc(pid)?.1.args.clone()],
            ["-p", pid, "-o", "pgid=,tpgid="] => {
                let p = self.proc(pid)?.1;
                vec![format!("{} {}", p.pgid, p.tpgid)]
            }
            ["-p", pid, "-o", "lstart="] => vec![self.proc(pid)?.1.lstart.clone()],
            ["-axo", "pid=,ppid=,pgid=,tpgid=,lstart="] => self
                .procs
                .iter()
                .map(|(pid, p)| format!("{pid} {} {} {} {}", p.ppid, p.pgid, p.tpgid, p.lstart))
                .collect(),
            ["-t", ttys, "-o", "pid=,ppid=,pgid=,tpgid=,lstart="] => self
                .procs
                .iter()
                .filter(|(_, p)| ttys.split(',').any(|tty| p.tty == tty))
                .map(|(pid, p)| format!("{pid} {} {} {} {}", p.ppid, p.pgid, p.tpgid, p.lstart))
                .collect(),
            // The processes on the ttys and the processes named: the union, as ps selects. A pid that
            // is gone is no error while the ttys select something.
            [
                "-t",
                ttys,
                "-p",
                pids,
                "-o",
                "pid=,ppid=,pgid=,tpgid=,lstart=",
            ] => self
                .procs
                .iter()
                .filter(|(pid, p)| {
                    ttys.split(',').any(|tty| p.tty == tty)
                        || pids.split(',').any(|named| named == pid.to_string())
                })
                .map(|(pid, p)| format!("{pid} {} {} {} {}", p.ppid, p.pgid, p.tpgid, p.lstart))
                .collect(),
            _ => return Err(Error::Msg(format!("FakeHost: unexpected ps {args:?}"))),
        };
        if lines.is_empty() {
            return Err(no_such_process("nothing matched"));
        }
        Ok(py_strip(&lines.join("\n")).to_string())
    }

    fn lsof(&self, args: &[&str]) -> Result<String> {
        match args {
            ["-a", "-p", pid, "-Ffan"] => {
                let found = pid.parse::<i32>().ok().and_then(|pid| self.lsof.get(&pid));
                found
                    .map(|out| py_strip(out).to_string())
                    .ok_or_else(|| no_such_process(pid))
            }
            _ => Err(Error::Msg(format!("FakeHost: unexpected lsof {args:?}"))),
        }
    }
}

/// `/bin/ps` -> `ps`.
fn program_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// What `ps -p` and `lsof -p` end in when there is no such process: a failed, checked command.
fn no_such_process(what: &str) -> Error {
    Error::Msg(format!("FakeHost: no such process: {what}"))
}

impl Host for FakeHost {
    fn run(&self, argv: &[String]) -> Result<String> {
        self.calls.borrow_mut().push(argv.to_vec());
        let Some((program, rest)) = argv.split_first() else {
            return Err(Error::Msg("FakeHost: empty command".into()));
        };
        let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
        match program_of(program) {
            "tmux" => match &self.tmux {
                TmuxMode::Real => RealHost.run(argv),
                TmuxMode::Scripted(script) => script(argv),
            },
            "ps" => self.ps(&rest),
            "lsof" => self.lsof(&rest),
            _ => Err(Error::Msg(format!(
                "FakeHost: unexpected command {program}"
            ))),
        }
    }

    fn parent_pid(&self) -> i32 {
        self.parent
    }
}

/// Whether a tmux of this `#{version}` (3.3a, 3.4, 3.8, next-3.5) prints `$` as `\$`: 3.4, and only 3.4.
fn prints_dollar_escaped(version: &str) -> bool {
    version
        .strip_prefix("3.4")
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_lowercase()))
}

/// Poll `read` until it gives `expected` on `SETTLED_READS` polls in a row, for up to 20 s, then assert
/// that the last read is `expected`. It returns as soon as that holds; the long limit is for a loaded
/// machine, where a new pane can take seconds to start `sleep`.
///
/// One match is not enough. A new pane that is started with `exec sleep 300` is seen on Linux as `sleep`
/// for a millisecond (tmux names the program from the command the pane was given while it cannot read
/// `/proc/<pgrp>/cmdline`), then as the shell that has yet to run the `exec`, and then as `sleep` for
/// good. A first match can be that first moment.
pub fn wait_for(read: impl Fn() -> String, expected: &str) {
    const SETTLED_READS: u32 = 3;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = read();
    let mut matches = 0;
    loop {
        matches = if last == expected { matches + 1 } else { 0 };
        if matches >= SETTLED_READS || Instant::now() >= deadline {
            break;
        }
        // A value that matched is read again soon; one that did not is waited for.
        thread::sleep(Duration::from_millis(if matches > 0 { 20 } else { 50 }));
        last = read();
    }
    assert_eq!(last, expected);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tmux_3_4_prints_a_dollar_escaped() {
        for version in ["3.4", "3.4a"] {
            assert!(prints_dollar_escaped(version), "{version}");
        }
        for version in [
            "3.3", "3.3a", "3.5", "3.8", "3.40", "3.4.1", "next-3.4", "next-3.5", "",
        ] {
            assert!(!prints_dollar_escaped(version), "{version}");
        }
    }

    #[test]
    fn a_dollar_prints_as_the_version_of_this_server_prints_it() {
        let scratch = ScratchTmux::new();
        scratch.tmux(&["set-option", "-g", "@probe", "a$b"]);
        let printed = scratch.tmux(&["show-options", "-gqv", "@probe"]);
        assert_eq!(printed, scratch.as_printed("a$b"));
        assert_ne!(
            printed,
            scratch.as_printed("a\\$b"),
            "a corrupted value is told apart"
        );
    }

    #[test]
    fn temp_dir_exists_until_dropped() {
        let dir = TempDir::new();
        let path = dir.path().to_path_buf();
        assert!(path.is_dir());
        std::fs::write(path.join("file"), "x").unwrap();
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn write_script_replaces_a_link_and_never_writes_through_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        let real = dir.path().join("real-tool");
        std::fs::write(&real, "the real tool").unwrap();
        let link = dir.path().join("tmux");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_script(&link, "#!/bin/sh\nexit 0\n");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "the real tool");
        assert!(!link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&link).unwrap(),
            "#!/bin/sh\nexit 0\n"
        );
        assert_eq!(link.metadata().unwrap().permissions().mode() & 0o777, 0o755);
        // A link to nowhere is replaced as well, and a script that is there is rewritten.
        let dangling = dir.path().join("fzf");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();
        write_script(&dangling, "one");
        write_script(&dangling, "two");
        assert_eq!(std::fs::read_to_string(&dangling).unwrap(), "two");
        assert!(!dir.path().join("nowhere").exists());
    }

    /// Whether this process holds `path` open for writing right now.
    #[allow(clippy::unnecessary_cast)] // dev_t and ino_t differ between macOS and Linux
    fn open_for_writing(path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Ok(target) = std::fs::metadata(path) else {
            return false;
        };
        let Ok(entries) = std::fs::read_dir("/dev/fd") else {
            return false;
        };
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .any(|fd| {
                // SAFETY: fstat and fcntl only read the state of a descriptor; one closed by now fails.
                unsafe {
                    let mut stat: libc::stat = std::mem::zeroed();
                    libc::fstat(fd, &mut stat) == 0
                        && stat.st_dev as u64 == target.dev()
                        && stat.st_ino as u64 == target.ino()
                        && libc::fcntl(fd, libc::F_GETFL) & libc::O_ACCMODE != libc::O_RDONLY
                }
            })
    }

    #[test]
    fn a_fake_is_never_open_for_writing_in_the_test_process() {
        // A child that another test thread forks holds every descriptor of this process until it
        // execs, and a fake that is open for writing anywhere fails to run with "Text file busy".
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let dir = TempDir::new();
        let path = dir.path().join("fake");
        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (path, done) = (path.clone(), Arc::clone(&done));
            thread::spawn(move || {
                let mut seen = false;
                while !done.load(Ordering::Relaxed) {
                    seen |= open_for_writing(&path);
                }
                seen
            })
        };
        let body = format!("#!/bin/sh\n: '{}'\nexit 7\n", "x".repeat(16 << 20));
        for _ in 0..3 {
            write_script(&path, &body);
        }
        done.store(true, Ordering::Relaxed);
        assert!(
            !watcher.join().unwrap(),
            "the test process held the fake open for writing"
        );
        assert_eq!(std::fs::read(&path).unwrap(), body.as_bytes());
        let ran = Command::new(&path).status().unwrap();
        assert_eq!(ran.code(), Some(7));
    }

    #[test]
    fn temp_dirs_do_not_collide() {
        let (a, b) = (TempDir::new(), TempDir::new());
        assert_ne!(a.path(), b.path());
    }

    #[test]
    fn scratch_server_follows_the_python_harness() {
        let s = ScratchTmux::new();
        assert_eq!(
            s.expand(
                "alpha",
                "#{session_name}:#{window_name}:#{window_width}x#{window_height}"
            ),
            "alpha:one:120x40"
        );
        assert_eq!(s.tmux(&["list-sessions", "-F", "#{session_name}"]), "alpha");
        assert!(s.socket().ends_with(&s.name), "{}", s.socket());
    }

    #[test]
    fn scratch_pane_waits_until_the_pane_runs_sleep() {
        let s = ScratchTmux::new();
        let pane = s.pane(&["split-window", "-t", "alpha:one"]);
        assert!(pane.starts_with('%'), "{pane}");
        assert_eq!(s.expand(&pane, "#{pane_current_command}"), "sleep");
        let other = s.pane(&["new-window", "-t", "alpha:"]);
        assert_ne!(pane, other);
    }

    #[test]
    fn dropping_the_scratch_server_stops_it_and_removes_the_socket() {
        let s = ScratchTmux::new();
        let socket = s.socket().to_string();
        let name = s.name.clone();
        assert!(Path::new(&socket).exists());
        drop(s);
        assert!(!Path::new(&socket).exists());
        let alive = Command::new("tmux")
            .args(["-L", &name, "list-sessions"])
            .env_remove("TMUX")
            .output()
            .unwrap();
        assert!(!alive.status.success());
    }

    #[test]
    fn bin_presets_the_environment_of_the_binary_under_test() {
        let s = ScratchTmux::new();
        let out = s
            .bin("sh")
            .args(["-c", r#"printf '%s|%s|%s|%s|%s' "$TMUX" "$TMUX_WORKSPACE_STATE_DIR" "$HOME" "$TERM" "${TMUX_PANE-unset}""#])
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let parts: Vec<&str> = text.split('|').collect();
        assert_eq!(parts[0], format!("{},1,0", s.socket()));
        assert_eq!(parts[1], s.state_dir().to_str().unwrap());
        assert_eq!(parts[2], s.home().to_str().unwrap());
        assert_ne!(parts[1], parts[2]);
        assert_eq!(parts[3], "xterm-256color");
        assert_eq!(parts[4], "unset");
    }

    #[test]
    fn bin_clears_the_callers_pane() {
        let s = ScratchTmux::new();
        let command = s.bin("sh");
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "TMUX_PANE" && value.is_none())
        );
    }

    #[test]
    fn workspace_variables_of_the_caller_are_removed() {
        let keys = [
            "PATH",
            "TMUX_WORKSPACE_PROJECTS",
            "TMUX_WORKSPACE_TRACE",
            "HOME",
            "TMUX_WORKSPACED",
        ]
        .map(OsString::from);
        let remove: Vec<String> = inherited_to_remove(keys)
            .into_iter()
            .map(|k| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            remove,
            [
                "TMUX",
                "TMUX_PANE",
                "XDG_CONFIG_HOME",
                "CODEX_HOME",
                "TMUX_WORKSPACE_PROJECTS",
                "TMUX_WORKSPACE_TRACE"
            ]
        );
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    fn tree() -> FakeHost {
        let mut host = FakeHost::new();
        host.parent = 30;
        host.procs.insert(
            30,
            Proc {
                ppid: 20,
                tty: "ttys001".into(),
                comm: "/usr/local/bin/claude".into(),
                args: "claude --resume abc".into(),
                pgid: 30,
                tpgid: 30,
                lstart: "Thu Oct  1 20:23:29 2026".into(),
            },
        );
        host.procs.insert(
            20,
            Proc {
                lstart: "Mon Sep 28 08:00:00 2026".into(),
                ..Proc::new(1, "ttys001", "-zsh")
            },
        );
        host.procs.insert(
            10,
            Proc {
                lstart: "Mon Sep 28 07:00:00 2026".into(),
                ..Proc::new(1, "??", "launchd-ish")
            },
        );
        host
    }

    #[test]
    fn fake_host_answers_every_ps_form_the_hook_issues() {
        let host = tree();
        let ps = |parts: &[&str]| {
            let mut all = vec!["/bin/ps"];
            all.extend(parts);
            host.run(&argv(&all))
        };
        assert_eq!(
            ps(&["-p", "30", "-o", "pid=,ppid=,tty=,comm="]).unwrap(),
            "30 20 ttys001 /usr/local/bin/claude"
        );
        assert_eq!(
            ps(&["-axo", "pid=,ppid=,tty=,comm="]).unwrap(),
            "10 1 ?? launchd-ish\n20 1 ttys001 -zsh\n30 20 ttys001 /usr/local/bin/claude"
        );
        assert_eq!(
            ps(&["-t", "ttys001", "-o", "pid=,ppid=,tty=,comm="]).unwrap(),
            "20 1 ttys001 -zsh\n30 20 ttys001 /usr/local/bin/claude"
        );
        assert_eq!(
            ps(&["-ww", "-p", "30", "-o", "args="]).unwrap(),
            "claude --resume abc"
        );
        assert_eq!(ps(&["-p", "30", "-o", "pgid=,tpgid="]).unwrap(), "30 30");
        assert_eq!(
            ps(&["-p", "30", "-o", "lstart="]).unwrap(),
            "Thu Oct  1 20:23:29 2026"
        );
        assert_eq!(
            ps(&["-axo", "pid=,ppid=,pgid=,tpgid=,lstart="]).unwrap(),
            "10 1 0 0 Mon Sep 28 07:00:00 2026\n20 1 0 0 Mon Sep 28 08:00:00 2026\n30 20 30 30 Thu Oct  1 20:23:29 2026"
        );
    }

    #[test]
    fn fake_host_fails_like_ps_for_what_it_does_not_know() {
        let host = tree();
        for parts in [
            vec!["-p", "99", "-o", "pid=,ppid=,tty=,comm="],
            vec!["-ww", "-p", "99", "-o", "args="],
            vec!["-p", "99", "-o", "pgid=,tpgid="],
            vec!["-p", "99", "-o", "lstart="],
            vec!["-t", "ttys999", "-o", "pid=,ppid=,tty=,comm="],
            vec!["-p", "30", "-o", "nonsense="],
        ] {
            let mut all = vec!["/bin/ps"];
            all.extend(parts.iter());
            assert!(host.run(&argv(&all)).is_err(), "{parts:?}");
        }
        assert!(host.run(&argv(&["/bin/echo", "hi"])).is_err());
    }

    #[test]
    fn fake_host_serves_lsof_and_the_parent_pid() {
        let mut host = tree();
        host.lsof.insert(30, "fcwd\nn/work".into());
        assert_eq!(
            host.run(&argv(&["/usr/sbin/lsof", "-a", "-p", "30", "-Ffan"]))
                .unwrap(),
            "fcwd\nn/work"
        );
        assert!(
            host.run(&argv(&["lsof", "-a", "-p", "31", "-Ffan"]))
                .is_err()
        );
        assert_eq!(host.parent_pid(), 30);
    }

    #[test]
    fn fake_host_records_every_call_and_filters_them_by_program() {
        let host = FakeHost::scripted(|argv| Ok(argv.join("|")));
        let mut with_ps = host;
        with_ps.procs.insert(5, Proc::new(1, "ttys002", "sh"));
        assert_eq!(
            with_ps
                .run(&argv(&["/opt/homebrew/bin/tmux", "-N", "list-panes"]))
                .unwrap(),
            "/opt/homebrew/bin/tmux|-N|list-panes"
        );
        with_ps
            .run(&argv(&["/bin/ps", "-p", "5", "-o", "lstart="]))
            .unwrap();
        assert_eq!(with_ps.calls.borrow().len(), 2);
        assert_eq!(
            with_ps.calls_to("tmux"),
            [argv(&["/opt/homebrew/bin/tmux", "-N", "list-panes"])]
        );
        assert_eq!(with_ps.calls_to("ps").len(), 1);
        assert!(with_ps.calls_to("lsof").is_empty());
    }

    #[test]
    fn fake_host_passes_tmux_to_the_real_one_in_real_mode() {
        let scratch = ScratchTmux::new();
        let host = FakeHost::new();
        let out = host
            .run(&argv(&[
                "tmux",
                "-N",
                "-S",
                scratch.socket(),
                "display-message",
                "-p",
                "#{session_name}",
            ]))
            .unwrap();
        assert_eq!(out, "alpha");
        assert_eq!(host.calls_to("tmux").len(), 1);
    }
}
