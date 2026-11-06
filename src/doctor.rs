//! `doctor`: read local settings and agent metadata, and report what is wrong. It never reads terminal
//! output, and it never starts a tmux server.

use crate::error::Result;
use crate::integrations::{codex_dir, hook_commands, old_mark, ours, render};
use crate::model::option_json;
use crate::paths::{TMUX_MIN, cannot_read, home, resolve, share_root, state_dir};
use crate::process::{Spawn, spawn, which};
use crate::pyjson::{Style, dumps, float_repr};
use crate::text::{ljust, now, py_strip};
use crate::tmux::{Fields, Tmux};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a program the report runs may take.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A snapshot older than this many seconds is a warning.
const SNAPSHOT_STALE: i64 = 900;

/// The features Ghostty needs for agent status to be right: true color, the clipboard, focus
/// reports and synchronized redraws.
const GHOSTTY_FEATURES: [&str; 4] = ["RGB", "clipboard", "focus", "sync"];

/// What the report looks at, so a test can supply it instead of the real machine.
struct Machine {
    home: PathBuf,
    /// The folder with `bin/`, `integrations/` and the shipped configs.
    share: PathBuf,
    /// Codex's folder, where `hooks.json` is.
    codex: PathBuf,
    /// The state folder, where `latest.json` is.
    state: PathBuf,
}

/// The lines of the report, and the checks that failed.
struct Report<'a> {
    out: &'a mut dyn Write,
    warnings: Vec<String>,
    /// The `WARN` line of each warning, as a bug report tells it.
    lines: Vec<String>,
    /// How many panes the report listed.
    panes: usize,
}

/// What the doctor found, for a bug report.
#[derive(Clone, Debug, Default)]
pub struct Findings {
    pub panes: usize,
    /// One `WARN  ...` line for each warning.
    pub warnings: Vec<String>,
    /// What stopped the report, when something did.
    pub errors: Vec<String>,
}

