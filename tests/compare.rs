//! The Rust port against the Python it replaces. Both run the same scenario on a scratch tmux server
//! of their own, and the tests assert that they leave the same state behind: the same files, the same
//! pane options, the same words on failure. During an upgrade the two write to one live server in
//! turn, so "the same" has to hold byte for byte where one reads what the other wrote.
//!
//! The Python is `legacy/bin/tmux-workspace` (run with `python3`, the way the binary is run: the
//! scratch `TMUX`, a temporary `HOME` and state folder) and `legacy/integrations/agent-hook.py`
//! (through `tests/compare/hook_driver.py`, which patches the process lookup the way the old
//! `HelperWrites` tests did). The Rust hook runs in the test process with a `FakeHost` whose process
//! tree makes `owner` give the same pid and birth.
//!
//! What differs on purpose is not compared (see the task's accepted differences): the text for an
//! unknown pane id, `park` without an argument, a snapshot that has no record for a pane, the wording
//! of OS errors, a save that turns a Codex lookup error or an nvim failure into an error record, the
//! half-built folder of a failed save, and the number of tmux calls.

mod common;

use common::{EXE, outside_tmux, text};
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};
use tmux_workspace::hook::{self, HookEnv};
use tmux_workspace::pyjson::{Style, dumps, float_repr};
use tmux_workspace::testing::{FakeHost, Proc, ScratchTmux, TempDir, TmuxMode, wait_for};
use tmux_workspace::tmux::Tmux;

const PYTHON_CLI: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/legacy/bin/tmux-workspace");
const HOOK_DRIVER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/compare/hook_driver.py");
const STATUS_CONF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/agent-status.conf");

/// Which implementation runs a scenario.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    Python,
    Rust,
}

const BOTH: [Side; 2] = [Side::Python, Side::Rust];

/// The two working folders of the scenarios. Their names carry non-ASCII text (an accent, a symbol
/// outside the Basic Multilingual Plane, Japanese, a space), so that the folder of a pane, the `-c` of a
/// restored pane and the `cwd` of a record all have to survive in both languages.
const HERE: &str = "a bé 😀";
const THERE: &str = "c ü日本";
/// A folder as a hook is told about it.
const ACCENTED: &str = "/wörk/日本 😀, é";

// ---------------------------------------------------------------------------------------------
// Running the two

/// The command line of this side, set up for the scratch server (`TMUX`, state folder, `HOME`).
fn cli(side: Side, scratch: &ScratchTmux, args: &[&str]) -> Command {
    let mut command = match side {
        Side::Rust => scratch.bin(EXE),
        Side::Python => {
            let mut command = scratch.bin("python3");
            command.arg(PYTHON_CLI);
            command
        }
    };
    // UTF-8 whatever the locale of the machine, as the Python of a user's account has it.
    command
        .args(args)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONUTF8", "1");
    command
}

/// What a command did.
#[derive(Debug, PartialEq)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn outcome(out: Output) -> Outcome {
    Outcome {
        code: out.status.code(),
        stdout: text(&out.stdout),
        stderr: text(&out.stderr),
    }
}

/// Run `command` with `typed` as its input and give what it did.
fn finish(mut command: Command, typed: &str) -> Outcome {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("start the command");
    // A command that does not read its input may be gone: the pipe is then closed.
    let _ = child.stdin.take().unwrap().write_all(typed.as_bytes());
    outcome(child.wait_with_output().unwrap())
}

fn run(side: Side, scratch: &ScratchTmux, args: &[&str]) -> Outcome {
    finish(cli(side, scratch, args), "")
}

/// A command that must succeed.
fn run_ok(side: Side, scratch: &ScratchTmux, args: &[&str]) -> Outcome {
    let done = run(side, scratch, args);
    assert_eq!(done.code, Some(0), "{side:?} {args:?}: {}", done.stderr);
    done
}

/// Poll until `read` gives `expected`, for up to 15 s, then assert that it does.
fn eventually<T: PartialEq + std::fmt::Debug>(read: impl Fn() -> T, expected: T) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while read() != expected && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(read(), expected);
}

// ---------------------------------------------------------------------------------------------
// Normalising what is not the same by nature: random ids, times, folders and pane numbers

/// The ids met so far, in the order they were first seen: the same position is the same placeholder.
#[derive(Default)]
struct Seen {
    uids: Vec<String>,
    panes: Vec<String>,
}

impl Seen {
    fn position(list: &mut Vec<String>, token: &str) -> usize {
        list.iter()
            .position(|seen| seen == token)
            .unwrap_or_else(|| {
                list.push(token.to_string());
                list.len() - 1
            })
            + 1
    }

    /// `text` with every 32-hex id and every pane id (`%N`) replaced.
    fn scrub(&mut self, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let hex = |c: char| matches!(c, '0'..='9' | 'a'..='f');
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '%' && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                let end = (i + 1..chars.len())
                    .find(|&j| !chars[j].is_ascii_digit())
                    .unwrap_or(chars.len());
                let token: String = chars[i..end].iter().collect();
                out += &format!("<pane {}>", Self::position(&mut self.panes, &token));
                i = end;
            } else if hex(c) {
                let end = (i..chars.len())
                    .find(|&j| !hex(chars[j]))
                    .unwrap_or(chars.len());
                let token: String = chars[i..end].iter().collect();
                let alone = i == 0 || !chars[i - 1].is_alphanumeric();
                if end - i == 32 && alone && chars.get(end).is_none_or(|c| !c.is_alphanumeric()) {
                    out += &format!("<uid {}>", Self::position(&mut self.uids, &token));
                } else {
                    out += &token;
                }
                i = end;
            } else {
                out.push(c);
                i += 1;
            }
        }
        out
    }

    fn value(&mut self, value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (key, value) in map {
                    let replaced = match (key.as_str(), value) {
                        ("saved_at", _) => json!("<saved_at>"),
                        ("directory", _) => json!("<directory>"),
                        ("path", _) => json!("<path>"),
                        ("parked", Value::String(stamp)) if !stamp.is_empty() => json!("<parked>"),
                        // The process of an application differs between two servers; its kind of value must not.
                        ("pid", Value::Number(_)) => json!("<pid>"),
                        ("birth", Value::String(birth)) => {
                            json!(format!("<birth of {} characters>", birth.chars().count()))
                        }
                        _ => self.value(value),
                    };
                    out.insert(key.clone(), replaced);
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.value(item)).collect())
            }
            Value::String(string) => Value::String(self.scrub(string)),
            other => other.clone(),
        }
    }
}

/// `v` with the 32-hex ids and pane ids replaced by placeholders that count from their first
/// appearance, `saved_at`, `directory`, `path` and parked stamps by one placeholder each, and the pid
/// and birth of an application by a placeholder that keeps their kind (a number; a text of that length).
fn normalise(v: &Value) -> Value {
    Seen::default().value(v)
}

/// The same for the text of a file in the layout `json.dumps(indent=2)` gives, which has one key to a
/// line: the volatile values go, and so do the ids, and everything else is compared byte for byte,
/// the end of each line and the end of the file included (Python writes no newline at the end).
fn normalise_text(raw: &str) -> String {
    let mut lines = Vec::new();
    for line in raw.split('\n') {
        let (line, cr) = line
            .strip_suffix('\r')
            .map_or((line, ""), |line| (line, "\r"));
        let indent = line.len() - line.trim_start().len();
        let rest = line.trim_start();
        let comma = if rest.ends_with(',') { "," } else { "" };
        let replaced = ["saved_at", "directory", "path", "pid"]
            .iter()
            .find_map(|key| {
                let head = format!("\"{key}\": ");
                rest.starts_with(&head)
                    .then(|| format!("{}{head}<{key}>{comma}{cr}", " ".repeat(indent)))
            });
        let birth = rest
            .strip_prefix("\"birth\": \"")
            .and_then(|value| {
                value
                    .strip_suffix('"')
                    .or_else(|| value.strip_suffix("\","))
            })
            .map(|value| {
                format!(
                    "{}\"birth\": <birth of {} characters>{comma}{cr}",
                    " ".repeat(indent),
                    value.chars().count()
                )
            });
        let parked = rest
            .strip_prefix("\"parked\": \"")
            .is_some_and(|value| !value.starts_with('"'));
        lines.push(match replaced.or(birth) {
            Some(line) => line,
            None if parked => format!("{}\"parked\": <parked>{comma}{cr}", " ".repeat(indent)),
            None => format!("{line}{cr}"),
        });
    }
    Seen::default().scrub(&lines.join("\n"))
}

/// The two volatile values of `latest.json`, which Python writes as one line with the separators of
/// `json.dumps` and no newline: `(saved_at as written, path)`. Anything else is not that file.
fn latest_values(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("{\"saved_at\": ")?;
    let (saved_at, path) = rest.split_once(", \"path\": \"")?;
    Some((saved_at, path.strip_suffix("\"}")?))
}

