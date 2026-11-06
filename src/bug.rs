//! `bug` and `idea`: build the report, show it, and with the user's yes open GitHub's issue form with
//! it filled in. Nothing is sent by this program; the user reviews and submits in GitHub.
//!
//! Every line of the report goes through the anonymizer before it is kept (see `anonymize`), and the
//! link is made from the report's lines, so there is no way for anything else to reach either.

use crate::anonymize::{Anonymizer, Clean, Identity, Names};
use crate::doctor;
use crate::error::{Error, Result, fail};
use crate::errorlog;
use crate::integrations::{Target, system_name};
use crate::pane::login_shell;
use crate::paths::{home, projects_dir, share_root, state_dir};
use crate::process::{self, Spawn, executable, spawn, which};
use crate::report::{Diagnostics, Kind, Link, Report};
use crate::snapshot::read_snapshot_in;
use crate::tmux::Tmux;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// How long a program that is asked for a version may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the browser's opener is waited for. One that still runs after this has started a browser,
/// and is left to it.
const OPEN_WAIT: Duration = Duration::from_secs(3);

/// How long the clipboard's tool is waited for.
const CLIP_WAIT: Duration = Duration::from_secs(5);

/// How many lines of errors.log a bug report holds.
const LAST_ERRORS: usize = 10;

/// What is asked before the browser opens.
const QUESTION: &str = "Open a GitHub issue with this? [Y/n]";

const ON_THE_CLIPBOARD: &str =
    "The report is on your clipboard; paste it into the form if a field is empty.";

const NOT_COPIED: &str =
    "The report could not be copied (tw-clip is missing); copy it from above if a field is empty.";

const SHORTENED: &str = "The link carries only the newest part of the diagnostics";

/// Write to stdout at once. A closed stdout is no reason to fail.
fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn on_a_terminal() -> bool {
    // SAFETY: `isatty` only inspects file descriptors 0 and 1.
    unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 }
}

/// `bug [--print]` and `idea [--print]`.
pub fn main(kind: Kind, args: &[String]) -> Result<i32> {
    let print_only = match args {
        [] => false,
        [flag] if flag == "--print" => true,
        _ => {
            let name = if kind == Kind::Bug { "bug" } else { "idea" };
            let _ = writeln!(std::io::stderr(), "Usage: tmux-workspace {name} [--print]");
            return Ok(2);
        }
    };
    let tmux = Tmux::new();
    let report = build(kind, &tmux)?;
    let text = report.text();
    say(&format!("{text}\n"));
    let link = report.link();
    if print_only || !on_a_terminal() {
        // The link is always there to be had: the report may be piped, or the browser on a screen that
        // is not in front of the user.
        say(&format!(
            "\nOpen this link to file the issue; nothing is sent until you submit it there:\n{}\n",
            link.url
        ));
        if link.shortened {
            say(&format!("{SHORTENED}; the report above is whole.\n"));
        }
        return Ok(0);
    }
    let opener = choose_opener(
        system_name(),
        in_ssh_session(&tmux),
        has_screen(),
        |program| which(program).is_some(),
    );
    if let Some(opener) = opener {
        if !ask() {
            say("Nothing was opened or sent.\n");
            return Ok(0);
        }
        if run_helper(&[opener, &link.url], None, OPEN_WAIT) {
            say(&format!(
                "Opened the issue form in your browser. Nothing is sent until you press Submit there.\n\
                 The link, in case the browser is not on this screen:\n{}\n",
                link.url
            ));
            if link.shortened {
                let copied = copy_to_clipboard(&text);
                say(&format!(
                    "{SHORTENED}. {}\n",
                    if copied {
                        "The full report is on your clipboard; paste it into the form."
                    } else {
                        "The full report is shown above; paste it into the form."
                    }
                ));
            }
            return Ok(0);
        }
        say(&format!("{opener} did not open it.\n"));
    }
    show_for_a_browser_elsewhere(&text, &link);
    Ok(0)
}

/// Over SSH, or with nothing to open a browser with: the link, and the report on the clipboard.
fn show_for_a_browser_elsewhere(text: &str, link: &Link) {
    say(&format!(
        "\nOpen this link in a browser to file the issue; nothing is sent until you submit it there:\n{}\n",
        link.url
    ));
    if link.shortened {
        say(&format!("{SHORTENED}.\n"));
    }
    say(&format!(
        "{}\n",
        if copy_to_clipboard(text) {
            ON_THE_CLIPBOARD
        } else {
            NOT_COPIED
        }
    ));
}

