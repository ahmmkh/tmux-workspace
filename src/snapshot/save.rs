//! Capturing pane layouts, terminal text, and writing snapshots.

use super::apps::{Applications, Codex, pane_ttys, process_table_on, recorded_pids};
use super::store::{atomic, lock_in, read_json};
use crate::cli::{in_tmux, message_via};
use crate::error::{Error, Result};
use crate::hook::process::py_int;
use crate::host::Host;
use crate::model::{BOOTSTRAP, PARKING};
use crate::pane::is_identity;
use crate::paths::{self, cannot_read, cannot_write, make_private_dir};
use crate::pyjson::{Style, dumps};
use crate::text::{now, now_ns, strftime, uuid4_hex};
use crate::tmux::{Fields, SEP, Tmux, parse_tagged, tagged_format};
use serde_json::{Map, Value, json};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// An autosave within this many seconds of the last save does nothing.
pub(super) const AUTOSAVE_SECONDS: f64 = 300.0;
/// How many snapshots are kept. Editor sessions and AI history are never pruned.
pub(super) const KEPT: usize = 3;
/// Panes whose terminal text is read at the same time.
pub(super) const CAPTURE_THREADS: usize = 8;
/// What `save` says when it is done by hand.
pub(super) const SAVED_MESSAGE: &str =
    "Workspace saved (layouts, labels, history, active AI/editor identities)";

/// The format values `save` reads of every pane. `pane_title` is intentionally omitted here
/// because arbitrary terminal titles may contain control characters or Unicode line separators;
/// Codex candidate panes query `#{pane_title}` on demand via a single `display-message`.
pub(super) const PANE_FIELDS: [&str; 14] = [
    "session_name",
    "window_index",
    "pane_id",
    "pane_index",
    "pane_pid",
    "pane_current_path",
    "pane_active",
    "pane_tty",
    "pane_current_command",
    "@workspace_id",
    "@workspace_app",
    "@workspace_label",
    "@workspace_parked",
    "@workspace_home",
];

/// The format values `save` reads of every tab; they are saved as they are.
pub(super) const WINDOW_FIELDS: [&str; 8] = [
    "session_name",
    "window_index",
    "window_name",
    "window_layout",
    "window_active",
    "window_width",
    "window_height",
    "window_zoomed_flag",
];

// ---------------------------------------------------------------------------------------------
// save

/// `save`, and the autosave when `force` is false: write the snapshot of all sessions. An autosave does
/// nothing within five minutes of the last save. Nothing is saved while a restore runs.
pub fn save(tmux: &Tmux, host: &dyn Host, force: bool) -> Result<()> {
    save_in(&paths::state_dir(), tmux, host, force)
}

/// `save` for the state folder `root`.
pub fn save_in(root: &Path, tmux: &Tmux, host: &dyn Host, force: bool) -> Result<()> {
    let outside_tmux = {
        let _lock = lock_in(root)?;
        match save_locked(root, tmux, host, force)? {
            // The message was in the final tmux call when tmux can show it.
            Saved::Done => force && !in_tmux(),
            Saved::Nothing => false,
        }
    };
    if outside_tmux {
        message_via(tmux, SAVED_MESSAGE);
    }
    Ok(())
}

pub(super) enum Saved {
    Nothing,
    Done,
}

/// One pane of a snapshot, before its terminal text is read.
pub(super) struct Entry {
    record: Value,
    pane_id: String,
    id: String,
}

