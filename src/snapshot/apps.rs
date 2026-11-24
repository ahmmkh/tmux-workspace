//! Process table scanning, live app verification, Codex lookup, and Neovim session save.

use crate::error::Result;
use crate::hook::codex::resolve_codex;
use crate::hook::process::{self as procs, Table, py_int};
use crate::host::Host;
use crate::model::option_json;
use crate::pane::{APP_COMMANDS, truthy};
use crate::process::{Spawn, spawn};
use crate::text::{py_split_ws, py_splitlines, py_strip};
use crate::tmux::{Fields, Tmux};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long Neovim may take to save its session.
pub(super) const NVIM_TIMEOUT: Duration = Duration::from_secs(10);
/// Asked of a running Neovim, which writes its session file.
pub(super) const NVIM_SAVE: &str = "luaeval(\"require('custom.tmux_workspace').save()\")";

// ---------------------------------------------------------------------------------------------
// What runs in each pane

/// Process id -> (parent id, process group, foreground group of its tty, start time as `ps` prints it).
pub type ProcessTable = HashMap<i32, (i32, i32, i32, String)>;

/// Every process, with what tells a running application from an old record of one.
pub fn process_table(host: &dyn Host) -> Result<ProcessTable> {
    let listing = host.run(&procs::ps(&["-axo", PROCESS_COLUMNS]))?;
    Ok(read_process_table(&listing))
}

/// What `process_table` asks `ps` for.
pub(super) const PROCESS_COLUMNS: &str = "pid=,ppid=,pgid=,tpgid=,lstart=";

/// The processes on these ttys (as `ps` names them: `ttys001`, `pts/3`) and the processes `pids` name:
/// the table a save looks things up in. It needs the processes of the panes, the agents in them and the
/// processes between, which share the tty of the pane, and the applications the panes record, which do
/// not always: Neovim 0.10 and later record the pid of `nvim --embed`, a child of the Neovim on the tty
/// that has a session of its own and no tty. `ps` takes the union of the two lists in one call. Asking for
/// those alone is what keeps `save` fast: `ps -ax` takes 20 ms on a Mac with a thousand processes, this 10.
/// A recorded process that is gone is not in the table, as before. When `ps` cannot take the lists (a pane
/// whose tty is gone) every process is read, as before.
pub(super) fn process_table_on(
    host: &dyn Host,
    ttys: &[String],
    pids: &[i32],
) -> Result<ProcessTable> {
    if !ttys.is_empty() {
        let ttys = ttys.join(",");
        let pids = pids
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut args = vec!["-t", ttys.as_str()];
        if !pids.is_empty() {
            args.extend(["-p", pids.as_str()]);
        }
        args.extend(["-o", PROCESS_COLUMNS]);
        if let Ok(listing) = host.run(&procs::ps(&args)) {
            return Ok(read_process_table(&listing));
        }
    }
    process_table(host)
}

pub(super) fn read_process_table(listing: &str) -> ProcessTable {
    let mut table = ProcessTable::new();
    for line in py_splitlines(listing) {
        let [pid, ppid, group, foreground, started] = py_split_ws(line, 4)[..] else {
            continue;
        };
        // A line that does not read as numbers is skipped.
        if let (Ok(pid), Ok(ppid), Ok(group), Ok(foreground)) = (
            pid.parse::<i32>(),
            ppid.parse::<i32>(),
            group.parse::<i32>(),
            foreground.parse::<i32>(),
        ) {
            table.insert(
                pid,
                (ppid, group, foreground, py_strip(started).to_string()),
            );
        }
    }
    table
}

/// The ttys of the panes as `ps` names them, each once: `/dev/ttys001` is `ttys001`.
pub(super) fn pane_ttys(panes: &[Fields]) -> Vec<String> {
    let mut ttys: Vec<String> = Vec::new();
    for pane in panes {
        let tty = pane.get("pane_tty");
        let tty = tty.strip_prefix("/dev/").unwrap_or(tty);
        if !tty.is_empty() && !ttys.iter().any(|seen| seen == tty) {
            ttys.push(tty.to_string());
        }
    }
    ttys
}

/// The pids the panes record in `@workspace_app`, each once. A record that is not JSON, or has no pid
/// that is a positive number, names none.
pub(super) fn recorded_pids(panes: &[Fields]) -> Vec<i32> {
    let mut pids = Vec::new();
    for pane in panes {
        let pid = option_json(pane.get("@workspace_app"))
            .ok()
            .and_then(|app| json_int(app.as_object()?.get("pid")?))
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|pid| *pid > 0);
        if let Some(pid) = pid
            && !pids.contains(&pid)
        {
            pids.push(pid);
        }
    }
    pids
}

