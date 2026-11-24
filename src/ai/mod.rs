//! On-demand workspace tools. Models select evidence; only users select actions.

pub mod core;
pub mod history;
pub mod panes;
pub mod suggest;

use crate::ai::core::{
    AiError, AiHost, AiResult, TmuxAiHost, Transport, UreqTransport, clean, collect_panes, judge,
};
use crate::error::{Error, Result, fail};
use crate::process::{self, Spawn};
use crate::pyjson::{Style, dumps};
use crate::text;
use crate::tmux::Tmux;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{IsTerminal, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const TOOLS: &[(&str, &str)] = &[
    (
        "find",
        "Jev live finder: type a task for fast live/parked pane reranking",
    ),
    (
        "attention",
        "Show agents that need input, are blocked, or need review",
    ),
    (
        "search-history",
        "Search retained history of opted-in panes",
    ),
    ("suggest", "Find an existing shortcut or installed skill"),
    (
        "name-tab",
        "Suggest a short tab name and confirm before renaming",
    ),
    ("share", "Manage per-pane terminal-content permission"),
    (
        "check",
        "Check OpenRouter credentials and native Jev judgments",
    ),
];

/// Interactive terminal I/O boundary so action confirmation and menus are tested directly.
pub trait AiUi {
    fn stdin_is_tty(&self) -> bool;
    fn stdout_is_tty(&self) -> bool;
    fn has_fzf(&self) -> bool;
    fn prompt(&self, prompt: &str) -> Result<String>;
    fn println(&self, line: &str);
    fn eprintln(&self, line: &str);
}

pub struct StdUi;

impl AiUi for StdUi {
    fn stdin_is_tty(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn stdout_is_tty(&self) -> bool {
        std::io::stdout().is_terminal()
    }

    fn has_fzf(&self) -> bool {
        process::which("fzf").is_some()
    }

    fn prompt(&self, prompt: &str) -> Result<String> {
        let mut out = std::io::stdout();
        write!(out, "{prompt}")?;
        out.flush()?;
        let mut line = String::new();
        let n = std::io::stdin().read_line(&mut line)?;
        if n == 0 {
            return Err(Error::Eof);
        }
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    }

    fn println(&self, line: &str) {
        let _ = writeln!(std::io::stdout(), "{line}");
    }

    fn eprintln(&self, line: &str) {
        let _ = writeln!(std::io::stderr(), "{line}");
    }
}

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn create(prefix: &str) -> Result<Self> {
        let base = std::env::temp_dir();
        for _ in 0..32 {
            let path = base.join(format!("{prefix}{}", text::uuid4_hex()));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(Error::Io(e)),
            }
        }
        fail("Could not create temporary directory")
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn get_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub fn choose(items: &[Value], title: &str, ui: &dyn AiUi) -> Result<Option<Value>> {
    if items.is_empty() {
        return Ok(None);
    }
    if !ui.has_fzf() {
        for (i, item) in items.iter().enumerate() {
            ui.println(&format!("{}. {}", i + 1, clean(get_str(item, "title"))));
        }
        let selected = ui.prompt("Number (Enter cancels): ")?;
        let trimmed = selected.trim();
        if let Ok(n) = trimmed.parse::<usize>()
            && n >= 1
            && n <= items.len()
        {
            return Ok(Some(items[n - 1].clone()));
        }
        return Ok(None);
    }
    let dir = TempDirGuard::create("tmux-ai-")?;
    let mut lines = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        std::fs::write(
            dir.path().join(format!("{i}.txt")),
            clean(get_str(item, "detail")),
        )?;
        let rel = get_str(item, "relevance");
        let t = clean(get_str(item, "title")).replace(['\n', '\t'], " ");
        lines.push(format!("{i}\t{rel} {t}"));
    }
    let preview = format!(
        "cat -- {}/{{1}}.txt",
        text::shlex_quote(&dir.path().to_string_lossy())
    );
    let out = process::spawn(&Spawn {
        argv: [
            "fzf",
            "--delimiter=\t",
            "--with-nth=2..",
            "--layout=reverse",
            "--height=100%",
            &format!("--header={}", clean(title)),
            "--prompt=Workspace AI > ",
            &format!("--preview={preview}"),
            "--preview-window=right:60%:wrap",
        ]
        .map(String::from)
        .to_vec(),
        stdin: Some(lines.join("\n")),
        ..Spawn::default()
    })?;
    if out.status != 0 || out.stdout.trim().is_empty() {
        return Ok(None);
    }
    let idx = out.stdout.split('\t').next().unwrap_or_default();
    if let Ok(n) = idx.parse::<usize>()
        && n < items.len()
    {
        Ok(Some(items[n].clone()))
    } else {
        Ok(None)
    }
}

fn open_finder_directory(directory: &str) -> Result<OwnedFd> {
    let path = Path::new(directory);
    let canonical = path
        .canonicalize()
        .map_err(|_| Error::Msg("Invalid finder directory.".into()))?;
    let name_ok = path
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with("tmux-jev-find-"));
    if !path.is_absolute() || canonical != path || !name_ok {
        return fail("Invalid finder directory.");
    }
    let c_path = CString::new(directory.as_bytes())
        .map_err(|_| Error::Msg("Invalid finder directory.".into()))?;
    // SAFETY: `c_path` is NUL-terminated; returned fd is immediately wrapped in `OwnedFd`.
    let raw = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        return fail("Invalid finder directory.");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `fstat` fills local stat struct.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd.as_raw_fd(), &mut st) != 0 {
            return fail("Finder directory must be private and owned by this user.");
        }
        if st.st_uid != libc::getuid() || (st.st_mode & 0o7777) != 0o700 {
            return fail("Finder directory must be private and owned by this user.");
        }
    }
    Ok(fd)
}

