//! `bug` and `idea`, and what they read: errors.log, the doctor, the opener. The binary runs on a scratch
//! tmux server with a home, a state folder and fake programs of its own, and no run reaches a real
//! browser, clipboard or terminal.

mod common;

use common::{EXE, Fakes, on_a_terminal, quietly, text};
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Stdio};
use tmux_workspace::testing::{ScratchTmux, TempDir, wait_for};

/// The lines of errors.log in a state folder.
fn log_lines(state: &Path) -> Vec<String> {
    fs::read_to_string(state.join("errors.log"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// Whether `line` starts with a UTC stamp, `2026-11-06 01:12`, and what follows it.
fn after_stamp(line: &str) -> &str {
    let (stamp, rest) = line.split_at(16);
    let digits = |range: std::ops::Range<usize>| stamp[range].bytes().all(|b| b.is_ascii_digit());
    assert!(
        digits(0..4)
            && &stamp[4..5] == "-"
            && digits(5..7)
            && &stamp[7..8] == "-"
            && digits(8..10)
            && &stamp[10..11] == " "
            && digits(11..13)
            && &stamp[13..14] == ":"
            && digits(14..16),
        "no stamp in {line:?}"
    );
    rest
}

// ---------------------------------------------------------------------------------------------
// errors.log and the line that offers a report

#[test]
fn a_failing_foreground_command_logs_one_line_and_offers_a_report() {
    let scratch = ScratchTmux::new();
    let out = scratch.bin(EXE).args(["update", "--now"]).output().unwrap();
    assert_eq!(
        text(&out.stderr),
        "Workspace error: Usage: tmux-workspace update [--check]\nReport it: work bug\n"
    );
    assert_eq!(out.status.code(), Some(1));
    let lines = log_lines(scratch.state_dir());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        after_stamp(&lines[0]),
        " update: Usage: tmux-workspace update [--check]"
    );
    assert!(
        !lines[0].contains("--now"),
        "never the arguments: {lines:?}"
    );
}

#[test]
fn the_log_and_its_folder_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = ScratchTmux::new();
    scratch.bin(EXE).args(["update", "--now"]).output().unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(scratch.state_dir()), 0o700);
    assert_eq!(mode(&scratch.state_dir().join("errors.log")), 0o600);
}

#[test]
fn a_command_that_ends_with_status_2_is_quiet_and_logs_nothing() {
    let scratch = ScratchTmux::new();
    let out = scratch
        .bin(EXE)
        .args(["open", "--update"])
        .output()
        .unwrap();
    assert_eq!(
        text(&out.stderr),
        "Workspace error: A workspace name cannot start with -: --update\n"
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(log_lines(scratch.state_dir()), Vec::<String>::new());
}

#[test]
fn a_background_command_logs_its_failure_but_never_offers_a_report() {
    let scratch = ScratchTmux::new();
    for command in ["save", "init"] {
        let out = scratch
            .bin(EXE)
            .arg(command)
            .env("TMUX", "/nonexistent/socket,1,0")
            .output()
            .unwrap();
        let stderr = text(&out.stderr);
        assert!(
            stderr.starts_with("Workspace error: "),
            "{command}: {stderr}"
        );
        assert!(!stderr.contains("Report it"), "{command}: {stderr}");
        assert_eq!(out.status.code(), Some(1), "{command}");
    }
    let lines = log_lines(scratch.state_dir());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        after_stamp(&lines[0]),
        " save: tmux list-panes exited with status 1"
    );
    assert_eq!(
        after_stamp(&lines[1]),
        " init: tmux set-option exited with status 1"
    );
}

#[test]
fn a_failed_autosave_is_logged_although_the_tick_goes_on() {
    let scratch = ScratchTmux::new();
    // The folder of the snapshots cannot be made: a file is in its place. The server is there, so the tick
    // gets as far as the save, which fails, and says so on the status line and in an option, not in its
    // status.
    fs::write(scratch.state_dir().join("snapshots"), "").unwrap();
    let out = scratch.bin(EXE).arg("tick").output().unwrap();
    assert_eq!(
        (text(&out.stderr).as_str(), out.status.code()),
        ("", Some(0))
    );
    let shown = scratch.tmux(&["show-options", "-gv", "@workspace_save_error"]);
    assert!(shown.contains("Cannot write"), "{shown}");
    let lines = log_lines(scratch.state_dir());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        after_stamp(&lines[0]).starts_with(" tick: Cannot write "),
        "{lines:?}"
    );
}

#[test]
fn the_log_has_no_argument_and_no_name_that_came_from_one() {
    let scratch = ScratchTmux::new();
    for args in [
        vec!["globex-merger-2027", "x"],
        vec!["park", "initech-layoffs"],
    ] {
        let out = scratch.bin(EXE).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
    }
    let log = fs::read_to_string(scratch.state_dir().join("errors.log")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        after_stamp(lines[0]),
        " unknown: Unknown workspace command: <arg>"
    );
    assert_eq!(after_stamp(lines[1]), " park: No such pane: <arg>");
    for name in ["globex-merger-2027", "initech-layoffs"] {
        assert!(!log.contains(name), "{name} in {log}");
    }
}