/// What opens the link: nothing over SSH (the screen of the machine in front of you is not that of the
/// machine you are on), `open` on macOS, `xdg-open` where there is a screen to open it on.
fn choose_opener(
    system: &str,
    ssh: bool,
    screen: bool,
    has: impl Fn(&str) -> bool,
) -> Option<&'static str> {
    if ssh {
        return None;
    }
    let program = if system == "Darwin" {
        "open"
    } else if screen {
        "xdg-open"
    } else {
        return None;
    };
    has(program).then_some(program)
}

fn set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Whether this is an SSH session: this process says so, or the session of tmux does. A pane keeps the
/// environment it was made with, when tmux was started at another screen; the environment of the session
/// is the one that a client that attaches updates.
fn in_ssh_session(tmux: &Tmux) -> bool {
    set("SSH_CONNECTION") || set("SSH_TTY") || session_says_ssh(tmux)
}

fn session_says_ssh(tmux: &Tmux) -> bool {
    if !crate::cli::in_tmux() {
        return false;
    }
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    ["SSH_CONNECTION", "SSH_TTY"].iter().any(|name| {
        let mut args = vec!["show-environment"];
        if !pane.is_empty() {
            args.extend(["-t", &pane]);
        }
        args.push(name);
        tmux.quiet(&args)
            .strip_prefix(&format!("{name}="))
            .is_some_and(|value| !value.is_empty())
    })
}

fn has_screen() -> bool {
    set("DISPLAY") || set("WAYLAND_DISPLAY")
}