fn finder_line(record_id: &str, item: &Value) -> String {
    let title = clean(get_str(item, "title")).replace(['\n', '\t'], " ");
    let rel = item
        .get("relevance")
        .and_then(Value::as_str)
        .unwrap_or("local");
    format!("{record_id}\t{rel} {title}")
}

fn write_finder_record(dir_fd: RawFd, record_id: &str, item: &Value) -> Result<()> {
    let fname = format!("{record_id}.json");
    let c_name = CString::new(fname.as_bytes()).map_err(|_| Error::Msg("invalid name".into()))?;
    // SAFETY: `openat` creates an exclusive regular file; wrapped immediately in `OwnedFd`.
    let raw = unsafe {
        libc::openat(
            dir_fd,
            c_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o600 as libc::c_uint,
        )
    };
    if raw < 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut file = File::from(fd);
    let data = serde_json::to_vec(item).map_err(|e| Error::Json(e.to_string()))?;
    file.write_all(&data)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o400))?;
    Ok(())
}

fn finder_rows(directory: &str, items: &[Value], notice: &str) -> Result<String> {
    // --with-nth=2.. applies to fzf header lines as well as selectable rows.
    let mut lines = vec![format!("\t{}", clean(notice).replace(['\n', '\t'], " "))];
    let dir_fd = open_finder_directory(directory)?;
    for item in items {
        // Immutable IDs bind each displayed row to its exact source snapshot.
        let record_id = text::uuid4_hex();
        write_finder_record(dir_fd.as_raw_fd(), &record_id, item)?;
        lines.push(finder_line(&record_id, item));
    }
    Ok(format!("{}\n", lines.join("\n")))
}

fn finder_record(directory: &str, record_id: &str) -> Result<Value> {
    if record_id.len() != 32
        || !record_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return fail("Invalid finder record ID.");
    }
    let dir_fd = open_finder_directory(directory)?;
    let fname = format!("{record_id}.json");
    let c_name = CString::new(fname.as_bytes()).map_err(|_| Error::Msg("invalid name".into()))?;
    // SAFETY: `openat` opens an existing file without following symlinks.
    let raw = unsafe {
        libc::openat(
            dir_fd.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        return fail("Invalid finder record file.");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `fstat` verifies regular file, ownership, no hard links, and exact mode 0400.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd.as_raw_fd(), &mut st) != 0
            || (st.st_mode & libc::S_IFMT) != libc::S_IFREG
            || st.st_uid != libc::getuid()
            || st.st_nlink != 1
            || (st.st_mode & 0o7777) != 0o400
        {
            return fail("Invalid finder record file.");
        }
    }
    let mut file = File::from(fd);
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    let item: Value = serde_json::from_str(&buf).map_err(|e| Error::Json(e.to_string()))?;
    let Some(pane) = item.get("pane").and_then(Value::as_object) else {
        return fail("Invalid finder pane record.");
    };
    let expected_id = format!(
        "pane:{}",
        pane.get("pane_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
    );
    if item.get("id").and_then(Value::as_str) != Some(expected_id.as_str()) {
        return fail("Finder pane identity does not match its record.");
    }
    Ok(item)
}