/// `agent-hook MODE` in `pane` of the server `scratch` runs, with this payload.
fn hook(scratch: &ScratchTmux, mode: &str, pane: &str, payload: &str) -> std::process::Output {
    let mut command = scratch.bin(EXE);
    command
        .args(["agent-hook", mode])
        .env("TMUX_PANE", pane)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn an_event_of_an_agent_whose_pane_is_gone_is_no_failure() {
    let scratch = ScratchTmux::new();
    // An agent that outlives its pane, like a shared Codex app-server, goes on telling the hook what it does.
    for (mode, event) in [
        ("codex", "PreToolUse"),
        ("codex", "PostToolUse"),
        ("codex", "Stop"),
        ("claude", "SessionEnd"),
    ] {
        let payload =
            format!(r#"{{"hook_event_name": "{event}", "session_id": "s1", "cwd": "/tmp"}}"#);
        let out = hook(&scratch, mode, "%999", &payload);
        assert_eq!(out.status.code(), Some(0), "{mode} {event}");
        assert_eq!(
            (text(&out.stdout).as_str(), text(&out.stderr).as_str()),
            ("", ""),
            "{mode} {event}"
        );
    }
    assert_eq!(log_lines(scratch.state_dir()), Vec::<String>::new());
}

#[test]
fn the_hook_stays_silent_when_it_fails_and_the_log_keeps_the_failure() {
    let scratch = ScratchTmux::new();
    let pane = scratch.expand("alpha:one", "#{pane_id}");
    let mut command = scratch.bin(EXE);
    command
        .args(["agent-hook", "claude"])
        // A server that is gone: every tmux call of the hook fails.
        .env("TMUX", "/nonexistent/socket,1,0")
        .env("TMUX_PANE", &pane)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/tmp"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(text(&out.stderr), "");
    let lines = log_lines(scratch.state_dir());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        after_stamp(&lines[0]).starts_with(" agent-hook: tmux "),
        "{lines:?}"
    );
    assert!(!lines[0].contains(&pane), "never an argument: {lines:?}");
}

// ---------------------------------------------------------------------------------------------
// The report: what `bug` and `idea` say

/// The program that opens a URL on the machine the tests run on.
const OPENER: &str = if cfg!(target_os = "macos") {
    "open"
} else {
    "xdg-open"
};

/// What the question is.
const QUESTION: &str = "Open a GitHub issue with this? [Y/n]";

/// A scratch tmux server, a home, and fake programs on a `PATH` of their own: the real tmux, an opener, and
/// a `tw-clip` where the binary looks for it, `~/.local/bin`. Every one records how it was called.
struct World {
    scratch: ScratchTmux,
    fakes: Fakes,
    /// The shipped files: a release, as far as the binary can tell.
    share: TempDir,
}

impl World {
    fn new() -> World {
        let fakes = Fakes::new();
        fakes.real_tmux();
        fakes.add(OPENER, "exit 0");
        fakes.add("tw-clip", "exit 0");
        let scratch = ScratchTmux::new();
        // ~/.local/bin is the folder of the fakes, so that the recording of tw-clip lands with theirs.
        fs::create_dir_all(scratch.home().join(".local")).unwrap();
        symlink(fakes.path(), scratch.home().join(".local/bin")).unwrap();
        let share = TempDir::new();
        fs::write(share.path().join("VERSION"), "9.9.9\n").unwrap();
        World {
            scratch,
            fakes,
            share,
        }
    }

    /// The binary inside tmux, with only the fakes on `PATH`, and none of the caller's surroundings.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = self.scratch.bin(EXE);
        command
            .args(args)
            .env("PATH", self.fakes.path())
            .env("TMUX_WORKSPACE_SHARE", self.share.path())
            .env("TERM_PROGRAM", "ghostty")
            .env("USER", "plantedusername")
            .env("LOGNAME", "plantedusername")
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_TTY")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY");
        command
    }

    /// `command`, on a machine that has a screen: a Linux needs `DISPLAY` for its opener.
    fn on_a_desktop(&self, args: &[&str]) -> Command {
        let mut command = self.command(args);
        command.env("DISPLAY", ":0");
        command
    }

    /// `command`, in an SSH session.
    fn over_ssh(&self, args: &[&str]) -> Command {
        let mut command = self.on_a_desktop(args);
        command.env("SSH_CONNECTION", "198.51.100.7 51234 192.0.2.10 22");
        command
    }

    /// Lines for errors.log.
    fn log(&self, lines: &[&str]) {
        fs::write(
            self.scratch.state_dir().join("errors.log"),
            lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }

    /// The URL the opener was given, when it was called once.
    fn opened(&self) -> String {
        assert_eq!(self.fakes.calls(OPENER), 1, "the opener's calls");
        let args = self.fakes.args(OPENER, 1);
        assert_eq!(args.len(), 1, "{args:?}");
        args[0].clone()
    }

    /// What `tw-clip copy` was given on its input, when it was called once.
    fn copied(&self) -> String {
        assert_eq!(self.fakes.calls("tw-clip"), 1, "tw-clip's calls");
        assert_eq!(self.fakes.args("tw-clip", 1), ["copy"]);
        self.fakes
            .input("tw-clip", 1)
            .expect("tw-clip read nothing")
    }
}

fn percent_decode(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            out.push(u8::from_str_radix(&encoded[at + 1..at + 3], 16).unwrap());
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// A query parameter of a link, decoded.
fn param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        .map(percent_decode)
}

/// Whether `word` is in `text` as a whole word.
fn has_word(text: &str, word: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(is_word)
            && !text[at + word.len()..].chars().next().is_some_and(is_word)
    })
}

/// What a program prints, or nothing when this machine does not have it.
fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|out| text(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// A pane that runs a program called `codex`: a copy of this program, which waits for the input of a hook
/// that never comes. (A link to `sleep` would be named `sleep` by tmux, and a copy of `sleep` is killed by
/// macOS.) The folder with the copy has to outlast the pane: macOS kills a program whose file is gone.
fn codex_pane(world: &World, session: &str) -> (String, TempDir) {
    let bin = TempDir::new();
    let program = bin.path().join("codex");
    fs::copy(EXE, &program).unwrap();
    let pane = world.scratch.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        session,
        &format!("exec {} agent-hook claude", program.display()),
    ]);
    wait_for(
        || world.scratch.expand(&pane, "#{pane_current_command}"),
        "codex",
    );
    (pane, bin)
}

#[test]
fn print_shows_the_environment_the_doctor_and_the_last_errors_and_nothing_else() {
    let world = World::new();
    world.fakes.add("fzf", "exit 0");
    world.fakes.add("zoxide", "exit 0");
    world.fakes.add("lazygit", "exit 0");
    world.fakes.add("python3", "echo 'Python 3.15.2'");
    world.fakes.add(
        "zsh",
        "case \"$1\" in --version) echo 'zsh 5.9 (arm-apple-darwin25.0)';; esac",
    );
    world.log(&[
        "2026-11-06 01:12 open: cannot write ~/Projects/folder-2",
        "2026-11-06 01:13 save: tmux list-panes exited with status 1",
    ]);
    let mut command = world.command(&["bug", "--print"]);
    command.env("TMUX_WORKSPACE_SHELL", world.fakes.path().join("zsh"));
    let out = quietly(command);
    let shown = text(&out.stdout);
    assert_eq!(
        (text(&out.stderr).as_str(), out.status.code()),
        ("", Some(0))
    );

    let version = env!("CARGO_PKG_VERSION");
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_env = "musl") {
        "unknown-linux-musl"
    } else {
        "unknown-linux-gnu"
    };
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines[0], "### Environment");
    assert_eq!(
        lines[1],
        format!(
            "tmux-workspace {version} (release, {}-{os})",
            std::env::consts::ARCH
        )
    );
    assert!(lines[2].contains(" · tmux 3."), "{}", lines[2]);
    assert!(lines[2].ends_with(" · zsh 5.9 · Ghostty"), "{}", lines[2]);
    assert_eq!(
        lines[3],
        "python3 3.15 · fzf ✓ · zoxide ✓ · sesh ✗ · lazygit ✓"
    );
    assert_eq!(lines[4], "integrations: claude ✗ codex ✗ omp ✗ nvim ✗");
    assert_eq!(lines[5], "");
    assert_eq!(lines[6], "### Doctor");
    assert!(
        lines[7].starts_with("1 pane · ") && lines[7].ends_with(" warnings · 0 errors"),
        "{}",
        lines[7]
    );
    assert!(
        shown.contains("\nWARN  ~/.local/bin/tmux-workspace: missing; run install.sh\n"),
        "{shown}"
    );
    assert!(
        shown.contains(
            "\n### Last errors (tmux-workspace log)\n\
             2026-11-06 01:12 open: cannot write ~/Projects/folder-2\n\
             2026-11-06 01:13 save: tmux list-panes exited with status 1\n\n\
             Open this link to file the issue; nothing is sent until you submit it there:\n\
             https://github.com/ahmmkh/tmux-workspace/issues/new?template=bug.yml&"
        ),
        "the report, then the link: {shown}"
    );
    assert!(
        shown.ends_with('\n') && shown.lines().last().unwrap().starts_with("https://"),
        "{shown}"
    );
    // No question, no browser, no clipboard.
    assert!(!shown.contains(QUESTION), "{shown}");
    assert_eq!(world.fakes.calls(OPENER) + world.fakes.calls("tw-clip"), 0);
}

