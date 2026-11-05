//! How a pane starts, which shell it gets, and its label.
//!
//! `pane-start` is the program every new tmux pane runs before the user's shell, so it does as
//! little as it can: a pane that has its identity and its label costs one tmux call, or two when
//! the shell has to be asked of the server.

use crate::error::{Error, Result, fail};
use crate::paths::{self, cannot_read};
use crate::process::{Spawn, exec_with_env, executable, spawn};
use crate::text::{clean_label, is_printable, py_strip, strftime, uuid4_hex};
use crate::tmux::{SEP, Tmux};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The program that reopens each kind of application's saved session; the session follows.
pub const APP_COMMANDS: [(&str, &[&str]); 4] = [
    ("claude", &["claude", "--resume"]),
    ("codex", &["codex", "resume"]),
    ("omp", &["omp", "--resume"]),
    ("nvim", &["nvim", "-S"]),
];

/// Shells known to accept `-l` for a login shell; elvish and others start without it.
pub const LOGIN_FLAG_SHELLS: [&str; 12] = [
    "sh", "bash", "zsh", "ksh", "mksh", "dash", "fish", "tcsh", "csh", "nu", "xonsh", "pwsh",
];

/// Names the shell to use instead of the server's or the account's.
const SHELL_ENV: &str = "TMUX_WORKSPACE_SHELL";

/// How long `pane-start` waits between looks at `@workspace_restoring`.
const RESTORING_POLL: Duration = Duration::from_millis(100);

/// The text of an environment variable, empty when it is unset.
fn env_text(name: &str) -> String {
    std::env::var_os(name)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Write to our stdout at once. A closed stdout is no reason to stop starting a pane.
fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

// ---------------------------------------------------------------------------------------------
// Identity and label

/// Whether this is a pane identity: 32 lowercase hexadecimal digits.
pub fn is_identity(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `current` when it is a valid identity, else a new one, which is written to the pane.
fn ensure_identity(tmux: &Tmux, pane: &str, current: &str) -> Result<String> {
    if is_identity(current) {
        return Ok(current.to_string());
    }
    let id = uuid4_hex();
    tmux.put("@workspace_id", &id, Some(pane))?;
    Ok(id)
}

/// The identity of a pane. A pane that has none, or a damaged one, gets a new one.
pub fn identity(tmux: &Tmux, pane: &str) -> Result<String> {
    ensure_identity(tmux, pane, &tmux.option("@workspace_id", Some(pane)))
}

/// Give a pane its label: printable characters only, at most 80, `shell` when nothing is left.
pub fn set_label(tmux: &Tmux, pane: &str, value: &str) -> Result<()> {
    tmux.put("@workspace_label", &clean_label(value), Some(pane))
}

/// Ask for a pane's label on the terminal and set it. An empty answer, the end of input and Ctrl-C all
/// give `initial`.
pub fn ask_label(tmux: &Tmux, pane: &str, initial: &str) -> Result<()> {
    let interrupts = Interrupts::catch();
    say(&format!("Pane label [{initial}]: "));
    let label = match read_answer(&interrupts) {
        Answer::Line(line) if !py_strip(&line).is_empty() => py_strip(&line).to_string(),
        Answer::Line(_) => initial.to_string(),
        Answer::End | Answer::Interrupted => {
            say("\n");
            initial.to_string()
        }
    };
    drop(interrupts);
    set_label(tmux, pane, &label)
}

/// `tmux-workspace label [PANE]`: ask for the label of a pane, with its current one as the default.
pub fn label(tmux: &Tmux, pane: Option<&str>) -> Result<()> {
    let pane = match pane {
        Some(pane) => pane.to_string(),
        None => tmux.run(&["display-message", "-p", "#{pane_id}"])?,
    };
    let current = tmux.option("@workspace_label", Some(&pane));
    ask_label(
        tmux,
        &pane,
        if current.is_empty() {
            "shell"
        } else {
            &current
        },
    )
}

// ---------------------------------------------------------------------------------------------
// Ctrl-C

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn note_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

/// While this exists, Ctrl-C does not end the process. A handler rather than "ignore", because a
/// handler goes back to the default in a program started meanwhile, and "ignore" would be inherited.
/// No `SA_RESTART`: a read that waits for input returns when Ctrl-C comes.
struct Interrupts {
    before: libc::sigaction,
}

impl Interrupts {
    fn catch() -> Interrupts {
        INTERRUPTED.store(false, Ordering::Relaxed);
        // SAFETY: plain-data structs filled in by `sigemptyset` and `sigaction`; the handler only
        // stores into an atomic, which is async-signal-safe.
        unsafe {
            let mut handler: libc::sigaction = std::mem::zeroed();
            handler.sa_sigaction =
                note_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&mut handler.sa_mask);
            handler.sa_flags = 0;
            let mut before: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGINT, &handler, &mut before);
            Interrupts { before }
        }
    }

    fn happened(&self) -> bool {
        INTERRUPTED.load(Ordering::Relaxed)
    }
}