fn finder_catalog(host: &dyn AiHost) -> Result<Vec<Value>> {
    Ok(collect_panes(host, false, None)?
        .iter()
        .map(panes::pane_record)
        .collect())
}

fn finder_private(host: &dyn AiHost, transport: &dyn Transport, argv: &[String]) {
    let parse_two = |rest: &[String]| -> Option<(String, String)> {
        let mut vals = Vec::new();
        let mut after_dd = false;
        for arg in rest {
            if !after_dd && arg == "--" {
                after_dd = true;
                continue;
            }
            vals.push(arg.clone());
        }
        if vals.len() == 2 {
            Some((vals.remove(0), vals.remove(0)))
        } else {
            None
        }
    };
    let res: Result<()> = (|| {
        let Some((directory, value)) = parse_two(&argv[1..]) else {
            return fail("Invalid finder arguments.");
        };
        if argv[0] == "--_find-preview" {
            let rec = finder_record(&directory, &value)?;
            let _ = writeln!(std::io::stdout(), "{}", clean(get_str(&rec, "detail")));
            return Ok(());
        }
        // Debounce before collecting evidence or sending anything to Jev.
        std::thread::sleep(Duration::from_millis(250));
        let _ = open_finder_directory(&directory)?;
        let query = value.trim();
        let (items, notice) = if !query.is_empty() {
            let (items, n) = run_feature(host, transport, "find", query)?;
            (items, format!("Query: {query} | {n}"))
        } else {
            (
                finder_catalog(host)?,
                "Local live/parked catalog; empty query sends nothing to Jev.".into(),
            )
        };
        let rows = finder_rows(&directory, &items, &notice)?;
        let _ = write!(std::io::stdout(), "{rows}");
        let _ = std::io::stdout().flush();
        Ok(())
    })();
    if res.is_err() {
        let _ = writeln!(
            std::io::stdout(),
            "Finder refresh unavailable; edit the query to retry."
        );
        let _ = std::io::stdout().flush();
    }
}

fn choose_live_find(
    host: &dyn AiHost,
    transport: &dyn Transport,
    query: &str,
) -> Result<(Option<Value>, String)> {
    let temp = TempDirGuard::create("tmux-jev-find-")?;
    let canonical = temp.path().canonicalize()?;
    std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(0o700))?;
    let directory = canonical.to_string_lossy().into_owned();
    let lines = finder_rows(
        &directory,
        &finder_catalog(host)?,
        "Local live/parked catalog | Metadata only until you type; excerpts require pane opt-in.",
    )?;
    let mut extra_env: Vec<(String, String)> = Vec::new();
    if let Ok(key) = transport.api_key() {
        extra_env.push(("OPENROUTER_API_KEY".into(), key));
    }
    let exe = host.self_exe();
    let reload_cmd = format!(
        "exec {} {{q}}",
        text::shlex_join(&[&exe, "ai", "--_find-reload", &directory, "--"])
    );
    let preview_cmd = format!(
        "exec {} {{1}}",
        text::shlex_join(&[&exe, "ai", "--_find-preview", &directory, "--"])
    );
    let mut argv = [
        "fzf",
        "--delimiter=\t",
        "--with-nth=2..",
        "--layout=reverse",
        "--height=100%",
        "--disabled",
        "--no-sort",
        "--no-multi",
        "--print-query",
        "--print0",
        "--header-lines=1",
        &format!("--query={query}"),
        "--prompt=Jev find > ",
        "--header=Type: fast semantic rerank after 250ms | Enter: inspect | Esc: cancel",
        &format!("--preview={preview_cmd}"),
        "--preview-window=right:60%:wrap",
        "--with-shell=/bin/sh -c",
        "--bind=esc:abort",
        &format!("--bind=change:reload-sync:{reload_cmd}"),
    ]
    .map(String::from)
    .to_vec();
    if !query.is_empty() {
        argv.push(format!("--bind=load:unbind(load)+reload-sync:{reload_cmd}"));
    }
    let out = process::spawn(&Spawn {
        argv,
        stdin: Some(lines),
        env: extra_env,
        ..Spawn::default()
    })?;
    if out.status != 0 {
        return Ok((None, query.to_string()));
    }
    let fields: Vec<&str> = out.stdout.split('\0').collect();
    if fields.len() != 3 || !fields[2].is_empty() {
        return Ok((None, query.to_string()));
    }
    let (new_query, line) = (fields[0].to_string(), fields[1]);
    let record_id = line.split('\t').next().unwrap_or_default();
    let selected = finder_record(&directory, record_id)?;
    if line != finder_line(record_id, &selected) {
        return fail("Finder selection changed; run the search again.");
    }
    Ok((Some(selected), new_query))
}

