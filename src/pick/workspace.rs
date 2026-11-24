//! Workspace picker (`open`, `choose_or_create`, `delete_workspace`) and onboarding.

use super::{
    Setting, WORKSPACE_FZF, agent_status, ask, numbered_pick, picked_name, position, py_path,
    workspace_row,
};
use crate::cli::message_via;
use crate::error::{Error, Result, fail};
use crate::model::{BOOTSTRAP, PARKING};
use crate::onboarding;
use crate::paths::{Locations, cannot_write, resolve};
use crate::process::{Output, Spawn, exec, spawn, which};
use crate::snapshot;
use crate::text::{printable, py_int, py_splitlines, py_strip};
use crate::tmux::{SEP, Tmux};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// Workspaces

/// The workspaces that are running, in the order tmux lists them.
pub fn session_names(tmux: &Tmux) -> Vec<String> {
    tmux.fields("list-sessions", &["session_name"], &[])
        .map(|rows| {
            rows.iter()
                .map(|row| row.get("session_name").to_string())
                .filter(|name| name != PARKING && name != BOOTSTRAP)
                .collect()
        })
        .unwrap_or_default()
}

/// The running workspaces, most recently attached first. `listing` is `list-sessions` with the
/// format of `SESSION_FORMAT`. Sessions attached at the same moment, or never, keep tmux's order.
pub(super) fn recent_sessions(listing: &str) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for line in py_splitlines(listing) {
        let mut parts = line.rsplitn(3, SEP);
        let (_initialized, attached, name) = (parts.next(), parts.next(), parts.next());
        let (Some(attached), Some(name)) = (attached, name) else {
            return fail("list index out of range");
        };
        let when = if attached.is_empty() {
            0
        } else {
            py_int(attached)?
        };
        found.push((when, name));
    }
    found.sort_by_key(|(when, _)| Reverse(*when));
    Ok(found
        .into_iter()
        .map(|(_, name)| name)
        .filter(|name| *name != PARKING && *name != BOOTSTRAP)
        .map(String::from)
        .collect())
}

/// The folders of the projects folder, by name: the ones that are not hidden.
pub(super) fn project_names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// The folders that zoxide lists (`output` is `zoxide query -l`) and the picker has no row for yet:
/// not a workspace that is running, not in the projects folder.
pub(super) fn zoxide_folders(
    loc: &Locations,
    output: &str,
    live: &[String],
    projects: &[String],
    root: &Path,
) -> Vec<String> {
    let root = root.to_string_lossy();
    py_splitlines(output)
        .into_iter()
        .filter_map(|line| {
            let path = py_path(loc, py_strip(line));
            let name = path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            let shown = path.to_string_lossy().into_owned();
            // A prefix of the text, as the Python had it: "Projects-old" is left out too.
            (path.is_dir()
                && !live.contains(&name)
                && !projects.contains(&name)
                && !shown.starts_with(root.as_ref()))
            .then_some(shown)
        })
        .collect()
}

/// The rows of the workspace picker: the running workspaces, then the other folders zoxide knows,
/// then the projects that are not running.
pub(super) fn workspace_rows(
    live: &[String],
    zoxide: &[String],
    projects: &[String],
    by_workspace: &HashMap<String, &'static str>,
) -> Vec<String> {
    let mut rows = Vec::new();
    for name in live {
        let status = by_workspace.get(name).copied().unwrap_or("");
        rows.push(workspace_row(name, &format!("● {name}"), status));
    }
    for folder in zoxide {
        rows.push(workspace_row(folder, &format!("⚡ {folder}"), ""));
    }
    for project in projects.iter().filter(|project| !live.contains(project)) {
        rows.push(workspace_row(project, &format!("📁 {project}"), ""));
    }
    rows
}

/// A typed workspace name: printable characters only, trimmed, and without the icon of a picker
/// row (the rows of the picker are `name\tlabel`, but a user can paste the label).
pub(super) fn clean_name(name: &str) -> String {
    let mut clean = py_strip(&printable(name)).to_string();
    for icon in ["● ", "📁 ", "⚡ "] {
        if let Some(rest) = clean.strip_prefix(icon) {
            clean = rest.to_string();
        }
    }
    clean
}

/// Run fzf on `rows` with these options; its answer and exit status.
pub(super) fn run_fzf(options: &[&str], extra: &[String], rows: &[String]) -> Result<Output> {
    let mut argv: Vec<String> = options.iter().map(|option| option.to_string()).collect();
    argv.extend(extra.iter().cloned());
    spawn(&Spawn {
        argv,
        stdin: Some(rows.join("\n")),
        ..Spawn::default()
    })
}

pub(super) fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

/// First-run setup: ask a new user for their projects folder once, before their first workspace.
pub fn onboard() -> Result<()> {
    onboard_in(&Setting::current())
}

pub(super) fn onboard_in(setting: &Setting) -> Result<()> {
    if setting.loc.needs_onboarding() && setting.terminal() {
        // A setup that was cancelled saved nothing, and the next `work` asks again.
        onboarding::setup_with(
            &setting.loc,
            &[],
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
            true,
        )?;
    }
    Ok(())
}