#[test]
fn only_the_last_ten_lines_of_the_log_are_in_the_report() {
    let world = World::new();
    let lines: Vec<String> = (1..=15)
        .map(|n| format!("2026-11-06 01:{n:02} open: failure number {n:02}"))
        .collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    world.log(&refs);
    let out = quietly(world.command(&["bug", "--print"]));
    let shown = text(&out.stdout);
    assert!(!shown.contains("number 05"), "{shown}");
    assert!(shown.contains("number 06\n"), "{shown}");
    assert!(
        shown.contains("number 15\n\nOpen this link to file the issue;"),
        "{shown}"
    );
}

#[test]
fn an_empty_log_says_none() {
    let world = World::new();
    let out = quietly(world.command(&["bug", "--print"]));
    assert!(
        text(&out.stdout).contains("### Last errors (tmux-workspace log)\nnone\n\nOpen this link"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn names_that_are_numbers_or_programs_leave_the_report_as_it_is() {
    let world = World::new();
    world.fakes.add("python3", "echo 'Python 3.15.2'");
    world.fakes.add(
        "zsh",
        "case \"$1\" in --version) echo 'zsh 5.9 (arm-apple-darwin25.0)';; esac",
    );
    // The default names of tmux's sessions, and tabs called after what runs in them.
    for (session, tab) in [("0", "zsh"), ("2", "claude")] {
        world.scratch.tmux(&[
            "new-session",
            "-d",
            "-s",
            session,
            "-n",
            tab,
            "exec sleep 300",
        ]);
    }
    let (pane, _program) = codex_pane(&world, "1");
    world.scratch.tmux(&["rename-window", "-t", &pane, "codex"]);
    let mut command = world.command(&["bug", "--print"]);
    command.env("TMUX_WORKSPACE_SHELL", world.fakes.path().join("zsh"));
    let out = quietly(command);
    let shown = text(&out.stdout);
    let lines: Vec<&str> = shown.lines().collect();
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        lines[1].starts_with(&format!("tmux-workspace {version} (release, ")),
        "{}",
        lines[1]
    );
    assert!(lines[2].contains(" · tmux 3."), "{}", lines[2]);
    assert!(lines[2].ends_with(" · zsh 5.9 · Ghostty"), "{}", lines[2]);
    assert!(
        !lines[2].contains("workspace-") && !lines[2].contains("tab-"),
        "{}",
        lines[2]
    );
    assert_eq!(
        lines[3],
        "python3 3.15 · fzf ✗ · zoxide ✗ · sesh ✗ · lazygit ✗"
    );
    assert_eq!(lines[4], "integrations: claude ✗ codex ✗ omp ✗ nvim ✗");
    assert!(
        lines[7].starts_with("4 panes · ") && lines[7].ends_with(" warnings · 0 errors"),
        "{}",
        lines[7]
    );
    // The name of a session that is a number is not hidden, and the program is what it is.
    assert!(
        shown.contains("\nWARN  1:0.0 codex: No Codex status. Review /hooks\n"),
        "{shown}"
    );
    assert!(
        !shown.contains("tab-") && !shown.contains("workspace-"),
        "{shown}"
    );
}

#[test]
fn a_report_is_not_made_when_the_names_of_the_server_cannot_be_read() {
    let world = World::new();
    let pane = world.scratch.expand("alpha:one", "#{pane_id}");
    // A label with a line break in it: tmux's listing cannot be told apart from two rows, so the names that
    // the anonymizer needs cannot be had, and a report would show them.
    world.scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &pane,
        "@workspace_label",
        "secret\nlabel",
    ]);
    let out = quietly(world.command(&["bug", "--print"]));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with("Workspace error: The names of the tmux server could not be read"),
        "{stderr}"
    );
    assert!(!stderr.contains("Report it"), "{stderr}");
    assert!(!stderr.contains("secret"), "{stderr}");
    assert!(stderr.contains("a pane label or a folder"), "{stderr}");
    // On a terminal nothing is shown, asked, opened or copied either.
    let (status, _, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), "");
    assert_eq!((status, shown.as_str()), (1, ""));
    assert_eq!(world.fakes.calls(OPENER) + world.fakes.calls("tw-clip"), 0);
    // An idea has no name of the server in it.
    let out = quietly(world.command(&["idea", "--print"]));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).starts_with("### Environment\n"));
}

#[test]
fn a_report_is_made_when_tmux_cannot_be_run_at_all() {
    // The user whose tmux is broken is the one who wants to report it. A tmux that is on the PATH and cannot
    // start (its interpreter is gone), like one that is not installed, has no names to hide.
    let world = World::new();
    tmux_workspace::testing::write_script(
        &world.fakes.path().join("tmux"),
        "#!/tw-no-such-interpreter\n",
    );
    let out = quietly(world.command(&["bug", "--print"]));
    assert_eq!(
        (text(&out.stderr).as_str(), out.status.code()),
        ("", Some(0))
    );
    let shown = text(&out.stdout);
    let lines: Vec<&str> = shown.lines().collect();
    assert!(lines[2].contains(" · tmux ✗ · "), "{}", lines[2]);
    assert!(shown.contains("\n### Doctor\n0 panes · "), "{shown}");
    assert!(shown.contains("\nWARN  tmux server: "), "{shown}");
    assert!(!shown.contains("could not be read"), "{shown}");
    // And on a terminal the question is asked as ever.
    let (status, _, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), "n\n");
    assert_eq!(status, 0);
    assert!(shown.contains(QUESTION), "{shown}");
}