/// Python's `int(value)` of a JSON value: a whole number, a number cut at the point, a bool or a number in text.
pub(super) fn json_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)),
        Value::Bool(flag) => Some(i64::from(*flag)),
        Value::String(text) => py_strip(text).parse().ok(),
        _ => None,
    }
}

/// The application recorded for the pane, when it is still the one running in it: the record must name
/// a process that started when it says, in the foreground, below the pane's process. `None` otherwise.
/// A record without a conversation identity becomes an error record. `pane` should hold `@workspace_app`;
/// without it, the option is read.
pub fn live_app(tmux: &Tmux, pane: &Fields, table: &ProcessTable) -> Option<Value> {
    let raw = if pane.has("@workspace_app") {
        pane.get("@workspace_app").to_string()
    } else {
        tmux.option("@workspace_app", Some(pane.get("pane_id")))
    };
    if raw.is_empty() {
        return None;
    }
    let app = option_json(&raw).ok()?;
    let record = app.as_object()?;
    let pid = i32::try_from(json_int(record.get("pid")?)?).ok()?;
    let (_parent, group, foreground, started) = table.get(&pid)?;
    let kind = record.get("kind")?.as_str()?;
    if record.get("birth")?.as_str() != Some(started)
        || !APP_COMMANDS.iter().any(|(name, _)| *name == kind)
    {
        return None;
    }
    // A stopped/background job is not what the user is currently running.
    if *foreground > 0 && group != foreground {
        return None;
    }
    let pane_pid = py_int(pane.get("pane_pid")).ok()?;
    let (mut ancestor, mut seen) = (pid, HashSet::new());
    loop {
        if ancestor == 0 || seen.contains(&ancestor) {
            return None;
        }
        if ancestor == pane_pid {
            break;
        }
        seen.insert(ancestor);
        ancestor = table.get(&ancestor).map_or(0, |process| process.0);
    }
    match record.get("id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => Some(app),
        // Antigravity CLI writes pid/cwd/birth before a conversation starts (or after `/new`);
        // treat an empty id as no conversation to reopen rather than a recovery error.
        _ if kind == "agy" => None,
        _ => Some(json!({
            "kind": kind,
            "error": "No exact conversation identity was recorded",
        })),
    }
}

/// What the Codex lookup found out about the processes, once for the whole save.
pub(super) struct Scan {
    processes: Table,
    /// Why the process table could not be read.
    error: Option<String>,
}

/// Looks for the Codex of a pane. The process table is read when the first pane needs it, and never again.
pub(super) struct Codex<'a> {
    pub(super) host: &'a dyn Host,
    pub(super) home: PathBuf,
    /// The ttys of the panes: the table is the processes on them, not every process.
    pub(super) ttys: Vec<String>,
    pub(super) scan: Option<Scan>,
}

impl Codex<'_> {
    /// The processes on the ttys of the panes, in the form the Codex lookup reads. When `ps` cannot take
    /// the list (a pane whose tty is gone), or there is none, every process.
    fn processes(&self) -> Result<Table> {
        if !self.ttys.is_empty()
            && let Ok(table) = procs::process_table(self.host, None, Some(&self.ttys.join(",")))
        {
            return Ok(table);
        }
        procs::process_table(self.host, None, None)
    }
}

impl Codex<'_> {
    fn ensure_scan(&mut self) -> &Scan {
        if self.scan.is_none() {
            self.scan = Some(match self.processes() {
                Ok(processes) => Scan {
                    processes,
                    error: None,
                },
                Err(e) => Scan {
                    processes: Table::new(),
                    error: Some(e.to_string()),
                },
            });
        }
        self.scan.as_ref().expect("scanned above")
    }

    /// Whether any process on `pane`'s tty could possibly be a Codex CLI (`codex`, `node`, `bun`),
    /// avoiding extra tmux `display-message` calls for non-Codex panes.
    fn has_candidate_on_tty(&mut self, pane: &Fields) -> bool {
        if pane.get("pane_current_command") == "codex" {
            return true;
        }
        let scan = self.ensure_scan();
        if scan.error.is_some() {
            return false;
        }
        let tty = pane.get("pane_tty");
        let tty = tty.strip_prefix("/dev/").unwrap_or(tty);
        scan.processes.values().any(|(_, proc_tty, comm)| {
            proc_tty == tty
                && matches!(
                    comm.rsplit('/').next().unwrap_or(comm),
                    "codex" | "node" | "bun"
                )
        })
    }

    /// The Codex conversation of the pane using its `pane_title` field (tests).
    #[cfg(test)]
    pub(super) fn identify(&mut self, pane: &Fields) -> Option<Value> {
        self.identify_with_title(pane, pane.get("pane_title"))
    }

    /// The Codex conversation of the pane: `None` when no Codex runs in it, else an object with its identity
    /// or with `error`, the reason it cannot be told.
    pub(super) fn identify_with_title(&mut self, pane: &Fields, pane_title: &str) -> Option<Value> {
        let host = self.host;
        let home = self.home.clone();
        let scan = self.ensure_scan();
        if let Some(error) = &scan.error
            && pane.get("pane_current_command") == "codex"
        {
            return Some(json!({"kind": "codex", "error": error}));
        }
        // Any failure of the lookup is a recorded reason, so the history of the pane is still saved.
        let found = py_int(pane.get("pane_pid")).and_then(|pid| {
            resolve_codex(
                host,
                &home,
                pid,
                pane.get("pane_tty"),
                pane_title,
                Some(pane.get("pane_current_path")),
                Some(&scan.processes),
            )
        });
        match found {
            Ok(found) => found,
            Err(e) => Some(json!({
                "kind": "codex",
                "error": format!("Identity lookup failed: {e}"),
            })),
        }
    }
}