/// Start a server with the workspaces of the last save. Restoring needs a running server, so a
/// placeholder session holds it until the saved sessions are there.
pub(super) fn start_with_saved_workspaces(state: &Path, tmux: &Tmux) -> Result<()> {
    let bootstrap = tmux.run(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_id}",
        "-s",
        BOOTSTRAP,
        "/bin/sleep 2147483647",
    ])?;
    snapshot::initialize_in(state, tmux)?;
    let restored = tmux
        .fields("list-panes", &["session_name"], &["-a"])?
        .iter()
        .any(|pane| pane.get("session_name") != BOOTSTRAP);
    if restored {
        tmux.run(&["kill-session", "-t", &bootstrap])?;
    }
    Ok(())
}

/// Go to a workspace: switch the client inside tmux, else attach the terminal to it.
pub(super) fn connect_session(setting: &Setting, tmux: &Tmux, name: &str) -> Result<()> {
    let target = format!("={name}");
    if setting.inside_tmux {
        tmux.quiet(&["switch-client", "-t", &target]);
        Ok(())
    } else {
        Err(exec(&tmux.argv(&["attach-session", "-t", &target])))
    }
}

/// `work [NAME]`: open the workspace `name`, or choose one.
pub fn open_workspace(tmux: &Tmux, name: Option<&str>) -> Result<()> {
    open_in(&Setting::current(), tmux, name)
}

pub(super) fn open_in(setting: &Setting, tmux: &Tmux, name: Option<&str>) -> Result<()> {
    onboard_in(setting)?;
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        return choose_in(setting, tmux);
    };

    let state = setting.state();
    match tmux.run(&["has-session"]) {
        Ok(_) => snapshot::initialize_in(&state, tmux)?,
        Err(Error::Command { .. }) => {
            if state.join("latest.json").is_file() {
                start_with_saved_workspaces(&state, tmux)?;
            }
        }
        Err(other) => return Err(other),
    }

    let clean = clean_name(name);
    if session_names(tmux).contains(&clean) {
        return connect_session(setting, tmux, &clean);
    }

    let (target_dir, session_name) = if let Some(found) = zoxide_match(setting, &clean) {
        let name = folder_name(&found);
        (found, name)
    } else if clean == "." {
        let here = resolve(&std::env::current_dir()?);
        let name = folder_name(&here);
        (here, name)
    } else if py_path(&setting.loc, &clean).is_dir() {
        let folder = resolve(&py_path(&setting.loc, &clean));
        let name = folder_name(&folder);
        (folder, name)
    } else {
        (resolve(&setting.projects().join(&clean)), clean)
    };
    std::fs::create_dir_all(&target_dir).map_err(|e| cannot_write(&target_dir, e))?;
    // The folder is known to zoxide from now on. If it cannot be told, the workspace opens anyway.
    let _ = spawn(&Spawn {
        argv: strings(&["zoxide", "add", &target_dir.to_string_lossy()]),
        ..Spawn::default()
    });

    if !session_names(tmux).contains(&session_name) {
        // -n: with automatic-rename off, tmux would name the tab after the default command.
        tmux.run(&[
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-n",
            &session_name,
            "-c",
            &target_dir.to_string_lossy(),
        ])?;
    }
    connect_session(setting, tmux, &session_name)
}

pub(super) fn folder_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// The folder zoxide finds for `query`, when it finds one. Neither a missing zoxide nor an unknown
/// name is an error.
pub(super) fn zoxide_match(setting: &Setting, query: &str) -> Option<PathBuf> {
    if query == "." {
        return None;
    }
    // A name like `--list` is a name here, as it is for tmux, and not one of zoxide's options.
    let mut argv = strings(&["zoxide", "query"]);
    if query.starts_with('-') {
        argv.push("--".to_string());
    }
    argv.push(query.to_string());
    let answer = spawn(&Spawn {
        argv,
        ..Spawn::default()
    })
    .ok()?;
    let line = py_strip(&answer.stdout);
    if answer.status != 0 || line.is_empty() {
        return None;
    }
    let folder = resolve(&py_path(&setting.loc, line));
    folder.is_dir().then_some(folder)
}

/// `work` without a name: choose a workspace, or type the name of a new one.
pub fn choose_or_create(tmux: &Tmux) -> Result<()> {
    choose_in(&Setting::current(), tmux)
}

/// The tmux format of the sessions the picker reads: name, last attach, and whether the server
/// has been set up.
pub(super) fn session_format() -> String {
    [
        "#{session_name}",
        "#{session_last_attached}",
        "#{@workspace_initialized}",
    ]
    .join(&SEP.to_string())
}