#[test]
fn without_a_server_there_are_no_names_to_read_and_the_report_is_made() {
    let world = World::new();
    let mut command = world.command(&["bug", "--print"]);
    command.env("TMUX", "/nonexistent/socket,1,0");
    let out = quietly(command);
    assert_eq!(
        (text(&out.stderr).as_str(), out.status.code()),
        ("", Some(0))
    );
    let shown = text(&out.stdout);
    assert!(shown.contains("\n### Doctor\n0 panes · "), "{shown}");
    assert!(shown.contains("\nWARN  tmux server: "), "{shown}");
}

#[test]
fn a_warning_about_another_binary_keeps_its_words_around_the_path() {
    let world = World::new();
    world.fakes.add("tmux-workspace", "exit 0");
    let out = quietly(world.command(&["bug", "--print"]));
    let shown = text(&out.stdout);
    let line = shown
        .lines()
        .find(|line| line.starts_with("WARN  tmux-workspace on PATH: "))
        .unwrap_or_else(|| panic!("no such warning in {shown}"));
    // A path goes on to the end of its segment, so the words that follow it would be taken for its name.
    assert!(
        line.starts_with("WARN  tmux-workspace on PATH: not the installed binary ("),
        "{line}"
    );
    assert!(line.ends_with("/tmux-workspace)"), "{line}");
}

#[test]
fn an_idea_has_only_the_environment() {
    let world = World::new();
    world.log(&["2026-11-06 01:12 open: x"]);
    let out = quietly(world.command(&["idea", "--print"]));
    let shown = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        shown.starts_with("### Environment\ntmux-workspace "),
        "{shown}"
    );
    assert!(
        !shown.contains("### Doctor") && !shown.contains("errors"),
        "{shown}"
    );
    assert!(
        shown.contains("integrations: claude ✗ codex ✗ omp ✗ nvim ✗\n\nOpen this link"),
        "{shown}"
    );
    assert!(shown.contains("template=idea.yml&"), "{shown}");
}