impl Drop for Interrupts {
    fn drop(&mut self) {
        // SAFETY: restores the disposition `catch` found.
        unsafe { libc::sigaction(libc::SIGINT, &self.before, std::ptr::null_mut()) };
    }
}

enum Answer {
    Line(String),
    /// The input ended before a character came.
    End,
    Interrupted,
}

/// One line from file descriptor 0, a byte at a time so that nothing past the line is consumed:
/// the shell that starts next reads the rest.
fn read_answer(interrupts: &Interrupts) -> Answer {
    let mut line = Vec::new();
    loop {
        if interrupts.happened() {
            return Answer::Interrupted;
        }
        let mut byte = 0u8;
        // SAFETY: reads at most one byte into `byte`.
        let count = unsafe { libc::read(0, (&raw mut byte).cast(), 1) };
        match count {
            1 if byte == b'\n' => break,
            1 => line.push(byte),
            0 if line.is_empty() => return Answer::End,
            0 => break,
            _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
            _ => return Answer::End,
        }
    }
    Answer::Line(String::from_utf8_lossy(&line).into_owned())
}

// ---------------------------------------------------------------------------------------------
// The shell

/// Whether this is a file the user can run.
pub fn usable_shell(path: &str) -> bool {
    !path.is_empty() && executable(Path::new(path))
}

/// The server's `SHELL` from the output of `show-environment -g SHELL`: empty when it has none
/// (`-SHELL`, or nothing).
pub fn server_shell(shown: &str) -> &str {
    shown.strip_prefix("SHELL=").unwrap_or("")
}

/// The first usable shell among the override, the server's, the account's and the environment's;
/// `/bin/sh` when none is.
pub fn choose_shell(
    override_shell: &str,
    server: &str,
    account: &str,
    environment: &str,
) -> String {
    [override_shell, server, account, environment]
        .into_iter()
        .find(|shell| usable_shell(shell))
        .unwrap_or("/bin/sh")
        .to_string()
}

/// What to run for this shell: with `-l` when it is known to accept it.
pub fn shell_argv(shell: &str) -> Vec<String> {
    let name = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let mut argv = vec![shell.to_string()];
    if LOGIN_FLAG_SHELLS.contains(&name) {
        argv.push("-l".to_string());
    }
    argv
}

/// `TMUX_WORKSPACE_SHELL` is the user's real shell. tmux.conf runs commands with /bin/sh and so hands
/// panes `SHELL=/bin/sh`; the real one is that override, the `SHELL` the server started with, or the
/// account's.
fn pick_shell(override_shell: &str, server: &str) -> String {
    choose_shell(
        override_shell,
        server,
        &paths::account_shell(),
        &env_text("SHELL"),
    )
}

/// The user's real shell.
pub fn login_shell(tmux: &Tmux) -> String {
    let override_shell = env_text(SHELL_ENV);
    if usable_shell(&override_shell) {
        return override_shell;
    }
    let shown = tmux.quiet(&["show-environment", "-g", "SHELL"]);
    pick_shell(&override_shell, server_shell(&shown))
}

/// Replace this process with the shell, which tells the programs it starts what it is.
fn exec_as(shell: &str) -> Error {
    exec_with_env(&shell_argv(shell), &[("SHELL", shell)])
}

/// Replace this process with the user's shell. Returns only when that fails.
pub fn exec_shell(tmux: &Tmux) -> Error {
    exec_as(&login_shell(tmux))
}

// ---------------------------------------------------------------------------------------------
// pane-start

/// `pane-start [SNAPSHOT UID]`: runs in a new pane before the shell. It gives the pane its identity
/// and label, or restores both from a snapshot, then execs the shell. Returns only an error.
pub fn pane_start(tmux: &Tmux, snapshot_path: Option<&str>, uid: Option<&str>) -> Result<()> {
    let pane = env_text("TMUX_PANE");
    if pane.is_empty() {
        return fail("pane-start must run inside tmux");
    }
    let shell = match snapshot_path.filter(|path| !path.is_empty()) {
        Some(path) => {
            restore_pane(tmux, &pane, path, uid.unwrap_or_default())?;
            login_shell(tmux)
        }
        None => new_pane(tmux, &pane)?,
    };
    Err(exec_as(&shell))
}