pub(super) fn choose_in(setting: &Setting, tmux: &Tmux) -> Result<()> {
    let format = session_format();
    let list = || tmux.quiet(&["list-sessions", "-F", &format]);
    let mut sessions = list();
    let state = setting.state();
    if sessions.is_empty() && state.join("latest.json").is_file() {
        start_with_saved_workspaces(&state, tmux)?;
        sessions = list();
    } else if !sessions.is_empty()
        && py_splitlines(&sessions)
            .iter()
            .any(|line| line.rsplit(SEP).next() != Some("1"))
    {
        snapshot::initialize_in(&state, tmux)?;
        sessions = list();
    }

    let live = recent_sessions(&sessions)?;
    let root = setting.projects();
    let projects = if root.is_dir() {
        project_names(&root)
    } else {
        Vec::new()
    };
    let known = spawn(&Spawn {
        argv: strings(&["zoxide", "query", "-l"]),
        ..Spawn::default()
    })
    .map(|answer| answer.stdout)
    .unwrap_or_default();
    let zoxide = zoxide_folders(&setting.loc, &known, &live, &projects, &root);
    let rows = workspace_rows(&live, &zoxide, &projects, &agent_status(tmux).by_workspace);

    if which("fzf").is_none() || !setting.terminal() {
        if which("sesh").is_some() {
            return Err(exec(&strings(&["sesh", "picker"])));
        }
        if !setting.terminal() {
            return fail("Install fzf to pick a workspace, or run: work NAME");
        }
        let target = numbered_pick(&rows, &mut io::stdin().lock(), &mut io::stdout().lock())?;
        if !target.is_empty() {
            open_in(setting, tmux, Some(&target))?;
        }
        return Ok(());
    }

    let chosen = run_fzf(
        &WORKSPACE_FZF,
        &strings(&[
            "--header=Recent workspaces first | Select or type a NEW name | Esc: cancel",
            "--prompt=Workspace > ",
            "--height=50%",
            "--layout=reverse",
            "--border=rounded",
            "--info=inline",
        ]),
        &rows,
    )?;
    // 1: no row matched, so the typed query names a new workspace.
    if !matches!(chosen.status, 0 | 1) {
        return Ok(());
    }
    let target = picked_name(&chosen.stdout);
    if !target.is_empty() {
        open_in(setting, tmux, Some(&target))?;
    }
    Ok(())
}

/// `kill-workspace [NAME]`: close a workspace, after asking on a terminal.
pub fn delete_workspace(tmux: &Tmux, name: Option<&str>) -> Result<()> {
    delete_with(
        &Setting::current(),
        tmux,
        name,
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
    )
}

pub(super) fn delete_with(
    setting: &Setting,
    tmux: &Tmux,
    name: Option<&str>,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<()> {
    let live = session_names(tmux);
    if live.is_empty() {
        message_via(tmux, "No active workspaces to delete.");
        return Ok(());
    }

    let target = match name.filter(|name| !name.is_empty()) {
        Some(name) => py_strip(name).to_string(),
        None if setting.stdin_tty && which("fzf").is_some() => {
            let chosen = run_fzf(
                &["fzf"],
                &strings(&[
                    "--header=Select workspace to close/delete | Esc: cancel",
                    "--prompt=Delete Workspace > ",
                    "--height=40%",
                    "--layout=reverse",
                    "--border=rounded",
                ]),
                &live,
            )?;
            let picked = py_strip(&chosen.stdout);
            if chosen.status != 0 || picked.is_empty() {
                return Ok(());
            }
            picked.to_string()
        }
        None => {
            writeln!(output, "Active workspaces:\n")?;
            for (number, item) in live.iter().enumerate() {
                writeln!(output, "  {}. {item}", number + 1)?;
            }
            let answer = ask(
                "\nNumber or name of workspace to delete (Enter cancels): ",
                input,
                output,
            )?;
            let answer = py_strip(&answer);
            if answer.is_empty() {
                return Ok(());
            }
            position(answer, live.len()).map_or_else(|| answer.to_string(), |n| live[n - 1].clone())
        }
    };

    if !live.contains(&target) {
        message_via(tmux, &format!("Workspace '{target}' not found."));
        return Ok(());
    }

    if setting.stdin_tty {
        let prompt = format!("Are you sure you want to close workspace '{target}'? (y/N): ");
        match ask(&prompt, input, output) {
            Err(Error::Eof) => return Ok(()),
            Err(other) => return Err(other),
            Ok(answer) => {
                let answer = py_strip(&answer).to_lowercase();
                if answer != "y" && answer != "yes" {
                    writeln!(output, "Cancelled.")?;
                    return Ok(());
                }
            }
        }
    }

    // Closing the workspace this client shows would end the client's session: move to another first.
    if setting.inside_tmux && tmux.quiet(&["display-message", "-p", "#{session_name}"]) == target {
        match live.iter().find(|session| **session != target) {
            Some(other) => {
                tmux.quiet(&["switch-client", "-t", &format!("={other}")]);
            }
            None => {
                tmux.run(&[
                    "new-session",
                    "-d",
                    "-s",
                    "main",
                    "-n",
                    "main",
                    "-c",
                    &setting.projects().to_string_lossy(),
                ])?;
                tmux.quiet(&["switch-client", "-t", "=main"]);
            }
        }
    }
    tmux.run(&["kill-session", "-t", &format!("={target}")])?;
    message_via(tmux, &format!("Workspace '{target}' closed."));
    Ok(())
}
