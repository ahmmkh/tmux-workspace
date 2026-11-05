//! Test support: a private tmux server and temporary directories. Behind the `testing` feature.
//!
//! Nothing here touches the user's tmux server: every tmux call names a scratch socket with `-L`.

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
/// running in, and every workspace setting. `TMUX` and `TMUX_PANE` are always on the list.
fn inherited_to_remove(keys: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut remove: Vec<OsString> = vec!["TMUX".into(), "TMUX_PANE".into()];
    remove.extend(
        keys.into_iter()
            .filter(|key| key.to_string_lossy().starts_with("TMUX_WORKSPACE_")),
    );
    remove
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

/// Poll `read` until it gives `expected`, for up to 5 s, then assert that it does.
pub fn wait_for(read: impl Fn() -> String, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while read() != expected && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(read(), expected);
}

#[cfg(test)]
mod tests {
    use super::*;

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
                "TMUX_WORKSPACE_PROJECTS",
                "TMUX_WORKSPACE_TRACE"
            ]
        );
    }
}