pub fn navigate(host: &dyn AiHost, pane: &Value, ui: &dyn AiUi) -> Result<bool> {
    let target_id = get_str(pane, "pane_id");
    let panes = collect_panes(host, false, None)?;
    let current = panes
        .into_iter()
        .find(|p| get_str(p, "pane_id") == target_id);
    let Some(current) = current else {
        ui.println("Pane identity or working context changed. Run the search again.");
        return Ok(false);
    };
    if get_str(&current, "fingerprint") != get_str(pane, "fingerprint") {
        ui.println("Pane identity or working context changed. Run the search again.");
        return Ok(false);
    }
    if current.get("parked").and_then(Value::as_bool) == Some(true) {
        let Some(target) = host.current_pane() else {
            ui.println("Open this tool from a normal workspace to bring back a parked pane.");
            return Ok(false);
        };
        let all = collect_panes(host, false, None)?;
        let destination = all.into_iter().find(|p| get_str(p, "pane_id") == target);
        let Some(destination) = destination else {
            ui.println("Open this tool from a normal workspace to bring back a parked pane.");
            return Ok(false);
        };
        if destination.get("parked").and_then(Value::as_bool) == Some(true) {
            ui.println("Open this tool from a normal workspace to bring back a parked pane.");
            return Ok(false);
        }
        let ans = ui.prompt("Bring this parked pane into the current window? [y/N] ")?;
        if ans.trim().to_lowercase() != "y" {
            return Ok(false);
        }
        let latest: HashMap<String, Value> = collect_panes(host, false, None)?
            .into_iter()
            .map(|p| (get_str(&p, "pane_id").to_string(), p))
            .collect();
        for before in [&current, &destination] {
            let pid = get_str(before, "pane_id");
            let unchanged = latest.get(pid).is_some_and(|after| {
                ["fingerprint", "window_id", "session_name", "parked"]
                    .iter()
                    .all(|k| after.get(*k) == before.get(*k))
            });
            if !unchanged {
                ui.println("Source or destination changed during confirmation; nothing moved.");
                return Ok(false);
            }
        }
        // Use the exact selected ID, never the most recently parked pane.
        let src = get_str(&current, "pane_id");
        host.run_tmux(&["join-pane", "-h", "-s", src, "-t", &target, "-l", "40%"])?;
        host.run_tmux(&["set-option", "-pu", "-t", src, "@workspace_parked"])?;
        host.run_tmux(&["set-option", "-pu", "-t", src, "@workspace_home"])?;
    } else {
        let session = format!("={}", get_str(&current, "session_name"));
        host.run_tmux(&["switch-client", "-t", &session])?;
        host.run_tmux(&["select-window", "-t", get_str(&current, "window_id")])?;
        host.run_tmux(&["select-pane", "-t", get_str(&current, "pane_id")])?;
    }
    Ok(true)
}

fn window_fingerprints(host: &dyn AiHost, window_id: &str) -> Result<Map<String, Value>> {
    let mut map = Map::new();
    for p in collect_panes(host, false, None)? {
        if get_str(&p, "window_id") == window_id {
            map.insert(
                get_str(&p, "pane_id").to_string(),
                Value::String(get_str(&p, "fingerprint").to_string()),
            );
        }
    }
    Ok(map)
}

