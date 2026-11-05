//! Codex: which conversation a pane shows, and what its hooks say about it.
//!
//! Codex names no pane and no process in its hook calls, so the process tree gives the owner and the
//! pane title (Codex publishes its thread id there) gives the conversation. Nothing here reads prompts,
//! output or transcripts: only the first line of the rollout files the process holds open.

use super::process::{Table, owner, process_kind, process_table, ps, py_int};
use super::{CODEX, OPTION, STATUS, tmuxcmd, truthy};
use crate::error::{Error, Result, fail};
use crate::host::Host;
use crate::process::which;
use crate::pyjson::{Style, dumps};
use crate::sha256;
use crate::text::{now, py_splitlines};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// Tool hooks can overlap. The whole read is retried this many times when another event wins the write.
const ATTEMPTS: usize = 5;
/// What identifies the agent behind a pane.
const OWNER: [&str; 4] = ["kind", "id", "pid", "birth"];
/// Tools that ask the user something, so the pane waits for an answer.
const QUESTIONS: [&str; 4] = [
    "request_user_input",
    "request_user_input_async",
    "AskUserQuestion",
    "ExitPlanMode",
];
/// The first line of a rollout longer than this (in characters, newline included) is not read.
const HEADER_LIMIT: usize = 1024 * 1024;
/// Codex's threads, as it records their origin: those a person talks to.
const ROOT_SOURCES: [&str; 4] = ["cli", "vscode", "appServer", "unknown"];

