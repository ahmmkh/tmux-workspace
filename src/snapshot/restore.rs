//! Restoring saved workspaces, `init`, autosave `tick`, and pane `history`.

use super::apps::json_int;
use super::save::save;
use super::store::{lock_in, read_snapshot_in};
use crate::cli::message_via;
use crate::error::{Error, Result, fail};
use crate::hook::process::py_int;
use crate::host::Host;
use crate::model::PARKING;
use crate::pane::identity;
use crate::paths;
use crate::process::exec;
use crate::text::{shlex_join, truthy};
use crate::tmux::{SEP, Tmux};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

// ---------------------------------------------------------------------------------------------
// restore, init and tick

/// One pane of a snapshot, as `restore` needs it.
pub(super) struct SavedPane {
    id: String,
    session: String,
    window: i64,
    index: i64,
    cwd: String,
    active: bool,
}

/// One tab of a snapshot.
pub(super) struct SavedWindow {
    session: String,
    /// The tab number as saved, for tmux.
    number: String,
    index: i64,
    name: String,
    layout: String,
    active: bool,
    width: String,
    height: String,
    zoomed: bool,
}

/// A text field of a saved object.
pub(super) fn saved_text(object: &Value, key: &str) -> Result<String> {
    match object.get(key).and_then(Value::as_str) {
        Some(text) => Ok(text.to_string()),
        None => fail(format!("The saved workspace is damaged: no {key}")),
    }
}

/// A whole-number field of a saved object.
pub(super) fn saved_number(object: &Value, key: &str) -> Result<i64> {
    match object.get(key).and_then(json_int) {
        Some(number) => Ok(number),
        None => fail(format!("The saved workspace is damaged: no {key}")),
    }
}

pub(super) fn saved_list<'a>(data: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    match data.get(key).and_then(Value::as_array) {
        Some(list) => Ok(list),
        None => fail(format!("The saved workspace is damaged: no {key}")),
    }
}

pub(super) fn saved_panes(data: &Value) -> Result<Vec<SavedPane>> {
    saved_list(data, "panes")?
        .iter()
        .map(|pane| {
            Ok(SavedPane {
                id: saved_text(pane, "id")?,
                session: saved_text(pane, "session")?,
                window: saved_number(pane, "window")?,
                index: saved_number(pane, "index")?,
                cwd: saved_text(pane, "cwd")?,
                active: pane.get("active").is_some_and(truthy),
            })
        })
        .collect()
}

pub(super) fn saved_windows(data: &Value) -> Result<Vec<SavedWindow>> {
    saved_list(data, "windows")?
        .iter()
        .map(|window| {
            let number = saved_text(window, "window_index")?;
            Ok(SavedWindow {
                session: saved_text(window, "session_name")?,
                index: py_int(&number).map(i64::from)?,
                number,
                name: saved_text(window, "window_name")?,
                layout: saved_text(window, "window_layout")?,
                active: saved_text(window, "window_active")? == "1",
                width: saved_text(window, "window_width")?,
                height: saved_text(window, "window_height")?,
                zoomed: saved_text(window, "window_zoomed_flag")? == "1",
            })
        })
        .collect()
}

/// The command that starts a pane of a snapshot.
pub(super) fn pane_command(me: &str, snapshot: &Path, id: &str) -> String {
    shlex_join(&[me, "pane-start", &snapshot.to_string_lossy(), id])
}

/// Where a pane starts: its folder, or the home folder when that is gone.
pub(super) fn start_folder(cwd: &str) -> String {
    if Path::new(cwd).is_dir() {
        cwd.to_string()
    } else {
        paths::home().to_string_lossy().into_owned()
    }
}

/// Bring back the sessions of the latest snapshot that tmux no longer has: their tabs, panes, layouts, which
/// pane and tab were active, and the active session. Gives whether it brought back any. The panes
/// restore their own labels and text, and wait for this to finish before they reopen their applications.
pub fn restore(tmux: &Tmux, locked: bool) -> Result<bool> {
    restore_in(&paths::state_dir(), tmux, locked)
}