/// Ask the question on the terminal. Enter, `y` and `yes` are yes; anything else, and the end of input, no.
fn ask() -> bool {
    say(&format!("\n{QUESTION} "));
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    matches!(read, Ok(count) if count > 0) && is_yes(&line)
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

/// Start a program that is given the link or the report, and say whether it did well: it ended with
/// status 0, or it still runs after `wait` (a browser that has started does not end). What it prints goes
/// nowhere and nothing is waited for but its end, because a program that leaves a process behind, as
/// xclip and wl-copy do, holds a pipe open for as long as that process lives.
fn run_helper(argv: &[&str], stdin: Option<&str>, wait: Duration) -> bool {
    let argv: Vec<String> = argv.iter().map(|arg| arg.to_string()).collect();
    matches!(process::start(&argv, stdin, wait), Ok(None | Some(0)))
}

/// Copy the report with `~/.local/bin/tw-clip`, which also reaches the clipboard of the terminal you are
/// in over SSH (OSC 52). Whether it did is whether the tool is there: it always ends well.
fn copy_to_clipboard(report: &str) -> bool {
    let clip = home().join(".local/bin/tw-clip");
    executable(&clip)
        && run_helper(
            &[&clip.to_string_lossy(), "copy"],
            Some(&format!("{report}\n")),
            CLIP_WAIT,
        )
}

// ---------------------------------------------------------------------------------------------
// The report

/// The report of a kind, from what this machine and the tmux server say, anonymized. A bug report
/// names the doctor's panes and the log's lines, which may hold the names of the server, so it is not
/// made when those cannot be read.
fn build(kind: Kind, tmux: &Tmux) -> Result<Report> {
    let names = if kind == Kind::Bug {
        names(tmux)?
    } else {
        Names::default()
    };
    let mut anonymizer = Anonymizer::new(&Identity::current(), &names);
    // What this program wrote holds no name of the server: a tab called zsh must not change the shell.
    let environment = environment(tmux)
        .iter()
        .map(|line| anonymizer.apply_own(line))
        .collect();
    let diagnostics = (kind == Kind::Bug).then(|| diagnostics(tmux, &mut anonymizer));
    Ok(Report {
        kind,
        environment,
        diagnostics,
    })
}

/// The doctor's findings and the last lines of errors.log.
fn diagnostics(tmux: &Tmux, anonymizer: &mut Anonymizer) -> Diagnostics {
    let found = doctor::findings(tmux);
    let summary = format!(
        "{} · {} · {}",
        counted(found.panes, "pane"),
        counted(found.warnings.len(), "warning"),
        counted(found.errors.len(), "error")
    );
    let lines: Vec<Clean> = found
        .errors
        .iter()
        .chain(&found.warnings)
        .map(|line| anonymizer.apply(line))
        .collect();
    let errors = errorlog::last(&state_dir(), LAST_ERRORS)
        .iter()
        .map(|line| anonymizer.apply(line))
        .collect();
    Diagnostics {
        summary: anonymizer.apply_own(&summary),
        warnings: lines,
        errors,
    }
}

/// `1 pane`, `12 panes`.
fn counted(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// What the anonymizer has to hide: the names of the sessions, windows and pane labels that the server
/// has, each once, in the order it lists them; the folders of its panes and sessions; the names in the
/// saved snapshot, which a report may still say after the server has lost them; and the folders of the
/// projects folder. A server that is not there has no names, and one whose names cannot be read is an
/// error: a report would show them.
fn names(tmux: &Tmux) -> Result<Names> {
    let mut names = Names::default();
    match tmux.fields(
        "list-panes",
        &[
            "session_name",
            "window_name",
            "@workspace_label",
            "pane_current_path",
            "session_path",
        ],
        &["-a"],
    ) {
        Ok(rows) => {
            for row in &rows {
                add_name(&mut names.sessions, row.get("session_name"));
                add_name(&mut names.windows, row.get("window_name"));
                add_name(&mut names.labels, row.get("@workspace_label"));
                names.add_folder_path(row.get("pane_current_path"));
                names.add_folder_path(row.get("session_path"));
            }
        }
        // No server runs, so it has no names.
        Err(Error::Command { .. }) => {}
        Err(_) => {
            return fail(
                "The names of the tmux server could not be read, so a report could show them: a session, tab or pane label has a line break or a control character in its name. Rename it and run the command again.",
            );
        }
    }
    add_snapshot_names(&mut names, &state_dir());
    add_projects(&mut names, &projects_dir());
    Ok(names)
}

fn add_name(list: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !list.iter().any(|known| known == name) {
        list.push(name.to_string());
    }
}

/// The names that the saved snapshot in `state` has: sessions, tabs, labels and the folders of its panes.
fn add_snapshot_names(names: &mut Names, state: &Path) {
    let Ok(Some((_, data))) = read_snapshot_in(state) else {
        return;
    };
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(String::from);
    for window in data["windows"].as_array().into_iter().flatten() {
        add_name(
            &mut names.sessions,
            &text(window, "session_name").unwrap_or_default(),
        );
        add_name(
            &mut names.windows,
            &text(window, "window_name").unwrap_or_default(),
        );
    }
    for pane in data["panes"].as_array().into_iter().flatten() {
        add_name(
            &mut names.sessions,
            &text(pane, "session").unwrap_or_default(),
        );
        // The workspace a parked pane came from.
        add_name(&mut names.sessions, &text(pane, "home").unwrap_or_default());
        add_name(&mut names.labels, &text(pane, "label").unwrap_or_default());
        if let Some(cwd) = text(pane, "cwd") {
            names.add_folder_path(&cwd);
        }
    }
}

/// How many entries of the projects folder are looked at.
const MAX_PROJECTS: usize = 10_000;

/// The projects folder's own path and what is in it: the folders that `work NAME` makes and opens.
fn add_projects(names: &mut Names, projects: &Path) {
    names.add_folder_path(&projects.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(projects) else {
        return;
    };
    for entry in entries.flatten().take(MAX_PROJECTS) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !names.folders.contains(&name) {
            names.folders.push(name);
        }
    }
}

fn mark(there: bool) -> &'static str {
    if there { "✓" } else { "✗" }
}

/// The four lines of the environment: this program, the machine, the tools, the integrations.
fn environment(tmux: &Tmux) -> Vec<String> {
    let tmux_version = Some(tmux.quiet(&["-V"]))
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| format!("tmux {}", mark(false)));
    let python = first_version(&doctor::python3_version())
        .map_or_else(|| mark(false).to_string(), |version| major_minor(&version));
    let tools: Vec<String> = ["fzf", "zoxide", "sesh", "lazygit"]
        .iter()
        .map(|tool| format!("{tool} {}", mark(which(tool).is_some())))
        .collect();
    let integrations: Vec<String> = Target::current(&home(), system_name())
        .each()
        .iter()
        .map(|(name, there)| format!("{name} {}", mark(*there)))
        .collect();
    vec![
        format!(
            "tmux-workspace {} ({}, {})",
            env!("CARGO_PKG_VERSION"),
            install_kind(&share_root()),
            target()
        ),
        format!(
            "{} {} · {tmux_version} · {} · {}",
            system(),
            architecture(),
            shell(tmux),
            terminal(tmux)
        ),
        format!("python3 {python} · {}", tools.join(" · ")),
        format!("integrations: {}", integrations.join(" ")),
    ]
}