#[test]
fn a_clone_is_a_clone_and_an_unknown_kind_is_said_so() {
    let world = World::new();
    fs::remove_file(world.share.path().join("VERSION")).unwrap();
    let out = quietly(world.command(&["idea", "--print"]));
    assert!(
        text(&out.stdout)
            .lines()
            .nth(1)
            .unwrap()
            .contains(" (unknown, "),
        "{}",
        text(&out.stdout)
    );
    fs::write(world.share.path().join("Cargo.toml"), "").unwrap();
    let out = quietly(world.command(&["idea", "--print"]));
    assert!(
        text(&out.stdout)
            .lines()
            .nth(1)
            .unwrap()
            .contains(" (clone, "),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn a_usage_mistake_is_status_2_with_the_usage() {
    let world = World::new();
    for (command, args) in [("bug", ["--nope"]), ("idea", ["--print=1"])] {
        let out = quietly(world.command(&[command, args[0]]));
        assert_eq!(
            text(&out.stderr),
            format!("Usage: tmux-workspace {command} [--print]\n")
        );
        assert_eq!(
            (text(&out.stdout).as_str(), out.status.code()),
            ("", Some(2))
        );
    }
    assert_eq!(world.fakes.calls(OPENER), 0);
    assert_eq!(log_lines(world.scratch.state_dir()), Vec::<String>::new());
}

// ---------------------------------------------------------------------------------------------
// The privacy promise

/// What the machine knows and a report must not tell: the names of a workspace, the folders, the login
/// and host names, an address, a key.
const EMAIL: &str = "planted.person@secret-corp.example";
const IPV4: &str = "203.0.113.77";
const SECRET_KEY: &str = "sk-live-51HqLyjWDarjtT1zdp7dcAbCdEf";
const MIXED_TOKEN: &str = "a1B2c3D4e5F6a7B8c9D0a1B2c3D4e5F6a7B8c9D0";
const SCREEN_TEXT: &str = "screen-secret-text";
const ENVIRONMENT_VALUE: &str = "env-secret-value-77";

#[test]
fn nothing_that_identifies_reaches_the_report_or_the_link() {
    let world = World::new();
    let home = world.scratch.home().to_string_lossy().into_owned();
    let canonical = world
        .scratch
        .home()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let folder = world.scratch.home().join("clients/acme");
    fs::create_dir_all(&folder).unwrap();
    // Folders whose names hold a space and an apostrophe, two in the projects folder and one outside it.
    let projects = world.scratch.home().join("Projects");
    for name in ["Globex Corp", "Jane's Bakery", "Piper ) Pied"] {
        fs::create_dir_all(projects.join(name)).unwrap();
    }
    let soylent = world.scratch.home().join("Clients/Soylent ) Green");
    fs::create_dir_all(&soylent).unwrap();
    // A workspace whose session, tab, pane label and folder are all secrets, with a pane that the doctor
    // warns about, so that its place is in a warning, and a pane with secret text on its screen.
    let (pane, _program) = codex_pane(&world, "acme-secret");
    world
        .scratch
        .tmux(&["rename-window", "-t", &pane, "client-x"]);
    world.scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &pane,
        "@workspace_label",
        "secret-label",
    ]);
    let screen = world.scratch.tmux(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &pane,
        "-c",
        &folder.to_string_lossy(),
        &format!("printf '{SCREEN_TEXT}\n'; exec sleep 300"),
    ]);
    world.scratch.settle(&screen);
    for (window, dir) in [
        ("gj", projects.join("Globex Corp")),
        ("jb", projects.join("Jane's Bakery")),
    ] {
        let pane = world.scratch.tmux(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "acme-secret:",
            "-n",
            window,
            "-c",
            &dir.to_string_lossy(),
            "exec sleep 300",
        ]);
        world.scratch.settle(&pane);
    }
    // A workspace that is gone from the server and is in the saved snapshot.
    let gone = world.scratch.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "wayne-enterprises",
        "-c",
        &soylent.to_string_lossy(),
        "exec sleep 300",
    ]);
    world.scratch.settle(&gone);
    let mut save = world.command(&["save"]);
    save.env(
        "PATH",
        format!("{}:/usr/bin:/bin", world.fakes.path().display()),
    );
    let saved = quietly(save);
    assert_eq!(saved.status.code(), Some(0), "{}", text(&saved.stderr));
    world
        .scratch
        .tmux(&["kill-session", "-t", "=wayne-enterprises"]);
    let real_user = command_output("id", &["-un"]);
    let real_host = command_output("hostname", &[]);
    let real_short = command_output("hostname", &["-s"]);
    let planted = [
        format!("2026-11-06 01:01 open: cannot write {home}/clients/acme: Permission denied; cannot read {canonical}/clients/acme/notes.txt"),
        format!("2026-11-06 01:03 setup: mail {EMAIL} from {IPV4} with {SECRET_KEY} and token {MIXED_TOKEN} for plantedusername"),
        format!(
            "2026-11-06 01:05 update: {real_user}@{real_host} then {real_host} and {real_short} as {real_user}"
        ),
        format!("2026-11-06 01:06 save: session acme-secret window client-x label secret-label; restore: tmux described the new session wayne-enterprises unexpectedly; Cannot write {home}/Clients/Soylent ) Green: Permission denied"),
        "2026-11-06 01:07 open: fetch https://deploy:hunter2@git.example.org/repo.git bound to ::1 and fe80::1%en0".to_string(),
        format!("2026-11-06 01:08 open: Cannot write {home}/Projects/Globex Corp: Permission denied (os error 13); Cannot write {home}/Projects/Piper ) Pied: denied"),
        format!("2026-11-06 01:09 open: Cannot write {home}/Projects/Jane's Bakery/notes.json: Permission denied"),
        // Nothing of these two is known to the server, the projects folder or the snapshot but the line.
        format!("2026-11-06 01:10 open: Cannot write {home}/Clients/Wayne Tower (Bruce W.): Permission denied, and {home}/Projects/Initech [Jane Doe]/x"),
    ];
    world.log(&planted.iter().map(String::as_str).collect::<Vec<_>>());
    // Failures of commands that were given names: the arguments are not in the log.
    for args in [
        ["work-unknown-cmd", "acme-secret"],
        ["park", "initech-layoffs"],
    ] {
        let failed = quietly(world.command(&args));
        assert_eq!(
            failed.status.code(),
            Some(1),
            "{args:?}: {}",
            text(&failed.stderr)
        );
    }

    let secrets: Vec<&str> = vec![
        "acme-secret",
        "client-x",
        "secret-label",
        "clients",
        "acme",
        "notes.txt",
        "Globex",
        "Corp",
        "Jane",
        "Bakery",
        "Soylent",
        "Green",
        "Piper",
        "Pied",
        "Wayne",
        "Tower",
        "Bruce",
        "Initech",
        "Doe",
        "wayne-enterprises",
        "work-unknown-cmd",
        "initech-layoffs",
        EMAIL,
        "secret-corp",
        IPV4,
        "sk-live",
        "51HqLyjWDarjtT1zdp7dc",
        MIXED_TOKEN,
        "plantedusername",
        "hunter2",
        "deploy",
        "fe80",
        SCREEN_TEXT,
        ENVIRONMENT_VALUE,
        &home,
        &canonical,
    ];
    let words: Vec<&str> = [real_user.as_str(), real_host.as_str(), real_short.as_str()]
        .into_iter()
        .filter(|word| !word.is_empty())
        .collect();
    let assert_clean = |what: &str, shown: &str| {
        for secret in &secrets {
            assert!(!shown.contains(secret), "{what} has {secret}:\n{shown}");
        }
        for word in &words {
            assert!(!has_word(shown, word), "{what} has {word}:\n{shown}");
        }
    };
    let trace = world.scratch.state_dir().join("trace");
    let with_surroundings = |mut command: Command| {
        command
            .env("TW_SECRET_ENV", ENVIRONMENT_VALUE)
            .env("TMUX_WORKSPACE_TRACE", &trace);
        command
    };

    // The report, as it is printed.
    let out = quietly(with_surroundings(world.command(&["bug", "--print"])));
    let printed = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_clean("the printed report", &printed);
    // And what stands in for them, so that it is not just empty.
    assert!(
        printed.contains("\nWARN  workspace-1:0.0 codex: No Codex status. Review /hooks\n"),
        "{printed}"
    );
    assert!(
        printed.contains("cannot write ~/folder-1/folder-2: Permission denied; cannot read ~/folder-1/folder-2/folder-3"),
        "{printed}"
    );
    assert!(
        printed.contains("mail <email> from <ip> with <token> and token <token> for <user>"),
        "{printed}"
    );
    // Folders with a space or an apostrophe in their names, known and unknown, are one folder each.
    for line in [
        "Cannot write ~/folder-5/folder-4: Permission denied",
        "Cannot write ~/Projects/folder-6: Permission denied (os error 13); Cannot write ~/Projects/folder-7: denied",
        "Cannot write ~/Projects/folder-8/folder-9: Permission denied",
        "Cannot write ~/folder-5/folder-10: Permission denied, and ~/Projects/folder-11/folder-12",
    ] {
        assert!(printed.contains(line), "{line}\n{printed}");
    }
    // The arguments of a failed command, and a workspace that is gone but was saved.
    assert!(
        printed.contains("unknown: Unknown workspace command: <arg>"),
        "{printed}"
    );
    assert!(printed.contains("park: No such pane: <arg>"), "{printed}");
    assert!(
        printed.contains("restore: tmux described the new session workspace-3 unexpectedly"),
        "{printed}"
    );
    assert!(
        printed.contains("update: <email> then <host> and <host> as <user>"),
        "{printed}"
    );
    assert!(
        printed.contains("session workspace-1 window tab-1 label label-1"),
        "{printed}"
    );
    assert!(
        printed.contains("fetch https://git.example.org/repo.git bound to <ip> and <ip>"),
        "{printed}"
    );

    // The report that is shown before the question, and the link the opener is given.
    let (status, stderr, shown) =
        on_a_terminal(with_surroundings(world.on_a_desktop(&["bug"])), "\n");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_clean("the report shown before the question", &shown);
    let url = world.opened();
    assert_clean("the link", &percent_decode(&url));
    assert_clean("the encoded link", &url);
    assert!(
        param(&url, "diagnostics")
            .unwrap()
            .contains("workspace-1:0.0 codex")
    );

    // Over SSH the link is shown and the report is copied.
    let (_, _, shown) = on_a_terminal(with_surroundings(world.over_ssh(&["bug"])), "");
    assert_clean("the report shown over SSH", &shown);
    assert_clean("what was copied", &world.copied());

    // No screen was read.
    let calls = fs::read_to_string(&trace).unwrap();
    assert!(!calls.contains("capture-pane"), "{calls}");
}

#[test]
fn the_names_of_the_server_are_numbered_alike_in_the_report_and_the_link() {
    let world = World::new();
    let (_pane, _program) = codex_pane(&world, "gamma");
    world.log(&["2026-11-06 01:00 save: gamma and alpha"]);
    let (_, _, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), "\n");
    let url = world.opened();
    for line in [
        "WARN  workspace-2:0.0 codex: No Codex status. Review /hooks",
        "save: workspace-2 and workspace-1",
    ] {
        assert!(shown.contains(line), "{line}\n{shown}");
        assert!(param(&url, "diagnostics").unwrap().contains(line), "{line}");
    }
}

// ---------------------------------------------------------------------------------------------
// The question, the browser and the clipboard

