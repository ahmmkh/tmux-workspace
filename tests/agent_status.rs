//! Check `agent-status.conf` and `tmux.conf` on a scratch tmux server that never loads the real config.
//! Ported from `tests/test_agent_status_tmux.py`, plus live tmux verification of the `agy` staleness guard.

use regex::Regex;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tmux_workspace::hook::TURN_OVER;
use tmux_workspace::testing::{ScratchTmux, TempDir};
use tmux_workspace::text;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const SLEEP: &str = "exec sleep 300";
const SHELL_ZSH: &str = "exec zsh -fc 'read -t 300'";

const BLOCKED: &str = "#[fg=#f7768e]●";
const DONE: &str = "#[fg=#7dcfff]●";
const WORKING: &str = "#[fg=#e0af68]●";
const IDLE: &str = "#[fg=#9ece6a]○";
const ORDER: &[(&str, &str)] = &[
    ("blocked", BLOCKED),
    ("done", DONE),
    ("working", WORKING),
    ("idle", IDLE),
];

fn conf_path() -> PathBuf {
    Path::new(ROOT).join("agent-status.conf")
}

fn tmux_conf_path() -> PathBuf {
    Path::new(ROOT).join("tmux.conf")
}

fn wait_until<T: PartialEq + std::fmt::Debug>(
    mut read: impl FnMut() -> T,
    expected: T,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    let mut last = read();
    while last != expected && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        last = read();
    }
    assert_eq!(
        last, expected,
        "did not reach expected value within {timeout:?}"
    );
}

struct StatusWorld {
    scratch: ScratchTmux,
}

impl StatusWorld {
    fn new() -> Self {
        let scratch = ScratchTmux::new();
        scratch.tmux(&["source-file", conf_path().to_str().unwrap()]);
        scratch.tmux(&["set-option", "-g", "@agent_sound_cmd", "true"]);
        Self { scratch }
    }

    fn status(&self, pane: &str) -> String {
        self.scratch
            .tmux(&["show-options", "-t", pane, "-pqv", "@workspace_agent"])
    }

    fn set_status(&self, pane: &str, value: &str) {
        if value.is_empty() {
            self.scratch
                .tmux(&["set-option", "-t", pane, "-pu", "@workspace_agent"]);
        } else {
            self.scratch
                .tmux(&["set-option", "-t", pane, "-p", "@workspace_agent", value]);
        }
    }

    fn turn_over(&self, pane: &str) -> String {
        self.scratch.tmux(&[
            "set-option",
            "-t",
            pane,
            "-pF",
            "@workspace_agent",
            TURN_OVER,
        ]);
        self.status(pane)
    }

    fn expand(&self, target: &str, format: &str) -> String {
        self.scratch.expand(target, format)
    }

    fn pane_with_cmd(&self, args: &[&str], cmd: &str) -> String {
        let mut argv: Vec<&str> = args.to_vec();
        argv.extend(["-d", "-P", "-F", "#{pane_id}", cmd]);
        self.scratch.tmux(&argv)
    }

    /// Attach a tmux control-mode client so tmux considers the session focused.
    fn control_client(&self, session: &str) -> ControlClient {
        let mut cmd = Command::new("tmux");
        cmd.args([
            "-S",
            self.scratch.socket(),
            "-f",
            "/dev/null",
            "-C",
            "attach-session",
            "-t",
            session,
        ])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        let child = cmd.spawn().expect("spawn tmux -C client");
        wait_until(
            || {
                self.scratch
                    .tmux(&["list-clients", "-t", session, "-F", "#{client_flags}"])
                    .matches("focused")
                    .count()
            },
            1,
            Duration::from_secs(5),
        );
        let name = self
            .scratch
            .tmux(&["list-clients", "-t", session, "-F", "#{client_name}"]);
        ControlClient { child, name }
    }