/// `release` for an unpacked release (it has a `VERSION`), `clone` for a checkout (it has the
/// `Cargo.toml` that the installer looks for), else `unknown`.
fn install_kind(share: &Path) -> &'static str {
    if share.join("VERSION").is_file() {
        "release"
    } else if share.join("Cargo.toml").is_file() {
        "clone"
    } else {
        "unknown"
    }
}

/// The Rust target this was built for, as the release archives name it.
fn target() -> String {
    let system = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_env = "musl") {
        "unknown-linux-musl"
    } else {
        "unknown-linux-gnu"
    };
    format!("{}-{system}", std::env::consts::ARCH)
}

/// The first line a program prints, when it ends well within a few seconds.
fn probe(argv: &[&str]) -> Option<String> {
    let out = spawn(&Spawn {
        argv: argv.iter().map(|arg| arg.to_string()).collect(),
        timeout: Some(PROBE_TIMEOUT),
        ..Spawn::default()
    })
    .ok()?;
    let said = if out.stdout.trim().is_empty() {
        out.stderr
    } else {
        out.stdout
    };
    Some(said.lines().next().unwrap_or_default().trim().to_string()).filter(|line| !line.is_empty())
}

/// `macOS 27.2`, or the pretty name of the Linux distribution.
fn system() -> String {
    if cfg!(target_os = "macos") {
        return match probe(&["/usr/bin/sw_vers", "-productVersion"]) {
            Some(version) => format!("macOS {version}"),
            None => "macOS".into(),
        };
    }
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| pretty_name(&text))
        .unwrap_or_else(|| "Linux".into())
}

/// `PRETTY_NAME` of an os-release file: `Debian GNU/Linux 12 (bookworm)`.
fn pretty_name(os_release: &str) -> Option<String> {
    os_release
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim().trim_matches(['"', '\'']).to_string())
        .filter(|name| !name.is_empty())
}

fn architecture() -> String {
    probe(&["uname", "-m"]).unwrap_or_else(|| std::env::consts::ARCH.to_string())
}

/// The first `1.2` or `1.2.3` in a text.
fn first_version(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() || (at > 0 && bytes[at - 1].is_ascii_digit()) {
            at += 1;
            continue;
        }
        let mut end = at;
        let mut dots = 0;
        while end < bytes.len() {
            if bytes[end].is_ascii_digit() {
                end += 1;
            } else if bytes[end] == b'.' && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
                dots += 1;
                end += 1;
            } else {
                break;
            }
        }
        if dots > 0 {
            return Some(text[at..end].to_string());
        }
        at = end;
    }
    None
}

/// `3.15` of `3.15.2`.
fn major_minor(version: &str) -> String {
    version.split('.').take(2).collect::<Vec<_>>().join(".")
}

/// The user's shell and its version: `zsh 5.9`.
fn shell(tmux: &Tmux) -> String {
    let shell = login_shell(tmux);
    let name = Path::new(&shell)
        .file_name()
        .map_or_else(|| shell.clone(), |name| name.to_string_lossy().into_owned());
    match probe(&[&shell, "--version"]).and_then(|said| first_version(&said)) {
        Some(version) => format!("{name} {version}"),
        None => name,
    }
}