impl<'a> Report<'a> {
    fn new(out: &'a mut dyn Write) -> Report<'a> {
        Report {
            out,
            warnings: Vec::new(),
            lines: Vec::new(),
            panes: 0,
        }
    }

    /// `print(text)`.
    fn say(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }

    /// List a pane: its row, and a warning when it is a Codex pane without a status.
    fn pane(&mut self, row: &Fields, now: f64) {
        let (lines, warning) = pane_lines(row, now);
        self.say(&lines);
        self.panes += 1;
        if let Some(location) = warning {
            self.lines.push(format!(
                "WARN  {location} codex: No Codex status. Review /hooks"
            ));
            self.warnings.push(location);
        }
    }

    fn check(&mut self, label: &str, ok: bool, detail: &str) {
        self.say(&format!(
            "{}  {label}: {detail}",
            if ok { "OK" } else { "WARN" }
        ));
        if !ok {
            self.lines.push(format!("WARN  {label}: {detail}"));
            self.warnings.push(label.to_string());
        }
    }
}

impl Machine {
    /// This machine.
    fn current() -> Machine {
        let home = home();
        Machine {
            codex: codex_dir(&home),
            share: share_root(),
            state: state_dir(),
            home,
        }
    }
}

/// What the report offers when it has warned: a bug report that carries these results.
const BUG_HINT: &str = "If this looks like a bug: work bug (it includes these results, anonymized)";

/// Print the diagnostics. The status is 1 when anything is wrong, else 0.
pub fn doctor(tmux: &Tmux) -> Result<i32> {
    report(&mut std::io::stdout().lock(), tmux, &Machine::current())
}

/// What the doctor finds, without printing it: for `bug`, which tells it with the names anonymized.
pub fn findings(tmux: &Tmux) -> Findings {
    findings_on(tmux, &Machine::current())
}

fn findings_on(tmux: &Tmux, machine: &Machine) -> Findings {
    let mut sink = std::io::sink();
    let mut report = Report::new(&mut sink);
    let outcome = run(&mut report, tmux, machine);
    Findings {
        panes: report.panes,
        warnings: report.lines,
        errors: outcome
            .err()
            .map(|error| format!("ERROR  doctor: {}", crate::errorlog::describe(&error)))
            .into_iter()
            .collect(),
    }
}

fn report(out: &mut dyn Write, tmux: &Tmux, machine: &Machine) -> Result<i32> {
    let mut report = Report::new(out);
    let status = run(&mut report, tmux, machine)?;
    if !report.warnings.is_empty() {
        report.say(&format!("\n{BUG_HINT}"));
    }
    Ok(status)
}

fn run(report: &mut Report, tmux: &Tmux, machine: &Machine) -> Result<i32> {
    report.say("Tmux workspace diagnostics\n");
    for name in ["tmux", "fzf", "sesh", "zoxide"] {
        let found = which(name);
        let detail = found.as_ref().map_or_else(
            || "not found on PATH".to_string(),
            |path| path.display().to_string(),
        );
        report.check(name, found.is_some(), &detail);
    }
    let (ok, detail) = version_check(py_strip(&tmux.quiet(&["-V"])));
    report.check("tmux version", ok, &detail);
    let clipboard = helper(&machine.share, "tw-clip");
    report.check(
        "clipboard",
        true,
        if clipboard.is_empty() {
            "unknown"
        } else {
            &clipboard
        },
    );
    let sound = Some(helper(&machine.share, "tw-sound")).filter(|sound| !sound.is_empty());
    let sound = sound.as_deref().unwrap_or("none");
    let silent = if sound == "none" {
        " (agent sounds stay silent)"
    } else {
        ""
    };
    report.say(&format!("      sounds: {sound}{silent}"));
    report.say(&format!(
        "      python3: {} (dash, ai and cleanup)",
        python3_version()
    ));
    links(report, machine);
    binary_on_path(report);
    agent_integrations(report, machine);
    codex_hooks(report, machine);

    let probe = spawn(&Spawn {
        argv: tmux.argv(&["-N", "display-message", "-p", "#{socket_path}"]),
        timeout: Some(TIMEOUT),
        ..Spawn::default()
    });
    let (running, said) = match probe {
        Ok(out) => (
            out.status == 0,
            Some(py_strip(&out.stdout))
                .filter(|text| !text.is_empty())
                .unwrap_or(py_strip(&out.stderr))
                .to_string(),
        ),
        Err(error) => (false, error.to_string()),
    };
    report.check("tmux server", running, &said);
    if !running {
        report.say("\nStart or attach with work, then run tmux-workspace doctor again.");
        return Ok(1);
    }

    report.say("\nLive settings");
    for (name, expected) in [
        ("mouse", "on"),
        ("focus-events", "on"),
        ("set-clipboard", "external"),
        ("default-terminal", "tmux-256color"),
    ] {
        let value = tmux.option(name, None);
        report.check(name, value == expected, or(&value, "unset"));
    }
    report.say(&format!(
        "    Sounds: {}",
        or(&tmux.option("@agent_sound", None), "unconfigured")
    ));
    let error = tmux.option("@workspace_save_error", None);
    let detail = if error.is_empty() {
        format!(
            "last saved {}",
            or(&tmux.option("@workspace_saved", None), "not yet")
        )
    } else {
        error.clone()
    };
    report.check("autosave", error.is_empty(), &detail);
    match snapshot_age(&machine.state) {
        Some(age) => report.check(
            "snapshot age",
            age < SNAPSHOT_STALE,
            &format!("{} minutes", age.div_euclid(60)),
        ),
        None => report.check("snapshot", false, "no readable latest snapshot"),
    }

    report.say("\nClients (focus controls whether completion is done or idle)");
    let clients = tmux.run(&[
        "list-clients",
        "-F",
        "#{client_name}  #{client_termname}  #{client_flags}\n  #{client_termfeatures}",
    ])?;
    report.say(or(&clients, "No attached clients"));
    for client in tmux.fields(
        "list-clients",
        &["client_termname", "client_termfeatures"],
        &[],
    )? {
        if client.get("client_termname") == "xterm-ghostty" {
            let (ok, detail) = ghostty(client.get("client_termfeatures"));
            report.check("Ghostty capabilities", ok, &detail);
        }
    }

    report.say("\nPanes (no prompts or terminal content are read)");
    let names = [
        "session_name",
        "window_index",
        "pane_index",
        "pane_id",
        "pane_current_command",
        "@workspace_agent",
        "@workspace_codex",
    ];
    for row in tmux.fields("list-panes", &names, &["-a"])? {
        report.pane(&row, now());
    }
    let count = report.warnings.len();
    report.say(&format!(
        "\n{count} warning(s). Ctrl+a r reloads tmux; Ctrl+a M toggles agent sounds."
    ));
    Ok(i32::from(count > 0))
}

/// `text`, or `fallback` when it is empty.
fn or<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    if text.is_empty() { fallback } else { text }
}