    /// Attach a client on a PTY so we can send focus-in (`\x1b[I`) and focus-out (`\x1b[O`).
    fn terminal_client(&self, session: &str) -> PtyClient {
        let (master, slave) = open_pty();
        let mut cmd = Command::new("tmux");
        cmd.args([
            "-S",
            self.scratch.socket(),
            "-f",
            "/dev/null",
            "attach-session",
            "-t",
            session,
        ])
        .env("TERM", "xterm-256color")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
        // SAFETY: `setsid` and `ioctl(0, TIOCSCTTY, 0)` are async-signal-safe in pre_exec
        // after Rust has dup2'd stdin (fd 0) to the slave PTY.
        unsafe {
            cmd.pre_exec(move || {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn pty client");
        drop(cmd);
        wait_until(
            || {
                self.scratch
                    .tmux(&["list-clients", "-t", session, "-F", "#{client_flags}"])
                    .contains("focused")
            },
            true,
            Duration::from_secs(5),
        );
        PtyClient {
            child,
            master,
            session: session.to_string(),
        }
    }
}

struct ControlClient {
    child: Child,
    name: String,
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        let _ = self.child.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct PtyClient {
    child: Child,
    master: OwnedFd,
    session: String,
}

impl PtyClient {
    fn set_focus(&self, world: &StatusWorld, on: bool) {
        let seq: &[u8] = if on { b"\x1b[I" } else { b"\x1b[O" };
        // SAFETY: writing 3 bytes to our open PTY master fd.
        unsafe {
            libc::write(self.master.as_raw_fd(), seq.as_ptr().cast(), seq.len());
        }
        wait_until(
            || {
                world
                    .scratch
                    .tmux(&["list-clients", "-t", &self.session, "-F", "#{client_flags}"])
                    .contains("focused")
            },
            on,
            Duration::from_secs(5),
        );
    }
}

impl Drop for PtyClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn open_pty() -> (OwnedFd, OwnedFd) {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    // SAFETY: `openpty` populates `master` and `slave` when it returns 0.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

fn tmux_version() -> (u32, u32) {
    let out = Command::new("tmux").arg("-V").output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    let re = Regex::new(r"(\d+)\.(\d+)").unwrap();
    if let Some(caps) = re.captures(&s) {
        (caps[1].parse().unwrap_or(99), caps[2].parse().unwrap_or(0))
    } else {
        (99, 0)
    }
}

fn report_popup_command() -> String {
    let conf = std::fs::read_to_string(tmux_conf_path()).unwrap();
    let bind_re = Regex::new(r"^bind(?:-key)?\s+(?:-\w+\s+)*F\s").unwrap();
    let bindings: Vec<&str> = conf.lines().filter(|l| bind_re.is_match(l)).collect();
    assert_eq!(bindings.len(), 1, "F is bound once");
    let full_re = Regex::new(r"^bind F display-popup -E (?:-\w \S+ )*'(.*)'$").unwrap();
    let caps = full_re
        .captures(bindings[0])
        .unwrap_or_else(|| panic!("unexpected F binding: {}", bindings[0]));
    caps[1].to_string()
}

// ---------------------------------------------------------------------------------------------
// Parse & config sanity

#[test]
fn parse_configs_no_author_paths_posix_shell_and_report_popup() {
    let world = StatusWorld::new();
    for path in [tmux_conf_path(), conf_path()] {
        world
            .scratch
            .tmux(&["source-file", "-n", path.to_str().unwrap()]);
    }

    for name in [
        "tmux.conf",
        "agent-status.conf",
        "shell.zsh",
        "shell/work.sh",
        "shell/work.fish",
    ] {
        let text = std::fs::read_to_string(Path::new(ROOT).join(name)).unwrap();
        assert!(!text.contains("/Users/"), "{name} contains /Users/");
    }

    let conf = std::fs::read_to_string(tmux_conf_path()).unwrap();
    assert!(conf.contains("set -g default-shell /bin/sh\n"));
    assert!(conf.contains(r#"tmux-workspace" exec-shell"#));
    assert!(!conf.contains(r#""$SHELL" -i"#));

    let cmd = report_popup_command();
    assert!(
        cmd.starts_with(r#""$HOME/.local/bin/tmux-workspace" report"#),
        "{cmd}"
    );
    assert!(!cmd.contains("report --print"), "{cmd}");

    // Status regex matches every known shell.
    let agent_conf = std::fs::read_to_string(conf_path()).unwrap();
    let line3 = agent_conf.lines().nth(2).unwrap();
    let shell_part = line3.split("$,").next().unwrap();
    for shell in [
        "zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "nu", "elvish", "xonsh", "pwsh",
    ] {
        assert!(shell_part.contains(shell), "missing shell {shell}");
    }

    // Real sourcing of tmux.conf produces zero errors.
    let home = TempDir::new();
    let cfg = home.path().join(".config/tmux-workspace");
    std::fs::create_dir_all(&cfg).unwrap();
    std::os::unix::fs::symlink(conf_path(), cfg.join("agent-status.conf")).unwrap();
    let sock_path = home.path().join("home-tmux.sock");
    let sock = sock_path.to_str().unwrap();
    let base = |args: &[&str]| {
        Command::new("tmux")
            .args(["-S", sock, "-f", "/dev/null"])
            .args(args)
            .env("HOME", home.path())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap()
    };
    assert!(
        base(&["new-session", "-d", "-s", "conf", "sleep 30"])
            .status
            .success()
    );
    let res = base(&["source-file", tmux_conf_path().to_str().unwrap()]);
    let _ = base(&["kill-server"]);
    assert!(
        res.status.success(),
        "{}",
        String::from_utf8_lossy(&res.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&res.stderr).trim(), "");
    assert_eq!(String::from_utf8_lossy(&res.stdout).trim(), "");

    // Unclosed brace is rejected by source-file -n.
    let bad = home.path().join("bad.conf");
    std::fs::write(&bad, "set-hook -g client-focus-in { if -F 1 {\n").unwrap();
    let res = Command::new("tmux")
        .args([
            "-S",
            world.scratch.socket(),
            "-f",
            "/dev/null",
            "source-file",
            "-n",
            bad.to_str().unwrap(),
        ])
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert!(!res.status.success());
}

#[test]
fn report_popup_stays_open_until_a_key_is_pressed() {
    let home = TempDir::new();
    let tool = home.path().join(".local/bin/tmux-workspace");
    std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
    std::fs::write(&tool, "#!/bin/sh\necho \"ran: $*\"\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (master, slave) = open_pty();
    let slave_fd = slave.as_raw_fd();
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", &report_popup_command()])
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
            libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let mut popup = cmd.spawn().unwrap();
    let mut reader = std::fs::File::from(master);
    let mut shown = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(5);
    while !String::from_utf8_lossy(&shown).contains("any key") && Instant::now() < deadline {
        let n = reader.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        shown.extend_from_slice(&buf[..n]);
    }
    let text_out = String::from_utf8_lossy(&shown);
    assert!(text_out.contains("ran: report"), "{text_out}");
    assert!(text_out.contains("any key"), "{text_out}");
    std::thread::sleep(Duration::from_millis(400));
    assert!(popup.try_wait().unwrap().is_none(), "popup waits for a key");
    reader.write_all(b"x").unwrap();
    let status = popup.wait().unwrap();
    assert_eq!(status.code(), Some(0));
}

// ---------------------------------------------------------------------------------------------
// Marks & roll-up & agy staleness guard

#[test]
fn marks_roll_up_for_tabs_workspaces_parked_shells_agy_and_width() {
    let world = StatusWorld::new();
    let assert_rolls_up = |panes: &[String], target: &str, option: &str| {
        for (pane, (status, _)) in panes.iter().zip(ORDER.iter()) {
            world.set_status(pane, status);
        }
        for (pane, (_, mark)) in panes.iter().zip(ORDER.iter()) {
            assert_eq!(world.expand(target, &format!("#{{E:{option}}}")), *mark);
            world.set_status(pane, "");
        }
        assert_eq!(world.expand(target, &format!("#{{E:{option}}}")), " ");
    };

    // Tab roll-up
    let mut panes = vec!["alpha:one.0".to_string()];
    for _ in 0..3 {
        panes.push(world.scratch.pane(&["split-window", "-t", "alpha:one"]));
    }
    world
        .scratch
        .tmux(&["select-layout", "-t", "alpha:one", "tiled"]);
    assert_rolls_up(&panes, "alpha:one", "@agent_tab");

    // Pane border marks
    for &(status, mark) in ORDER {
        world.set_status("alpha:one.0", status);
        assert_eq!(
            world.expand("alpha:one.0", "#{E:@agent_pane}"),
            format!("{mark} ")
        );
    }
    world.set_status("alpha:one.0", "");
    assert_eq!(world.expand("alpha:one.0", "#{E:@agent_pane}"), "");

    // Workspace roll-up across tabs
    let mut ws_panes = vec!["alpha:one.0".to_string()];
    for _ in 0..3 {
        ws_panes.push(world.scratch.pane(&["new-window", "-t", "alpha:"]));
    }
    assert_rolls_up(&ws_panes, "alpha", "@agent_workspace");

    // Parked pane counts toward its home workspace
    world
        .scratch
        .tmux(&["new-session", "-d", "-s", "beta", SLEEP]);
    world
        .scratch
        .tmux(&["new-session", "-d", "-s", "__parked", SLEEP]);
    world.scratch.tmux(&[
        "set-option",
        "-t",
        "__parked",
        "-p",
        "@workspace_home",
        "beta",
    ]);
    world.set_status("__parked", "blocked");
    world.set_status("beta", "working");
    assert_eq!(world.expand("beta", "#{E:@agent_workspace}"), BLOCKED);
    assert_eq!(world.expand("alpha", "#{E:@agent_workspace}"), " ");

    // Bash & zsh panes show no mark
    for (cmd, name) in [
        ("exec bash --norc --noprofile -c 'read -t 300'", "bash"),
        (SHELL_ZSH, "zsh"),
    ] {
        let pane = world.pane_with_cmd(&["split-window", "-t", "alpha:one"], cmd);
        wait_until(
            || world.expand(&pane, "#{pane_current_command}"),
            name.to_string(),
            Duration::from_secs(5),
        );
        world.set_status(&pane, "working");
        assert_eq!(world.expand(&pane, "#{E:@agent_pane}"), "");
    }

    // Post-exit `agy` staleness guard in `agent-status.conf`: pane running `sleep` with
    // `@workspace_app` naming `agy` must hide its stale `@workspace_agent`.
    let agy_pane = world
        .scratch
        .pane(&["new-window", "-t", "alpha:", "-n", "agystale"]);
    world.scratch.tmux(&[
        "set-option",
        "-t",
        &agy_pane,
        "-p",
        "@workspace_app",
        r#"{"kind":"agy","id":"conv-1"}"#,
    ]);
    world.set_status(&agy_pane, "working");
    assert_eq!(world.expand(&agy_pane, "#{E:@agent_pane}"), "");
    // Whereas a Claude pane running `sleep` keeps its mark.
    world.scratch.tmux(&[
        "set-option",
        "-t",
        &agy_pane,
        "-p",
        "@workspace_app",
        r#"{"kind":"claude","id":"s-1"}"#,
    ]);
    assert_eq!(
        world.expand(&agy_pane, "#{E:@agent_pane}"),
        format!("{WORKING} ")
    );

    // status-left fits a 20-char workspace name and separates the mark.
    let long = "n".repeat(20);
    world
        .scratch
        .tmux(&["new-session", "-d", "-s", &long, SLEEP]);
    world.set_status(&long, "blocked");
    let conf = std::fs::read_to_string(tmux_conf_path()).unwrap();
    let max_len: usize = conf
        .split("status-left-length ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let actual_w: usize = world
        .expand(&long, "#{w:#{E:status-left}}")
        .parse()
        .unwrap();
    assert!(actual_w <= max_len);
    assert!(
        world
            .expand(&long, "#{E:status-left}")
            .ends_with(&format!("{BLOCKED}#[default] │ "))
    );

    // Tab width stays invariant with and without a mark.
    let widths_fmt = "#{w:#{E:window-status-format}} #{w:#{E:window-status-current-format}}";
    let expected = format!("{} {}", " 0:one ".len(), " 0:one ".len());
    assert_eq!(world.expand("alpha:one", widths_fmt), expected);
    for &(status, _) in ORDER {
        world.set_status("alpha:one.0", status);
        assert_eq!(world.expand("alpha:one", widths_fmt), expected);
    }
}

// ---------------------------------------------------------------------------------------------
// Turn-over, focus, seen, and parked rejoin

#[test]
fn turn_over_visibility_focus_and_seen_hooks() {
    let world = StatusWorld::new();
    let one = "alpha:one.0";
    let two = world
        .scratch
        .pane(&["new-window", "-t", "alpha:", "-n", "two"]);

    // Unattached workspace is not visible -> done.
    world.set_status(one, "working");
    assert_eq!(world.turn_over(one), "done");

    // Control client makes alpha:one visible -> idle; hidden tab alpha:two -> done.
    {
        let client = world.control_client("alpha");
        world.set_status(one, "working");
        assert_eq!(world.turn_over(one), "idle");

        for (before, after) in [
            ("working", "done"),
            ("blocked", "done"),
            ("done", "done"),
            ("idle", "idle"),
            ("", "idle"),
        ] {
            world.set_status(&two, before);
            assert_eq!(world.turn_over(&two), after, "before={before:?}");
        }

        // Viewing a tab makes all its done panes idle while keeping blocked/working.
        let other = world.scratch.pane(&["split-window", "-t", "alpha:two"]);
        let blocked = world.scratch.pane(&["split-window", "-t", "alpha:two"]);
        let working = world.scratch.pane(&["split-window", "-t", "alpha:two"]);
        world.set_status(&two, "done");
        world.set_status(&other, "done");
        world.set_status(&blocked, "blocked");
        world.set_status(&working, "working");
        world.scratch.tmux(&["select-window", "-t", "alpha:two"]);
        wait_until(
            || (world.status(&two), world.status(&other)),
            ("idle".into(), "idle".into()),
            Duration::from_secs(5),
        );
        assert_eq!(
            (world.status(&blocked), world.status(&working)),
            ("blocked".into(), "working".into())
        );

        // Switching workspace makes done panes idle.
        world
            .scratch
            .tmux(&["new-session", "-d", "-s", "beta", SLEEP]);
        world.set_status("beta", "done");
        world
            .scratch
            .tmux(&["switch-client", "-t", "beta", "-c", &client.name]);
        wait_until(
            || world.status("beta"),
            "idle".into(),
            Duration::from_secs(5),
        );

        // Parked pane becomes idle when joining a visible tab.
        world
            .scratch
            .tmux(&["switch-client", "-t", "alpha", "-c", &client.name]);
        world.scratch.tmux(&["select-window", "-t", "alpha:one"]);
        world
            .scratch
            .tmux(&["new-session", "-d", "-s", "__parked", SLEEP]);
        let parked = world.expand("__parked", "#{pane_id}");
        world.scratch.tmux(&[
            "set-option",
            "-t",
            &parked,
            "-p",
            "@workspace_home",
            "alpha",
        ]);
        world.set_status(&parked, "working");
        assert_eq!(world.turn_over(&parked), "done");
        world
            .scratch
            .tmux(&["join-pane", "-t", "alpha:one", "-d", "-s", &parked]);
        wait_until(
            || world.status(&parked),
            "idle".into(),
            Duration::from_secs(5),
        );
    }

    // PTY client focus-out / focus-in.
    let world = StatusWorld::new();
    let _two = world
        .scratch
        .pane(&["new-window", "-t", "alpha:", "-n", "two"]);
    let pty = world.terminal_client("alpha");
    pty.set_focus(&world, false);
    world.set_status("alpha:one.0", "working");
    assert_eq!(world.turn_over("alpha:one.0"), "done");
    world.scratch.tmux(&["select-window", "-t", "alpha:two"]);
    world.scratch.tmux(&["select-window", "-t", "alpha:one"]);
    assert_eq!(world.status("alpha:one.0"), "done");
    pty.set_focus(&world, true);
    wait_until(
        || world.status("alpha:one.0"),
        "idle".into(),
        Duration::from_secs(5),
    );
}

// ---------------------------------------------------------------------------------------------
// PaneZoom (requires tmux >= 3.4 for send-keys -K)

#[test]
fn zoom_shortcut_restores_layout_and_keeps_processes() {
    if tmux_version() < (3, 4) {
        return;
    }
    let world = StatusWorld::new();
    let conf = std::fs::read_to_string(tmux_conf_path()).unwrap();
    let settings: Vec<&str> = conf
        .lines()
        .filter(|l| {
            l.starts_with("set -g prefix ")
                || l.starts_with("bind z ")
                || l.starts_with("set -g status-right ")
        })
        .collect();
    assert!(settings.iter().any(|l| l.starts_with("bind z ")));
    for line in settings {
        let parts = text::shlex_split(line).unwrap();
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        world.scratch.tmux(&refs);
    }
    let pane = world
        .scratch
        .pane(&["split-window", "-t", "alpha:one", "-h"]);
    let client = world.control_client("alpha");
    world.scratch.tmux(&["select-pane", "-t", &pane]);
    let layout = world.expand(&pane, "#{window_layout}");
    let procs = world.scratch.tmux(&[
        "list-panes",
        "-t",
        "alpha:one",
        "-F",
        "#{pane_id}:#{pane_pid}:#{pane_dead}",
    ]);
    for expected in ["1", "0"] {
        world
            .scratch
            .tmux(&["send-keys", "-K", "-c", &client.name, "C-a", "z"]);
        wait_until(
            || world.expand(&pane, "#{window_zoomed_flag}"),
            expected.to_string(),
            Duration::from_secs(5),
        );
        assert_eq!(
            world.expand(&pane, "#{E:status-right}").contains("ZOOM"),
            expected == "1"
        );
        if expected == "1" {
            assert_eq!(
                world.expand(&pane, "#{pane_width}"),
                world.expand(&pane, "#{window_width}")
            );
        }
    }
    assert_eq!(world.expand(&pane, "#{window_layout}"), layout);
    assert_eq!(
        world.scratch.tmux(&[
            "list-panes",
            "-t",
            "alpha:one",
            "-F",
            "#{pane_id}:#{pane_pid}:#{pane_dead}",
        ]),
        procs
    );
}

// ---------------------------------------------------------------------------------------------
// Sound hooks

#[test]
fn sound_plays_once_for_blocked_and_hidden_done_and_stays_silent_for_visible_mute_and_shells() {
    let dir = TempDir::new();
    let log = dir.path().join("log");
    let stub = dir.path().join("stub");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\necho \"$1\" >> {}\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).unwrap();

    let played = |log: &Path| -> Vec<String> {
        std::fs::read_to_string(log)
            .map(|s| s.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default()
    };

    // Blocked plays once per change.
    let world = StatusWorld::new();
    world.scratch.tmux(&[
        "set-option",
        "-g",
        "@agent_sound_cmd",
        stub.to_str().unwrap(),
    ]);
    let hidden = world
        .scratch
        .pane(&["new-window", "-t", "alpha:", "-n", "two"]);
    world.set_status(&hidden, "working");
    world.set_status(&hidden, "blocked");
    wait_until(
        || played(&log),
        vec!["blocked".into()],
        Duration::from_secs(5),
    );
    world.set_status(&hidden, "blocked");
    wait_until(
        || {
            world
                .scratch
                .tmux(&["show-options", "-t", &hidden, "-pqv", "@agent_heard"])
        },
        "blocked".into(),
        Duration::from_secs(5),
    );
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(played(&log), vec!["blocked".to_string()]);

    // Done plays only for a pane that is not visible.
    let _ = std::fs::remove_file(&log);
    world.set_status(&hidden, "working");
    assert_eq!(world.turn_over(&hidden), "done");
    wait_until(|| played(&log), vec!["done".into()], Duration::from_secs(5));
    let _client = world.control_client("alpha");
    let shown = world
        .scratch
        .tmux(&["display-message", "-t", "alpha:one", "-p", "#{pane_id}"]);
    world.set_status(&shown, "working");
    assert_eq!(world.turn_over(&shown), "idle");
    wait_until(
        || {
            world
                .scratch
                .tmux(&["show-options", "-t", &shown, "-pqv", "@agent_heard"])
        },
        "idle".into(),
        Duration::from_secs(5),
    );
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(played(&log), vec!["done".to_string()]);

    // Mute and shell panes stay silent.
    let _ = std::fs::remove_file(&log);
    world
        .scratch
        .tmux(&["set-option", "-g", "@agent_sound", "off"]);
    world.set_status(&hidden, "blocked");
    wait_until(
        || {
            world
                .scratch
                .tmux(&["show-options", "-t", &hidden, "-pqv", "@agent_heard"])
        },
        "blocked".into(),
        Duration::from_secs(5),
    );
    std::thread::sleep(Duration::from_millis(250));
    assert!(played(&log).is_empty());

    world
        .scratch
        .tmux(&["set-option", "-g", "@agent_sound", "on"]);
    let shell = world.pane_with_cmd(&["new-window", "-t", "alpha:", "-n", "three"], SHELL_ZSH);
    wait_until(
        || world.expand(&shell, "#{pane_current_command}"),
        "zsh".into(),
        Duration::from_secs(5),
    );
    world.set_status(&shell, "blocked");
    wait_until(
        || {
            world
                .scratch
                .tmux(&["show-options", "-t", &shell, "-pqv", "@agent_heard"])
        },
        "blocked".into(),
        Duration::from_secs(5),
    );
    std::thread::sleep(Duration::from_millis(250));
    assert!(played(&log).is_empty());
}