/// The terminal program, or `TERM`. Inside tmux `TERM_PROGRAM` says `tmux`, and the terminal is the one
/// the server was started from.
fn terminal(tmux: &Tmux) -> String {
    let variable = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty() && value != "tmux")
    };
    let server = || {
        tmux.quiet(&["show-environment", "-g", "TERM_PROGRAM"])
            .strip_prefix("TERM_PROGRAM=")
            .map(String::from)
            .filter(|value| !value.is_empty() && value != "tmux")
    };
    variable("TERM_PROGRAM")
        .or_else(server)
        .or_else(|| variable("LC_TERMINAL"))
        .map(|program| terminal_name(&program))
        .or_else(|| variable("TERM"))
        .unwrap_or_else(|| "unknown terminal".into())
}

/// The name a terminal gives itself in `TERM_PROGRAM`, as its makers write it.
fn terminal_name(program: &str) -> String {
    match program {
        "ghostty" => "Ghostty",
        "iTerm.app" | "iTerm2" => "iTerm2",
        "Apple_Terminal" => "Terminal",
        "vscode" => "VS Code",
        "WarpTerminal" => "Warp",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_browser_is_chosen_by_system_screen_and_what_is_installed() {
        let all = |_: &str| true;
        let none = |_: &str| false;
        assert_eq!(choose_opener("Darwin", false, false, all), Some("open"));
        assert_eq!(choose_opener("Darwin", false, true, all), Some("open"));
        assert_eq!(choose_opener("Linux", false, true, all), Some("xdg-open"));
        assert_eq!(choose_opener("Linux", false, false, all), None);
        assert_eq!(choose_opener("Darwin", false, true, none), None);
        assert_eq!(choose_opener("Linux", false, true, none), None);
        // Over SSH nothing opens, whatever is installed and whatever the screen.
        assert_eq!(choose_opener("Darwin", true, true, all), None);
        assert_eq!(choose_opener("Linux", true, true, all), None);
        // Only the right program is asked for.
        assert_eq!(
            choose_opener("Darwin", false, true, |program| program == "xdg-open"),
            None
        );
        assert_eq!(
            choose_opener("Linux", false, true, |program| program == "open"),
            None
        );
    }

    #[test]
    fn enter_y_and_yes_are_yes_and_everything_else_is_no() {
        for yes in ["\n", "y\n", "Y\n", "yes\n", " YES \n", "", "  \n"] {
            assert!(is_yes(yes), "{yes:?}");
        }
        for no in ["n\n", "N\n", "no\n", "nope\n", "yy\n", "open\n", "0\n"] {
            assert!(!is_yes(no), "{no:?}");
        }
    }

    #[test]
    fn the_first_version_in_a_text_is_the_one_with_a_dot() {
        for (text, version) in [
            ("zsh 5.9 (arm-apple-darwin25.0)", Some("5.9")),
            (
                "GNU bash, version 5.2.15(1)-release (aarch64-apple-darwin)",
                Some("5.2.15"),
            ),
            ("fish, version 3.7.1", Some("3.7.1")),
            ("3.15.2", Some("3.15.2")),
            ("tmux next-3.6", Some("3.6")),
            ("tmux 3.5a", Some("3.5")),
            ("version 12 and then 4.0.", Some("4.0")),
            ("v1", None),
            ("1.", None),
            ("", None),
            ("not found", None),
        ] {
            assert_eq!(first_version(text).as_deref(), version, "{text:?}");
        }
        assert_eq!(major_minor("3.15.2"), "3.15");
        assert_eq!(major_minor("5.9"), "5.9");
    }

    #[test]
    fn the_pretty_name_of_a_distribution_is_read_without_its_quotes() {
        let text = "NAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\nPRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nID=debian\n";
        assert_eq!(
            pretty_name(text).as_deref(),
            Some("Debian GNU/Linux 12 (bookworm)")
        );
        assert_eq!(
            pretty_name("PRETTY_NAME=Alpine\n").as_deref(),
            Some("Alpine")
        );
        assert_eq!(pretty_name("NAME=x\n"), None);
        assert_eq!(pretty_name("PRETTY_NAME=\"\"\n"), None);
    }

    #[test]
    fn terminals_are_called_what_their_makers_call_them() {
        assert_eq!(terminal_name("ghostty"), "Ghostty");
        assert_eq!(terminal_name("iTerm.app"), "iTerm2");
        assert_eq!(terminal_name("Apple_Terminal"), "Terminal");
        assert_eq!(terminal_name("WezTerm"), "WezTerm");
    }

    #[test]
    fn a_count_is_singular_for_one() {
        assert_eq!(counted(1, "pane"), "1 pane");
        assert_eq!(counted(0, "error"), "0 errors");
        assert_eq!(counted(12, "pane"), "12 panes");
    }

    fn script(dir: &crate::testing::TempDir, body: &str) -> String {
        let path = dir.path().join("helper");
        crate::testing::write_script(&path, &format!("#!/bin/sh\n{body}\n"));
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn a_helper_is_judged_by_its_end_and_not_by_what_it_leaves_behind() {
        let dir = crate::testing::TempDir::new();
        let wait = Duration::from_secs(5);
        let started = std::time::Instant::now();
        // It copied, and left a process that holds on to its output.
        assert!(run_helper(
            &[&script(&dir, "sleep 5 &\nexit 0")],
            None,
            wait
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(!run_helper(&[&script(&dir, "exit 1")], None, wait));
        assert!(!run_helper(&["/tw-no-such-dir/helper"], None, wait));
        // One that still runs when the time is up has started its browser.
        assert!(run_helper(
            &[&script(&dir, "exec sleep 2")],
            None,
            Duration::from_millis(100)
        ));
    }

    #[test]
    fn names_are_added_once_and_the_empty_ones_not_at_all() {
        let mut list = Vec::new();
        for name in ["a", "", "b", "a"] {
            add_name(&mut list, name);
        }
        assert_eq!(list, ["a", "b"]);
    }

    #[test]
    fn the_saved_snapshot_has_names_the_server_may_have_lost() {
        let dir = crate::testing::TempDir::new();
        let workspace = dir.path().join("workspace.json");
        std::fs::write(
            &workspace,
            serde_json::json!({
                "version": 1,
                "windows": [{"session_name": "wayne-enterprises", "window_name": "plans"}],
                "panes": [
                    {"session": "wayne-enterprises", "home": "", "label": "boardroom", "cwd": "/Users/x/Clients/Soylent Green"},
                    {"session": "__parked", "home": "initech", "label": "shell", "cwd": ""}
                ]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("latest.json"),
            serde_json::json!({"path": workspace.to_string_lossy()}).to_string(),
        )
        .unwrap();
        let mut names = Names::default();
        add_name(&mut names.sessions, "alive");
        add_snapshot_names(&mut names, dir.path());
        assert_eq!(
            names.sessions,
            ["alive", "wayne-enterprises", "__parked", "initech"]
        );
        assert_eq!(names.windows, ["plans"]);
        assert_eq!(names.labels, ["boardroom", "shell"]);
        assert_eq!(names.folders, ["Users", "x", "Clients", "Soylent Green"]);
        // No snapshot, or one that cannot be read, has none.
        let mut none = Names::default();
        add_snapshot_names(&mut none, &dir.path().join("elsewhere"));
        std::fs::write(dir.path().join("latest.json"), "{").unwrap();
        add_snapshot_names(&mut none, dir.path());
        assert!(none.sessions.is_empty() && none.folders.is_empty());
    }

    #[test]
    fn the_projects_folder_and_what_is_in_it_are_folders_to_hide() {
        let dir = crate::testing::TempDir::new();
        let projects = dir.path().join("Code/Clients");
        for name in ["Globex Corp", "Jane's Bakery"] {
            std::fs::create_dir_all(projects.join(name)).unwrap();
        }
        let mut names = Names::default();
        add_projects(&mut names, &projects);
        for name in ["Code", "Clients", "Globex Corp", "Jane's Bakery"] {
            assert!(names.folders.iter().any(|folder| folder == name), "{name}");
        }
        // A folder that is not there adds its own path and nothing else.
        let mut none = Names::default();
        add_projects(&mut none, &dir.path().join("absent"));
        assert_eq!(none.folders.last().map(String::as_str), Some("absent"));
    }

    #[test]
    fn an_install_is_a_release_a_clone_or_unknown() {
        let dir = crate::testing::TempDir::new();
        assert_eq!(install_kind(dir.path()), "unknown");
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(install_kind(dir.path()), "clone");
        std::fs::write(dir.path().join("VERSION"), "2.1.0\n").unwrap();
        assert_eq!(install_kind(dir.path()), "release");
    }
}