/// `restore` for the state folder `root`. `locked`: the caller holds the lock.
pub fn restore_in(root: &Path, tmux: &Tmux, locked: bool) -> Result<bool> {
    let _lock = if locked { None } else { Some(lock_in(root)?) };
    let Some((path, data)) = read_snapshot_in(root)? else {
        return Ok(false);
    };
    let (panes, windows) = (saved_panes(&data)?, saved_windows(&data)?);
    let existing: HashSet<String> = tmux
        .fields("list-panes", &["session_name"], &["-a"])?
        .iter()
        .map(|row| row.get("session_name").to_string())
        .collect();
    let wanted: BTreeSet<&str> = panes
        .iter()
        .map(|pane| pane.session.as_str())
        .filter(|session| !existing.contains(*session))
        .collect();
    if wanted.is_empty() {
        return Ok(false);
    }
    tmux.put("@workspace_restoring", "1", None)?;
    let rebuilt = rebuild(tmux, &path, &data, &panes, &windows, &wanted);
    let finished = tmux.put("@workspace_restoring", "0", None);
    rebuilt?;
    finished?;
    Ok(true)
}

/// Create the `wanted` sessions, tab by tab, pane by pane.
pub(super) fn rebuild(
    tmux: &Tmux,
    path: &Path,
    data: &Value,
    panes: &[SavedPane],
    windows: &[SavedWindow],
    wanted: &BTreeSet<&str>,
) -> Result<()> {
    let me = paths::self_path();
    for name in wanted {
        let mut tabs: Vec<&SavedWindow> = windows.iter().filter(|w| w.session == *name).collect();
        tabs.sort_by_key(|w| w.index);
        let mut session: Option<String> = None;
        for window in tabs {
            let mut records: Vec<&SavedPane> = panes
                .iter()
                .filter(|p| p.session == *name && p.window == window.index)
                .collect();
            records.sort_by_key(|p| p.index);
            let Some((first, others)) = records.split_first() else {
                continue;
            };
            let command = pane_command(&me, path, &first.id);
            let folder = start_folder(&first.cwd);
            let (tab, first_pane) = match &session {
                None => {
                    let format = format!("#{{session_id}}{SEP}#{{window_id}}{SEP}#{{pane_id}}");
                    let out = tmux.run(&[
                        "new-session",
                        "-d",
                        "-P",
                        "-F",
                        &format,
                        "-s",
                        name,
                        "-n",
                        &window.name,
                        "-x",
                        &window.width,
                        "-y",
                        &window.height,
                        "-c",
                        &folder,
                        &command,
                    ])?;
                    let [id, tab, pane] = out.split(SEP).collect::<Vec<_>>()[..] else {
                        return fail(format!(
                            "tmux described the new session {name} unexpectedly"
                        ));
                    };
                    let index =
                        tmux.run(&["display-message", "-p", "-t", tab, "#{window_index}"])?;
                    if index != window.number {
                        tmux.run(&[
                            "move-window",
                            "-s",
                            tab,
                            "-t",
                            &format!("{id}:{}", window.number),
                        ])?;
                    }
                    session = Some(id.to_string());
                    (tab.to_string(), pane.to_string())
                }
                Some(id) => {
                    let format = format!("#{{window_id}}{SEP}#{{pane_id}}");
                    let out = tmux.run(&[
                        "new-window",
                        "-d",
                        "-P",
                        "-F",
                        &format,
                        "-t",
                        &format!("{id}:{}", window.number),
                        "-n",
                        &window.name,
                        "-c",
                        &folder,
                        &command,
                    ])?;
                    let Some((tab, pane)) = out.split_once(SEP) else {
                        return fail(format!(
                            "tmux described the new tab {} unexpectedly",
                            window.name
                        ));
                    };
                    (tab.to_string(), pane.to_string())
                }
            };
            let mut active_pane = first_pane;
            for record in others {
                let command = pane_command(&me, path, &record.id);
                let new_pane = tmux.run(&[
                    "split-window",
                    "-d",
                    "-h",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    &tab,
                    "-c",
                    &start_folder(&record.cwd),
                    &command,
                ])?;
                tmux.run(&["select-layout", "-t", &tab, "tiled"])?;
                if record.active {
                    active_pane = new_pane;
                }
            }
            tmux.run(&["select-layout", "-t", &tab, &window.layout])?;
            tmux.run(&["select-pane", "-t", &active_pane])?;
            if window.zoomed {
                tmux.run(&["resize-pane", "-Z", "-t", &active_pane])?;
            }
            if window.active {
                tmux.run(&["select-window", "-t", &tab])?;
            }
        }
    }
    if let Some(preferred) = data.get("active_session").and_then(Value::as_str)
        && wanted.contains(preferred)
        && preferred != PARKING
    {
        tmux.quiet(&["switch-client", "-t", &format!("={preferred}")]);
    }
    Ok(())
}