/// Whether the tmux version is new enough, and the line that says so.
fn version_check(version: &str) -> (bool, String) {
    let floor = format!("{}.{}", TMUX_MIN.0, TMUX_MIN.1);
    let floor_numbers = (u64::from(TMUX_MIN.0), u64::from(TMUX_MIN.1));
    if version_numbers(version).is_some_and(|found| found < floor_numbers) {
        return (false, format!("{version} (needs {floor} or newer)"));
    }
    (true, or(version, "unknown").to_string())
}

/// The first `major.minor` in a version text: `3.5` in `tmux 3.5a` and `3.6` in `tmux next-3.6`.
fn version_numbers(text: &str) -> Option<(u64, u64)> {
    let bytes = text.as_bytes();
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let number = |from: usize, len: usize| text[from..from + len].parse().unwrap_or(u64::MAX);
    let mut at = 0;
    while at < bytes.len() {
        let major = digits(at);
        if major == 0 {
            at += 1;
            continue;
        }
        let dot = at + major;
        if bytes.get(dot) == Some(&b'.') && digits(dot + 1) > 0 {
            return Some((number(at, major), number(dot + 1, digits(dot + 1))));
        }
        at = dot;
    }
    None
}

/// What `<share>/bin/<name> which` prints.
fn helper(share: &Path, name: &str) -> String {
    let program = share.join("bin").join(name);
    spawn(&Spawn {
        argv: vec![program.to_string_lossy().into_owned(), "which".into()],
        timeout: Some(TIMEOUT),
        ..Spawn::default()
    })
    .map(|out| py_strip(&out.stdout).to_string())
    .unwrap_or_default()
}

/// The version of the `python3` on `PATH`, which `dash`, `ai` and `cleanup` still run on.
pub fn python3_version() -> String {
    const NOT_FOUND: &str = "not found";
    let Some(python) = which("python3") else {
        return NOT_FOUND.into();
    };
    // macOS ships a stub at this path; running it without the developer tools offers to install them.
    if cfg!(target_os = "macos") && python == Path::new("/usr/bin/python3") && !developer_tools() {
        return NOT_FOUND.into();
    }
    let Ok(out) = spawn(&Spawn {
        argv: vec![python.to_string_lossy().into_owned(), "--version".into()],
        timeout: Some(TIMEOUT),
        ..Spawn::default()
    }) else {
        return NOT_FOUND.into();
    };
    // Before Python 3.4 the version went to stderr.
    let said = Some(py_strip(&out.stdout))
        .filter(|text| !text.is_empty())
        .unwrap_or(py_strip(&out.stderr));
    or(said.strip_prefix("Python ").unwrap_or(said), "unknown").to_string()
}

/// Whether macOS has developer tools, so that its `python3` is more than a stub.
fn developer_tools() -> bool {
    spawn(&Spawn {
        argv: vec!["xcode-select".into(), "-p".into()],
        timeout: Some(TIMEOUT),
        ..Spawn::default()
    })
    .is_ok_and(|out| out.status == 0)
}