/// Read `pane_title` from `pane` when supplied (tests), else query tmux on demand for one pane
/// without separator splitting so control chars or line breaks in titles never abort a save.
pub(super) fn read_pane_title(tmux: &Tmux, pane: &Fields) -> String {
    if pane.has("pane_title") {
        return pane.get("pane_title").to_string();
    }
    let pane_id = pane.get("pane_id");
    if pane_id.is_empty() {
        return String::new();
    }
    tmux.quiet(&["display-message", "-p", "-t", pane_id, "#{pane_title}"])
}

/// Whether the title of the pane shows the conversation `app` is running, as legacy Codex prints it:
/// the full thread id, or its first 29 characters and `...`. Modern Codex `0.171.0` titles return
/// `false` so `save` resolves against `state_*.sqlite` and catches `/new` or `/resume` switches.
pub(super) fn shows_current_conversation(app: &Value, title: &str) -> bool {
    if app.get("kind").and_then(Value::as_str) != Some("codex")
        || app.get("error").is_some_and(truthy)
    {
        return false;
    }
    let Some(id) = app.get("id").and_then(Value::as_str) else {
        return false;
    };
    let shortened: String = id.chars().take(29).collect();
    title == format!("codex | {id}") || title == format!("codex | {shortened}...")
}

/// Decides what application a pane runs, and has Neovim write its session.
pub(super) struct Applications<'a> {
    pub(super) tmux: &'a Tmux,
    pub(super) table: ProcessTable,
    pub(super) codex: Codex<'a>,
}

impl Applications<'_> {
    /// The application of the pane for its record: what it runs, with the exact conversation or session
    /// to reopen, or the reason that cannot be told. `None` when it runs none.
    pub(super) fn of(&mut self, pane: &Fields) -> Option<Value> {
        let mut app = live_app(self.tmux, pane, &self.table);
        let live_is_codex = app
            .as_ref()
            .is_some_and(|app| app.get("kind").and_then(Value::as_str) == Some("codex"));
        let codex = if app.is_some() && !live_is_codex {
            None
        } else if live_is_codex || self.codex.has_candidate_on_tty(pane) {
            let title = read_pane_title(self.tmux, pane);
            let current = app
                .as_ref()
                .is_some_and(|app| shows_current_conversation(app, &title));
            if current {
                None
            } else {
                self.codex.identify_with_title(pane, &title)
            }
        } else {
            None
        };
        let command = pane.get("pane_current_command");
        if codex.is_some() {
            app = codex;
        } else if app.is_none() && APP_COMMANDS.iter().any(|(name, _)| *name == command) {
            app = Some(json!({
                "kind": command,
                "error": "Active application has no verified conversation/session identity",
            }));
        }
        app.map(save_editor_session)
    }
}

/// A running Neovim writes its session file now; if that fails the pane has no session to reopen.
pub(super) fn save_editor_session(app: Value) -> Value {
    if app.get("kind").and_then(Value::as_str) != Some("nvim") {
        return app;
    }
    let Some(socket) = app.get("socket").filter(|socket| truthy(socket)) else {
        return app;
    };
    let saved = socket.as_str().is_some_and(|socket| {
        let run = spawn(&Spawn {
            argv: ["nvim", "--server", socket, "--remote-expr", NVIM_SAVE]
                .map(String::from)
                .to_vec(),
            timeout: Some(NVIM_TIMEOUT),
            ..Spawn::default()
        });
        run.is_ok_and(|out| out.status == 0)
            && app
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| Path::new(id).is_file())
    });
    if saved {
        app
    } else {
        json!({"kind": "nvim", "error": "Could not save the active Neovim session"})
    }
}