#[test]
fn a_yes_opens_the_issue_form_with_the_report_in_the_link() {
    for answer in ["\n", "y\n", "Y\n", "yes\n"] {
        let world = World::new();
        world.log(&["2026-11-06 01:12 open: cannot write ~/Projects/folder-2"]);
        let (status, stderr, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), answer);
        assert_eq!((status, stderr.as_str()), (0, ""), "{answer:?}");
        let url = world.opened();
        assert!(
            url.starts_with("https://github.com/ahmmkh/tmux-workspace/issues/new?template=bug.yml&title=%5BBug%5D%3A%20&environment="),
            "{url}"
        );
        // The report is in full on the screen, then the question, then what happened.
        assert!(
            shown.starts_with("### Environment\ntmux-workspace "),
            "{shown}"
        );
        assert!(shown.contains("\n### Doctor\n"), "{shown}");
        assert!(shown.contains("\n### Last errors (tmux-workspace log)\n2026-11-06 01:12 open: cannot write ~/Projects/folder-2\n"), "{shown}");
        assert!(shown.contains(&format!("\n\n{QUESTION} ")), "{shown}");
        // What happened, and the link, in case the browser is on a screen nobody here sees.
        assert!(shown.contains("Opened the issue form in your browser. Nothing is sent until you press Submit there.\nThe link, in case the browser is not on this screen:\nhttps://github.com/"), "{shown}");
        assert!(shown.ends_with(&format!("{url}\n")), "{shown}");
        // The form gets what the report says, as the two fields the issue form has.
        let environment = param(&url, "environment").unwrap();
        assert!(
            shown.contains(&format!("### Environment\n{environment}\n")),
            "{shown}"
        );
        let diagnostics = param(&url, "diagnostics").unwrap();
        assert!(shown.contains(&diagnostics), "{diagnostics}\n{shown}");
        assert!(!diagnostics.contains("shortened"));
        assert_eq!(
            world.fakes.calls("tw-clip"),
            0,
            "a link that fits needs no clipboard"
        );
    }
}

#[test]
fn an_idea_opens_the_idea_form_with_only_the_environment() {
    let world = World::new();
    let (status, _, shown) = on_a_terminal(world.on_a_desktop(&["idea"]), "y\n");
    assert_eq!(status, 0);
    let url = world.opened();
    assert!(
        url.starts_with("https://github.com/ahmmkh/tmux-workspace/issues/new?template=idea.yml&title=%5BIdea%5D%3A%20&environment="),
        "{url}"
    );
    assert_eq!(param(&url, "diagnostics"), None);
    assert!(
        shown.contains(QUESTION) && !shown.contains("Doctor"),
        "{shown}"
    );
}

#[test]
fn any_other_answer_opens_nothing_and_sends_nothing() {
    for answer in ["n\n", "N\n", "no\n", "nope\n", "x\n"] {
        let world = World::new();
        let (status, stderr, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), answer);
        assert_eq!((status, stderr.as_str()), (0, ""), "{answer:?}");
        assert!(shown.contains(QUESTION), "{answer:?}: {shown}");
        assert!(
            shown.ends_with("Nothing was opened or sent.\n"),
            "{answer:?}: {shown}"
        );
        assert_eq!(
            world.fakes.calls(OPENER) + world.fakes.calls("tw-clip"),
            0,
            "{answer:?}"
        );
    }
}

#[test]
fn print_asks_nothing_even_on_a_terminal() {
    let world = World::new();
    // Nothing is typed: a question would wait for an answer that does not come.
    let (status, stderr, shown) = on_a_terminal(world.on_a_desktop(&["bug", "--print"]), "");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(shown.starts_with("### Environment\n"), "{shown}");
    // The link is shown, so that a report is never stuck behind a browser nobody sees; but nothing asks.
    assert!(!shown.contains(QUESTION), "{shown}");
    assert!(
        shown.contains("\nhttps://github.com/ahmmkh/tmux-workspace/issues/new?template=bug.yml&"),
        "{shown}"
    );
    assert_eq!(world.fakes.calls(OPENER) + world.fakes.calls("tw-clip"), 0);
}

#[test]
fn without_a_terminal_the_report_and_the_link_are_printed_and_nothing_is_opened() {
    let world = World::new();
    let out = quietly(world.on_a_desktop(&["bug"]));
    let shown = text(&out.stdout);
    assert_eq!(
        (text(&out.stderr).as_str(), out.status.code()),
        ("", Some(0))
    );
    assert!(shown.starts_with("### Environment\n"), "{shown}");
    assert!(!shown.contains(QUESTION), "{shown}");
    let (_, link) = shown
        .split_once(
            "Open this link to file the issue; nothing is sent until you submit it there:\n",
        )
        .unwrap_or_else(|| panic!("no link in {shown}"));
    let url = link.trim_end();
    assert!(
        url.starts_with("https://github.com/ahmmkh/tmux-workspace/issues/new?template=bug.yml&"),
        "{url}"
    );
    assert!(param(url, "diagnostics").is_some());
    assert_eq!(world.fakes.calls(OPENER) + world.fakes.calls("tw-clip"), 0);
}

#[test]
fn over_ssh_nothing_opens_the_link_is_shown_and_the_report_is_copied() {
    let world = World::new();
    world.log(&["2026-11-06 01:12 open: x"]);
    let (status, stderr, shown) = on_a_terminal(world.over_ssh(&["bug"]), "");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(world.fakes.calls(OPENER), 0, "no browser over SSH");
    assert!(
        !shown.contains(QUESTION),
        "there is nothing to ask: {shown}"
    );
    // The whole report went to tw-clip, which does OSC 52.
    let copied = world.copied();
    assert!(copied.starts_with("### Environment\n"), "{copied}");
    assert!(
        copied.contains("### Last errors (tmux-workspace log)\n2026-11-06 01:12 open: x"),
        "{copied}"
    );
    assert!(
        shown.contains(copied.trim_end()),
        "what was copied is what is shown"
    );
    // The link is shown, and the clipboard is said.
    let url = shown
        .lines()
        .find(|line| line.starts_with("https://github.com/"))
        .unwrap_or_else(|| panic!("no link in {shown}"));
    assert_eq!(
        param(url, "environment").unwrap(),
        copied
            .split("\n\n")
            .next()
            .unwrap()
            .strip_prefix("### Environment\n")
            .unwrap()
    );
    assert!(
        shown.ends_with(
            "The report is on your clipboard; paste it into the form if a field is empty.\n"
        ),
        "{shown}"
    );
}

#[test]
fn a_pane_that_predates_the_ssh_session_is_told_by_the_environment_of_the_session() {
    // A pane keeps the environment it was made with; the session's is updated when a client attaches, and
    // from a laptop over SSH it says so.
    let world = World::new();
    world.scratch.tmux(&[
        "set-environment",
        "-t",
        "alpha",
        "SSH_CONNECTION",
        "198.51.100.7 1 192.0.2.10 22",
    ]);
    let (status, _, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), "");
    assert_eq!(status, 0);
    assert_eq!(
        world.fakes.calls(OPENER),
        0,
        "no browser on the far machine"
    );
    assert_eq!(world.fakes.calls("tw-clip"), 1);
    assert!(shown.contains("\nhttps://github.com/"), "{shown}");
    // A session that says nothing of SSH does not stop the browser.
    let world = World::new();
    let (status, _, _) = on_a_terminal(world.on_a_desktop(&["bug"]), "\n");
    assert_eq!(status, 0);
    assert_eq!(world.fakes.calls(OPENER), 1);
}

