//! Tab picker (`pick_window`), `window_preview`, and `new_tab`.

use super::workspace::run_fzf;
use super::{Setting, TAB_FZF, tab_index, tab_row};
use crate::cli::message_via;
use crate::error::Result;
use crate::model::roll_up;
use crate::paths;
use crate::process::which;
use crate::text::{printable, py_strip, shlex_quote, short_path};
use crate::tmux::{Fields, Tmux};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

// ---------------------------------------------------------------------------------------------
// Tabs

/// What the preview shows of window `index` of `session`: the panes, then the screen. Anything
/// that goes wrong is said in the preview itself.
pub(super) fn preview_of(tmux: &Tmux, home: &Path, session: &str, index: &str) -> String {
    let target = format!("={session}:{index}");
    let window = tmux.quiet(&[
        "display-message",
        "-p",
        "-t",
        &target,
        "#{window_name} (#{window_panes} panes)",
    ]);
    let mut text = format!("=== Window {index}: {window} ===\n\n");
    let names = [
        "pane_index",
        "pane_current_command",
        "pane_current_path",
        "@workspace_label",
    ];
    match tmux.fields("list-panes", &names, &["-t", &target]) {
        Ok(panes) => {
            for pane in panes {
                let command = pane.get("pane_current_command");
                let label = match pane.get("@workspace_label") {
                    "" => command,
                    label => label,
                };
                text += &format!(
                    "  • Pane {}: {label} ({command}) in {}\n",
                    pane.get("pane_index"),
                    short_path(pane.get("pane_current_path"), home)
                );
            }
            text += "\n--- Terminal View ---\n\n";
            text += &tmux.quiet(&["capture-pane", "-p", "-t", &target]);
            text.push('\n');
        }
        Err(error) => text += &format!("Preview unavailable: {error}\n"),
    }
    text
}

/// `window-preview INDEX`: what the tab picker shows beside the list. `index` may be the whole first
/// field of a row (`3: editor`), of which the number counts.
pub fn window_preview(tmux: &Tmux, index: Option<&str>) {
    let session = tmux.quiet(&["display-message", "-p", "#{session_name}"]);
    if session.is_empty() {
        return;
    }
    let index = index
        .filter(|index| !index.is_empty())
        .map_or("1", |index| py_strip(index).split(':').next().unwrap_or(""));
    let text = preview_of(tmux, &paths::home(), &session, index);
    // A reader that went away, such as a preview that fzf replaced, is not an error.
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// `pick-window`: choose one of the tabs of the current workspace, with a live preview.
pub fn pick_window(tmux: &Tmux) -> Result<()> {
    pick_window_in(&Setting::current(), tmux)
}

pub(super) fn pick_window_in(setting: &Setting, tmux: &Tmux) -> Result<()> {
    let session = tmux.quiet(&["display-message", "-p", "#{session_name}"]);
    if session.is_empty() {
        message_via(tmux, "No active tmux session.");
        return Ok(());
    }
    let target = format!("={session}");
    let names = [
        "session_name",
        "window_id",
        "window_index",
        "window_name",
        "window_active",
        "window_panes",
        "pane_id",
        "pane_index",
        "pane_current_command",
        "@workspace_label",
        "@workspace_agent",
        "@workspace_home",
    ];
    let panes = tmux.fields("list-panes", &names, &["-s", "-t", &target])?;
    if panes.is_empty() {
        message_via(tmux, "No windows found.");
        return Ok(());
    }

    let by_tab = roll_up(&panes).by_tab;
    let mut order: Vec<&str> = Vec::new();
    let mut windows: HashMap<&str, Vec<&Fields>> = HashMap::new();
    for pane in &panes {
        let id = pane.get("window_id");
        if !windows.contains_key(id) {
            order.push(id);
        }
        windows.entry(id).or_default().push(pane);
    }
    let rows: Vec<String> = order
        .iter()
        .map(|id| {
            let members = &windows[id];
            let labels: Vec<String> = members
                .iter()
                .map(|pane| {
                    let label = match pane.get("@workspace_label") {
                        "" => pane.get("pane_current_command"),
                        label => label,
                    };
                    format!("{}:{label}", pane.get("pane_index"))
                })
                .collect();
            let first = members[0];
            tab_row(
                first.get("window_index"),
                first.get("window_name"),
                first.get("window_panes"),
                &labels.join(", "),
                first.get("window_active") == "1",
                by_tab.get(*id).copied().unwrap_or(""),
            )
        })
        .collect();

    if which("fzf").is_none() || !setting.terminal() {
        tmux.run(&[
            "choose-tree",
            "-Zw",
            "-t",
            &target,
            "-f",
            &format!("#{{==:#{{session_name}},{session}}}"),
        ])?;
        return Ok(());
    }

    let chosen = run_fzf(
        &TAB_FZF,
        &[
            format!(
                "--preview={} window-preview {{1}}",
                shlex_quote(&paths::self_path())
            ),
            "--preview-window=right:55%:wrap".to_string(),
            "--header=Select window to switch | Esc: cancel".to_string(),
            "--prompt=Window > ".to_string(),
            "--height=100%".to_string(),
            "--layout=reverse".to_string(),
            "--border=rounded".to_string(),
            "--info=inline".to_string(),
        ],
        &rows,
    )?;
    if chosen.status != 0 || py_strip(&chosen.stdout).is_empty() {
        return Ok(());
    }
    tmux.run(&[
        "select-window",
        "-t",
        &format!("={session}:{}", tab_index(&chosen.stdout)),
    ])
    .map(|_| ())
}

/// `new-tab NAME [FOLDER]`: a new tab with this name, in the folder, else in the folder of the
/// workspace, else in the projects folder.
pub fn new_tab(tmux: &Tmux, name: Option<&str>, folder: Option<&str>) -> Result<()> {
    new_tab_in(&Setting::current(), tmux, name, folder)
}

pub(super) fn new_tab_in(
    setting: &Setting,
    tmux: &Tmux,
    name: Option<&str>,
    folder: Option<&str>,
) -> Result<()> {
    let folder = match folder.filter(|folder| !folder.is_empty() && Path::new(folder).is_dir()) {
        Some(folder) => folder.to_string(),
        None => {
            let session = tmux.quiet(&["display-message", "-p", "#{session_path}"]);
            let session = py_strip(&session);
            if !session.is_empty() && Path::new(session).is_dir() {
                session.to_string()
            } else {
                setting.projects().to_string_lossy().into_owned()
            }
        }
    };
    let clean = printable(name.unwrap_or(""));
    let tab = match py_strip(&clean) {
        "" => "tab",
        name => name,
    };
    let pane = tmux.run(&[
        "new-window",
        "-P",
        "-F",
        "#{pane_id}",
        "-c",
        &folder,
        "-n",
        tab,
    ])?;
    tmux.put("@workspace_label", tab, Some(&pane))
}