/// A pane without a snapshot: one tmux call reads its identity and label, and the server's shell if
/// the override does not name one. A new identity is the one write. Returns the shell to run.
fn new_pane(tmux: &Tmux, pane: &str) -> Result<String> {
    let override_shell = env_text(SHELL_ENV);
    let ask_server = !usable_shell(&override_shell);
    let format = format!("#{{@workspace_id}}{SEP}#{{@workspace_label}}");
    let mut args = vec!["display-message", "-p", "-t", pane, format.as_str()];
    if ask_server {
        // Fails when the server has no SHELL; the pane's values were printed before that.
        args.extend([";", "show-environment", "-g", "SHELL"]);
    }
    let output = tmux.quiet(&args);
    let (fields, shown) = output.split_once('\n').unwrap_or((&output, ""));
    let (id, label) = fields.split_once(SEP).unwrap_or((fields, ""));
    ensure_identity(tmux, pane, id)?;
    if label.is_empty() {
        ask_label(tmux, pane, "shell")?;
    }
    Ok(if ask_server {
        pick_shell(&override_shell, server_shell(shown))
    } else {
        override_shell
    })
}

/// The text of a field of a JSON object, empty when it has none.
fn text_of<'a>(object: &'a Value, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A pane of a snapshot: restore its identity and label with one write, replay its saved terminal
/// text, wait for the rest of the workspace and reopen the application that ran in it.
fn restore_pane(tmux: &Tmux, pane: &str, path: &str, uid: &str) -> Result<()> {
    let file = Path::new(path);
    let bytes = std::fs::read(file).map_err(|e| cannot_read(file, e))?;
    let data: Value = serde_json::from_slice(&bytes).map_err(|e| Error::Json(e.to_string()))?;
    let Some(record) = data
        .get("panes")
        .and_then(Value::as_array)
        .and_then(|panes| panes.iter().find(|p| text_of(p, "id") == uid))
    else {
        return fail(format!("{path} has no pane {uid}"));
    };

    let write = |name: &str, value: &str| -> Vec<String> {
        ["set-option", "-p", "-t", pane, name, value]
            .map(String::from)
            .to_vec()
    };
    let mut writes = vec![
        write("@workspace_id", uid),
        write("@workspace_label", &clean_label(text_of(record, "label"))),
    ];
    for key in ["parked", "home"] {
        let value = text_of(record, key);
        if !value.is_empty() {
            writes.push(write(&format!("@workspace_{key}"), value));
        }
    }
    tmux.chain(&writes)?;

    let history = Path::new(text_of(&data, "directory")).join(format!("{uid}.txt"));
    if history.is_file() {
        let saved_at = data.get("saved_at").and_then(Value::as_f64).unwrap_or(0.0);
        say(&format!(
            "── Saved terminal text · {} ──\n",
            strftime("%Y-%m-%d %H:%M", saved_at)
        ));
        let raw = std::fs::read(&history).map_err(|e| cannot_read(&history, e))?;
        say(&replay_text(&String::from_utf8_lossy(&raw)));
        say("── End saved text · prior processes are not running ──\n");
    }

    // Hold application startup until every pane and layout has been reconstructed.
    while tmux.option("@workspace_restoring", None) == "1" {
        std::thread::sleep(RESTORING_POLL);
    }
    reopen_app(record);
    Ok(())
}

/// Saved terminal text as it may be shown: line ends as a text file reads them, and nothing that
/// could act as a control sequence, only line breaks and tabs.
fn replay_text(raw: &str) -> String {
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || is_printable(c))
        .collect()
}

/// What a pane's recorded application asks for.
#[derive(Debug, PartialEq, Eq)]
enum Recovery {
    Nothing,
    /// Say why the application cannot be reopened.
    Attention(String),
    Reopen {
        kind: String,
        argv: Vec<String>,
        cwd: Option<PathBuf>,
    },
}