pub fn rename_tab(host: &dyn AiHost, proposal: &Value, ui: &dyn AiUi) -> Result<bool> {
    let window_id = get_str(proposal, "window_id");
    let previous_name = get_str(proposal, "previous_name");
    let new_name = get_str(proposal, "name");
    let expected_panes = proposal
        .get("panes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let windows = host.list_windows(&["window_id", "window_name"])?;
    let current = windows
        .into_iter()
        .find(|w| w.get("window_id") == window_id);
    let panes = window_fingerprints(host, window_id)?;
    let Some(current) = current else {
        ui.println("Window name or task context changed. Request a new suggestion.");
        return Ok(false);
    };
    if current.get("window_name") != previous_name || panes != expected_panes {
        ui.println("Window name or task context changed. Request a new suggestion.");
        return Ok(false);
    }
    let prompt = format!(
        "Rename {} to {}? [y/N] ",
        crate::error::py_repr_str(&clean(current.get("window_name"))),
        crate::error::py_repr_str(new_name)
    );
    if ui.prompt(&prompt)?.trim().to_lowercase() != "y" {
        return Ok(false);
    }
    // Revalidate task identity as well as its display name after confirmation.
    let latest = host.list_windows(&["window_id", "window_name"])?;
    let latest_panes = window_fingerprints(host, window_id)?;
    let still_same = latest.iter().any(|w| {
        w.get("window_id") == current.get("window_id")
            && w.get("window_name") == current.get("window_name")
    });
    if !still_same || latest_panes != expected_panes {
        ui.println("Window changed during confirmation; nothing renamed.");
        return Ok(false);
    }
    host.run_tmux(&["rename-window", "-t", window_id, new_name])?;
    ui.println(&format!("Tab renamed: {new_name}"));
    Ok(true)
}

pub fn sharing(
    host: &dyn AiHost,
    pane_id: Option<&str>,
    allow: bool,
    off: bool,
    ui: &dyn AiUi,
) -> Result<()> {
    let panes = collect_panes(host, false, None)?;
    let chosen_id: String = match pane_id {
        Some(id) => id.to_string(),
        None => {
            let records: Vec<Value> = panes
                .iter()
                .map(|p| {
                    let shared = p.get("shared").and_then(Value::as_bool) == Some(true);
                    json!({
                        "id": get_str(p, "pane_id"),
                        "title": format!(
                            "{} | {}:{}.{} | {}",
                            if shared { "SHARED" } else { "private" },
                            get_str(p, "session_name"),
                            get_str(p, "window_index"),
                            get_str(p, "pane_index"),
                            get_str(p, "label"),
                        ),
                        "detail": format!(
                            "Current permission: {}\nPane: {}",
                            if shared { "shared" } else { "private" },
                            get_str(p, "pane_id")
                        ),
                        "pane": p,
                    })
                })
                .collect();
            let Some(selected) = choose(&records, "Choose pane to change content permission", ui)?
            else {
                return Ok(());
            };
            get_str(&selected, "id").to_string()
        }
    };
    let Some(pane) = panes.iter().find(|p| get_str(p, "pane_id") == chosen_id) else {
        return fail("Select an existing pane ID from this tmux server.");
    };
    let shared = pane.get("shared").and_then(Value::as_bool) == Some(true);
    if off || (shared && !allow) {
        host.run_tmux(&["set-option", "-pu", "-t", &chosen_id, "@workspace_ai_share"])?;
        ui.println(&format!("Content sharing disabled for {chosen_id}"));
        return Ok(());
    }
    if get_str(pane, "workspace_id").is_empty() {
        return fail("Pane has no stable workspace identity; save the workspace before sharing.");
    }
    if !allow {
        if !ui.stdin_is_tty() {
            return fail("Sharing requires an interactive confirmation or explicit --allow.");
        }
        ui.println(
            "This permits recent terminal output AND retained snapshot history for this pane to be \
             sent to OpenRouter and its model provider when you run AI tools. Output may contain \
             secrets or client data. Redaction is best-effort, not a guarantee. Permission lasts \
             until revoked or tmux restarts. Already-sent requests cannot be recalled.",
        );
        if ui.prompt("Type SHARE to allow: ")?.trim() != "SHARE" {
            ui.println("No permission changed.");
            return Ok(());
        }
    }
    let current = collect_panes(host, false, None)?
        .into_iter()
        .find(|p| get_str(p, "pane_id") == chosen_id);
    if !current
        .as_ref()
        .is_some_and(|c| get_str(c, "fingerprint") == get_str(pane, "fingerprint"))
    {
        return fail("Pane changed during confirmation; choose it again.");
    }
    host.run_tmux(&[
        "set-option",
        "-p",
        "-t",
        &chosen_id,
        "@workspace_ai_share",
        "1",
    ])?;
    ui.println(&format!(
        "Content sharing enabled for {chosen_id}; revoke with: tmux-workspace ai share {chosen_id} --off"
    ));
    Ok(())
}

pub fn run_feature(
    host: &dyn AiHost,
    transport: &dyn Transport,
    command: &str,
    query: &str,
) -> AiResult<(Vec<Value>, String)> {
    match command {
        "find" => panes::find(host, transport, query),
        "attention" => panes::attention(host, transport),
        "name-tab" => panes::name_tab(host, transport, query),
        "search-history" => history::search(host, transport, query),
        _ => Ok(suggest::suggest(host, transport, query)),
    }
}

struct ParsedArgs {
    command: Option<String>,
    query: Vec<String>,
    as_json: bool,
    allow: bool,
    off: bool,
}

fn parse_args(argv: &[String]) -> std::result::Result<ParsedArgs, String> {
    let mut command: Option<String> = None;
    let mut query: Vec<String> = Vec::new();
    let mut as_json = false;
    let mut allow = false;
    let mut off = false;
    let mut after_dd = false;

    for arg in argv {
        if !after_dd && arg == "--" {
            after_dd = true;
            continue;
        }
        if !after_dd {
            match arg.as_str() {
                "--json" => {
                    as_json = true;
                    continue;
                }
                "--allow" => {
                    allow = true;
                    continue;
                }
                "--off" => {
                    off = true;
                    continue;
                }
                "-h" | "--help" => {
                    return Err("Usage: tmux-workspace ai [find|attention|search-history|suggest|name-tab|share|check|home] [query...] [--json] [--allow|--off]".into());
                }
                s if s.starts_with('-') => {
                    return Err(format!("unrecognized argument: {s}"));
                }
                _ => {}
            }
        }
        if command.is_none() {
            let valid = TOOLS.iter().any(|(name, _)| *name == arg) || arg == "home";
            if !valid {
                return Err(format!("invalid choice: {arg}"));
            }
            command = Some(arg.clone());
        } else {
            query.push(arg.clone());
        }
    }
    if allow && off {
        return Err("--allow and --off are mutually exclusive".into());
    }
    Ok(ParsedArgs {
        command,
        query,
        as_json,
        allow,
        off,
    })
}

pub fn main_with(
    host: &dyn AiHost,
    transport: &dyn Transport,
    ui: &dyn AiUi,
    argv: &[String],
) -> Result<i32> {
    if argv
        .first()
        .is_some_and(|a| a == "--_find-reload" || a == "--_find-preview")
    {
        finder_private(host, transport, argv);
        return Ok(0);
    }
    let parsed = match parse_args(argv) {
        Ok(p) => p,
        Err(msg) => {
            ui.eprintln(&format!("tmux-workspace ai: {msg}"));
            return Ok(2);
        }
    };
    let explicit_command = parsed.command.is_some();
    let home_str = host.share_root().to_string_lossy().into_owned();
    let command = match parsed.command {
        Some(cmd) => cmd,
        None => {
            if parsed.as_json || !ui.stdin_is_tty() {
                let mut tools = Map::new();
                for &(name, desc) in TOOLS {
                    tools.insert(name.into(), Value::String(desc.into()));
                }
                let out = json!({
                    "home": home_str,
                    "tools": tools,
                    "usage": "tmux-workspace ai <tool> [query] [--json]",
                });
                ui.println(&dumps(&out, Style::Indent2, false));
                return Ok(0);
            }
            let menu_items: Vec<Value> = TOOLS
                .iter()
                .map(|&(name, desc)| {
                    json!({
                        "id": name,
                        "title": format!("{name} | {desc}"),
                        "detail": format!(
                            "{desc}\n\nMetadata is sent for semantic search. Terminal content \
                             requires per-pane permission. No model output is executed."
                        ),
                    })
                })
                .collect();
            let Some(selected) = choose(
                &menu_items,
                &format!(
                    "Jev tools | Find reranks live as you type | Esc closes | Config: {home_str}"
                ),
                ui,
            )?
            else {
                return Ok(0);
            };
            get_str(&selected, "id").to_string()
        }
    };

    if (parsed.allow || parsed.off) && command != "share" {
        ui.eprintln("tmux-workspace ai: --allow/--off apply only to share");
        return Ok(2);
    }
    if command == "home" {
        ui.println(&home_str);
        return Ok(0);
    }
    if command == "share" {
        if parsed.query.len() > 1 {
            ui.eprintln("tmux-workspace ai: share takes one pane ID");
            return Ok(2);
        }
        sharing(
            host,
            parsed.query.first().map(String::as_str),
            parsed.allow,
            parsed.off,
            ui,
        )?;
        return Ok(0);
    }
    if command == "check" {
        let mut questions = Map::new();
        questions.insert(
            "check".into(),
            json!({
                "type": "noul",
                "instructions": "Does `text` say the workspace helper is ready?",
                "criteria": {
                    "true": "Text says it is ready.",
                    "false": "Text does not say it is ready.",
                },
            }),
        );
        let check_res: AiResult<String> = (|| {
            let answers = judge(
                transport,
                &json!({"text": "The workspace helper is ready."}),
                &questions,
                None,
            )?;
            let prob = answers
                .get("check")
                .and_then(|v| v.get("noul"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if prob <= 0.5 {
                return Err(AiError::Other(
                    "Native Noul output was valid but the readiness probability was not above 0.5."
                        .into(),
                ));
            }
            transport.model()
        })();
        match check_res {
            Err(err) => {
                if !explicit_command {
                    ui.println(&err.to_string());
                    let _ = ui.prompt("Press Enter to close.");
                }
                return Err(err.into());
            }
            Ok(model) => {
                ui.println(&format!(
                    "OpenRouter {model}: ready (native Jev Noul verified)."
                ));
                if !explicit_command {
                    let _ = ui.prompt("Press Enter to close.");
                }
                return Ok(0);
            }
        }
    }

    let mut query = parsed.query.join(" ").trim().to_string();
    let interactive = ui.stdin_is_tty() && ui.stdout_is_tty() && !parsed.as_json;
    let live_find = command == "find" && interactive && ui.has_fzf();
    let mut items = Vec::new();
    let mut notice = String::new();
    if !live_find {
        if command == "name-tab" && query.is_empty() && ui.stdin_is_tty() && !parsed.as_json {
            query = ui
                .prompt("Task hint (optional; Enter uses window context): ")?
                .trim()
                .to_string();
        }
        if !matches!(command.as_str(), "attention" | "name-tab") && query.is_empty() {
            if !ui.stdin_is_tty() || parsed.as_json {
                ui.eprintln(&format!("tmux-workspace ai: {command} requires a query"));
                return Ok(2);
            }
            query = ui.prompt("What are you looking for? ")?.trim().to_string();
            if query.is_empty() {
                return Ok(0);
            }
        }
        if ui.stdin_is_tty() && !parsed.as_json {
            ui.println("Reading evidence and requesting Jev judgments...");
        }
        let (found_items, found_notice) = run_feature(host, transport, &command, &query)?;
        items = found_items;
        notice = found_notice;
        if !interactive {
            let out = json!({
                "tool": command,
                "notice": clean(&notice),
                "items": items,
            });
            ui.println(&dumps(&out, Style::Indent2, false));
            return Ok(0);
        }
        ui.println(&clean(&notice));
        if items.is_empty() {
            let _ = ui.prompt("Press Enter to close.");
            return Ok(0);
        }
    }

    loop {
        let selected = if live_find {
            let (sel, next_query) = choose_live_find(host, transport, &query)?;
            query = next_query;
            sel
        } else {
            choose(&items, &notice, ui)?
        };
        let Some(selected) = selected else {
            return Ok(0);
        };
        ui.println(&format!("\n{}\n", clean(get_str(&selected, "detail"))));
        let action = if let Some(rename) = selected.get("rename") {
            let ans = ui
                .prompt("[r] Rename tab  [Enter] Back  [q] Close: ")?
                .trim()
                .to_lowercase();
            if ans == "r" && rename_tab(host, rename, ui)? {
                return Ok(0);
            }
            ans
        } else if let Some(pane) = selected.get("pane") {
            let ans = ui
                .prompt("[g] Go to pane  [Enter] Back  [q] Close: ")?
                .trim()
                .to_lowercase();
            if ans == "g" && navigate(host, pane, ui)? {
                return Ok(0);
            }
            ans
        } else {
            ui.prompt("[Enter] Back  [q] Close: ")?
                .trim()
                .to_lowercase()
        };
        if action == "q" {
            return Ok(0);
        }
    }
}

pub fn main(tmux: &Tmux, args: &[String]) -> Result<i32> {
    let host = TmuxAiHost::new(tmux);
    let transport = UreqTransport::new();
    main_with(&host, &transport, &StdUi, args)
}