/// Run `init` when the server starts: restore the missing sessions once per server, then start the
/// autosave. The server remembers it, so a reload of the configuration neither starts a second timer nor
/// restores a workspace the user closed on purpose.
pub fn initialize(tmux: &Tmux) -> Result<()> {
    initialize_in(&paths::state_dir(), tmux)
}

/// `initialize` for the state folder `root`.
pub fn initialize_in(root: &Path, tmux: &Tmux) -> Result<()> {
    let initialized = || tmux.option("@workspace_initialized", None) == "1";
    if initialized() {
        return Ok(());
    }
    let _lock = lock_in(root)?;
    if initialized() {
        return Ok(());
    }
    restore_in(root, tmux, true)?;
    tmux.put("@workspace_initialized", "1", None)?;
    tmux.run(&schedule_args()).map(|_| ())
}

/// The tmux command that runs the next autosave in five minutes.
pub(super) fn schedule_args() -> [&'static str; 5] {
    static COMMAND: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let command = COMMAND.get_or_init(|| shlex_join(&[&paths::self_path(), "tick"]));
    ["run-shell", "-b", "-d", "300", command]
}

/// The autosave: save at a low priority, and say so when it fails. The next one is scheduled either way.
pub fn tick(tmux: &Tmux, host: &dyn Host) -> Result<()> {
    // SAFETY: `nice` only lowers the priority of this process. Its result is -1 on failure, which does not matter.
    unsafe { libc::nice(10) };
    let saved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| save(tmux, host, false)))
        .unwrap_or_else(|panic| Err(Error::Msg(panic_text(panic.as_ref()))));
    let reported = saved.or_else(|e| -> Result<()> {
        // The save went wrong and the tick goes on, so nothing else would tell the log.
        crate::errorlog::record("tick", &[], &e);
        tmux.put("@workspace_save_error", &e.to_string(), None)?;
        message_via(tmux, &format!("Workspace autosave failed: {e}"));
        Ok(())
    });
    tmux.quiet(&schedule_args());
    crate::update::maybe_auto_update(tmux);
    reported
}

/// What a panic said.
pub(super) fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    let text = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown cause");
    format!("internal error: {text}")
}

// ---------------------------------------------------------------------------------------------
// history

/// `history [PANE]`: show the pane's saved terminal text in `less`. Returns only an error.
pub fn history(tmux: &Tmux, pane: &str) -> Result<()> {
    history_in(&paths::state_dir(), tmux, pane)
}

/// `history` for the state folder `root`.
pub fn history_in(root: &Path, tmux: &Tmux, pane: &str) -> Result<()> {
    let Some((_, data)) = read_snapshot_in(root)? else {
        return fail("No saved history yet; use prefix Ctrl+s first");
    };
    let id = identity(tmux, pane)?;
    let directory = data
        .get("directory")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let path = Path::new(directory).join(format!("{id}.txt"));
    if !path.is_file() {
        return fail("This pane is not in the latest snapshot");
    }
    Err(exec(&[
        "less".to_string(),
        "--".to_string(),
        path.to_string_lossy().into_owned(),
    ]))
}