/// The text of `latest.json` with its two volatile values replaced; a text of another shape is kept
/// as it is, so that it differs from the right one.
fn normalise_latest(raw: &str) -> String {
    match latest_values(raw) {
        Some(_) => "{\"saved_at\": <saved_at>, \"path\": \"<path>\"}".to_string(),
        None => format!("not the shape of latest.json: {raw:?}"),
    }
}

#[test]
fn normalise_replaces_only_what_differs_by_nature() {
    let uid_a = "0123456789abcdef0123456789abcdef";
    let uid_b = "fedcba9876543210fedcba9876543210";
    let one = json!({"saved_at": 1.5, "directory": "/x/1", "panes": [
        {"id": uid_a, "parked": "170", "label": "%7 in text", "cwd": "/w", "n": 3},
        {"id": uid_b, "parked": "", "label": "b", "cwd": "/w", "n": 4}]});
    let other = json!({"saved_at": 9.25, "directory": "/y/2", "panes": [
        {"id": uid_b, "parked": "999", "label": "%2 in text", "cwd": "/w", "n": 3},
        {"id": uid_a, "parked": "", "label": "b", "cwd": "/w", "n": 4}]});
    assert_eq!(normalise(&one), normalise(&other));
    // What is real still counts: a label, a folder, a number, an empty stamp that is not, an id
    // that repeats where the other does not.
    for (pane, key, change) in [
        (1, "label", json!("c")),
        (1, "cwd", json!("/v")),
        (1, "n", json!(5)),
        (0, "parked", json!("")),
        (1, "parked", json!("5")),
    ] {
        let mut changed = other.clone();
        changed["panes"][pane][key] = change;
        assert_ne!(normalise(&one), normalise(&changed), "{key}");
    }
    let mut repeated = other.clone();
    repeated["panes"][1]["id"] = json!(uid_b);
    assert_ne!(normalise(&one), normalise(&repeated));
}

#[test]
fn normalise_text_keeps_the_bytes_that_are_not_volatile() {
    let uid = "0123456789abcdef0123456789abcdef";
    let file = |at: &str, dir: &str, stamp: &str, gap: &str| {
        format!(
            "{{\n  \"saved_at\": {at},\n  \"directory\": \"{dir}\",\n  \"panes\": [\n    {{\n      \"id\": \"{uid}\",\n      \"parked\": \"{stamp}\",\n      \"label\": \"a{gap}b\"\n    }}\n  ]\n}}"
        )
    };
    let base = file("1.5", "/x/1", "17", " ");
    assert_eq!(
        normalise_text(&base),
        normalise_text(&file("2.75", "/y/22", "99", " "))
    );
    assert_ne!(
        normalise_text(&base),
        normalise_text(&file("1.5", "/x/1", "17", "  "))
    );
    assert_ne!(
        normalise_text(&base),
        normalise_text(&file("1.5", "/x/1", "", " "))
    );
    // The end of the file and of every line count: a newline at the end, and a carriage return.
    assert_ne!(normalise_text(&base), normalise_text(&format!("{base}\n")));
    assert_ne!(
        normalise_text(&base),
        normalise_text(&base.replace('\n', "\r\n"))
    );
    assert_ne!(
        normalise_text(&base),
        normalise_text(&base.replace("\"saved_at\": 1.5,", "\"saved_at\": 1.5,\r"))
    );
}