/// The links the installer makes, each of which should lead into the share folder.
fn links(report: &mut Report, machine: &Machine) {
    let home = &machine.home;
    let share = &machine.share;
    for (live, source) in [
        (
            home.join(".local/bin/tmux-workspace"),
            share.join("bin/tmux-workspace"),
        ),
        (
            home.join(".config/tmux-workspace/agent-status.conf"),
            share.join("agent-status.conf"),
        ),
        (home.join(".tmux.conf"), share.join("tmux.conf")),
    ] {
        let target = resolve(&live);
        let detail = if live.exists() {
            target.display().to_string()
        } else {
            "missing; run install.sh".to_string()
        };
        report.check(
            &live.display().to_string(),
            target == resolve(&source),
            &detail,
        );
    }
}

/// Whether the `tmux-workspace` a shell finds is the binary that runs now.
fn binary_on_path(report: &mut Report) {
    const LABEL: &str = "tmux-workspace on PATH";
    let Some(found) = which("tmux-workspace") else {
        report.check(LABEL, false, "not found on PATH");
        return;
    };
    let running = std::env::current_exe().and_then(|exe| exe.canonicalize());
    let same = matches!((found.canonicalize(), running), (Ok(a), Ok(b)) if a == b);
    if same {
        report.check(LABEL, true, &found.display().to_string());
    } else {
        report.check(
            LABEL,
            false,
            &format!("not the installed binary ({})", found.display()),
        );
    }
}

/// Whether Claude and Codex run this program's hook: the hooks of the Python version still work with
/// their helper, but the installer rewrites them.
fn agent_integrations(report: &mut Report, machine: &Machine) {
    const LABEL: &str = "agent integrations";
    let mut registered = Vec::new();
    let mut old = false;
    for (name, path) in [
        ("Claude", machine.home.join(".claude/settings.json")),
        ("Codex", machine.codex.join("hooks.json")),
    ] {
        let commands = hook_commands(&path);
        old |= commands.iter().any(|c| old_mark(c));
        if commands
            .iter()
            .any(|c| ours(c) && c.contains(" agent-hook "))
        {
            registered.push(name);
        }
    }
    if old {
        report.check(LABEL, false, "old Python hooks; run install.sh --agents");
    } else if registered.is_empty() {
        report.say("      agent integrations: not installed (install.sh --agents)");
    } else {
        report.check(LABEL, true, &registered.join(", "));
    }
}

fn codex_hooks(report: &mut Report, machine: &Machine) {
    if !machine.codex.is_dir() {
        report.say("      Codex: not installed");
        return;
    }
    match missing_codex_hooks(machine) {
        Ok(missing) => {
            let detail = if missing.is_empty() {
                "all workspace hooks installed".to_string()
            } else {
                format!("missing: {}", missing.join(", "))
            };
            report.check("Codex hooks", missing.is_empty(), &detail);
            report.say(
                "    Codex /hooks must show these definitions as trusted. New or changed hooks require review.",
            );
        }
        Err(why) => report.check("Codex hooks", false, &why),
    }
}

/// The events for which Codex's `hooks.json` lacks the hook this project registers.
fn missing_codex_hooks(machine: &Machine) -> std::result::Result<Vec<String>, String> {
    let path = machine.codex.join("hooks.json");
    let unreadable = |why: &dyn std::fmt::Display| cannot_read(&path, why).to_string();
    let text = fs::read_to_string(&path).map_err(|e| unreadable(&e))?;
    let settings: Value = serde_json::from_str(&text).map_err(|e| unreadable(&e))?;
    let Some(settings) = settings.as_object() else {
        return Err(unreadable(&"the top level must be an object"));
    };
    let installed = match settings.get("hooks") {
        None => None,
        Some(Value::Object(hooks)) => Some(hooks),
        Some(_) => return Err(unreadable(&"\"hooks\" must be an object")),
    };
    let template = machine.share.join("integrations/codex-hooks.json");
    let wanted = render(&template, &machine.home).map_err(|e| e.to_string())?;
    let Some(expected) = wanted.get("hooks").and_then(Value::as_object) else {
        return Err(format!("{} holds no hooks", template.display()));
    };
    Ok(expected
        .iter()
        .filter(|(event, groups)| {
            let first = groups.get(0);
            let present = installed
                .and_then(|hooks| hooks.get(event.as_str()))
                .and_then(Value::as_array)
                .is_some_and(|list| first.is_some_and(|group| list.contains(group)));
            !present
        })
        .map(|(event, _)| event.clone())
        .collect())
}