#[test]
fn ssh_is_told_by_either_variable() {
    for variable in ["SSH_CONNECTION", "SSH_TTY"] {
        let world = World::new();
        let mut command = world.on_a_desktop(&["bug"]);
        command
            .env_remove("SSH_CONNECTION")
            .env(variable, "/dev/ttys042");
        let (status, _, shown) = on_a_terminal(command, "");
        assert_eq!(status, 0, "{variable}");
        assert_eq!(world.fakes.calls(OPENER), 0, "{variable}");
        assert_eq!(world.fakes.calls("tw-clip"), 1, "{variable}");
        assert!(
            shown.contains("The report is on your clipboard"),
            "{variable}: {shown}"
        );
    }
}

#[test]
fn without_an_opener_it_is_the_same_as_over_ssh() {
    let world = World::new();
    fs::remove_file(world.fakes.path().join(OPENER)).unwrap();
    let (status, _, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), "");
    assert_eq!(status, 0);
    assert!(!shown.contains(QUESTION), "{shown}");
    assert!(
        shown.contains("\nhttps://github.com/ahmmkh/tmux-workspace/issues/new?template=bug.yml&"),
        "{shown}"
    );
    assert!(
        shown.ends_with(
            "The report is on your clipboard; paste it into the form if a field is empty.\n"
        ),
        "{shown}"
    );
    assert_eq!(world.fakes.calls("tw-clip"), 1);
}

#[test]
fn an_opener_that_fails_leaves_the_link_and_the_clipboard() {
    let world = World::new();
    world.fakes.add(OPENER, "exit 1");
    let (status, stderr, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), "\n");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(world.fakes.calls(OPENER), 1);
    assert!(
        shown.contains(&format!("{OPENER} did not open it.\n")),
        "{shown}"
    );
    assert!(
        shown.contains("\nhttps://github.com/ahmmkh/tmux-workspace/issues/new?template=bug.yml&"),
        "{shown}"
    );
    assert_eq!(world.fakes.calls("tw-clip"), 1);
}

#[test]
fn without_tw_clip_it_says_the_report_was_not_copied() {
    let world = World::new();
    fs::remove_file(world.fakes.path().join("tw-clip")).unwrap();
    let (status, _, shown) = on_a_terminal(world.over_ssh(&["bug"]), "");
    assert_eq!(status, 0);
    assert!(
        shown.ends_with("The report could not be copied (tw-clip is missing); copy it from above if a field is empty.\n"),
        "{shown}"
    );
}

/// A program that does what it was started for and leaves a process behind that keeps what it was
/// given, as xclip, wl-copy and a browser that starts do.
fn leaves_a_process_behind() -> String {
    format!("{} 6 &\nexit 0", common::find_in_path("sleep").display())
}

#[test]
fn an_opener_that_leaves_a_process_behind_does_not_keep_the_report_waiting() {
    let world = World::new();
    world.fakes.add(OPENER, &leaves_a_process_behind());
    let started = std::time::Instant::now();
    let (status, stderr, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), "\n");
    let took = started.elapsed();
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(took < std::time::Duration::from_secs(4), "{took:?}");
    assert_eq!(world.fakes.calls(OPENER), 1);
    assert!(
        shown.contains("Opened the issue form in your browser."),
        "{shown}"
    );
    assert!(!shown.contains("did not open it"), "{shown}");
}

#[test]
fn a_clipboard_tool_that_leaves_a_process_behind_does_not_keep_the_report_waiting() {
    let world = World::new();
    world.fakes.add("tw-clip", &leaves_a_process_behind());
    let started = std::time::Instant::now();
    let (status, stderr, shown) = on_a_terminal(world.over_ssh(&["bug"]), "");
    let took = started.elapsed();
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(took < std::time::Duration::from_secs(4), "{took:?}");
    assert!(
        shown.ends_with(
            "The report is on your clipboard; paste it into the form if a field is empty.\n"
        ),
        "{shown}"
    );
    assert!(world.copied().starts_with("### Environment\n"));
}

/// Lines for errors.log that make the link too long.
fn long_errors() -> Vec<String> {
    (0..10)
        .map(|n| {
            format!(
                "2026-11-06 01:{n:02} open: failure {n} {}",
                "word ".repeat(150)
            )
        })
        .collect()
}

#[test]
fn a_link_over_eight_thousand_bytes_is_trimmed_and_the_full_report_is_copied() {
    let world = World::new();
    let lines = long_errors();
    world.log(&lines.iter().map(String::as_str).collect::<Vec<_>>());
    let (status, stderr, shown) = on_a_terminal(world.on_a_desktop(&["bug"]), "\n");
    assert_eq!((status, stderr.as_str()), (0, ""));
    let url = world.opened();
    assert!(url.len() <= 8000, "{}", url.len());
    // The newest lines are in the link, the oldest are not, the environment is whole.
    let diagnostics = param(&url, "diagnostics").unwrap();
    assert!(diagnostics.contains("failure 9 "), "{diagnostics}");
    assert!(!diagnostics.contains("failure 0 "), "{diagnostics}");
    assert!(
        diagnostics.ends_with("(shortened to fit the link)"),
        "{diagnostics}"
    );
    assert_eq!(param(&url, "environment").unwrap().lines().count(), 4);
    // The clipboard has all ten.
    let copied = world.copied();
    for n in 0..10 {
        assert!(copied.contains(&format!("failure {n} ")), "{n}");
    }
    assert!(
        !copied.contains("shortened"),
        "the full report is the full report"
    );
    assert!(shown.ends_with("The link carries only the newest part of the diagnostics. The full report is on your clipboard; paste it into the form.\n"), "{shown}");
}

#[test]
fn a_long_report_without_a_terminal_says_the_link_is_short_and_copies_nothing() {
    let world = World::new();
    let lines = long_errors();
    world.log(&lines.iter().map(String::as_str).collect::<Vec<_>>());
    let out = quietly(world.on_a_desktop(&["bug"]));
    let shown = text(&out.stdout);
    assert!(shown.contains("failure 0 "), "the full report is shown");
    assert!(
        shown.ends_with(
            "The link carries only the newest part of the diagnostics; the report above is whole.\n"
        ),
        "{shown}"
    );
    assert_eq!(world.fakes.calls(OPENER) + world.fakes.calls("tw-clip"), 0);
}