#[test]
fn normalise_latest_keeps_the_shape_of_latest_json() {
    let latest = |at: &str, path: &str| format!("{{\"saved_at\": {at}, \"path\": \"{path}\"}}");
    let base = latest("1.5", "/x/snapshots/1/workspace.json");
    assert_eq!(
        normalise_latest(&base),
        normalise_latest(&latest("2.25", "/y/snapshots/22/workspace.json"))
    );
    assert_eq!(
        latest_values(&base),
        Some(("1.5", "/x/snapshots/1/workspace.json"))
    );
    for other in [
        format!("{base}\n"),
        base.replace(", ", ","),
        base.replace("\"path\": ", "\"path\":"),
        base.replace("{\"saved_at\"", "{ \"saved_at\""),
        "{\"path\": \"/x\", \"saved_at\": 1.5}".to_string(),
    ] {
        assert_ne!(
            normalise_latest(&base),
            normalise_latest(&other),
            "{other:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The scenario of the snapshot tests

/// What `list-panes -a` says about every pane, one sorted line each: where it is, what it is called,
/// where it runs, and how it is laid out. An unlabelled pane counts as `shell`, as the record of a
/// snapshot says it does.
fn listing(scratch: &ScratchTmux) -> Vec<String> {
    let format = [
        "#{session_name}",
        "#{window_index}",
        "#{window_name}",
        "#{pane_index}",
        "#{@workspace_label}",
        "#{pane_current_path}",
        "#{@workspace_parked}",
        "#{@workspace_home}",
        "#{pane_left},#{pane_top},#{pane_width}x#{pane_height}",
        "zoomed=#{window_zoomed_flag}",
        "window=#{window_active}",
        "pane=#{pane_active}",
    ]
    .join("|");
    let out = scratch.tmux(&["list-panes", "-a", "-F", &format]);
    let mut lines: Vec<String> = out
        .lines()
        .map(|line| {
            let mut fields: Vec<&str> = line.split('|').collect();
            if fields[4].is_empty() {
                fields[4] = "shell";
            }
            fields.join("|")
        })
        .collect();
    lines.sort();
    lines
}

/// A server with the sessions `café 😀` (windows `wïn 1` with two panes, and `win 2` at index 5) and
/// `beta` (window `code` with two panes, one of them zoomed, and a third that is parked), besides
/// `alpha`. Labels with quotes and non-ASCII text, one pane without a label, one id that is a valid
/// uid and one that is not. Restored panes exec `/bin/sh`, never the developer's shell.
#[rustfmt::skip]
fn populate(s: &ScratchTmux, work: &Path) {
    let sleep = "exec sleep 300";
    let (here, there) = (work.join(HERE), work.join(THERE));
    let (here, there) = (here.to_str().unwrap(), there.to_str().unwrap());
    s.tmux(&["set-option", "-g", "default-shell", "/bin/sh"]);
    s.tmux(&["set-environment", "-g", "SHELL", "/bin/sh"]);
    let runs_sleep = |pane: &str| wait_for(|| s.expand(pane, "#{pane_current_command}"), "sleep");
    let session = |name: &str, window: &str, dir: &str, command: &str| {
        let pane = s.tmux(&["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", name, "-n", window, "-x", "120", "-y", "40", "-c", dir, command]);
        runs_sleep(&pane);
        pane
    };

    // Text on the screen that a snapshot keeps as plain text: an accent, a colour, wide characters, and
    // a line longer than the pane, which wraps and is joined again.
    let shown = r"printf 'caf\303\251 \033[31mred\033[0m\n\346\227\245\346\234\254 \360\237\230\200  \n'; printf '%0200d\n' 7; exec sleep 300";
    let cafe_main = session("café 😀", "wïn 1", here, shown);
    let cafe_side = s.pane(&["split-window", "-t", &cafe_main, "-h", "-l", "30%", "-c", there]);
    s.pane(&["new-window", "-t", "café 😀:5", "-n", "win 2", "-c", here]);

    let beta_api = session("beta", "code", there, sleep);
    let beta_db = s.pane(&["split-window", "-t", &beta_api, "-v", "-c", here]);
    let beta_logs = s.pane(&["split-window", "-t", &beta_db, "-h", "-c", there]);

    // The last pane is parked, the way `park` leaves one.
    let holding = s.tmux(&["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", "__parked", "-n", "holding", sleep]);
    s.tmux(&["break-pane", "-d", "-s", &beta_logs, "-t", "=__parked:", "-n", "logs"]);
    s.tmux(&["kill-pane", "-t", &holding]);
    s.tmux(&["set-option", "-p", "-t", &beta_logs, "@workspace_parked", "1700000000000000001"]);
    s.tmux(&["set-option", "-p", "-t", &beta_logs, "@workspace_home", "beta"]);
    // One pane zoomed, so that the zoom has to come back with the layout.
    s.tmux(&["resize-pane", "-Z", "-t", &beta_db]);

    for (pane, label) in [
        (&cafe_main, r#"a'b "c""#),
        (&cafe_side, "日本語 🙂"),
        (&beta_api, "api"),
        (&beta_db, "db"),
        (&beta_logs, "logs"),
    ] {
        s.tmux(&["set-option", "-p", "-t", pane, "@workspace_label", label]);
    }
    s.tmux(&["set-option", "-p", "-t", &cafe_main, "@workspace_id", "0123456789abcdef0123456789abcdef"]);
    s.tmux(&["set-option", "-p", "-t", &cafe_side, "@workspace_id", "not-a-uid"]);
}

/// Panes that a snapshot leaves out (the bootstrap session, and a pane of the parking session that is
/// not parked), and records of applications in some panes, as the hook leaves them in `@workspace_app`:
/// a Claude and an omp that run, a Claude with no conversation id, an nvim with a session file, and a
/// record of a process that is gone. The ids are awkward on purpose. The pid and the birth are those
/// of the pane's own process, so that the record counts as running.
#[rustfmt::skip]
fn add_extras(s: &ScratchTmux) {
    // The bootstrap session, and a pane in the parking session that is not parked, are not saved.
    s.tmux(&["new-session", "-d", "-s", "__workspace_bootstrap", "-n", "boot", "exec sleep 300"]);
    s.tmux(&["new-window", "-d", "-t", "__parked:", "-n", "stray", "exec sleep 300"]);
    let format = "#{pane_id}|#{@workspace_label}|#{pane_pid}";
    for line in s.tmux(&["list-panes", "-a", "-F", format]).lines() {
        let mut parts = line.split('|');
        let (pane, label, pid) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        let birth = Command::new("ps").args(["-p", pid, "-o", "lstart="]).output().unwrap();
        let birth = text(&birth.stdout).trim().to_string();
        let live = |kind: &str, id: &str| json!({"kind": kind, "id": id, "pid": pid.parse::<i64>().unwrap(), "cwd": "/wörk dir 😀", "birth": birth});
        let record = match label {
            "日本語 🙂" => live("claude", "$HOME \"q\" ünï 😀 #{x}"),
            r#"a'b "c""# => live("omp", "/sessions/a b.jsonl"),
            "api" => live("claude", ""),
            "db" => live("nvim", "/sessions/db.vim"),
            "" => json!({"kind": "codex", "id": "gone", "pid": 999999, "cwd": "/work", "birth": "Thu Oct  1 20:23:29 2026"}),
            _ => continue,
        };
        s.tmux(&["set-option", "-p", "-t", pane, "@workspace_app", &dumps(&record, Style::Compact, false)]);
    }
}

/// A shared working folder with the two subfolders the scenario runs panes in.
fn work_folder() -> TempDir {
    let work = TempDir::new();
    for name in [HERE, THERE] {
        std::fs::create_dir(work.path().join(name)).unwrap();
    }
    work
}

/// Where a save left its snapshot: the folder, `workspace.json` as text and as a value.
struct Saved {
    directory: PathBuf,
    text: String,
    value: Value,
}

fn saved(scratch: &ScratchTmux) -> Saved {
    let latest = std::fs::read_to_string(scratch.state_dir().join("latest.json")).unwrap();
    let latest: Value = serde_json::from_str(&latest).unwrap();
    let path = PathBuf::from(latest["path"].as_str().unwrap());
    let text = std::fs::read_to_string(&path).unwrap();
    Saved {
        directory: path.parent().unwrap().to_path_buf(),
        value: serde_json::from_str(&text).unwrap(),
        text,
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The files of a snapshot folder besides `workspace.json`, as (name with the id replaced by its
/// position among the panes, contents), sorted.
fn snapshot_files(saved: &Saved) -> Vec<(String, String)> {
    let uids: Vec<&str> = saved.value["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| pane["id"].as_str().unwrap())
        .collect();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&saved.directory).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "workspace.json" {
            continue;
        }
        let shown = match uids.iter().position(|uid| name == format!("{uid}.txt")) {
            Some(i) => format!("<uid {}>.txt", i + 1),
            None => name,
        };
        files.push((shown, std::fs::read_to_string(entry.path()).unwrap()));
    }
    files.sort();
    files
}

#[test]
fn save_writes_the_same_snapshot() {
    let work = work_folder();
    let (python, rust) = (ScratchTmux::new(), ScratchTmux::new());
    populate(&python, work.path());
    populate(&rust, work.path());
    add_extras(&python);
    add_extras(&rust);
    run_ok(Side::Python, &python, &["save"]);
    run_ok(Side::Rust, &rust, &["save"]);
    let (py, rs) = (saved(&python), saved(&rust));
    // The applications that run are saved; the one that is gone is not, and an idless one says so.
    // Neither the bootstrap session nor the stray pane of the parking session is in the snapshot.
    let windows = py.value["windows"].as_array().unwrap();
    assert!(
        windows
            .iter()
            .all(|w| w["window_name"] != "boot" && w["window_name"] != "stray")
    );
    let apps: Vec<&Value> = py.value["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["app"])
        .collect();
    assert_eq!(
        apps.iter().filter(|app| !app.is_null()).count(),
        4,
        "{apps:?}"
    );
    assert!(
        apps.iter()
            .any(|app| app["error"] == "No exact conversation identity was recorded")
    );

    // The records, windows and their order, as values; the file, byte for byte.
    // alpha's pane, three in `café 😀`, two live in `beta` and the parked one.
    assert_eq!(py.value["panes"].as_array().unwrap().len(), 7);
    assert_eq!(normalise(&py.value), normalise(&rs.value));
    assert_eq!(normalise_text(&py.text), normalise_text(&rs.text));
    // Python writes no newline at the end; the comparison above would see one more, and so does this.
    assert!(!py.text.ends_with('\n') && !rs.text.ends_with('\n'));

    // Each side says where its snapshot is: `directory` is the folder that holds workspace.json, a
    // folder named by digits under `snapshots`, and `latest.json` points at the file in it.
    // `latest.json` is compared as written, with only its two volatile values replaced.
    let latest_text = |scratch: &ScratchTmux| {
        std::fs::read_to_string(scratch.state_dir().join("latest.json")).unwrap()
    };
    for (side, scratch, saved) in [("python", &python, &py), ("rust", &rust, &rs)] {
        let (snapshots, folder) = (scratch.state_dir().join("snapshots"), &saved.directory);
        assert_eq!(folder.parent().unwrap(), snapshots, "{side}");
        let name = folder.file_name().unwrap().to_str().unwrap();
        assert!(name.bytes().all(|b| b.is_ascii_digit()), "{side}: {name}");
        assert_eq!(
            saved.value["directory"],
            json!(folder.to_str().unwrap()),
            "{side}"
        );
        let raw = latest_text(scratch);
        let (at, path) = latest_values(&raw).unwrap_or_else(|| panic!("{side}: {raw:?}"));
        assert_eq!(
            path,
            folder.join("workspace.json").to_str().unwrap(),
            "{side}"
        );
        assert!(folder.join("workspace.json").is_file(), "{side}");
        // Both files carry the time of the save, as Python writes a float.
        let line = saved
            .text
            .lines()
            .find_map(|l| l.trim().strip_prefix("\"saved_at\": "));
        let in_snapshot = line.unwrap().trim_end_matches(',');
        assert_eq!(in_snapshot, at, "{side}: the same time in both files");
        assert_eq!(float_repr(at.parse().unwrap()), at, "{side}");
    }
    assert_eq!(
        normalise_latest(&latest_text(&python)),
        normalise_latest(&latest_text(&rust))
    );
    assert!(latest_values(&latest_text(&python)).is_some());

    // The saved text of every pane, and who may read the files.
    let (py_files, rs_files) = (snapshot_files(&py), snapshot_files(&rs));
    assert_eq!(py_files, rs_files);
    assert_eq!(py_files.len(), 7);
    assert!(
        py_files
            .iter()
            .any(|(_, text)| text.starts_with("café red")),
        "{py_files:?}"
    );
    for (scratch, saved) in [(&python, &py), (&rust, &rs)] {
        let state = scratch.state_dir();
        assert_eq!(
            (
                mode(state),
                mode(&state.join("snapshots")),
                mode(&saved.directory),
                mode(&state.join("latest.json"))
            ),
            (0o700, 0o700, 0o700, 0o600)
        );
        for entry in std::fs::read_dir(&saved.directory).unwrap() {
            assert_eq!(mode(&entry.unwrap().path()), 0o600);
        }
    }
    // What the save did to the server: every pane has an id, the same options are set.
    let server = |scratch: &ScratchTmux| {
        let format = "#{session_name}|#{window_index}|#{pane_index}|#{@workspace_id}";
        let ids = Seen::default().scrub(&scratch.tmux(&["list-panes", "-a", "-F", format]));
        let option = |name: &str| scratch.tmux(&["show-options", "-gqv", name]);
        let at = option("@workspace_saved");
        assert!(at.len() == 5 && at.as_bytes()[2] == b':', "{at}");
        (ids, option("@workspace_save_error"))
    };
    assert_eq!(server(&python), server(&rust));
}

/// Save with `saver`, lose the server, and restore on a new one with `restorer`: the panes of the new
/// server are the panes of the old one.
fn save_and_restore_across(saver: Side, restorer: Side) {
    let work = work_folder();
    let old = ScratchTmux::new();
    populate(&old, work.path());
    let expected = listing(&old);
    run_ok(saver, &old, &["save"]);
    let snapshots = old.state_dir().to_path_buf();
    old.tmux(&["kill-server"]);

    let new = ScratchTmux::new();
    new.tmux(&["set-option", "-g", "default-shell", "/bin/sh"]);
    new.tmux(&["set-environment", "-g", "SHELL", "/bin/sh"]);
    let mut command = cli(restorer, &new, &["restore"]);
    command.env("TMUX_WORKSPACE_STATE_DIR", &snapshots);
    let done = finish(command, "");
    assert_eq!(done.code, Some(0), "{restorer:?}: {}", done.stderr);
    // The panes start themselves, so their labels arrive a moment later.
    eventually(|| listing(&new), expected);
    // The restore is over, and the server knows.
    assert_eq!(
        new.tmux(&["show-options", "-gqv", "@workspace_restoring"]),
        "0"
    );
}

#[test]
fn python_snapshot_restores_with_rust() {
    save_and_restore_across(Side::Python, Side::Rust);
}

#[test]
fn rust_snapshot_restores_with_python() {
    save_and_restore_across(Side::Rust, Side::Python);
}

/// A session `gamma` with a window of four panes in a tiled layout. Python's restore does not bring
/// such a window back as it was (two of its panes change places), so the window is not compared with
/// the original: Rust has to give the answer that Python gives.
#[rustfmt::skip]
fn crowd(s: &ScratchTmux, work: &Path) {
    s.tmux(&["set-option", "-g", "default-shell", "/bin/sh"]);
    s.tmux(&["set-environment", "-g", "SHELL", "/bin/sh"]);
    let dir = work.join(THERE);
    let dir = dir.to_str().unwrap();
    let first = s.tmux(&["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", "gamma", "-n", "grid", "-x", "120", "-y", "40", "-c", dir, "exec sleep 300"]);
    wait_for(|| s.expand(&first, "#{pane_current_command}"), "sleep");
    let second = s.pane(&["split-window", "-t", &first, "-h", "-c", dir]);
    let third = s.pane(&["split-window", "-t", &second, "-v", "-c", dir]);
    let fourth = s.pane(&["split-window", "-t", &first, "-v", "-c", dir]);
    s.tmux(&["select-layout", "-t", &first, "tiled"]);
    for (pane, label) in [(&first, "a"), (&second, "b"), (&third, "c"), (&fourth, "d")] {
        s.tmux(&["set-option", "-p", "-t", pane, "@workspace_label", label]);
    }
}

#[test]
fn restore_lays_out_a_crowded_window_as_python_does() {
    let work = work_folder();
    let old = ScratchTmux::new();
    crowd(&old, work.path());
    run_ok(Side::Python, &old, &["save"]);
    let restored = |restorer: Side| {
        let new = ScratchTmux::new();
        new.tmux(&["set-option", "-g", "default-shell", "/bin/sh"]);
        new.tmux(&["set-environment", "-g", "SHELL", "/bin/sh"]);
        let mut command = cli(restorer, &new, &["restore"]);
        command.env("TMUX_WORKSPACE_STATE_DIR", old.state_dir());
        let done = finish(command, "");
        assert_eq!(done.code, Some(0), "{restorer:?}: {}", done.stderr);
        let labels = || -> Vec<String> {
            let mut labels: Vec<String> = new
                .tmux(&[
                    "list-panes",
                    "-s",
                    "-t",
                    "gamma",
                    "-F",
                    "#{@workspace_label}",
                ])
                .lines()
                .map(String::from)
                .collect();
            labels.sort();
            labels
        };
        eventually(labels, ["a", "b", "c", "d"].map(String::from).to_vec());
        let listed = listing(&new);
        (new, listed)
    };
    let (_python, by_python) = restored(Side::Python);
    let (_rust, by_rust) = restored(Side::Rust);
    assert_eq!(
        by_python.iter().filter(|l| l.starts_with("gamma|")).count(),
        4
    );
    assert_eq!(by_python, by_rust);
}

/// Wait for `child` to end, for up to `limit`; `None` if it is still there.
fn wait_for_exit(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How long a save that does not wait for the lock is given to finish before it counts as waiting. It
/// takes a fraction of this. A machine too slow for that can only make a save that ignores the lock
/// look as if it waited; it can never make one that waits fail, because nothing else is timed.
const GRACE: Duration = Duration::from_millis(700);

/// Both lock `<state>/lock` with flock, so a Python save and a Rust save take turns. Each half starts
/// the save while the other side holds the lock, shows that the save has not finished (it has not
/// written `latest.json`) after the grace time, and only then lets go of the lock: the save must then
/// finish by itself. The holder says when it holds the lock and keeps it until it is told to let go, so
/// nothing depends on how long a process takes to start.
#[test]
fn a_save_waits_for_the_lock_of_the_other_side() {
    // Python holds the lock; the Rust save waits for it.
    let scratch = ScratchTmux::new();
    let latest = scratch.state_dir().join("latest.json");
    let mut holder = Command::new("python3")
        .args([
            "-c",
            "import fcntl, sys; f = open(sys.argv[1], 'a'); fcntl.flock(f, fcntl.LOCK_EX); print('held', flush=True); sys.stdin.readline()",
        ])
        .arg(scratch.state_dir().join("lock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut said = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(holder.stdout.take().unwrap()),
        &mut said,
    )
    .unwrap();
    assert_eq!(said, "held\n", "Python holds the lock");
    let mut save = cli(Side::Rust, &scratch, &["save"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(GRACE);
    assert!(
        save.try_wait().unwrap().is_none() && !latest.exists(),
        "the Rust save went on while Python held the lock"
    );
    writeln!(holder.stdin.take().unwrap(), "let go").unwrap();
    let status = wait_for_exit(&mut save, Duration::from_secs(60));
    assert!(
        status.is_some_and(|status| status.success()),
        "the Rust save: {status:?}"
    );
    assert!(latest.exists());
    holder.wait().unwrap();

    // Rust holds the lock; the Python save waits for it.
    let scratch = ScratchTmux::new();
    let latest = scratch.state_dir().join("latest.json");
    let held = tmux_workspace::snapshot::lock_in(scratch.state_dir()).unwrap();
    let mut save = cli(Side::Python, &scratch, &["save"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(GRACE);
    assert!(
        save.try_wait().unwrap().is_none() && !latest.exists(),
        "the Python save went on while Rust held the lock"
    );
    drop(held);
    let status = wait_for_exit(&mut save, Duration::from_secs(60));
    assert!(
        status.is_some_and(|status| status.success()),
        "the Python save: {status:?}"
    );
    assert!(latest.exists());
}

/// The snapshot folders of a state folder, oldest first.
fn snapshot_folders(scratch: &ScratchTmux) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(scratch.state_dir().join("snapshots"))
        .map(|dir| {
            dir.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Save over and over, tick, and save while a restore is on: what is kept, and when a save is skipped.
/// What each does is written down as facts, and the facts are the same.
#[test]
fn save_keeps_three_snapshots_and_tick_and_restoring_skip_a_save() {
    let facts = |side: Side| -> Vec<String> {
        let scratch = ScratchTmux::new();
        scratch.pane(&["split-window", "-t", "alpha:one"]);
        let mut facts = Vec::new();
        let newest = |facts: &mut Vec<String>, what: &str| {
            let folders = snapshot_folders(&scratch);
            let latest = std::fs::read_to_string(scratch.state_dir().join("latest.json")).unwrap();
            let latest: Value = serde_json::from_str(&latest).unwrap();
            let last = folders.last().unwrap();
            let points_at_newest = latest["path"].as_str().unwrap().contains(last.as_str());
            let digits = folders
                .iter()
                .all(|n| n.bytes().all(|b| b.is_ascii_digit()));
            facts.push(format!(
                "{what}: {} folders, digits {digits}, latest is the newest {points_at_newest}",
                folders.len()
            ));
            last.clone()
        };
        let mut last = String::new();
        for round in 1..=5 {
            run_ok(side, &scratch, &["save"]);
            last = newest(&mut facts, &format!("save {round}"));
            std::thread::sleep(Duration::from_millis(20));
        }
        // A tick right after a save does nothing; one after the time is up saves.
        run_ok(side, &scratch, &["tick"]);
        facts.push(format!(
            "tick after a save: nothing new {}",
            newest(&mut Vec::new(), "") == last
        ));
        let latest = scratch.state_dir().join("latest.json");
        let mut old: Value =
            serde_json::from_str(&std::fs::read_to_string(&latest).unwrap()).unwrap();
        old["saved_at"] = json!(1.0);
        std::fs::write(&latest, old.to_string()).unwrap();
        run_ok(side, &scratch, &["tick"]);
        let after = newest(&mut facts, "tick after 5 minutes");
        facts.push(format!("a new folder {}", after != last));
        // A save while a restore is on is skipped, even a forced one.
        scratch.tmux(&["set-option", "-g", "@workspace_restoring", "1"]);
        run_ok(side, &scratch, &["save"]);
        facts.push(format!(
            "save while restoring: nothing new {}",
            newest(&mut Vec::new(), "") == after
        ));
        scratch.tmux(&["set-option", "-g", "@workspace_restoring", "0"]);
        facts.push(format!(
            "save error [{}], saved at {}",
            scratch.tmux(&["show-options", "-gqv", "@workspace_save_error"]),
            scratch
                .tmux(&["show-options", "-gqv", "@workspace_saved"])
                .len()
        ));
        facts
    };
    let (python, rust) = (facts(Side::Python), facts(Side::Rust));
    assert_eq!(python.len(), 10, "{python:?}");
    assert!(python[4].starts_with("save 5: 3 folders"), "{python:?}");
    assert_eq!(python, rust);
}

// ---------------------------------------------------------------------------------------------
// pane-start and history read what a save wrote

/// The fake programs a pane could start, in a folder of their own: the apps note where they ran and
/// with what, and exit with `$APP_STATUS`; the shell notes itself.
struct Starts {
    scratch: ScratchTmux,
    files: TempDir,
    pane: String,
}

/// What a command is given as its input: a pipe that holds this and ends, or a terminal on which this
/// was typed.
#[derive(Clone, Copy)]
enum Input<'a> {
    Pipe(&'a str),
    Terminal(&'a str),
}

/// What a `pane-start` left behind.
#[derive(Debug, PartialEq)]
struct Started {
    done: Outcome,
    apps: Vec<String>,
    shells: Vec<String>,
    options: Vec<String>,
}

const UID: &str = "0123456789abcdef0123456789abcdef";
/// The folder an application is started in: its name is not ASCII.
const CWD: &str = "cwd é 日本 😀";

impl Starts {
    fn new() -> Starts {
        let scratch = ScratchTmux::new();
        let pane = scratch.expand("alpha:one", "#{pane_id}");
        let files = TempDir::new();
        let dir = files.path();
        for name in ["bin", "shells", CWD] {
            std::fs::create_dir(dir.join(name)).unwrap();
        }
        for app in ["claude", "codex", "omp", "nvim"] {
            let log = dir.join("apps.log");
            let body = format!(
                "#!/bin/sh\nprintf '%s|%s\\n' \"$(pwd -P)\" \"$*\" >> '{}'\nexit ${{APP_STATUS:-0}}\n",
                log.display()
            );
            tmux_workspace::testing::write_script(&dir.join("bin").join(app), &body);
        }
        let body = format!(
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$0\" \"$*\" \"$SHELL\" >> '{}'\n",
            dir.join("shells.log").display()
        );
        tmux_workspace::testing::write_script(&dir.join("shells/quiet"), &body);
        std::fs::write(dir.join("session.jsonl"), "{}").unwrap();
        Starts {
            scratch,
            files,
            pane,
        }
    }

    fn path(&self, name: &str) -> String {
        self.files
            .path()
            .canonicalize()
            .unwrap()
            .join(name)
            .to_str()
            .unwrap()
            .to_string()
    }

    /// The snapshot a save would have written with this one pane, and `saved` as its text.
    fn snapshot(&self, record: Value, saved: Option<&str>) -> String {
        let folder = self.files.path().canonicalize().unwrap();
        if let Some(saved) = saved {
            std::fs::write(folder.join(format!("{UID}.txt")), saved).unwrap();
        }
        let data = json!({"version": 1, "saved_at": 1_700_000_000, "directory": folder, "windows": [], "panes": [record]});
        std::fs::write(folder.join("snapshot.json"), data.to_string()).unwrap();
        self.path("snapshot.json")
    }

    /// `pane-start` by this side, in the pane, with the fakes first on `PATH`, UTC, and `APP_STATUS`.
    fn start(&self, side: Side, args: &[&str], input: Input, status: &str) -> Started {
        let mut command = cli(side, &self.scratch, args);
        let path = format!(
            "{}:{}",
            self.path("bin"),
            std::env::var("PATH").unwrap_or_default()
        );
        command
            .env("TMUX_PANE", &self.pane)
            .env("PATH", path)
            .env("TMUX_WORKSPACE_SHELL", self.path("shells/quiet"))
            .env("APP_STATUS", status)
            .env("TZ", "UTC");
        let here = self
            .files
            .path()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let plain = |text: &str| text.replace(&here, "<files>");
        let lines = |file: &str| {
            let text = std::fs::read_to_string(self.files.path().join(file)).unwrap_or_default();
            plain(&text).lines().map(String::from).collect()
        };
        let mut done = match input {
            Input::Pipe(typed) => finish(command, typed),
            Input::Terminal(typed) => {
                // Python's input() writes its prompt to the error output when it talks to a terminal,
                // and Rust writes it to the output. The pane shows both, so both count as one.
                let (code, stderr, shown) = common::on_a_terminal(command, typed);
                Outcome {
                    code: Some(code),
                    stdout: shown + &stderr,
                    stderr: String::new(),
                }
            }
        };
        done.stdout = plain(&done.stdout);
        done.stderr = plain(&done.stderr);
        // The id of a pane that had none is made up on the spot.
        let mut seen = Seen::default();
        let options = pane_options(&self.scratch)
            .into_iter()
            .filter(|line| line.contains(&self.pane))
            .map(|line| seen.scrub(&line))
            .collect();
        Started {
            done,
            apps: lines("apps.log"),
            shells: lines("shells.log"),
            options,
        }
    }
}

/// A record of the one pane of a snapshot, as a save writes it.
fn record(label: &str, app: Value) -> Value {
    json!({"id": UID, "session": "alpha", "window": 0, "index": 0, "label": label, "cwd": "/", "active": true, "parked": "", "home": "", "app": app})
}

#[test]
#[rustfmt::skip]
fn pane_start_prints_and_starts_the_same() {
    type Case = (&'static str, fn(&Starts) -> Value, Option<&'static str>, &'static str);
    let cases: Vec<Case> = vec![
        ("nothing to reopen", |_| record("agent", json!(null)), Some("plain\n"), "0"),
        ("label, parked and home", |_| { let mut r = record("  my\u{7} agent  ", json!(null)); r["parked"] = json!("1700000000"); r["home"] = json!("alpha:one"); r }, None, "0"),
        ("an unlabelled pane", |_| record("", json!(null)), None, "0"),
        ("a long label", |_| record(&"x".repeat(100), json!(null)), None, "0"),
        ("saved text with control characters", |_| record("t", json!(null)), Some("plain\ttab\n\u{1b}[31mred\u{7}\r\nnext\rlast\ncaf\u{e9}\u{200b}\u{85}\u{2028}x\n"), "0"),
        ("saved text that ends without a newline", |_| record("t", json!(null)), Some("no newline"), "0"),
        ("claude", |r| record("c", json!({"kind": "claude", "id": "abc", "cwd": r.path(CWD)})), None, "0"),
        ("claude that fails", |r| record("c", json!({"kind": "claude", "id": "abc", "cwd": r.path(CWD)})), None, "3"),
        ("codex without a folder of its own", |_| record("c", json!({"kind": "codex", "id": "abc"})), None, "0"),
        ("omp with a session file", |r| record("o", json!({"kind": "omp", "id": r.path("session.jsonl")})), None, "0"),
        ("omp with a file that is gone", |_| record("o", json!({"kind": "omp", "id": "/nowhere/s.jsonl"})), None, "0"),
        ("nvim with a file that is gone", |_| record("n", json!({"kind": "nvim", "id": "/nowhere/s.vim"})), None, "0"),
        ("nvim with a name", |_| record("n", json!({"kind": "nvim", "id": "name"})), None, "0"),
        ("an error", |_| record("e", json!({"kind": "codex", "error": "Codex has not published a current thread-id title"})), None, "0"),
        ("an unknown program", |_| record("v", json!({"kind": "vim", "id": "abc"})), None, "0"),
        ("an id that is an option", |_| record("v", json!({"kind": "claude", "id": "--help"})), None, "0"),
        ("no id", |_| record("v", json!({"kind": "claude"})), None, "0"),
        ("an id that is not text", |_| record("v", json!({"kind": "claude", "id": 7})), None, "0"),
    ];
    for (what, make, saved, status) in cases {
        let (python, rust) = (Starts::new(), Starts::new());
        let mut results = Vec::new();
        for (side, rig) in [(Side::Python, &python), (Side::Rust, &rust)] {
            let snapshot = rig.snapshot(make(rig), saved);
            results.push(rig.start(side, &["pane-start", &snapshot, UID], Input::Pipe(""), status));
        }
        assert_eq!(results[0], results[1], "{what}");
        assert_eq!(results[0].done.code, Some(0), "{what}: {}", results[0].done.stderr);
        assert_eq!(results[0].shells.len(), 1, "{what}: the shell starts last");
    }
}

#[test]
fn pane_start_without_a_snapshot_asks_for_the_label() {
    // On a terminal, as in a pane: what is typed becomes the label.
    let long = format!("{}\n", "x".repeat(100));
    for typed in [
        "my label\n",
        "  spaced  \n",
        "\n",
        "caf\u{e9} \u{1F600}\n",
        long.as_str(),
    ] {
        let (python, rust) = (Starts::new(), Starts::new());
        let py = python.start(Side::Python, &["pane-start"], Input::Terminal(typed), "0");
        let rs = rust.start(Side::Rust, &["pane-start"], Input::Terminal(typed), "0");
        assert_eq!(py, rs, "{typed:?}");
        assert!(
            py.done.stdout.starts_with("Pane label [shell]: "),
            "{typed:?}: {}",
            py.done.stdout
        );
        assert_eq!(
            py.options.len(),
            3,
            "an id, a label and the pane: {:?}",
            py.options
        );
    }
    // At the end of input on a pipe, Python's newline after the prompt is lost: it replaces itself with
    // the shell without flushing its output. On a terminal each line is written at once, so the
    // pane shows the same. Everything else is the same.
    let (python, rust) = (Starts::new(), Starts::new());
    let py = python.start(Side::Python, &["pane-start"], Input::Pipe(""), "0");
    let rs = rust.start(Side::Rust, &["pane-start"], Input::Pipe(""), "0");
    assert_eq!(py.done.stdout, "Pane label [shell]: ");
    assert_eq!(rs.done.stdout, "Pane label [shell]: \n");
    assert_eq!(
        (py.apps, py.shells, py.options, py.done.code),
        (rs.apps, rs.shells, rs.options, rs.done.code)
    );
    assert_eq!(py.done.stderr, rs.done.stderr);
    // A pane that has its label is not asked.
    let (python, rust) = (Starts::new(), Starts::new());
    for rig in [&python, &rust] {
        rig.scratch.tmux(&[
            "set-option",
            "-p",
            "-t",
            &rig.pane,
            "@workspace_label",
            "kept",
        ]);
    }
    let py = python.start(Side::Python, &["pane-start"], Input::Pipe(""), "0");
    let rs = rust.start(Side::Rust, &["pane-start"], Input::Pipe(""), "0");
    assert_eq!(py, rs);
    assert_eq!(py.done.stdout, "");
}

#[test]
fn history_opens_the_saved_text_of_the_pane() {
    let work = work_folder();
    let (python, rust) = (ScratchTmux::new(), ScratchTmux::new());
    let mut opened = Vec::new();
    for (side, scratch) in [(Side::Python, &python), (Side::Rust, &rust)] {
        populate(scratch, work.path());
        run_ok(side, scratch, &["save"]);
        let pane = scratch
            .tmux(&["list-panes", "-a", "-F", "#{pane_id}|#{@workspace_id}"])
            .lines()
            .find_map(|l| {
                l.split_once('|')
                    .filter(|(_, id)| *id == UID)
                    .map(|(pane, _)| pane.to_string())
            })
            .unwrap();
        // `less` is the fake of the test: it records how it was started and what the file holds.
        let fakes = common::Fakes::new();
        fakes.add("less", r#"/bin/cat "$2" > "$dir/shown""#);
        let mut command = cli(side, scratch, &["history", &pane]);
        let path = format!(
            "{}:{}",
            fakes.path().display(),
            std::env::var("PATH").unwrap()
        );
        command.env("PATH", path);
        let done = finish(command, "");
        assert_eq!(done.code, Some(0), "{side:?}: {}", done.stderr);
        let args = fakes.args("less", 1);
        let directory = saved(scratch).directory;
        assert_eq!(
            args,
            [
                "--".to_string(),
                directory
                    .join(format!("{UID}.txt"))
                    .to_str()
                    .unwrap()
                    .to_string()
            ]
        );
        opened.push((done, fakes.read("shown").unwrap()));
    }
    assert_eq!(opened[0], opened[1]);
    assert!(opened[0].1.starts_with("café red\n"), "{:?}", opened[0].1);
}

// ---------------------------------------------------------------------------------------------
// park and unpark

/// Panes of the parking scenario.
struct Parking {
    /// `alpha:one` has three panes: this one (labelled `main`), `logs` and one without a label.
    first: String,
    logs: String,
    bare: String,
    /// `alpha:two` has two panes, one of them labelled with quotes.
    quoted: String,
    /// `alpha:three` has this one only.
    lonely: String,
}

fn park_world() -> (ScratchTmux, Parking) {
    let scratch = ScratchTmux::new();
    let first = scratch.expand("alpha:one", "#{pane_id}");
    let logs = scratch.pane(&["split-window", "-t", &first, "-h"]);
    let bare = scratch.pane(&["split-window", "-t", &first, "-v"]);
    let quoted = scratch.pane(&["new-window", "-t", "alpha:", "-n", "two"]);
    scratch.pane(&["split-window", "-t", &quoted, "-h"]);
    let lonely = scratch.pane(&["new-window", "-t", "alpha:", "-n", "three"]);
    for (pane, label) in [
        (&first, "main"),
        (&logs, "logs"),
        (&quoted, r#"it's "two""#),
    ] {
        scratch.tmux(&["set-option", "-p", "-t", pane, "@workspace_label", label]);
    }
    let panes = Parking {
        first,
        logs,
        bare,
        quoted,
        lonely,
    };
    (scratch, panes)
}

/// Every pane with where it is and the options it holds, sorted, with the stamps of parking replaced
/// by their length: they are nanoseconds, and differ by nature.
fn pane_options(scratch: &ScratchTmux) -> Vec<String> {
    let format = "#{pane_id}|#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{pane_left},#{pane_top},#{pane_width}x#{pane_height}";
    let mut lines = Vec::new();
    for pane in scratch.tmux(&["list-panes", "-a", "-F", format]).lines() {
        let id = pane.split('|').next().unwrap();
        lines.push(pane.to_string());
        for line in scratch.tmux(&["show-options", "-p", "-t", id]).lines() {
            let name = line.split_once(' ').map_or(line, |(name, _)| name);
            let value = scratch.tmux(&["show-options", "-pqv", "-t", id, name]);
            let is_stamp = name == "@workspace_parked" && !value.is_empty();
            let shown = if is_stamp {
                assert!(value.bytes().all(|b| b.is_ascii_digit()), "{value}");
                format!("<stamp of {} digits>", value.len())
            } else {
                value
            };
            lines.push(format!("  {id} {name} = {shown}"));
        }
    }
    lines.sort();
    lines
}

#[test]
fn park_and_unpark_write_the_same_options() {
    let ((python, py), (rust, rs)) = (park_world(), park_world());
    type Args = fn(&Parking) -> Vec<String>;
    let steps: Vec<(&str, Args, &str)> = vec![
        (
            "park a labelled pane",
            |p| vec!["park".into(), p.logs.clone()],
            "",
        ),
        (
            "park a pane without a label",
            |p| vec!["park".into(), p.bare.clone()],
            "",
        ),
        (
            "park a label with quotes",
            |p| vec!["park".into(), p.quoted.clone()],
            "",
        ),
        (
            "unpark the newest to the right",
            |p| vec!["unpark".into(), p.first.clone()],
            "",
        ),
        (
            "unpark the next below",
            |p| vec!["unpark".into(), p.first.clone(), "down".into()],
            "",
        ),
        (
            "choose the only one left",
            |p| vec!["choose-park".into(), p.first.clone(), "down".into()],
            "1\n",
        ),
        (
            "park it again, the other window",
            |p| vec!["park".into(), p.quoted.clone()],
            "",
        ),
    ];
    assert_eq!(
        pane_options(&python),
        pane_options(&rust),
        "before anything"
    );
    for (i, (what, args, typed)) in steps.iter().enumerate() {
        let (py_args, rs_args) = (args(&py), args(&rs));
        let py_args: Vec<&str> = py_args.iter().map(String::as_str).collect();
        let rs_args: Vec<&str> = rs_args.iter().map(String::as_str).collect();
        let py_done = finish(cli(Side::Python, &python, &py_args), typed);
        let rs_done = finish(cli(Side::Rust, &rust, &rs_args), typed);
        assert_eq!(py_done.code, Some(0), "step {i} {what}: {}", py_done.stderr);
        assert_eq!(py_done, rs_done, "step {i} {what}");
        assert_eq!(
            pane_options(&python),
            pane_options(&rust),
            "after step {i} {what}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Failures: the same words

/// Run the same failing command on both sides, each in a world of its own, and check that they say the
/// same, that it is what `expected` says on standard error, and that the status is 1. `prepare` makes
/// the world ready for one side and gives the arguments. Outside tmux the message also goes to
/// standard output, which counts as well.
fn same_failure(
    outside: bool,
    typed: &str,
    expected: &str,
    prepare: impl Fn(Side, &ScratchTmux, &Parking) -> Vec<String>,
) -> Outcome {
    let mut done = Vec::new();
    for side in BOTH {
        let (scratch, panes) = park_world();
        let args = prepare(side, &scratch, &panes);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut command = cli(side, &scratch, &args);
        let wrapper = TempDir::new();
        if outside {
            outside_tmux(&mut command, wrapper.path(), scratch.socket());
        }
        done.push((side, finish(command, typed)));
    }
    let (python, rust) = (done.remove(0).1, done.remove(0).1);
    assert_eq!(python.stderr, expected, "python");
    assert_eq!(python.code, Some(1), "python");
    assert_eq!(python, rust);
    python
}

#[test]
fn every_cli_error_text_matches() {
    let error = |text: &str| format!("Workspace error: {text}\n");

    // The four the task names.
    same_failure(
        false,
        "",
        &error("This is the only pane. Create a split first, or switch windows."),
        |_, _, p| vec!["park".into(), p.lonely.clone()],
    );
    same_failure(false, "", &error("No parked panes"), |_, _, p| {
        vec!["unpark".into(), p.first.clone()]
    });
    same_failure(
        false,
        "",
        &error("No saved history yet; use prefix Ctrl+s first"),
        |_, _, p| vec!["history".into(), p.first.clone()],
    );
    let outside = same_failure(
        true,
        "",
        &error("pane-start must run inside tmux"),
        |_, _, _| vec!["pane-start".into()],
    );
    // Outside tmux the message is also printed.
    assert_eq!(
        outside.stdout,
        "Workspace: pane-start must run inside tmux\n"
    );

    // More that both say.
    let unknown = error("Unknown workspace command: nope");
    same_failure(false, "", &unknown, |_, _, _| vec!["nope".into()]);
    let outside = same_failure(true, "", &unknown, |_, _, _| vec!["nope".into()]);
    assert_eq!(
        outside.stdout,
        "Workspace: Unknown workspace command: nope\n"
    );
    same_failure(false, "", &error("No parked panes"), |_, _, p| {
        vec!["choose-park".into(), p.first.clone()]
    });
    let park_logs = |side: Side, scratch: &ScratchTmux, p: &Parking| {
        run_ok(side, scratch, &["park", &p.logs]);
    };
    same_failure(
        false,
        "",
        &error("This pane is already parked"),
        |side, scratch, p| {
            park_logs(side, scratch, p);
            vec!["park".into(), p.logs.clone()]
        },
    );
    let into_parked = error("Switch to a project window before restoring a parked pane");
    same_failure(false, "", &into_parked, |side, scratch, p| {
        park_logs(side, scratch, p);
        vec!["unpark".into(), p.logs.clone()]
    });
    for typed in ["9\n", "0\n", "x\n", "-1\n"] {
        let listed = same_failure(
            false,
            typed,
            &error("Choose a listed pane number"),
            |side, scratch, p| {
                park_logs(side, scratch, p);
                vec!["choose-park".into(), p.first.clone()]
            },
        );
        assert!(listed.stdout.contains("  [alpha]"), "{}", listed.stdout);
    }
    same_failure(
        false,
        "",
        &error("This pane is not in the latest snapshot"),
        |side, scratch, p| {
            run_ok(side, scratch, &["save"]);
            let newer = scratch.pane(&["split-window", "-t", &p.first, "-h"]);
            vec!["history".into(), newer]
        },
    );
    same_failure(
        false,
        "",
        &error("Unsupported workspace snapshot version"),
        |_, scratch, _| {
            let state = scratch.state_dir();
            std::fs::write(state.join("workspace.json"), r#"{"version": 2}"#).unwrap();
            let latest = json!({"saved_at": 1, "path": state.join("workspace.json")});
            std::fs::write(state.join("latest.json"), latest.to_string()).unwrap();
            vec!["restore".into()]
        },
    );
}

// ---------------------------------------------------------------------------------------------
// The agent hook

const PID: i32 = 4242;
const BIRTH: &str = "Thu Oct  1 20:23:29 2026";
const SESSION: &str = "session-1";
/// Later than any wait value: a state file with this time is newer than the hook that blocked.
const FAR: u64 = 99_999_999_999_999;

/// A server with `agent-status.conf`, a pane to publish to, and a folder for the state files of
/// Claude Code. Either side can write to it.
struct HookRig {
    scratch: ScratchTmux,
    hidden: String,
    sessions: TempDir,
    env: HookEnv,
}

impl HookRig {
    fn new() -> HookRig {
        let scratch = ScratchTmux::new();
        scratch.tmux(&["source-file", STATUS_CONF]);
        scratch.tmux(&["set-option", "-g", "@agent_sound_cmd", "true"]); // Tests never play a real sound.
        let hidden = scratch.pane(&["new-window", "-t", "alpha:", "-n", "two"]);
        let sessions = TempDir::new();
        let env = HookEnv {
            pane: hidden.clone(),
            socket: scratch.socket().to_string(),
            tmux: Tmux::new().socket(scratch.socket()).argv(&[]),
            sessions: sessions.path().to_path_buf(),
            codex_home: sessions.path().to_path_buf(),
        };
        HookRig {
            scratch,
            hidden,
            sessions,
            env,
        }
    }

    /// A host whose process tree makes the real `owner` give (PID, BIRTH): the pane's process runs
    /// `sleep` and PID, its child, runs `kind`.
    fn host(&self, kind: &str) -> FakeHost {
        let pane_pid: i32 = self
            .scratch
            .expand(&self.hidden, "#{pane_pid}")
            .parse()
            .unwrap();
        let tty = self.scratch.expand(&self.hidden, "#{pane_tty}");
        let tty = tty.strip_prefix("/dev/").unwrap().to_string();
        let mut host = FakeHost {
            tmux: TmuxMode::Real,
            parent: PID,
            ..FakeHost::new()
        };
        host.procs.insert(
            PID,
            Proc {
                args: kind.into(),
                pgid: PID,
                tpgid: PID,
                lstart: BIRTH.into(),
                ..Proc::new(pane_pid, &tty, kind)
            },
        );
        host.procs.insert(pane_pid, Proc::new(1, &tty, "sleep"));
        host
    }

    /// The state file of Claude Code for the process of the agent.
    fn file(&self, state: &Value) {
        std::fs::write(
            self.sessions.path().join(format!("{PID}.json")),
            state.to_string(),
        )
        .unwrap();
    }

    /// One hook call by this side; give what it printed.
    fn call(&self, side: Side, mode: &str, extra: &Value) -> String {
        let mut payload = json!({"session_id": SESSION, "cwd": "/work"});
        for (key, value) in extra.as_object().expect("an object") {
            payload[key] = value.clone();
        }
        match side {
            Side::Rust => {
                let kind = if mode == "claude-status" {
                    "claude"
                } else {
                    mode
                };
                let host = self.host(kind);
                hook::run(mode, &payload, &self.env, &host)
                    .expect("the hook does not fail")
                    .unwrap_or_default()
            }
            Side::Python => {
                let mut command = self.scratch.bin("python3");
                command
                    .args([HOOK_DRIVER, mode, &PID.to_string(), BIRTH])
                    .arg(self.sessions.path())
                    .env("PYTHONDONTWRITEBYTECODE", "1")
                    .env("PYTHONUTF8", "1")
                    .env("TMUX_PANE", &self.hidden);
                let done = finish(command, &payload.to_string());
                assert_eq!(
                    (done.code, done.stderr.as_str()),
                    (Some(0), ""),
                    "the Python hook"
                );
                done.stdout
            }
        }
    }

    /// The options of the pane, `name = value`, with the times replaced: the wait is the moment of the
    /// call in milliseconds, and `at` in the Codex record is its second.
    fn options(&self) -> Vec<String> {
        let listed = self
            .scratch
            .tmux(&["show-options", "-p", "-t", &self.hidden]);
        listed
            .lines()
            .map(|line| {
                let name = line.split_once(' ').map_or(line, |(name, _)| name);
                let value = self
                    .scratch
                    .tmux(&["show-options", "-pqv", "-t", &self.hidden, name]);
                let value = match name {
                    "@workspace_agent_wait" if value.bytes().all(|b| b.is_ascii_digit()) => {
                        format!("<{} digits>", value.len())
                    }
                    "@workspace_codex" => scrub_at(&value),
                    _ => value,
                };
                format!("{name} = {value}")
            })
            .collect()
    }
}

/// `record` with the number after `"at":` replaced.
fn scrub_at(record: &str) -> String {
    let Some(start) = record.find("\"at\":") else {
        return record.to_string();
    };
    let digits = start + "\"at\":".len();
    let end = record[digits..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(record.len(), |n| digits + n);
    format!("{}<at>{}", &record[..digits], &record[end..])
}

#[test]
fn scrub_at_replaces_only_the_time() {
    assert_eq!(
        scrub_at(r#"{"turn":"t","pending":[],"event":"Stop","at":1762345678}"#),
        r#"{"turn":"t","pending":[],"event":"Stop","at":<at>}"#
    );
    assert_ne!(
        scrub_at(r#"{"event":"Stop","at":1}"#),
        scrub_at(r#"{"event":"Start","at":1}"#)
    );
}

/// One step of a hook scenario.
enum Step {
    /// A hook call: the mode and the payload besides the session and folder.
    Call(&'static str, Value),
    /// The state file of Claude Code, as it is when the next call reads it.
    File(Value),
}

fn event(mode: &'static str, name: &str, extra: Value) -> Step {
    let mut payload = extra;
    payload["hook_event_name"] = name.into();
    Step::Call(mode, payload)
}

fn file(status: &str, updated: u64) -> Step {
    Step::File(json!({"status": status, "statusUpdatedAt": updated}))
}

/// Claude Code from start to finish: the status line, the hooks that block, a failed turn and a new
/// session; then oh-my-pi.
#[rustfmt::skip]
fn claude_steps() -> Vec<(&'static str, Step)> {
    let special = r#"it's #1, {a} "b" #{pane_id} $HOME ; kill-server"#;
    let (elsewhere, nowhere) = (json!({"session_id": special, "cwd": "/else, where}"}), json!({}));
    let status = |extra: &Value| Step::Call("claude-status", extra.clone());
    let claude = |name: &str, extra: Value| event("claude", name, extra);
    let omp = |name: &str, cwd: &str| event("omp", name, json!({"pid": PID, "cwd": cwd}));
    vec![
        ("SessionStart", claude("SessionStart", json!({}))),
        ("file: busy", file("busy", 5)),
        ("status: busy", status(&nowhere)),
        ("file: waiting", file("waiting", 5)),
        ("status: waiting", status(&nowhere)),
        ("file: idle", file("idle", 5)),
        ("status: idle", status(&nowhere)),
        ("file: busy again", file("busy", 5)),
        ("status: busy again", status(&nowhere)),
        ("PermissionRequest", claude("PermissionRequest", json!({}))),
        ("status: an older file does not unblock", status(&nowhere)),
        ("PreToolUse Bash is ignored", claude("PreToolUse", json!({"tool_name": "Bash"}))),
        ("PreToolUse AskUserQuestion", claude("PreToolUse", json!({"tool_name": "AskUserQuestion"}))),
        ("StopFailure of a subagent is ignored", claude("StopFailure", json!({"agent_id": "child"}))),
        ("StopFailure", claude("StopFailure", json!({}))),
        ("status: still blocked", status(&nowhere)),
        ("file: idle, a little later", file("idle", 5)),
        ("status: a failed turn stays blocked", status(&nowhere)),
        ("file: idle, far later", file("idle", FAR)),
        ("status: the failed turn ends", status(&nowhere)),
        ("file: busy, far later", file("busy", FAR)),
        ("status: working again", status(&nowhere)),
        ("fork", claude("SessionStart", json!({"source": "fork"}))),
        ("status after the fork", status(&nowhere)),
        ("status: a new session", status(&json!({"session_id": special}))),
        ("status: a new folder", status(&elsewhere)),
        ("SessionEnd of the new session", claude("SessionEnd", elsewhere.clone())),
        ("status: back to the first session", status(&nowhere)),
        ("status: a folder with accents", status(&json!({"cwd": ACCENTED}))),
        ("SessionEnd", claude("SessionEnd", json!({}))),
        ("omp SessionStart", omp("SessionStart", "/work")),
        ("omp CwdChanged", omp("CwdChanged", ACCENTED)),
        ("omp SessionEnd", omp("SessionEnd", ACCENTED)),
    ]
}

/// Codex: the lifecycle, an approval that only its own tool clears, a question and an interrupt, an old
/// turn, compaction, calls that are not the pane's, parallel approvals matched by their arguments and
/// several questions.
#[rustfmt::skip]
fn codex_steps() -> Vec<(&'static str, Step)> {
    let codex = |name: &str, extra: Value| event("codex", name, extra);
    let tool = |turn: &str, tool: &str, id: &str| json!({"turn_id": turn, "tool_name": tool, "tool_use_id": id});
    let ask = |id: &str| tool("turn-5", "request_user_input", id);
    let turn = |turn: &str| json!({"turn_id": turn});
    let command = |text: &str| json!({"turn_id": "turn-4", "tool_name": "Bash", "tool_input": {"command": text}});
    let finished = |text: &str, id: &str| json!({"turn_id": "turn-4", "tool_name": "Bash", "tool_use_id": id, "tool_input": {"command": text}});
    let (one, two) = ("private command one ünï 😀", "private command two");
    vec![
        ("SessionStart", codex("SessionStart", json!({"cwd": ACCENTED}))),
        ("UserPromptSubmit", codex("UserPromptSubmit", turn("turn-1"))),
        ("PermissionRequest", codex("PermissionRequest", json!({"turn_id": "turn-1", "tool_name": "Bash"}))),
        ("PostToolUse of another tool", codex("PostToolUse", json!({"turn_id": "turn-1", "tool_name": "Read"}))),
        ("PostToolUse of the tool", codex("PostToolUse", tool("turn-1", "Bash", "call-1"))),
        ("Stop", codex("Stop", turn("turn-1"))),
        ("PostToolUse after Stop", codex("PostToolUse", json!({"turn_id": "turn-1", "tool_name": "Bash"}))),
        ("UserPromptSubmit again", codex("UserPromptSubmit", turn("turn-2"))),
        ("Stop of an old turn", codex("Stop", turn("turn-1"))),
        ("a question", codex("PreToolUse", tool("turn-2", "request_user_input", "question"))),
        ("the answer", codex("PostToolUse", tool("turn-2", "request_user_input", "question"))),
        ("Interrupt", codex("Interrupt", turn("turn-2"))),
        ("a turn to compact", codex("UserPromptSubmit", turn("turn-3"))),
        ("compaction", codex("SessionStart", json!({"source": "compact"}))),
        ("Stop of a subagent", codex("Stop", json!({"turn_id": "turn-3", "agent_id": "child"}))),
        ("Stop of another session", codex("Stop", json!({"turn_id": "turn-3", "session_id": "other-session"}))),
        ("SessionEnd of another session", codex("SessionEnd", json!({"session_id": "other-session"}))),
        ("a turn with approvals", codex("UserPromptSubmit", turn("turn-4"))),
        ("approval one", codex("PermissionRequest", command(one))),
        ("approval two", codex("PermissionRequest", command(two))),
        ("approval one is done", codex("PostToolUse", finished(one, "first"))),
        ("approval two is done", codex("PostToolUse", finished(two, "second"))),
        ("a turn with questions", codex("UserPromptSubmit", json!({"turn_id": "turn-5", "cwd": ACCENTED}))),
        ("question a", codex("PreToolUse", ask("a"))),
        ("question b", codex("PreToolUse", ask("b"))),
        ("answer a", codex("PostToolUse", ask("a"))),
        ("answer b", codex("PostToolUse", ask("b"))),
        ("Stop", codex("Stop", turn("turn-5"))),
        ("SessionEnd", codex("SessionEnd", json!({}))),
    ]
}

fn apply(rig: &HookRig, side: Side, step: &Step) -> String {
    match step {
        Step::Call(mode, extra) => rig.call(side, mode, extra),
        Step::File(state) => {
            rig.file(state);
            String::new()
        }
    }
}

/// Every step on both sides, each on a server of its own, and after each the pane holds the same
/// options and the call printed the same.
fn compare_hook_steps(steps: &[(&str, Step)]) {
    let (python, rust) = (HookRig::new(), HookRig::new());
    assert_eq!(python.options(), rust.options());
    for (i, (what, step)) in steps.iter().enumerate() {
        let printed = (
            apply(&python, Side::Python, step),
            apply(&rust, Side::Rust, step),
        );
        assert_eq!(printed.0, printed.1, "output of step {i} ({what})");
        assert_eq!(
            python.options(),
            rust.options(),
            "options after step {i} ({what})"
        );
    }
}

#[test]
fn hook_events_write_the_same_options() {
    compare_hook_steps(&claude_steps());
    compare_hook_steps(&codex_steps());
}

/// The same steps, with the other side writing every second one to a single server, against a server
/// that only Python wrote to.
fn interleave(steps: &[(&str, Step)], first: Side) {
    let other = |side: Side| {
        if side == Side::Python {
            Side::Rust
        } else {
            Side::Python
        }
    };
    let (reference, mixed) = (HookRig::new(), HookRig::new());
    for (i, (what, step)) in steps.iter().enumerate() {
        let side = if i % 2 == 0 { first } else { other(first) };
        let expected = apply(&reference, Side::Python, step);
        let printed = apply(&mixed, side, step);
        assert_eq!(printed, expected, "output of step {i} ({what}) by {side:?}");
        assert_eq!(
            mixed.options(),
            reference.options(),
            "options after step {i} ({what}) by {side:?}"
        );
    }
}

#[test]
fn hook_writers_interleave() {
    for first in BOTH {
        interleave(&codex_steps(), first);
        interleave(&claude_steps(), first);
    }
}