pub(super) fn save_locked(root: &Path, tmux: &Tmux, host: &dyn Host, force: bool) -> Result<Saved> {
    let first = tmux.quiet(&[
        "display-message",
        "-p",
        &format!("#{{@workspace_restoring}}{SEP}#{{client_session}}"),
    ]);
    let (restoring, active_session) = first.split_once(SEP).unwrap_or((&first, ""));
    if restoring == "1" {
        return Ok(Saved::Nothing);
    }
    let started = now();
    if !force && saved_recently(root, started)? {
        return Ok(Saved::Nothing);
    }
    let (panes, windows) = list(tmux)?;
    if panes.is_empty() {
        return Ok(Saved::Nothing);
    }
    let ttys = pane_ttys(&panes);
    let table = process_table_on(host, &ttys, &recorded_pids(&panes))?;

    // Every pane gets an identity, in one tmux call.
    let kept: Vec<&Fields> = panes
        .iter()
        .filter(|pane| pane.get("session_name") != BOOTSTRAP)
        .collect();
    let mut new_ids = Vec::new();
    let ids: Vec<String> = kept
        .iter()
        .map(|pane| {
            let id = pane.get("@workspace_id");
            if is_identity(id) {
                return id.to_string();
            }
            let id = uuid4_hex();
            new_ids.push(
                [
                    "set-option",
                    "-p",
                    "-t",
                    pane.get("pane_id"),
                    "@workspace_id",
                    &id,
                ]
                .map(String::from)
                .to_vec(),
            );
            id
        })
        .collect();
    if !new_ids.is_empty() {
        tmux.chain(&new_ids)?;
    }

    let mut applications = Applications {
        tmux,
        table,
        codex: Codex {
            host,
            home: paths::codex_home(&paths::home()),
            ttys,
            scan: None,
        },
    };
    let mut entries = Vec::new();
    for (pane, id) in kept.iter().zip(ids) {
        let app = applications.of(pane);
        let parked = pane.get("@workspace_parked");
        if pane.get("session_name") == PARKING && parked.is_empty() {
            continue;
        }
        entries.push(Entry {
            record: pane_record(pane, &id, app)?,
            pane_id: pane.get("pane_id").to_string(),
            id,
        });
    }

    // Plain text deliberately excludes executable terminal escape sequences.
    let texts = capture_all(tmux, entries.iter().map(|entry| entry.pane_id.as_str()))?;

    let saved_windows = windows
        .iter()
        .filter(|window| {
            entries.iter().any(|entry| {
                entry.record["session"].as_str() == Some(window.get("session_name"))
                    && Some(entry.record["window"].clone()) == window_number(window)
            })
        })
        .map(window_record)
        .collect::<Vec<_>>();
    let data = json!({
        "version": 1,
        "saved_at": started,
        "directory": "",
        "active_session": active_session,
        "windows": saved_windows,
        "panes": entries.iter().map(|entry| entry.record.clone()).collect::<Vec<_>>(),
    });

    let snapshots = root.join("snapshots");
    make_private_dir(&snapshots).map_err(|e| cannot_write(&snapshots, e))?;
    let directory = snapshots.join(now_ns().to_string());
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|e| cannot_write(&directory, e))?;
    let written = write_snapshot(root, &directory, &entries, &texts, data);
    if written.is_err() {
        // A half snapshot would crowd out a whole one.
        let _ = fs::remove_dir_all(&directory);
    }
    written?;

    let mut writes = vec![
        [
            "set-option",
            "-g",
            "@workspace_saved",
            &strftime("%H:%M", started),
        ]
        .map(String::from)
        .to_vec(),
        ["set-option", "-g", "@workspace_save_error", ""]
            .map(String::from)
            .to_vec(),
    ];
    if force && in_tmux() {
        writes.push(
            ["display-message", SAVED_MESSAGE]
                .map(String::from)
                .to_vec(),
        );
    }
    tmux.chain(&writes)?;
    prune(&snapshots)?;
    Ok(Saved::Done)
}

/// The files of one snapshot: the text of each pane, then `workspace.json`, then `latest.json`.
pub(super) fn write_snapshot(
    root: &Path,
    directory: &Path,
    entries: &[Entry],
    texts: &[String],
    mut data: Value,
) -> Result<()> {
    for (entry, text) in entries.iter().zip(texts) {
        atomic(
            &directory.join(format!("{}.txt", entry.id)),
            &format!("{text}\n"),
        )?;
    }
    data["directory"] = json!(directory.to_string_lossy());
    let workspace = directory.join("workspace.json");
    atomic(&workspace, &dumps(&data, Style::Indent2, false))?;
    let latest = json!({"saved_at": data["saved_at"], "path": workspace.to_string_lossy()});
    atomic(
        &root.join("latest.json"),
        &dumps(&latest, Style::Default, false),
    )
}