#[cfg(target_os = "macos")]
#[test]
fn on_macos_the_browser_is_open_and_no_display_is_needed() {
    let world = World::new();
    let (status, _, _) = on_a_terminal(world.command(&["bug"]), "\n");
    assert_eq!(status, 0);
    assert!(world.opened().starts_with("https://github.com/"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn on_linux_a_screen_is_needed_for_xdg_open() {
    let world = World::new();
    let (status, _, shown) = on_a_terminal(world.command(&["bug"]), "");
    assert_eq!(status, 0);
    assert_eq!(world.fakes.calls("xdg-open"), 0);
    assert!(shown.contains("The report is on your clipboard"), "{shown}");
    for display in ["DISPLAY", "WAYLAND_DISPLAY"] {
        let world = World::new();
        let mut command = world.command(&["bug"]);
        command.env(display, ":0");
        on_a_terminal(command, "\n");
        assert_eq!(world.fakes.calls("xdg-open"), 1, "{display}");
    }
}

#[test]
fn no_run_of_these_tests_can_reach_a_real_opener_or_clipboard() {
    let world = World::new();
    let command = world.on_a_desktop(&["bug"]);
    let path = command
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value)
        .unwrap();
    assert_eq!(
        path,
        world.fakes.path().as_os_str(),
        "only the fakes are on PATH"
    );
    assert!(
        world.scratch.home().join(".local/bin/tw-clip").is_file(),
        "tw-clip is the fake"
    );
}

// ---------------------------------------------------------------------------------------------
// What ships with it: the issue forms, the guide, the help

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn shipped(relative: &str) -> String {
    fs::read_to_string(root().join(relative)).unwrap_or_else(|error| panic!("{relative}: {error}"))
}

/// The ids of the fields of an issue form, by their type: `("textarea", "environment")`. The forms are
/// read as text, which is all that is needed of YAML laid out as GitHub's own examples are.
fn form_fields(form: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut kind = String::new();
    for line in form.lines() {
        let line = line.trim();
        if let Some(found) = line.strip_prefix("- type:") {
            kind = found.trim().to_string();
        } else if let Some(id) = line.strip_prefix("id:") {
            fields.push((kind.clone(), id.trim().to_string()));
        }
    }
    fields
}

fn has_field(form: &str, id: &str) -> bool {
    form_fields(form)
        .iter()
        .any(|(kind, name)| kind == "textarea" && name == id)
}

#[test]
fn the_issue_forms_have_the_fields_the_links_fill_in() {
    use tmux_workspace::anonymize::{Anonymizer, Identity, Names};
    use tmux_workspace::report::{Diagnostics, Kind, Report};
    let mut anonymizer = Anonymizer::new(&Identity::default(), &Names::default());
    for (kind, file) in [(Kind::Bug, "bug.yml"), (Kind::Idea, "idea.yml")] {
        let form = shipped(&format!(".github/ISSUE_TEMPLATE/{file}"));
        assert_eq!(kind.template(), file);
        let report = Report {
            kind,
            environment: vec![anonymizer.apply("tmux-workspace 2.1.0")],
            diagnostics: (kind == Kind::Bug).then(|| Diagnostics {
                summary: anonymizer.apply("1 pane · 0 warnings · 0 errors"),
                warnings: Vec::new(),
                errors: Vec::new(),
            }),
        };
        let url = report.link().url;
        let query = url.split_once('?').unwrap().1;
        for pair in query.split('&') {
            let name = pair.split_once('=').unwrap().0;
            match name {
                "template" => {}
                "title" => assert!(
                    form.contains(&format!("title: \"{}\"", kind.title())),
                    "the form starts the title as the link does"
                ),
                id => assert!(
                    has_field(&form, id),
                    "{file} has no textarea with the id {id}"
                ),
            }
        }
        for key in ["name:", "description:", "labels:", "body:"] {
            assert!(
                form.lines().any(|line| line.starts_with(key)),
                "{file} has no {key}"
            );
        }
    }
}

#[test]
fn the_bug_form_asks_what_happened_what_was_expected_and_how_to_reproduce() {
    let form = shipped(".github/ISSUE_TEMPLATE/bug.yml");
    assert!(form.contains("\nlabels: [bug]\n"), "{form}");
    for id in ["environment", "diagnostics"] {
        assert!(has_field(&form, id), "{id}");
    }
    let labels: Vec<&str> = form
        .lines()
        .filter_map(|line| line.trim().strip_prefix("label:"))
        .map(str::trim)
        .collect();
    for label in [
        "What happened",
        "What you expected",
        "Steps to reproduce",
        "Environment",
        "Diagnostics",
    ] {
        assert!(labels.contains(&label), "{label} in {labels:?}");
    }
    // Both fields keep the lines as they are, in a block, so that the Markdown stays readable.
    assert_eq!(form.matches("render: markdown").count(), 2, "{form}");
}

#[test]
fn the_idea_form_asks_what_and_why() {
    let form = shipped(".github/ISSUE_TEMPLATE/idea.yml");
    assert!(form.contains("\nlabels: [enhancement]\n"), "{form}");
    assert!(has_field(&form, "environment"));
    assert!(!has_field(&form, "diagnostics"));
    let labels: Vec<&str> = form
        .lines()
        .filter_map(|line| line.trim().strip_prefix("label:"))
        .map(str::trim)
        .collect();
    for label in ["What you want", "Why", "Environment"] {
        assert!(labels.contains(&label), "{label} in {labels:?}");
    }
}

#[test]
fn blank_issues_stay_allowed() {
    let config = shipped(".github/ISSUE_TEMPLATE/config.yml");
    assert!(config.contains("blank_issues_enabled: true"), "{config}");
}

#[test]
fn the_guide_lists_the_key_and_the_commands() {
    let guide = shipped("WORKSPACE_GUIDE.md");
    assert!(
        guide.contains("| `Ctrl+a` `F` | "),
        "the cheatsheet has a row for F"
    );
    for text in [
        "work bug",
        "work idea",
        "work bug --print",
        "work ./bug",
        "errors.log",
    ] {
        assert!(guide.contains(text), "{text}");
    }
}

#[test]
fn the_help_has_a_topic_that_names_the_key_and_what_is_left_out() {
    let home = TempDir::new();
    let out = quietly({
        let mut command = Command::new(EXE);
        command
            .args(["help-topic", "report-bug"])
            .env("HOME", home.path());
        command
    });
    let shown = text(&out.stdout);
    assert!(
        shown.starts_with("=== Report a Bug or an Idea ===\n"),
        "{shown}"
    );
    for line in [
        "Ctrl+a then F",
        "work bug",
        "work idea",
        "--print",
        "folder-1",
        "Nothing is sent",
    ] {
        assert!(shown.contains(line), "{line}\n{shown}");
    }
}