/// Seconds since the latest snapshot was saved; `None` when there is none to read.
fn snapshot_age(state: &Path) -> Option<i64> {
    let text = fs::read_to_string(state.join("latest.json")).ok()?;
    let latest: Value = serde_json::from_str(&text).ok()?;
    let saved = latest.get("saved_at")?.as_f64()?;
    Some((now() - saved) as i64)
}

/// Whether a Ghostty client has the features agent status needs, and the line that says so.
fn ghostty(features: &str) -> (bool, String) {
    let have: HashSet<&str> = features.split(',').collect();
    let mut missing: Vec<&str> = GHOSTTY_FEATURES
        .into_iter()
        .filter(|feature| !have.contains(feature))
        .collect();
    if missing.is_empty() {
        return (
            true,
            "true color, clipboard, focus and synchronized redraws enabled".into(),
        );
    }
    missing.sort_unstable();
    (false, format!("missing: {}", missing.join(", ")))
}

/// The row of a pane, and when it is a Codex pane without a status, the warning to add: its location.
fn pane_lines(row: &Fields, now: f64) -> (String, Option<String>) {
    let location = format!(
        "{}:{}.{}",
        row.get("session_name"),
        row.get("window_index"),
        row.get("pane_index")
    );
    let command = row.get("pane_current_command");
    let status = row.get("@workspace_agent");
    let mut lines = format!(
        "{} {} {} {}{}",
        ljust(&location, 32),
        ljust(row.get("pane_id"), 5),
        ljust(command, 14),
        ljust(or(status, "-"), 8),
        codex_detail(row.get("@workspace_codex"), now)
    );
    if command != "codex" || !status.is_empty() {
        return (lines, None);
    }
    lines.push_str("\n    WARN  No Codex status. Review /hooks, then restart/resume this CLI.");
    (lines, Some(location))
}

/// What the last Codex event of a pane adds to its row: `  Stop 12s ago`, or nothing without an event.
fn codex_detail(raw: &str, now: f64) -> String {
    const UNREADABLE: &str = "  unreadable Codex event metadata";
    let Ok(event) = option_json(or(raw, "{}")) else {
        return UNREADABLE.into();
    };
    if !truthy(&event) {
        return String::new();
    }
    let at = event.get("at").and_then(Value::as_f64);
    match (event.get("event"), at) {
        (Some(name), Some(at)) => format!("  {} {}s ago", py_str(name), (now - at) as i64),
        _ => UNREADABLE.into(),
    }
}