/// Whether the last save is less than five minutes old. A missing or unreadable record counts as old.
pub(super) fn saved_recently(root: &Path, now: f64) -> Result<bool> {
    let last = match read_json(&root.join("latest.json")) {
        Ok(last) => last,
        Err(Error::Json(_)) => None,
        Err(e) => return Err(e),
    };
    Ok(last
        .and_then(|last| last.get("saved_at").and_then(Value::as_f64))
        .is_some_and(|saved_at| now - saved_at < AUTOSAVE_SECONDS))
}

/// Every pane and every tab, in one tmux call.
pub(super) fn list(tmux: &Tmux) -> Result<(Vec<Fields>, Vec<Fields>)> {
    let output = tmux.chain(&[
        ["list-panes", "-a", "-F", &tagged_format("P", &PANE_FIELDS)]
            .map(String::from)
            .to_vec(),
        [
            "list-windows",
            "-a",
            "-F",
            &tagged_format("W", &WINDOW_FIELDS),
        ]
        .map(String::from)
        .to_vec(),
    ])?;
    let mut listings = parse_tagged(&output, &[("P", &PANE_FIELDS), ("W", &WINDOW_FIELDS)])?;
    let windows = listings.pop().unwrap_or_default();
    let panes = listings.pop().unwrap_or_default();
    Ok((panes, windows))
}

/// `int(window['window_index'])` as a JSON number.
pub(super) fn window_number(window: &Fields) -> Option<Value> {
    py_int(window.get("window_index")).ok().map(Value::from)
}

pub(super) fn window_record(window: &Fields) -> Value {
    Value::Object(
        WINDOW_FIELDS
            .iter()
            .map(|name| (name.to_string(), Value::from(window.get(name))))
            .collect::<Map<_, _>>(),
    )
}

pub(super) fn pane_record(pane: &Fields, id: &str, app: Option<Value>) -> Result<Value> {
    let label = pane.get("@workspace_label");
    Ok(json!({
        "id": id,
        "session": pane.get("session_name"),
        "window": py_int(pane.get("window_index"))?,
        "index": py_int(pane.get("pane_index"))?,
        "label": if label.is_empty() { "shell" } else { label },
        "cwd": pane.get("pane_current_path"),
        "active": pane.get("pane_active") == "1",
        "parked": pane.get("@workspace_parked"),
        "home": pane.get("@workspace_home"),
        "app": app,
    }))
}

/// The terminal text of each pane, whole history included, read on a few threads at once. The first
/// failure, in pane order, is the error.
pub(super) fn capture_all<'a>(
    tmux: &Tmux,
    panes: impl Iterator<Item = &'a str>,
) -> Result<Vec<String>> {
    let panes: Vec<&str> = panes.collect();
    let results: Vec<Mutex<Option<Result<String>>>> =
        panes.iter().map(|_| Mutex::new(None)).collect();
    let (next, failed) = (AtomicUsize::new(0), AtomicBool::new(false));
    std::thread::scope(|scope| {
        for _ in 0..panes.len().min(CAPTURE_THREADS) {
            scope.spawn(|| {
                while !failed.load(Ordering::Relaxed) {
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    let Some(pane) = panes.get(at) else { break };
                    let text = tmux.run(&["capture-pane", "-p", "-J", "-S", "-", "-t", pane]);
                    failed.fetch_or(text.is_err(), Ordering::Relaxed);
                    *results[at].lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
                }
            });
        }
    });
    let texts: Vec<String> = results
        .into_iter()
        .map_while(|slot| slot.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect::<Result<_>>()?;
    // Nothing but a failure stops the reading, and a failure came back above.
    debug_assert_eq!(texts.len(), panes.len());
    Ok(texts)
}

/// Keep the three newest snapshot folders (by name, which is the time). A name that is not all digits, or
/// is not a folder, is never removed, but it takes a place among the newest.
pub(super) fn prune(snapshots: &Path) -> Result<()> {
    let mut names: Vec<String> = fs::read_dir(snapshots)
        .and_then(|dir| {
            dir.map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect()
        })
        .map_err(|e| cannot_read(snapshots, e))?;
    names.sort_by(|a, b| b.cmp(a));
    for name in names.iter().skip(KEPT) {
        let path = snapshots.join(name);
        if path.is_dir() && !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) {
            fs::remove_dir_all(&path).map_err(|e| cannot_write(&path, e))?;
        }
    }
    Ok(())
}