/// Python's truthiness of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// Reopen exactly the saved conversation, or explain why that is not possible; never guess.
fn plan_recovery(record: &Value) -> Recovery {
    let Some(app) = record
        .get("app")
        .filter(|app| app.is_object() && truthy(app))
    else {
        return Recovery::Nothing;
    };
    if let Some(error) = app.get("error").filter(|error| truthy(error)) {
        return Recovery::Attention(
            error
                .as_str()
                .map_or_else(|| error.to_string(), String::from),
        );
    }
    let (kind, token) = (text_of(app, "kind"), text_of(app, "id"));
    let command = APP_COMMANDS
        .iter()
        .find(|(name, _)| *name == kind)
        .filter(|_| !token.is_empty() && !token.starts_with('-'));
    let Some((_, command)) = command else {
        return Recovery::Attention("Missing or invalid exact recovery identity".to_string());
    };
    if matches!(kind, "nvim" | "omp") && token.starts_with('/') && !Path::new(token).is_file() {
        return Recovery::Attention(format!("Saved session file no longer exists: {token}"));
    }
    let cwd = [text_of(app, "cwd"), text_of(record, "cwd")]
        .into_iter()
        .find(|cwd| !cwd.is_empty())
        .map(PathBuf::from);
    Recovery::Reopen {
        kind: kind.to_string(),
        argv: command
            .iter()
            .copied()
            .chain([token])
            .map(String::from)
            .collect(),
        cwd,
    }
}