/// `[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{n}`, whole.
fn is_uuid(text: &str, last: usize) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, last]).all(|(group, len)| {
            group.len() == len
                && group
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// A thread id as a whole.
fn is_thread_id(text: &str) -> bool {
    is_uuid(text, 12)
}

/// `codex | <thread id>`, or its form cut to 29 characters and `...`: the id, as shown.
///
/// 0.153.4 truncates its native thread-id title item to 29 chars plus "...". It still identifies the
/// CURRENT ChatWidget immediately, before any prompt.
fn visible_id(title: &str) -> Option<&str> {
    let id = title.strip_prefix("codex | ")?;
    let truncated = id.strip_suffix("...").is_some_and(|head| is_uuid(head, 5));
    (is_thread_id(id) || truncated).then_some(id)
}

/// `re.fullmatch(r"rollout-.*\.jsonl", name)`: `.` does not match a newline.
fn is_rollout(name: &str) -> bool {
    name.len() >= "rollout-.jsonl".len()
        && name.starts_with("rollout-")
        && name.ends_with(".jsonl")
        && !name.contains('\n')
}

/// `re.fullmatch(r"state_\d+\.sqlite", name)`, for ASCII digits.
fn is_database(name: &str) -> bool {
    name.strip_prefix("state_")
        .and_then(|rest| rest.strip_suffix(".sqlite"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// The base name of an absolute path.
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `lsof`, as a command line: the one on `PATH`, else the one Apple and most distributions ship.
fn lsof(args: &[&str]) -> Vec<String> {
    static PROGRAM: OnceLock<String> = OnceLock::new();
    let program = PROGRAM.get_or_init(|| {
        which("lsof").map_or_else(
            || "/usr/sbin/lsof".to_string(),
            |path| path.to_string_lossy().into_owned(),
        )
    });
    let mut argv = vec![program.clone()];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    argv
}

/// The one foreground Codex of each process on the tty that owns the pane, as `(pid, start time)`.
fn codex_owners(
    host: &dyn Host,
    pane_pid: i32,
    pane_tty: &str,
    processes: &Table,
) -> Result<BTreeSet<(i32, String)>> {
    let tty = pane_tty.strip_prefix("/dev/").unwrap_or(pane_tty);
    let mut identities = BTreeSet::new();
    for (&pid, (_parent, process_tty, command)) in processes {
        if process_tty != tty || process_kind(host, pid, command)? != Some("codex") {
            continue;
        }
        let identity = owner(
            host,
            "codex",
            pane_pid,
            pane_tty,
            Some(pid),
            Some(pid),
            Some(processes),
        )?;
        identities.extend(identity);
    }
    Ok(identities)
}

/// `{**base, "error": message}`.
fn failure(base: &Map<String, Value>, message: impl Into<String>) -> Result<Option<Value>> {
    let mut found = base.clone();
    found.insert("error".into(), Value::String(message.into()));
    Ok(Some(Value::Object(found)))
}

/// The first line of a rollout, if it is short enough to be a header.
fn header_line(path: &str) -> Result<Option<String>> {
    // A character is at most four bytes, so this holds every line the limit allows.
    let cap = 4 * (HEADER_LIMIT as u64 + 1);
    let mut line = Vec::new();
    BufReader::new(std::fs::File::open(path)?.take(cap)).read_until(b'\n', &mut line)?;
    let text = match std::str::from_utf8(&line) {
        Ok(text) => text,
        // A line cut at the cap is far over the limit, whatever its last character.
        Err(cut) if cut.error_len().is_none() && line.len() as u64 >= cap => {
            std::str::from_utf8(&line[..cut.valid_up_to()]).unwrap_or_default()
        }
        Err(e) => return fail(format!("'utf-8' codec can't decode {path}: {e}")),
    };
    Ok((text.chars().count() <= HEADER_LIMIT).then(|| text.to_string()))
}

/// What the file descriptors of the process say: `(descriptor, access, path)` of each absolute name.
fn open_files(listing: &str) -> Vec<(Option<String>, Option<String>, String)> {
    let mut files = Vec::new();
    let (mut descriptor, mut access) = (None, None);
    for line in py_splitlines(listing) {
        if let Some(name) = line.strip_prefix('f') {
            (descriptor, access) = (Some(name.to_string()), None);
        } else if let Some(mode) = line.strip_prefix('a') {
            access = Some(mode.to_string());
        } else if let Some(path) = line.strip_prefix('n').filter(|path| path.starts_with('/')) {
            files.push((descriptor.clone(), access.clone(), path.to_string()));
        }
    }
    files
}

/// Resolve the foreground CLI's current title, never a latest/unique-old rollout.
///
/// `None`: no Codex runs in the pane. Otherwise an object: `id`, `pid`, `cwd` and `birth` for the
/// conversation on screen, or `error` with the reason it cannot be told. `codex_home` is where Codex
/// keeps its databases when the process holds none open.
#[allow(clippy::too_many_arguments)]
pub fn resolve_codex(
    host: &dyn Host,
    codex_home: &Path,
    pane_pid: i32,
    pane_tty: &str,
    pane_title: &str,
    pane_cwd: Option<&str>,
    processes: Option<&Table>,
) -> Result<Option<Value>> {
    let scanned;
    let processes = match processes {
        Some(table) => table,
        None => {
            scanned = process_table(host, None, None)?;
            &scanned
        }
    };
    let mut identities = codex_owners(host, pane_pid, pane_tty, processes)?;
    let Some((pid, birth)) = identities.pop_first() else {
        return Ok(None);
    };
    if !identities.is_empty() {
        return Ok(Some(
            serde_json::json!({"kind": "codex", "error": "Multiple foreground Codex owners"}),
        ));
    }
    let mut error = Map::new();
    error.insert("kind".into(), "codex".into());
    error.insert("pid".into(), pid.into());
    error.insert("birth".into(), birth.clone().into());
    let Some(visible) = visible_id(pane_title) else {
        return failure(&error, "Codex has not published a current thread-id title");
    };
    let prefix = visible.strip_suffix("...").unwrap_or(visible);
    let files = open_files(&host.run(&lsof(&["-a", "-p", &pid.to_string(), "-Ffan"]))?);
    // Each conversation the evidence names, with the folder it records (when that is a folder).
    let mut matches: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut cwd = pane_cwd
        .filter(|cwd| cwd.starts_with('/'))
        .map(str::to_string);
    for (descriptor, access, path) in &files {
        if descriptor.as_deref() == Some("cwd") && cwd.is_none() {
            cwd = Some(path.clone());
        }
        let name = base_name(path);
        if !matches!(access.as_deref(), Some("w" | "u")) || !is_rollout(name) {
            continue;
        }
        if !name.contains(prefix) {
            continue;
        }
        let Some(first) = header_line(path)? else {
            continue;
        };
        let header: Value = serde_json::from_str(&first).map_err(|e| Error::Json(e.to_string()))?;
        let Some(header) = header.as_object() else {
            return fail("'list' object has no attribute 'get'");
        };
        let none = Map::new();
        let payload = match header.get("payload") {
            None => &none,
            Some(Value::Object(payload)) => payload,
            Some(_) => return fail("rollout header payload is not an object"),
        };
        if header.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let session_id = match payload.get("id") {
            None => "",
            Some(Value::String(id)) => id,
            Some(_) => return fail("expected string or bytes-like object"),
        };
        if !is_thread_id(session_id) || !session_id.starts_with(prefix) {
            continue;
        }
        if truthy(payload.get("parent_thread_id"))
            || payload.get("source").is_some_and(Value::is_object)
        {
            return failure(
                &error,
                "Codex is displaying a subagent, not a root conversation",
            );
        }
        let recorded = match payload.get("cwd") {
            Some(Value::String(recorded)) => Some(recorded.clone()),
            Some(_) => None,
            None => cwd.clone(),
        };
        matches.insert(
            session_id.to_string(),
            recorded.filter(|recorded| recorded.starts_with('/')),
        );
    }
    // The native title is the key; never infer identity from database recency.
    let mut databases: BTreeSet<String> = files
        .iter()
        .filter(|(_, _, path)| is_database(base_name(path)))
        .map(|(_, _, path)| path.clone())
        .collect();
    if databases.is_empty() {
        // Detached app-server releases keep the database out of the CLI's descriptors.
        if let Ok(entries) = std::fs::read_dir(codex_home) {
            databases = entries
                .flatten()
                .filter(|entry| is_database(&entry.file_name().to_string_lossy()))
                .map(|entry| entry.path().to_string_lossy().into_owned())
                .collect();
        }
    }
    for path in &databases {
        let rows = match threads(path, prefix) {
            Ok(rows) => rows,
            Err(e) => {
                return failure(
                    &error,
                    format!(
                        "Cannot read current Codex identity: {}",
                        sqlite_reason(&e, path)
                    ),
                );
            }
        };
        for (session_id, recorded, source) in rows {
            let rusqlite::types::Value::Text(session_id) = session_id else {
                return fail("expected string or bytes-like object");
            };
            if !is_thread_id(&session_id) {
                continue;
            }
            match source {
                rusqlite::types::Value::Text(source) if ROOT_SOURCES.contains(&source.as_str()) => {
                }
                _ => return failure(&error, "Current Codex title resolves to a non-root thread"),
            }
            let recorded = match recorded {
                rusqlite::types::Value::Text(recorded) if recorded.starts_with('/') => {
                    Some(recorded)
                }
                _ => None,
            };
            matches.insert(session_id, recorded);
        }
    }
    let mut found = matches.into_iter();
    let (Some((session_id, recorded)), None) = (found.next(), found.next()) else {
        return failure(
            &error,
            "Current Codex thread is not persisted or its title prefix is ambiguous",
        );
    };
    let Some(cwd) = recorded.or(cwd) else {
        return failure(&error, "Cannot establish current Codex working directory");
    };
    let start = host.run(&ps(&["-p", &pid.to_string(), "-o", "lstart="]))?;
    if start != birth {
        return failure(&error, "Codex process changed during identity lookup");
    }
    Ok(Some(serde_json::json!({
        "kind": "codex", "id": session_id, "pid": pid, "cwd": cwd, "birth": birth,
    })))
}

/// One row of `threads`, as SQLite typed it.
type Row = (
    rusqlite::types::Value,
    rusqlite::types::Value,
    rusqlite::types::Value,
);

/// `SELECT id, cwd, source FROM threads WHERE id LIKE '<prefix>%' LIMIT 2`, from a database opened
/// read-only, waiting at most a second for a lock.
fn threads(path: &str, prefix: &str) -> rusqlite::Result<Vec<Row>> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_secs(1))?;
    let mut query = db.prepare("SELECT id, cwd, source FROM threads WHERE id LIKE ?1 LIMIT 2")?;
    query
        .query_map([format!("{prefix}%")], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect()
}

/// What SQLite says went wrong, as Python's `sqlite3.Error` prints it: rusqlite adds the path to the
/// message of a database that does not open.
fn sqlite_reason(error: &rusqlite::Error, path: &str) -> String {
    match error {
        rusqlite::Error::SqliteFailure(_, Some(message)) => message
            .strip_suffix(&format!(": {path}"))
            .unwrap_or(message)
            .to_string(),
        other => other.to_string(),
    }
}

/// A shared app-server's inherited TMUX_PANE does not identify its client.
///
/// The pane that shows this thread in its title, and the Codex that owns it, when both are unique.
pub fn detached_codex(
    host: &dyn Host,
    tmux: &[String],
    session_id: &str,
) -> Result<Option<(String, (i32, String))>> {
    if !is_thread_id(session_id) {
        return Ok(None);
    }
    let titles = [
        format!("codex | {session_id}"),
        format!("codex | {}...", &session_id[..29]),
    ];
    let rows = tmuxcmd::run(
        host,
        tmux,
        &[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{pane_pid}\t#{pane_tty}\t#{pane_title}",
        ],
    )?;
    let matches: Vec<Vec<&str>> = py_splitlines(&rows)
        .into_iter()
        .map(|row| row.splitn(4, '\t').collect::<Vec<_>>())
        .filter(|fields| fields.len() == 4 && titles.iter().any(|title| title == fields[3]))
        .collect();
    let [fields] = &matches[..] else {
        return Ok(None);
    };
    let (pane, pane_pid, tty) = (fields[0], fields[1], fields[2]);
    let processes = process_table(host, None, Some(tty))?;
    // The pid is read only once a process needs it, as Python does.
    let mut identities = BTreeSet::new();
    for (&pid, (_parent, process_tty, command)) in &processes {
        if process_tty == tty.strip_prefix("/dev/").unwrap_or(tty)
            && process_kind(host, pid, command)? == Some("codex")
        {
            let identity = owner(
                host,
                "codex",
                py_int(pane_pid)?,
                tty,
                Some(pid),
                Some(pid),
                Some(&processes),
            )?;
            identities.extend(identity);
        }
    }
    let mut found = identities.into_iter();
    Ok(match (found.next(), found.next()) {
        (Some(identity), None) => Some((pane.to_string(), identity)),
        _ => None,
    })
}

/// Publish native events without inspecting prompts, output, or transcripts.
///
/// `data` is the hook's payload and `metadata` the identity of the pane's Codex. Failing tmux calls are
/// errors; every other way an event does not apply ends quietly.
pub fn codex_event(
    host: &dyn Host,
    tmux: &[String],
    pane: &str,
    data: &Value,
    metadata: &Value,
) -> Result<()> {
    let (Some(data), Some(metadata)) = (data.as_object(), metadata.as_object()) else {
        return Ok(());
    };
    let Some(event) = data.get("hook_event_name").and_then(Value::as_str) else {
        return Ok(());
    };
    if event == "SessionEnd" {
        return tmuxcmd::clear_owned(host, tmux, pane, metadata);
    }
    let value = dumps(&Value::Object(metadata.clone()), Style::Compact, false);
    for _ in 0..ATTEMPTS {
        if attempt(host, tmux, pane, data, metadata, event, &value)? {
            break;
        }
    }
    Ok(())
}

/// One read of the pane and one guarded write. `false`: another event won the write, so read again.
fn attempt(
    host: &dyn Host,
    tmux: &[String],
    pane: &str,
    data: &Map<String, Value>,
    metadata: &Map<String, Value>,
    event: &str,
    value: &str,
) -> Result<bool> {
    let state = tmuxcmd::pane_state(host, tmux, pane)?;
    let raw = tmuxcmd::option_value(&tmuxcmd::run(
        host,
        tmux,
        &["show-option", "-pqv", "-t", pane, CODEX],
    )?);
    let previous = tmuxcmd::published(&state.app);
    let same_owner = OWNER
        .iter()
        .all(|key| previous.get(*key) == metadata.get(*key));
    let starts = matches!(event, "SessionStart" | "UserPromptSubmit");
    if !same_owner && !starts {
        return Ok(true);
    }
    let mut record = if same_owner {
        tmuxcmd::published(&raw)
    } else {
        Map::new()
    };
    let turn = match data.get("turn_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(turn)) => Some(turn.as_str()),
        Some(_) => return Ok(true),
    };
    if !starts && truthy(record.get("turn")) {
        let current =
            matches!(record.get("turn"), Some(Value::String(held)) if Some(held.as_str()) == turn);
        if !current {
            return Ok(true);
        }
    }
    if event == "PostToolUse"
        && matches!(
            record.get("event").and_then(Value::as_str),
            Some("Stop" | "Interrupt")
        )
    {
        return Ok(true);
    }
    let mut pending = match record.get("pending") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return Ok(true),
    };
    let tool = match data.get("tool_name") {
        Some(name) if truthy(Some(name)) => match name.as_str() {
            Some(name) => name,
            None => return Ok(true),
        },
        _ => "approval",
    };
    let mut fingerprint = tool.to_string();
    if let Some(input) = data.get("tool_input") {
        // A digest correlates a PermissionRequest, which omits tool_use_id, without storing arguments.
        fingerprint += ":";
        fingerprint += &sha256::hex(dumps(input, Style::Default, true).as_bytes());
    }
    let key = match data.get("tool_use_id") {
        Some(id) if truthy(Some(id)) => match id.as_str() {
            Some(id) => id.to_string(),
            None => return Ok(true),
        },
        _ => fingerprint.clone(),
    };
    let holds =
        |pending: &[Value], text: &str| pending.iter().any(|item| item.as_str() == Some(text));
    let asks = matches!(
        data.get("tool_name"),
        Some(Value::String(name)) if QUESTIONS.contains(&name.as_str())
    );
    let mut wanted = if same_owner {
        state.status.as_str()
    } else {
        "idle"
    };
    let waiting = |pending: &[Value]| {
        if pending.is_empty() {
            "working"
        } else {
            "blocked"
        }
    };
    if event == "PermissionRequest" || (event == "PreToolUse" && asks) {
        if !truthy(data.get("tool_use_id")) || !holds(&pending, &key) {
            pending.push(Value::String(key.clone()));
        }
        wanted = "blocked";
    } else {
        match event {
            // Compaction continues the current turn and must not acknowledge done.
            "SessionStart" => {
                if !same_owner {
                    pending.clear();
                }
            }
            "UserPromptSubmit" => {
                pending.clear();
                wanted = "working";
                record.insert("turn".into(), turn.map_or(Value::Null, Value::from));
            }
            "PostToolUse" => {
                for candidate in [&key, &fingerprint] {
                    if let Some(at) = pending
                        .iter()
                        .position(|item| item.as_str() == Some(candidate))
                    {
                        pending.remove(at);
                        break;
                    }
                }
                wanted = waiting(&pending);
            }
            "PreToolUse" => wanted = waiting(&pending),
            "Stop" | "Interrupt" => {
                pending.clear();
                wanted = if event == "Stop" { "over" } else { "idle" };
            }
            _ => {}
        }
    }
    record.insert("pending".into(), Value::Array(pending));
    record.insert("event".into(), event.into());
    record.insert("at".into(), (now() as i64).into());
    let mut write = tmuxcmd::assign(pane, OPTION, value);
    write += " ; ";
    write += &tmuxcmd::assign(
        pane,
        CODEX,
        &dumps(&Value::Object(record), Style::Compact, false),
    );
    write += " ; ";
    write += &if wanted == "over" {
        tmuxcmd::turn_over(pane)
    } else {
        tmuxcmd::assign(pane, STATUS, wanted)
    };
    let guard = format!(
        "#{{&&:{},{}}}",
        tmuxcmd::same(OPTION, &state.app),
        tmuxcmd::same(CODEX, &raw)
    );
    let command = format!("{write} ; display-message -p updated");
    let answer = tmuxcmd::run(
        host,
        tmux,
        &["if-shell", "-F", "-t", pane, &guard, &command],
    )?;
    Ok(answer == "updated")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeHost, Proc, TempDir};
    use rusqlite::{Connection, params};
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SESSION: &str = "01a128f8-8621-7561-8972-8d02a1234567";
    const OTHER: &str = "01a128f8-8621-7561-8972-8d02a1234568";
    const TTY: &str = "/dev/ttys001";

    fn title() -> String {
        format!("codex | {}...", &SESSION[..29])
    }

    fn codex_proc(ppid: i32, birth: &str) -> Proc {
        Proc {
            args: "codex".into(),
            pgid: 100,
            tpgid: 100,
            lstart: birth.into(),
            ..Proc::new(ppid, "ttys001", "codex")
        }
    }

    /// Codex is the pane's own process, as in test_performance.py::ProcessLookups.
    fn host() -> FakeHost {
        let mut host = FakeHost::new();
        host.procs.insert(100, codex_proc(1, "birth"));
        host.lsof.insert(100, "fcwd\nn/work".into());
        host
    }

    fn table() -> Table {
        Table::from([(100, (1, "ttys001".to_string(), "codex".to_string()))])
    }

    fn database(folder: &Path, name: &str, rows: &[(&str, Option<&str>, Option<&str>)]) -> PathBuf {
        let path = folder.join(name);
        let db = Connection::open(&path).unwrap();
        db.execute("CREATE TABLE threads (id TEXT, cwd TEXT, source TEXT)", [])
            .unwrap();
        for (id, cwd, source) in rows {
            db.execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3)",
                params![id, cwd, source],
            )
            .unwrap();
        }
        path
    }

    fn rollout(folder: &Path, id: &str, header: Value) -> PathBuf {
        let path = folder.join(format!("rollout-2026-01-01T00-00-00-{id}.jsonl"));
        std::fs::write(&path, format!("{header}\n{{\"type\":\"event\"}}\n")).unwrap();
        path
    }

    fn meta(id: &str, extra: Value) -> Value {
        let mut payload = json!({"id": id});
        for (key, value) in extra.as_object().unwrap() {
            payload[key] = value.clone();
        }
        json!({"type": "session_meta", "payload": payload})
    }

    /// What `lsof -a -p 100 -Ffan` prints for a process in /work with these files open.
    fn lsof(files: &[(&str, &Path)]) -> String {
        let mut out = String::from("p100\nfcwd\naw\nn/work\nftxt\nn/usr/bin/codex\n");
        for (access, path) in files {
            out += &format!("f20\na{access}\nn{}\n", path.display());
        }
        out
    }

    fn resolve(host: &FakeHost, home: &Path, title: &str, cwd: Option<&str>) -> Option<Value> {
        resolve_codex(host, home, 100, TTY, title, cwd, Some(&table())).unwrap()
    }

    fn failed(message: &str) -> Option<Value> {
        Some(json!({"kind": "codex", "pid": 100, "birth": "birth", "error": message}))
    }

    const NOT_PERSISTED: &str =
        "Current Codex thread is not persisted or its title prefix is ambiguous";

    // test_performance.py::ProcessLookups

    #[test]
    fn resolver_reuses_supplied_process_table() {
        let host = FakeHost::new();
        let found = resolve_codex(
            &host,
            Path::new("/nonexistent"),
            1,
            TTY,
            "",
            None,
            Some(&Table::new()),
        )
        .unwrap();
        assert_eq!(found, None);
        assert!(host.calls.borrow().is_empty(), "repeated process scan");
    }

    #[test]
    fn detached_daemon_database_uses_only_unique_current_title() {
        let folder = TempDir::new();
        let path = database(
            folder.path(),
            "state_5.sqlite",
            &[(SESSION, Some("/work"), Some("cli"))],
        );
        let host = host();
        let resolved = resolve(&host, folder.path(), &title(), None).unwrap();
        assert_eq!(resolved["id"], SESSION);
        let db = Connection::open(&path).unwrap();
        db.execute(
            "INSERT INTO threads VALUES (?1, ?2, ?3)",
            params![OTHER, "/other", "cli"],
        )
        .unwrap();
        drop(db);
        let resolved = resolve(&host, folder.path(), &title(), None).unwrap();
        assert!(resolved.get("error").is_some(), "{resolved}");
    }

    // Beyond the Python tests: resolve_codex

    #[test]
    fn without_a_codex_process_on_the_tty_there_is_no_identity() {
        let mut host = FakeHost::new();
        host.procs.insert(100, Proc::new(1, "ttys001", "sleep"));
        host.procs.insert(101, codex_proc(100, "b"));
        host.procs.get_mut(&101).unwrap().tty = "ttys002".into();
        let found = resolve_codex(&host, Path::new("/none"), 100, TTY, &title(), None, None);
        assert_eq!(found.unwrap(), None);
        let scans: Vec<_> = host
            .calls_to("ps")
            .into_iter()
            .filter(|call| call[1] == "-axo")
            .collect();
        assert_eq!(
            scans.len(),
            1,
            "one scan of every process when none is supplied"
        );
    }

    #[test]
    fn two_codex_owners_on_the_tty_are_an_error() {
        let mut host = FakeHost::new();
        host.procs.insert(100, Proc::new(1, "ttys001", "sleep"));
        host.procs.insert(101, codex_proc(100, "first"));
        host.procs.insert(102, codex_proc(100, "second"));
        let found = resolve_codex(&host, Path::new("/none"), 100, TTY, &title(), None, None);
        assert_eq!(
            found.unwrap(),
            Some(json!({"kind": "codex", "error": "Multiple foreground Codex owners"}))
        );
    }

    #[test]
    fn a_title_that_is_not_a_thread_id_is_an_error() {
        let host = host();
        let message = "Codex has not published a current thread-id title";
        for bad in [
            "",
            "codex",
            "codex | ",
            "codex | not-a-uuid",
            &format!("codex |  {SESSION}"),
            &format!("codex | {SESSION}x"),
            &format!("codex | {SESSION}..."),
            &format!("codex | {SESSION}\n"),
            &format!("Codex | {SESSION}"),
            &format!("codex | {}", SESSION.to_uppercase()),
            &format!("codex | {}...", &SESSION[..28]),
            &format!("codex | {}...", &SESSION[..30]),
            &format!("codex | {}..", &SESSION[..29]),
            &format!("codex | {}", &SESSION[..29]),
            &format!("codex | {}...", SESSION.replace('-', "_")),
        ] {
            assert_eq!(
                resolve(&host, Path::new("/none"), bad, None),
                failed(message),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn both_title_forms_name_a_thread() {
        let folder = TempDir::new();
        database(
            folder.path(),
            "state_5.sqlite",
            &[(SESSION, Some("/work"), Some("cli"))],
        );
        let host = host();
        for shown in [format!("codex | {SESSION}"), title()] {
            let resolved = resolve(&host, folder.path(), &shown, None).unwrap();
            assert_eq!(
                resolved,
                json!({"kind": "codex", "id": SESSION, "pid": 100, "cwd": "/work", "birth": "birth"}),
                "{shown}"
            );
        }
    }

    #[test]
    fn a_rollout_open_for_writing_gives_the_identity_and_its_folder() {
        let folder = TempDir::new();
        let path = rollout(
            folder.path(),
            SESSION,
            meta(SESSION, json!({"cwd": "/rollout/cwd"})),
        );
        let mut host = host();
        host.lsof.insert(100, lsof(&[("w", &path)]));
        let resolved = resolve(&host, Path::new("/none"), &title(), None);
        assert_eq!(
            resolved,
            Some(
                json!({"kind": "codex", "id": SESSION, "pid": 100, "cwd": "/rollout/cwd", "birth": "birth"})
            )
        );
        // Read-write counts too; read-only does not.
        host.lsof.insert(100, lsof(&[("u", &path)]));
        assert!(resolve(&host, Path::new("/none"), &title(), None).unwrap()["id"] == SESSION);
        host.lsof.insert(100, lsof(&[("r", &path)]));
        assert_eq!(
            resolve(&host, Path::new("/none"), &title(), None),
            failed(NOT_PERSISTED)
        );
    }

    #[test]
    fn the_folder_comes_from_the_pane_then_the_process_when_the_thread_has_none() {
        let folder = TempDir::new();
        let path = rollout(folder.path(), SESSION, meta(SESSION, json!({})));
        let mut host = host();
        host.lsof.insert(100, lsof(&[("w", &path)]));
        let cwd = |pane: Option<&str>| {
            resolve(&host, Path::new("/none"), &title(), pane).unwrap()["cwd"].clone()
        };
        assert_eq!(cwd(None), "/work", "the process's folder");
        assert_eq!(cwd(Some("/pane")), "/pane", "the pane's folder first");
        assert_eq!(
            cwd(Some("relative")),
            "/work",
            "a relative folder is no folder"
        );
    }

    #[test]
    fn a_thread_without_any_folder_is_an_error() {
        let folder = TempDir::new();
        database(
            folder.path(),
            "state_5.sqlite",
            &[(SESSION, None, Some("cli"))],
        );
        let mut host = host();
        host.lsof
            .insert(100, "p100\nf5\naw\nn/elsewhere/file".into());
        assert_eq!(
            resolve(&host, folder.path(), &title(), None),
            failed("Cannot establish current Codex working directory")
        );
        // A folder that is not absolute does not count either.
        assert_eq!(
            resolve(&host, folder.path(), &title(), Some("relative")),
            failed("Cannot establish current Codex working directory")
        );
    }

    #[test]
    fn files_that_are_not_rollouts_for_this_title_are_ignored() {
        let folder = TempDir::new();
        let right = rollout(folder.path(), SESSION, meta(SESSION, json!({"cwd": "/a"})));
        // A rollout of another conversation is never opened, so it need not exist.
        let other = folder
            .path()
            .join("rollout-x-11111111-2222-3333-4444-555555555555.jsonl");
        let backup = folder.path().join(format!("rollout-x-{SESSION}.jsonl.bak"));
        let not_rollout = folder.path().join(format!("log-{SESSION}.jsonl"));
        let mut host = host();
        host.lsof.insert(
            100,
            lsof(&[
                ("w", &other),
                ("w", &backup),
                ("w", &not_rollout),
                ("w", &right),
            ]),
        );
        let resolved = resolve(&host, Path::new("/none"), &title(), None).unwrap();
        assert_eq!(resolved["id"], SESSION);
    }

    #[test]
    fn a_rollout_whose_id_does_not_extend_the_prefix_is_ignored() {
        let folder = TempDir::new();
        let path = rollout(folder.path(), SESSION, meta(OTHER, json!({"cwd": "/a"})));
        let wrong_type = folder.path().join(format!("rollout-y-{SESSION}.jsonl"));
        std::fs::write(
            &wrong_type,
            json!({"type": "event_msg", "payload": {"id": SESSION}}).to_string(),
        )
        .unwrap();
        let mut host = host();
        host.lsof
            .insert(100, lsof(&[("w", &path), ("w", &wrong_type)]));
        // OTHER shares the 29 characters of the title, so it does extend the prefix.
        assert_eq!(
            resolve(&host, Path::new("/none"), &title(), None).unwrap()["id"],
            OTHER
        );
        let path = rollout(
            folder.path(),
            SESSION,
            meta("11111111-2222-3333-4444-555555555555", json!({})),
        );
        host.lsof
            .insert(100, lsof(&[("w", &path), ("w", &wrong_type)]));
        assert_eq!(
            resolve(&host, Path::new("/none"), &title(), None),
            failed(NOT_PERSISTED)
        );
    }

    #[test]
    fn a_subagent_rollout_is_not_a_root_conversation() {
        let folder = TempDir::new();
        for extra in [
            json!({"parent_thread_id": "abc"}),
            json!({"source": {"subagent": {"thread_spawn": {}}}}),
        ] {
            let path = rollout(folder.path(), SESSION, meta(SESSION, extra.clone()));
            let mut host = host();
            host.lsof.insert(100, lsof(&[("w", &path)]));
            assert_eq!(
                resolve(&host, Path::new("/none"), &title(), None),
                failed("Codex is displaying a subagent, not a root conversation"),
                "{extra}"
            );
        }
        // An empty parent and a source that is not an object are a root conversation.
        let path = rollout(
            folder.path(),
            SESSION,
            meta(SESSION, json!({"parent_thread_id": "", "source": "cli"})),
        );
        let mut host = host();
        host.lsof.insert(100, lsof(&[("w", &path)]));
        assert!(resolve(&host, Path::new("/none"), &title(), None).unwrap()["id"] == SESSION);
    }

    #[test]
    fn an_oversized_first_line_is_skipped_and_a_missing_file_is_an_error() {
        let folder = TempDir::new();
        let big = folder.path().join(format!("rollout-big-{SESSION}.jsonl"));
        std::fs::write(&big, "x".repeat(1024 * 1024 + 1)).unwrap();
        let mut host = host();
        host.lsof.insert(100, lsof(&[("w", &big)]));
        assert_eq!(
            resolve(&host, Path::new("/none"), &title(), None),
            failed(NOT_PERSISTED)
        );
        // A line of exactly 1 MiB with its newline is still read.
        let fits = folder.path().join(format!("rollout-fits-{SESSION}.jsonl"));
        let header = meta(SESSION, json!({"cwd": "/a", "pad": ""})).to_string();
        let pad = "x".repeat(1024 * 1024 - 1 - header.len());
        let header = meta(SESSION, json!({"cwd": "/a", "pad": pad})).to_string();
        assert_eq!(header.len() + 1, 1024 * 1024);
        std::fs::write(&fits, format!("{header}\n")).unwrap();
        host.lsof.insert(100, lsof(&[("w", &fits)]));
        assert_eq!(
            resolve(&host, Path::new("/none"), &title(), None).unwrap()["id"],
            SESSION
        );

        let missing = folder
            .path()
            .join(format!("rollout-missing-{SESSION}.jsonl"));
        host.lsof.insert(100, lsof(&[("w", &missing)]));
        let result = resolve_codex(
            &host,
            Path::new("/none"),
            100,
            TTY,
            &title(),
            None,
            Some(&table()),
        );
        assert!(result.is_err(), "{result:?}");
        // Python raises on a first line that is not JSON, too.
        std::fs::write(&missing, "not json\n").unwrap();
        let result = resolve_codex(
            &host,
            Path::new("/none"),
            100,
            TTY,
            &title(),
            None,
            Some(&table()),
        );
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn the_database_the_process_holds_open_wins_over_the_codex_home() {
        let held = TempDir::new();
        let home = TempDir::new();
        let path = database(
            held.path(),
            "state_5.sqlite",
            &[(SESSION, Some("/held"), Some("cli"))],
        );
        // The folder would make the title ambiguous if it were read.
        database(
            home.path(),
            "state_9.sqlite",
            &[(OTHER, Some("/home"), Some("cli"))],
        );
        let mut host = host();
        host.lsof.insert(100, lsof(&[("r", &path)]));
        let resolved = resolve(&host, home.path(), &title(), None).unwrap();
        assert_eq!(
            (resolved["id"].clone(), resolved["cwd"].clone()),
            (json!(SESSION), json!("/held"))
        );
        // Without it, the folder is read.
        let resolved = resolve(&self::host(), home.path(), &title(), None).unwrap();
        assert_eq!(resolved["id"], OTHER);
    }

    #[test]
    fn only_state_digits_sqlite_files_of_the_codex_home_are_databases() {
        let home = TempDir::new();
        for name in [
            "state_x.sqlite",
            "state_5.sqlite.bak",
            "state_.sqlite",
            "xstate_5.sqlite",
            "state_5-wal",
        ] {
            database(home.path(), name, &[(SESSION, Some("/wrong"), Some("cli"))]);
        }
        assert_eq!(
            resolve(&host(), home.path(), &title(), None),
            failed(NOT_PERSISTED)
        );
        database(
            home.path(),
            "state_12.sqlite",
            &[(SESSION, Some("/right"), Some("cli"))],
        );
        assert_eq!(
            resolve(&host(), home.path(), &title(), None).unwrap()["cwd"],
            "/right"
        );
    }

    #[test]
    fn a_rollout_and_a_database_that_agree_are_one_conversation_with_the_database_folder() {
        let home = TempDir::new();
        database(
            home.path(),
            "state_5.sqlite",
            &[(SESSION, Some("/db"), Some("appServer"))],
        );
        let files = TempDir::new();
        let path = rollout(
            files.path(),
            SESSION,
            meta(SESSION, json!({"cwd": "/rollout"})),
        );
        let mut host = host();
        host.lsof.insert(100, lsof(&[("w", &path)]));
        // The process holds no database open, so the codex home is read as well. Both name one
        // conversation, and the database's folder is the one recorded last.
        let resolved = resolve(&host, home.path(), &title(), None).unwrap();
        assert_eq!(resolved["cwd"], "/db");
    }

    #[test]
    fn a_thread_that_is_not_a_root_thread_is_an_error() {
        for source in ["cli", "vscode", "appServer", "unknown"] {
            let home = TempDir::new();
            database(
                home.path(),
                "state_5.sqlite",
                &[(SESSION, Some("/w"), Some(source))],
            );
            let resolved = resolve(&host(), home.path(), &title(), None).unwrap();
            assert_eq!(resolved["id"], SESSION, "{source}");
        }
        for source in [Some("exec"), Some("subAgent"), Some(""), Some("CLI"), None] {
            let home = TempDir::new();
            database(
                home.path(),
                "state_5.sqlite",
                &[(SESSION, Some("/w"), source)],
            );
            assert_eq!(
                resolve(&host(), home.path(), &title(), None),
                failed("Current Codex title resolves to a non-root thread"),
                "{source:?}"
            );
        }
        // A row whose id is not a whole UUID is not a thread of the title.
        let home = TempDir::new();
        database(
            home.path(),
            "state_5.sqlite",
            &[("short", Some("/w"), Some("exec"))],
        );
        assert_eq!(
            resolve(&host(), home.path(), &title(), None),
            failed(NOT_PERSISTED)
        );
    }

    #[test]
    fn a_database_that_cannot_be_read_is_reported_with_the_reason() {
        let home = TempDir::new();
        std::fs::write(
            home.path().join("state_5.sqlite"),
            "this is not a database at all",
        )
        .unwrap();
        assert_eq!(
            resolve(&host(), home.path(), &title(), None),
            failed("Cannot read current Codex identity: file is not a database")
        );
        let home = TempDir::new();
        Connection::open(home.path().join("state_5.sqlite"))
            .unwrap()
            .execute("CREATE TABLE other (id TEXT)", [])
            .unwrap();
        assert_eq!(
            resolve(&host(), home.path(), &title(), None),
            failed("Cannot read current Codex identity: no such table: threads")
        );
        // A database the process lists but that has gone.
        let mut host = host();
        host.lsof
            .insert(100, lsof(&[("r", &home.path().join("state_7.sqlite"))]));
        assert_eq!(
            resolve(&host, home.path(), &title(), None),
            failed("Cannot read current Codex identity: unable to open database file")
        );
    }

    #[test]
    fn a_missing_codex_home_has_no_database() {
        assert_eq!(
            resolve(&host(), Path::new("/nonexistent/codex"), &title(), None),
            failed(NOT_PERSISTED)
        );
    }

    #[test]
    fn the_database_is_opened_read_only() {
        let home = TempDir::new();
        let path = database(
            home.path(),
            "state_5.sqlite",
            &[(SESSION, Some("/w"), Some("cli"))],
        );
        let before = std::fs::read(&path).unwrap();
        resolve(&host(), home.path(), &title(), None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            std::fs::read_dir(home.path()).unwrap().count() == 1,
            "no journal or wal left behind"
        );
    }

    /// A host whose process changes its start time after the first lookup.
    struct Reborn {
        inner: FakeHost,
        lookups: Cell<u32>,
    }

    impl Host for Reborn {
        fn run(&self, argv: &[String]) -> Result<String> {
            let out = self.inner.run(argv)?;
            if argv.iter().any(|arg| arg == "lstart=") {
                let seen = self.lookups.get();
                self.lookups.set(seen + 1);
                if seen > 0 {
                    return Ok("reborn".into());
                }
            }
            Ok(out)
        }

        fn parent_pid(&self) -> i32 {
            self.inner.parent_pid()
        }
    }

    #[test]
    fn a_process_that_changed_during_the_lookup_is_an_error() {
        let home = TempDir::new();
        database(
            home.path(),
            "state_5.sqlite",
            &[(SESSION, Some("/w"), Some("cli"))],
        );
        let reborn = Reborn {
            inner: host(),
            lookups: Cell::new(0),
        };
        let found = resolve_codex(
            &reborn,
            home.path(),
            100,
            TTY,
            &title(),
            None,
            Some(&table()),
        );
        assert_eq!(
            found.unwrap(),
            failed("Codex process changed during identity lookup")
        );
        assert_eq!(reborn.lookups.get(), 2);
    }

    #[test]
    fn lsof_is_asked_about_the_codex_process_only_once_a_title_names_a_thread() {
        let host = host();
        resolve(&host, Path::new("/none"), "", None);
        assert!(host.calls_to("lsof").is_empty());
        resolve(&host, Path::new("/none"), &title(), None);
        let calls = host.calls_to("lsof");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][1..], ["-a", "-p", "100", "-Ffan"]);
    }

    // Beyond the Python tests: detached_codex

    fn prefix() -> Vec<String> {
        ["/opt/homebrew/bin/tmux", "-N", "-S", "/tmp/sock"]
            .map(String::from)
            .to_vec()
    }

    fn listing(rows: &'static [&'static str]) -> FakeHost {
        let mut host = FakeHost::scripted(move |_| Ok(rows.join("\n")));
        host.procs.insert(300, codex_proc(250, "birth"));
        host.procs.insert(250, Proc::new(1, "ttys002", "sleep"));
        host.procs.get_mut(&300).unwrap().tty = "ttys002".into();
        host.procs.get_mut(&300).unwrap().pgid = 300;
        host.procs.get_mut(&300).unwrap().tpgid = 300;
        host
    }

    #[test]
    fn detached_codex_ignores_a_session_id_that_is_not_a_uuid() {
        let host = listing(&[]);
        for id in [
            "session-1",
            "",
            &SESSION.to_uppercase(),
            &format!("{SESSION}\n"),
            &SESSION[..35],
            &format!("{SESSION}0"),
        ] {
            assert_eq!(
                detached_codex(&host, &prefix(), id).unwrap(),
                None,
                "{id:?}"
            );
        }
        assert!(host.calls.borrow().is_empty());
    }

    const SHOWN: &str = "%5\t250\t/dev/ttys002\tcodex | 01a128f8-8621-7561-8972-8d02a...";
    const FULL: &str = "%6\t250\t/dev/ttys002\tcodex | 01a128f8-8621-7561-8972-8d02a1234567";

    #[test]
    fn detached_codex_finds_the_only_pane_that_shows_the_thread() {
        let host = listing(&[
            "%1\t10\t/dev/ttys009\tshell",
            SHOWN,
            "%2\t11\t/dev/ttys010\tcodex | 01a128f8-8621-7561-8972-8d02a1234568",
            "%3\t12\t/dev/ttys011",
            "garbage",
        ]);
        let found = detached_codex(&host, &prefix(), SESSION).unwrap();
        assert_eq!(found, Some(("%5".to_string(), (300, "birth".to_string()))));
        let tmux = host.calls_to("tmux");
        assert_eq!(tmux.len(), 1);
        assert_eq!(
            tmux[0][4..],
            [
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}\t#{pane_pid}\t#{pane_tty}\t#{pane_title}"
            ]
        );
        let scans = host.calls_to("ps");
        assert!(
            scans
                .iter()
                .any(|call| call[1..] == ["-t", "ttys002", "-o", "pid=,ppid=,tty=,comm="]),
            "one scan of the pane's tty: {scans:?}"
        );
        assert!(scans.iter().all(|call| call[1] != "-axo"));
        // The full form of the title counts as well.
        let host = listing(&[FULL]);
        assert_eq!(
            detached_codex(&host, &prefix(), SESSION)
                .unwrap()
                .map(|found| found.0),
            Some("%6".to_string())
        );
    }

    #[test]
    fn detached_codex_needs_exactly_one_pane_and_one_owner() {
        // No pane, and two panes, with the title: no process is looked at.
        for rows in [&["%1\t10\t/dev/ttys009\tshell"][..], &[SHOWN, FULL][..]] {
            let rows: &'static [&'static str] = Vec::leak(rows.to_vec());
            let host = listing(rows);
            assert_eq!(detached_codex(&host, &prefix(), SESSION).unwrap(), None);
            assert!(host.calls_to("ps").is_empty());
        }
        // A pane without a Codex owner on its tty.
        let mut host = listing(&[SHOWN]);
        host.procs.get_mut(&300).unwrap().comm = "vim".into();
        assert_eq!(detached_codex(&host, &prefix(), SESSION).unwrap(), None);
        // Two owners on the tty.
        let mut host = listing(&[SHOWN]);
        host.procs.insert(301, codex_proc(250, "again"));
        host.procs.get_mut(&301).unwrap().tty = "ttys002".into();
        assert_eq!(detached_codex(&host, &prefix(), SESSION).unwrap(), None);
    }

    // Beyond the Python tests: codex_event, with a scripted tmux

    const APP: &str = r#"{"kind":"codex","id":"session-1","pid":4242,"cwd":"/work","birth":"Thu Oct  1 20:23:29 2026"}"#;

    fn metadata() -> Value {
        serde_json::from_str(APP).unwrap()
    }

    /// What the pane holds, as the scripted tmux reads it.
    #[derive(Default)]
    struct Held {
        status: String,
        app: String,
        codex: String,
    }

    fn pane(status: &str, app: &str, codex: &str) -> Rc<RefCell<Held>> {
        Rc::new(RefCell::new(Held {
            status: status.into(),
            app: app.into(),
            codex: codex.into(),
        }))
    }

    /// A tmux that reads the pane from `held` and answers every `if-shell` with `reply`.
    fn tmux(held: &Rc<RefCell<Held>>, reply: &'static str) -> FakeHost {
        let held = Rc::clone(held);
        FakeHost::scripted(move |argv| {
            let held = held.borrow();
            Ok(if argv.iter().any(|arg| arg == "display-message") {
                ["99", TTY, &held.status, "", "", "", &held.app].join("\t")
            } else if argv.iter().any(|arg| arg == "show-option") {
                held.codex.clone()
            } else {
                reply.to_string()
            })
        })
    }

    fn writes(host: &FakeHost) -> Vec<Vec<String>> {
        host.calls_to("tmux")
            .into_iter()
            .filter(|call| call[4] == "if-shell")
            .collect()
    }

    fn event(host: &FakeHost, data: Value) -> Result<()> {
        let mut data = data;
        data["session_id"] = json!("session-1");
        data["cwd"] = json!("/work");
        codex_event(host, &prefix(), "%7", &data, &metadata())
    }

    fn seconds() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// The record a write assigns to `@workspace_codex`, with its time taken out and checked.
    fn recorded(write: &[String]) -> (String, i64) {
        let command = &write[9];
        let start = command.find("@workspace_codex '").unwrap() + "@workspace_codex '".len();
        let end = start + command[start..].find("' ; ").unwrap();
        let record = &command[start..end];
        let at_start = record.find("\"at\":").unwrap() + 5;
        let digits = record[at_start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let at: i64 = record[at_start..at_start + digits].parse().unwrap();
        let masked = format!("{}@{}", &record[..at_start], &record[at_start + digits..]);
        (masked, at)
    }

    #[test]
    fn a_prompt_on_a_fresh_pane_writes_the_identity_the_record_and_working_in_one_guarded_command()
    {
        let held = pane("", "", "");
        let host = tmux(&held, "updated");
        let before = seconds();
        event(
            &host,
            json!({"hook_event_name": "UserPromptSubmit", "turn_id": "turn-1"}),
        )
        .unwrap();
        let after = seconds();
        let writes = writes(&host);
        assert_eq!(writes.len(), 1);
        let write = &writes[0];
        assert_eq!(write[..4], prefix()[..]);
        assert_eq!(write[4..8], ["if-shell", "-F", "-t", "%7"]);
        assert_eq!(
            write[8],
            "#{&&:#{==:#{@workspace_app},},#{==:#{@workspace_codex},}}"
        );
        let (record, at) = recorded(write);
        assert!((before..=after).contains(&at), "{at}");
        assert_eq!(
            write[9],
            format!(
                "set-option -p -t %7 @workspace_app '{APP}' ; \
                 set-option -p -t %7 @workspace_codex '{}' ; \
                 set-option -p -t %7 @workspace_agent 'working' ; display-message -p updated",
                record.replace('@', &at.to_string())
            )
        );
        assert_eq!(
            record,
            r#"{"turn":"turn-1","pending":[],"event":"UserPromptSubmit","at":@}"#
        );
    }

    #[test]
    fn the_guard_holds_the_values_that_were_read_escaped_for_tmux() {
        let held = pane(
            "working",
            APP,
            r##"{"turn": "t,1}", "pending": ["a#b"], "event": "PreToolUse", "at": 7}"##,
        );
        let host = tmux(&held, "updated");
        event(
            &host,
            json!({"hook_event_name": "PreToolUse", "turn_id": "t,1}", "tool_name": "Read"}),
        )
        .unwrap();
        let writes = writes(&host);
        assert_eq!(
            writes[0][8],
            format!(
                "#{{&&:#{{==:#{{@workspace_app}},{}}},#{{==:#{{@workspace_codex}},{}}}}}",
                APP.replace('#', "##").replace(',', "#,").replace('}', "#}"),
                r##"{"turn": "t,1}", "pending": ["a#b"], "event": "PreToolUse", "at": 7}"##
                    .replace('#', "##")
                    .replace(',', "#,")
                    .replace('}', "#}")
            )
        );
        // The pending approval keeps the pane blocked.
        assert!(writes[0][9].ends_with("@workspace_agent 'blocked' ; display-message -p updated"));
    }

    #[test]
    fn a_turn_is_added_after_the_keys_the_record_already_has() {
        let held = pane(
            "idle",
            APP,
            r#"{"pending": [], "event": "SessionStart", "at": 1}"#,
        );
        let host = tmux(&held, "updated");
        event(
            &host,
            json!({"hook_event_name": "UserPromptSubmit", "turn_id": "turn-1"}),
        )
        .unwrap();
        let (record, _) = recorded(&writes(&host)[0]);
        assert_eq!(
            record,
            r#"{"pending":[],"event":"UserPromptSubmit","at":@,"turn":"turn-1"}"#
        );
        // A prompt without a turn records null.
        let host = tmux(&held, "updated");
        event(&host, json!({"hook_event_name": "UserPromptSubmit"})).unwrap();
        let (record, _) = recorded(&writes(&host)[0]);
        assert_eq!(
            record,
            r#"{"pending":[],"event":"UserPromptSubmit","at":@,"turn":null}"#
        );
    }

    #[test]
    fn an_approval_is_recorded_as_a_digest_of_its_arguments_never_the_arguments() {
        // The digests were computed with Python: sha256(json.dumps(tool_input, sort_keys=True)).
        for (input, digest) in [
            (
                json!({"command": "private command one"}),
                "87fe7d768695f48eb453647323522ff0ae77b1224eed72c4d420bd9b558e3152",
            ),
            (
                json!({"b": 1, "a": {"z": "é", "y": [1, 2.5, null, true]}}),
                "95dbb9b0423d8dd43e01d6464b759ac9bc95eacd4cf9c6b2fcb735dc0b7c799b",
            ),
            (
                Value::Null,
                "74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b",
            ),
            (
                json!("plain"),
                "945603a8f587786b463c3f94fce115c0fae88fac2728cc96ddf5981cf7f61741",
            ),
            (
                json!({}),
                "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
            ),
        ] {
            let held = pane("working", APP, r#"{"turn":"turn-1","pending":[]}"#);
            let host = tmux(&held, "updated");
            event(
                &host,
                json!({"hook_event_name": "PermissionRequest", "turn_id": "turn-1",
                       "tool_name": "Bash", "tool_input": input}),
            )
            .unwrap();
            let (record, _) = recorded(&writes(&host)[0]);
            assert_eq!(
                record,
                format!(
                    r#"{{"turn":"turn-1","pending":["Bash:{digest}"],"event":"PermissionRequest","at":@}}"#
                ),
                "{input}"
            );
        }
        // Without arguments, the tool alone; without a tool, "approval".
        let held = pane("working", APP, r#"{"turn":"turn-1","pending":[]}"#);
        for (data, key) in [
            (json!({"tool_name": "Bash"}), "Bash"),
            (json!({}), "approval"),
            (json!({"tool_name": ""}), "approval"),
            (
                json!({"tool_name": "Bash", "tool_use_id": "call-1"}),
                "call-1",
            ),
        ] {
            let host = tmux(&held, "updated");
            let mut data = data;
            data["hook_event_name"] = json!("PermissionRequest");
            data["turn_id"] = json!("turn-1");
            event(&host, data).unwrap();
            let (record, _) = recorded(&writes(&host)[0]);
            assert!(
                record.contains(&format!(r#""pending":["{key}"]"#)),
                "{record}"
            );
        }
    }

    #[test]
    fn a_post_tool_use_removes_the_approval_it_matches_by_id_or_by_digest() {
        let digest = "87fe7d768695f48eb453647323522ff0ae77b1224eed72c4d420bd9b558e3152";
        let held = pane(
            "blocked",
            APP,
            &format!(
                r#"{{"turn":"t","pending":["a","Bash:{digest}","a"],"event":"PermissionRequest","at":1}}"#
            ),
        );
        let after = |data: Value| -> String {
            let host = tmux(&held, "updated");
            let mut data = data;
            data["hook_event_name"] = json!("PostToolUse");
            data["turn_id"] = json!("t");
            event(&host, data).unwrap();
            recorded(&writes(&host)[0]).0
        };
        // By tool_use_id: only the first of two equal entries goes.
        assert!(
            after(json!({"tool_name": "Read", "tool_use_id": "a"}))
                .contains(&format!(r#""pending":["Bash:{digest}","a"]"#))
        );
        // By digest, when the id matches nothing.
        let record = after(json!({"tool_name": "Bash", "tool_use_id": "zzz",
                                  "tool_input": {"command": "private command one"}}));
        assert!(record.contains(r#""pending":["a","a"]"#), "{record}");
        // Nothing matches: nothing goes, and the pane stays blocked.
        let host = tmux(&held, "updated");
        event(
            &host,
            json!({"hook_event_name": "PostToolUse", "turn_id": "t", "tool_name": "Read"}),
        )
        .unwrap();
        assert!(writes(&host)[0][9].contains("@workspace_agent 'blocked'"));
    }

    #[test]
    fn stop_and_interrupt_empty_the_approvals_and_stop_leaves_the_decision_to_tmux() {
        let held = pane(
            "blocked",
            APP,
            r#"{"turn":"t","pending":["a"],"event":"PreToolUse","at":1}"#,
        );
        let host = tmux(&held, "updated");
        event(&host, json!({"hook_event_name": "Stop", "turn_id": "t"})).unwrap();
        let write = &writes(&host)[0][9];
        assert!(write.contains(r#""pending":[]"#));
        assert!(
            write.contains(&format!(
                "set-option -pF -t %7 @workspace_agent '{}' ; display-message",
                crate::hook::TURN_OVER
            )),
            "{write}"
        );
        let host = tmux(&held, "updated");
        event(
            &host,
            json!({"hook_event_name": "Interrupt", "turn_id": "t"}),
        )
        .unwrap();
        assert!(writes(&host)[0][9].contains("@workspace_agent 'idle' ; display-message"));
    }

    #[test]
    fn events_that_do_not_belong_to_the_record_publish_nothing() {
        let record = r#"{"turn":"turn-2","pending":[],"event":"PreToolUse","at":1}"#;
        let other = r#"{"kind":"codex","id":"other","pid":4242,"cwd":"/work","birth":"Thu Oct  1 20:23:29 2026"}"#;
        let cases: Vec<(&str, &str, &str, Value)> = vec![
            // Another owner may only start a session or a turn.
            (
                "other owner stop",
                other,
                record,
                json!({"hook_event_name": "Stop", "turn_id": "turn-2"}),
            ),
            (
                "other owner tool",
                other,
                record,
                json!({"hook_event_name": "PreToolUse", "turn_id": "turn-2"}),
            ),
            (
                "no owner permission",
                "",
                "",
                json!({"hook_event_name": "PermissionRequest"}),
            ),
            // A turn that is not the current one.
            (
                "old turn",
                APP,
                record,
                json!({"hook_event_name": "Stop", "turn_id": "turn-1"}),
            ),
            ("no turn", APP, record, json!({"hook_event_name": "Stop"})),
            (
                "null turn",
                APP,
                record,
                json!({"hook_event_name": "Stop", "turn_id": null}),
            ),
            (
                "numeric turn",
                APP,
                record,
                json!({"hook_event_name": "Stop", "turn_id": 2}),
            ),
            // A tool result after the turn ended.
            (
                "late result",
                APP,
                r#"{"turn":"turn-2","pending":[],"event":"Stop","at":1}"#,
                json!({"hook_event_name": "PostToolUse", "turn_id": "turn-2"}),
            ),
            (
                "late result after interrupt",
                APP,
                r#"{"turn":"turn-2","pending":[],"event":"Interrupt","at":1}"#,
                json!({"hook_event_name": "PostToolUse", "turn_id": "turn-2"}),
            ),
            // Fields of the wrong type.
            (
                "tool name",
                APP,
                record,
                json!({"hook_event_name": "PreToolUse", "turn_id": "turn-2", "tool_name": 5}),
            ),
            (
                "tool name list",
                APP,
                record,
                json!({"hook_event_name": "PreToolUse", "turn_id": "turn-2", "tool_name": ["Bash"]}),
            ),
            (
                "tool use id",
                APP,
                record,
                json!({"hook_event_name": "PreToolUse", "turn_id": "turn-2", "tool_use_id": 7}),
            ),
            (
                "pending",
                APP,
                r#"{"turn":"turn-2","pending":"x"}"#,
                json!({"hook_event_name": "PreToolUse", "turn_id": "turn-2"}),
            ),
            (
                "null pending",
                APP,
                r#"{"turn":"turn-2","pending":null}"#,
                json!({"hook_event_name": "PreToolUse", "turn_id": "turn-2"}),
            ),
            (
                "record turn is a number",
                APP,
                r#"{"turn":2,"pending":[]}"#,
                json!({"hook_event_name": "PreToolUse", "turn_id": "2"}),
            ),
        ];
        for (what, app, record, data) in cases {
            let held = pane("working", app, record);
            let host = tmux(&held, "updated");
            event(&host, data).unwrap();
            assert!(writes(&host).is_empty(), "{what}: {:?}", writes(&host));
        }
    }

    #[test]
    fn a_start_or_a_prompt_is_taken_whatever_turn_the_record_holds() {
        let record = r#"{"turn":"turn-2","pending":["x"],"event":"Stop","at":1}"#;
        for (app, data, status, pending) in [
            // Compaction continues the turn: the record and the status of the same owner stay.
            (
                APP,
                json!({"hook_event_name": "SessionStart"}),
                "working",
                r#""pending":["x"]"#,
            ),
            // A new owner starts empty and idle.
            (
                "",
                json!({"hook_event_name": "SessionStart"}),
                "idle",
                r#""pending":[]"#,
            ),
            // A prompt starts the turn.
            (
                APP,
                json!({"hook_event_name": "UserPromptSubmit", "turn_id": "turn-3"}),
                "working",
                r#""pending":[]"#,
            ),
        ] {
            let held = pane("working", app, record);
            let host = tmux(&held, "updated");
            event(&host, data.clone()).unwrap();
            let write = &writes(&host)[0][9];
            assert!(write.contains(pending), "{data}: {write}");
            assert!(
                write.contains(&format!("@workspace_agent '{status}'")),
                "{data}: {write}"
            );
        }
    }

    #[test]
    fn the_compare_and_set_is_tried_five_times_and_then_given_up_quietly() {
        let held = pane("", "", "");
        let host = tmux(&held, "");
        event(
            &host,
            json!({"hook_event_name": "UserPromptSubmit", "turn_id": "t"}),
        )
        .unwrap();
        assert_eq!(writes(&host).len(), 5);
        // Every try reads the pane again.
        assert_eq!(
            host.calls_to("tmux")
                .iter()
                .filter(|call| call[4] == "display-message")
                .count(),
            5
        );
        // One that wins on the third try stops there.
        let tries = Rc::new(Cell::new(0));
        let seen = Rc::clone(&tries);
        let host = FakeHost::scripted(move |argv| {
            Ok(if argv.iter().any(|arg| arg == "display-message") {
                "99\t/dev/ttys001\t\t\t\t\t".to_string()
            } else if argv.iter().any(|arg| arg == "show-option") {
                String::new()
            } else {
                seen.set(seen.get() + 1);
                if seen.get() == 3 {
                    "updated".into()
                } else {
                    String::new()
                }
            })
        });
        event(
            &host,
            json!({"hook_event_name": "UserPromptSubmit", "turn_id": "t"}),
        )
        .unwrap();
        assert_eq!(tries.get(), 3);
    }

    #[test]
    fn a_failing_tmux_is_an_error_for_the_caller_to_swallow() {
        let host = FakeHost::scripted(|_| Err(crate::error::Error::Msg("no server".into())));
        assert!(event(&host, json!({"hook_event_name": "UserPromptSubmit"})).is_err());
    }

    #[test]
    fn session_end_clears_the_pane_only_while_the_owner_is_the_same() {
        let held = pane("working", APP, r#"{"turn":"t","pending":[]}"#);
        let host = tmux(&held, "");
        event(&host, json!({"hook_event_name": "SessionEnd"})).unwrap();
        let tmux_calls = host.calls_to("tmux");
        assert_eq!(
            tmux_calls[0][4..],
            [
                "show-option",
                "-p",
                "-v",
                "-q",
                "-t",
                "%7",
                "@workspace_app"
            ]
        );
        // `show-option` answers with the record here, which is not this owner's identity.
        assert!(writes(&host).is_empty());
        let host = FakeHost::scripted(|argv| {
            Ok(if argv.iter().any(|arg| arg == "show-option") {
                APP.to_string()
            } else {
                String::new()
            })
        });
        event(&host, json!({"hook_event_name": "SessionEnd"})).unwrap();
        let writes = writes(&host);
        assert_eq!(writes.len(), 1);
        for option in [
            "@workspace_app",
            "@workspace_agent",
            "@workspace_agent_wait",
            "@workspace_agent_failed",
            "@workspace_codex",
        ] {
            assert!(
                writes[0][9].contains(&format!("set-option -pu -t %7 {option}")),
                "{option}"
            );
        }
    }

    #[test]
    fn data_and_metadata_that_are_not_objects_publish_nothing() {
        let host = FakeHost::new();
        codex_event(&host, &prefix(), "%7", &json!([]), &metadata()).unwrap();
        codex_event(
            &host,
            &prefix(),
            "%7",
            &json!({"hook_event_name": "Stop"}),
            &json!([]),
        )
        .unwrap();
        codex_event(&host, &prefix(), "%7", &json!({}), &metadata()).unwrap();
        assert!(host.calls.borrow().is_empty());
    }
}