/// Python truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `str(value)` for a value read from JSON.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) if number.is_f64() => float_repr(number.as_f64().unwrap_or_default()),
        Value::Bool(flag) => if *flag { "True" } else { "False" }.into(),
        Value::Null => "None".into(),
        other => dumps(other, Style::Default, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use serde_json::json;

    fn row(pairs: &[(&str, &str)]) -> Fields {
        let mut row = Fields::default();
        for (name, value) in pairs {
            row.set(name, *value);
        }
        row
    }

    #[test]
    fn the_tmux_version_is_the_first_major_dot_minor() {
        assert_eq!(version_numbers("tmux 3.5a"), Some((3, 5)));
        assert_eq!(version_numbers("tmux next-3.6"), Some((3, 6)));
        assert_eq!(version_numbers("tmux 10.12"), Some((10, 12)));
        assert_eq!(version_numbers("tmux 3a.5"), None);
        assert_eq!(version_numbers("tmux 3"), None);
        assert_eq!(version_numbers("3.x 4.2"), Some((4, 2)));
        assert_eq!(version_numbers(""), None);
        assert_eq!(
            version_numbers("1.99999999999999999999999"),
            Some((1, u64::MAX))
        );
    }

    #[test]
    fn the_floor_is_three_three() {
        assert_eq!(
            version_check("tmux 3.2a"),
            (false, "tmux 3.2a (needs 3.3 or newer)".into())
        );
        assert_eq!(version_check("tmux 3.3"), (true, "tmux 3.3".into()));
        assert_eq!(
            version_check("tmux 2.9"),
            (false, "tmux 2.9 (needs 3.3 or newer)".into())
        );
        // A version that cannot be read is not too old.
        assert_eq!(version_check("tmux master"), (true, "tmux master".into()));
        assert_eq!(version_check(""), (true, "unknown".into()));
    }

    #[test]
    fn ghostty_needs_four_features() {
        assert_eq!(
            ghostty("RGB,clipboard,focus,sync,title"),
            (
                true,
                "true color, clipboard, focus and synchronized redraws enabled".into()
            )
        );
        assert_eq!(
            ghostty("clipboard,sync"),
            (false, "missing: RGB, focus".into())
        );
        assert_eq!(
            ghostty(""),
            (false, "missing: RGB, clipboard, focus, sync".into())
        );
    }

    #[test]
    fn a_pane_row_is_padded_like_the_python_report() {
        let (text, warning) = pane_lines(
            &row(&[
                ("session_name", "alpha"),
                ("window_index", "0"),
                ("pane_index", "1"),
                ("pane_id", "%3"),
                ("pane_current_command", "claude"),
                ("@workspace_agent", "working"),
            ]),
            100.0,
        );
        assert_eq!(
            text,
            format!(
                "{:32} {:5} {:14} {:8}",
                "alpha:0.1", "%3", "claude", "working"
            )
        );
        assert_eq!(warning, None);
    }

    #[test]
    fn a_codex_pane_without_a_status_is_a_warning() {
        let silent = row(&[
            ("session_name", "alpha"),
            ("window_index", "0"),
            ("pane_index", "2"),
            ("pane_id", "%4"),
            ("pane_current_command", "codex"),
        ]);
        let (text, warning) = pane_lines(&silent, 100.0);
        assert_eq!(warning.as_deref(), Some("alpha:0.2"));
        assert!(text.starts_with("alpha:0.2 "));
        assert!(text.contains(" codex          -       \n"), "{text}");
        assert!(
            text.ends_with(
                "\n    WARN  No Codex status. Review /hooks, then restart/resume this CLI."
            ),
            "{text}"
        );
        let mut known = silent.clone();
        known.set("@workspace_agent", "idle");
        assert_eq!(pane_lines(&known, 100.0).1, None);
        let mut other = silent;
        other.set("pane_current_command", "zsh");
        assert_eq!(pane_lines(&other, 100.0).1, None);
    }

    #[test]
    fn the_last_codex_event_is_told_in_seconds() {
        assert_eq!(
            codex_detail(r#"{"event": "Stop", "at": 90.9}"#, 100.0),
            "  Stop 9s ago"
        );
        assert_eq!(
            codex_detail(r#"{"event": "Stop", "at": 90}"#, 100.0),
            "  Stop 10s ago"
        );
        // tmux 3.4 prints a dollar sign as \$.
        assert_eq!(
            codex_detail(r#"{"event": "a\$b", "at": 100}"#, 100.0),
            "  a$b 0s ago"
        );
        assert_eq!(
            codex_detail(r#"{"event": 7, "at": 99}"#, 100.0),
            "  7 1s ago"
        );
        assert_eq!(
            codex_detail(r#"{"event": null, "at": 99}"#, 100.0),
            "  None 1s ago"
        );
    }

    #[test]
    fn no_event_is_no_detail_and_a_bad_one_is_unreadable() {
        for raw in ["", "{}", "[]", "0", "null", r#""""#, "false"] {
            assert_eq!(codex_detail(raw, 100.0), "", "{raw:?}");
        }
        for raw in [
            "not json",
            "[1]",
            r#""abc""#,
            "5",
            "true",
            r#"{"event": "Stop"}"#,
            r#"{"at": 5}"#,
            r#"{"event": "Stop", "at": "now"}"#,
        ] {
            assert_eq!(
                codex_detail(raw, 100.0),
                "  unreadable Codex event metadata",
                "{raw:?}"
            );
        }
    }

    #[test]
    fn the_snapshot_age_needs_a_number() {
        let dir = TempDir::new();
        let latest = dir.path().join("latest.json");
        assert_eq!(snapshot_age(dir.path()), None);
        fs::write(&latest, json!({"saved_at": now() - 90.0}).to_string()).unwrap();
        assert!(matches!(snapshot_age(dir.path()), Some(89..=91)));
        for bad in [r#"{"saved_at": "x"}"#, "{}", "[]", "nope", "null"] {
            fs::write(&latest, bad).unwrap();
            assert_eq!(snapshot_age(dir.path()), None, "{bad}");
        }
    }

    #[test]
    fn what_the_doctor_found_is_kept_as_lines_for_a_bug_report() {
        let dir = TempDir::new();
        let machine = Machine {
            home: dir.path().join("home"),
            share: dir.path().join("share"),
            codex: dir.path().join("home/.codex"),
            state: dir.path().join("state"),
        };
        let tmux = Tmux::new().program("/nonexistent/tmux");
        let found = findings_on(&tmux, &machine);
        assert_eq!(found.panes, 0);
        assert!(found.errors.is_empty());
        assert!(
            found
                .warnings
                .iter()
                .any(|line| line.starts_with("WARN  tmux server: ")),
            "{:?}",
            found.warnings
        );
        let missing = format!(
            "WARN  {}: missing; run install.sh",
            dir.path().join("home/.tmux.conf").display()
        );
        assert!(found.warnings.contains(&missing), "{:?}", found.warnings);
        assert!(
            found
                .warnings
                .iter()
                .all(|line| line.starts_with("WARN  ") && !line.contains('\n'))
        );
    }

    #[test]
    fn a_codex_pane_without_a_status_is_a_warning_line_with_its_place() {
        let mut sink = Vec::new();
        let mut report = Report::new(&mut sink);
        let silent = row(&[
            ("session_name", "alpha"),
            ("window_index", "0"),
            ("pane_index", "2"),
            ("pane_id", "%4"),
            ("pane_current_command", "codex"),
        ]);
        report.pane(&silent, 100.0);
        assert_eq!(report.panes, 1);
        assert_eq!(
            report.lines,
            ["WARN  alpha:0.2 codex: No Codex status. Review /hooks"]
        );
        assert_eq!(report.warnings, ["alpha:0.2"]);
    }

    #[test]
    fn the_report_names_what_it_found_without_a_server() {
        let dir = TempDir::new();
        let machine = Machine {
            home: dir.path().join("home"),
            share: dir.path().join("share"),
            codex: dir.path().join("home/.codex"),
            state: dir.path().join("state"),
        };
        let mut out = Vec::new();
        let tmux = Tmux::new().program("/nonexistent/tmux");
        let code = report(&mut out, &tmux, &machine).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(code, 1);
        assert!(text.starts_with("Tmux workspace diagnostics\n\n"), "{text}");
        assert!(text.contains("WARN  tmux server: "), "{text}");
        assert!(text.contains("OK  clipboard: unknown\n"), "{text}");
        assert!(
            text.contains("      sounds: none (agent sounds stay silent)\n"),
            "{text}"
        );
        assert!(
            text.ends_with(
                "\nStart or attach with work, then run tmux-workspace doctor again.\n\n\
                 If this looks like a bug: work bug (it includes these results, anonymized)\n"
            ),
            "{text}"
        );
    }
}