/// Run the recorded application's resume command in the pane's terminal, and say what became of it.
fn reopen_app(record: &Value) {
    match plan_recovery(record) {
        Recovery::Nothing => {}
        Recovery::Attention(why) => say(&format!("Recovery needs attention: {why}\n")),
        Recovery::Reopen { kind, argv, cwd } => {
            say(&format!(
                "Reopening {kind} for pane {} (no prompt replay).\n",
                text_of(record, "label")
            ));
            // The terminal sends Ctrl-C to the application and to this process alike. The pane must
            // outlive it: the shell starts when the application ends.
            let interrupts = Interrupts::catch();
            let result = spawn(&Spawn {
                argv,
                cwd,
                inherit_stdio: true,
                ..Spawn::default()
            });
            drop(interrupts);
            match result {
                Ok(done) if done.status == 0 => {}
                Ok(done) => say(&format!(
                    "{kind} exited with status {}; no other conversation was substituted.\n",
                    done.status
                )),
                Err(error) => say(&format!("Recovery needs attention: {error}\n")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ScratchTmux, TempDir};
    use serde_json::json;

    fn client(scratch: &ScratchTmux) -> Tmux {
        Tmux::new().socket(scratch.socket())
    }

    #[test]
    fn an_identity_is_32_lowercase_hex_digits() {
        assert!(is_identity(&"0123456789abcdef".repeat(2)));
        for bad in ["", "abc", &"A".repeat(32), &"g".repeat(32), &"a".repeat(33)] {
            assert!(!is_identity(bad), "{bad}");
        }
        assert!(is_identity(&uuid4_hex()));
    }

    #[test]
    fn identity_creates_once_and_repairs_a_damaged_one() {
        let scratch = ScratchTmux::new();
        let (tmux, pane) = (client(&scratch), scratch.expand("alpha:one", "#{pane_id}"));
        let id = identity(&tmux, &pane).unwrap();
        assert!(is_identity(&id));
        assert_eq!(identity(&tmux, &pane).unwrap(), id, "kept");
        tmux.put("@workspace_id", "damaged", Some(&pane)).unwrap();
        let new = identity(&tmux, &pane).unwrap();
        assert!(is_identity(&new) && new != id);
        assert_eq!(tmux.option("@workspace_id", Some(&pane)), new);
    }

    #[test]
    fn identity_of_a_pane_that_is_gone_is_an_error() {
        let scratch = ScratchTmux::new();
        assert!(identity(&client(&scratch), "%999").is_err());
    }

    #[test]
    fn set_label_cleans_the_label() {
        let scratch = ScratchTmux::new();
        let (tmux, pane) = (client(&scratch), scratch.expand("alpha:one", "#{pane_id}"));
        for (given, set) in [
            ("  api \u{7}server ", "api server"),
            ("", "shell"),
            ("\u{1b}\u{7}", "shell"),
            (&"x".repeat(90), &"x".repeat(80)),
        ] {
            set_label(&tmux, &pane, given).unwrap();
            assert_eq!(tmux.option("@workspace_label", Some(&pane)), set);
        }
    }

    #[test]
    fn replay_keeps_line_breaks_and_tabs_and_drops_every_other_control_character() {
        assert_eq!(replay_text("a\tb\nc"), "a\tb\nc");
        assert_eq!(replay_text("a\r\nb\rc\n\n"), "a\nb\nc\n\n");
        assert_eq!(
            replay_text("\u{1b}[31mred\u{1b}[0m\u{7}\u{8}\u{0}"),
            "[31mred[0m"
        );
        assert_eq!(replay_text("x\u{85}\u{200b}\u{a0}y\u{2028}z"), "xyz");
        assert_eq!(replay_text("naïve ✓ 日本"), "naïve ✓ 日本");
    }

    #[test]
    fn truthiness_follows_python() {
        for yes in [
            json!(true),
            json!(1),
            json!(-0.5),
            json!("x"),
            json!([0]),
            json!({"a": 0}),
        ] {
            assert!(truthy(&yes), "{yes}");
        }
        for no in [
            json!(null),
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(!truthy(&no), "{no}");
        }
    }

    fn plan(app: Value) -> Recovery {
        plan_recovery(&json!({"label": "agent", "cwd": "/pane", "app": app}))
    }

    #[test]
    fn a_recorded_error_is_shown_whatever_else_the_record_says() {
        assert_eq!(
            plan(json!({"kind": "claude", "id": "abc", "error": "gone"})),
            Recovery::Attention("gone".into())
        );
        // Not a string: still an error, and never a command.
        assert_eq!(
            plan(json!({"kind": "claude", "id": "abc", "error": 7})),
            Recovery::Attention("7".into())
        );
        // A falsy error is none.
        assert!(matches!(
            plan(json!({"kind": "claude", "id": "abc", "error": ""})),
            Recovery::Reopen { .. }
        ));
    }

    #[test]
    fn each_kind_reopens_with_its_own_command() {
        for (kind, argv) in [
            ("claude", ["claude", "--resume", "abc"]),
            ("codex", ["codex", "resume", "abc"]),
            ("omp", ["omp", "--resume", "abc"]),
            ("nvim", ["nvim", "-S", "abc"]),
        ] {
            assert_eq!(
                plan(json!({"kind": kind, "id": "abc"})),
                Recovery::Reopen {
                    kind: kind.into(),
                    argv: argv.map(String::from).to_vec(),
                    cwd: Some("/pane".into()),
                }
            );
        }
    }

    #[test]
    fn the_applications_folder_wins_over_the_panes() {
        let Recovery::Reopen { cwd, .. } = plan(json!({"kind": "codex", "id": "x", "cwd": "/app"}))
        else {
            panic!()
        };
        assert_eq!(cwd, Some("/app".into()));
        let Recovery::Reopen { cwd, .. } = plan(json!({"kind": "codex", "id": "x", "cwd": ""}))
        else {
            panic!()
        };
        assert_eq!(cwd, Some("/pane".into()), "an empty folder is none");
    }

    #[test]
    fn a_session_file_is_only_checked_for_nvim_and_omp_paths() {
        let dir = TempDir::new();
        let file = dir.path().join("session");
        std::fs::write(&file, "").unwrap();
        for kind in ["nvim", "omp"] {
            assert!(matches!(
                plan(json!({"kind": kind, "id": file})),
                Recovery::Reopen { .. }
            ));
            assert_eq!(
                plan(json!({"kind": kind, "id": dir.path().join("gone")})),
                Recovery::Attention(format!(
                    "Saved session file no longer exists: {}",
                    dir.path().join("gone").display()
                ))
            );
            assert!(matches!(
                plan(json!({"kind": kind, "id": "relative-id"})),
                Recovery::Reopen { .. }
            ));
        }
        // A path is an id like another for the others.
        assert!(matches!(
            plan(json!({"kind": "claude", "id": "/gone/anywhere"})),
            Recovery::Reopen { .. }
        ));
    }

    #[test]
    fn an_empty_or_missing_app_asks_for_nothing() {
        for app in [
            json!(null),
            json!({}),
            json!(""),
            json!(false),
            json!("claude"),
        ] {
            assert_eq!(plan(app), Recovery::Nothing);
        }
        assert_eq!(plan_recovery(&json!({"label": "x"})), Recovery::Nothing);
    }

    #[test]
    fn exec_names_the_shell_and_its_flag() {
        assert_eq!(shell_argv("/usr/bin/zsh"), ["/usr/bin/zsh", "-l"]);
        assert_eq!(shell_argv("/usr/bin/elvish"), ["/usr/bin/elvish"]);
        assert_eq!(shell_argv("bash"), ["bash", "-l"]);
    }

    #[test]
    fn an_empty_or_missing_shell_is_not_usable() {
        assert!(!usable_shell(""));
        assert!(!usable_shell("/nonexistent/zsh"));
        assert!(!usable_shell("/etc"));
        assert!(usable_shell("/bin/sh"));
        let dir = TempDir::new();
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "").unwrap();
        assert!(!usable_shell(plain.to_str().unwrap()), "not executable");
    }
}
