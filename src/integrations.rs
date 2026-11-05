//! `integrations install|uninstall`: register the agent hook with Claude Code and Codex, and link
//! the omp extension, the Neovim helper and the iTerm2 profile.
//!
//! Claude's and Codex's settings are merged, never replaced: this project's own hook entries are
//! dropped and added again, and everything else in the file is kept as it was. Every file that is
//! changed or replaced is backed up first, and running it again changes nothing.

use crate::error::{Error, Result, fail};
use crate::paths::{cannot_read, cannot_write, home, resolve, share_root};
use crate::pyjson::{Style, dumps};
use crate::snapshot::create_temporary;
use crate::text::{now_ns, shlex_quote};
use serde_json::{Map, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, symlink};
use std::path::{Path, PathBuf};

/// The part of a share root that holds the shipped integration files.
const INTEGRATIONS: &str = "integrations";

/// The installed binary, which every hook command runs.
pub fn binary(home: &Path) -> PathBuf {
    home.join(".local/bin/tmux-workspace")
}

/// The folder of Codex's hooks: `$CODEX_HOME` when it is set and not empty, else `~/.codex`.
pub fn codex_dir(home: &Path) -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".codex"), PathBuf::from)
}

/// Whether a hook command is the form this project registered before the Rust rewrite: the Python
/// helper, `.../tmux-workspace/agent-hook.py`, in the share folder of a clone or in `~/.local/lib`. A
/// user's own `agent-hook.py` is not, so the project's name has to be in the path as well.
pub fn old_mark(command: &str) -> bool {
    command.contains("/agent-hook.py") && command.contains("tmux-workspace")
}

/// Whether a hook command is one this project registered: the current form runs
/// `<path>/tmux-workspace agent-hook <kind>`, and the form before the Rust rewrite ran the Python helper
/// (see [`old_mark`]). A user's own hook is neither.
pub fn ours(command: &str) -> bool {
    old_mark(command) || (command.contains("tmux-workspace") && command.contains(" agent-hook "))
}

/// Every hook command and the status line command in a Claude or Codex settings file. A file that
/// cannot be read or understood has none.
pub fn hook_commands(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(settings) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let events = settings.get("hooks").and_then(Value::as_object);
    let hooks = events
        .into_iter()
        .flat_map(|events| events.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten();
    let line = settings.get("statusLine");
    hooks
        .chain(line)
        .filter_map(|hook| hook.get("command").and_then(Value::as_str))
        .map(String::from)
        .collect()
}

/// Whether this project's hook runs from Claude Code's settings or Codex's hooks: what `install.sh
/// --update` asks before it registers the integrations again, so that an update keeps that choice.
pub fn installed_on(target: &Target) -> bool {
    [target.claude_settings(), target.codex_hooks()]
        .iter()
        .any(|path| hook_commands(path).iter().any(|command| ours(command)))
}

/// An `integrations/*-hooks.json` template with `{{BIN}}` filled in: the installed binary of this
/// `home`, quoted for the shell, so a home folder with a space still makes a working command.
pub fn render(template: &Path, home: &Path) -> Result<Value> {
    let text = fs::read_to_string(template).map_err(|e| cannot_read(template, e))?;
    let mut value: Value = serde_json::from_str(&text).map_err(|e| cannot_read(template, e))?;
    fill(&mut value, &shlex_quote(&binary(home).to_string_lossy()));
    Ok(value)
}

/// `{{BIN}}` in every string of the template. It is filled in after parsing, so no character of the
/// path needs a JSON escape.
fn fill(value: &mut Value, binary: &str) {
    match value {
        Value::String(text) => {
            if text.contains("{{BIN}}") {
                *text = text.replace("{{BIN}}", binary);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| fill(item, binary)),
        Value::Object(map) => map.values_mut().for_each(|item| fill(item, binary)),
        _ => {}
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

/// The `command` of a hook or status line object.
fn command_of(value: &Value) -> Option<&str> {
    value.get("command").and_then(Value::as_str)
}

fn is_ours(hook: &Value) -> bool {
    command_of(hook).is_some_and(ours)
}

/// A group of hooks without this project's entries, or `None` when none is left. A group that
/// is not an object is not ours to judge and stays.
fn without_ours(group: &Value) -> Option<Value> {
    let Some(object) = group.as_object() else {
        return Some(group.clone());
    };
    let kept: Vec<Value> = object
        .get("hooks")
        .and_then(Value::as_array)
        .map(|hooks| {
            hooks
                .iter()
                .filter(|hook| !is_ours(hook))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if kept.is_empty() {
        return None;
    }
    let mut group = object.clone();
    group.insert("hooks".into(), Value::Array(kept));
    Some(Value::Object(group))
}

/// Drop this project's old hook entries, keep everyone else's, then add the wanted groups. Gives the
/// merged settings, and whether they differ from `existing`. Settings that do not have the layout of
/// Claude's and Codex's hooks come back unchanged; [`check_layout`] is what reports them.
pub fn merge_hooks(existing: &Value, wanted: &Map<String, Value>) -> (Value, bool) {
    let mut data = existing.clone();
    let Some(object) = data.as_object_mut() else {
        return (data, false);
    };
    let mut hooks = match object.get("hooks") {
        None => Map::new(),
        Some(Value::Object(hooks)) => hooks.clone(),
        Some(_) => return (data, false),
    };
    for groups in hooks.values_mut() {
        if let Some(list) = groups.as_array() {
            *groups = Value::Array(list.iter().filter_map(without_ours).collect());
        }
    }
    for (event, groups) in wanted {
        let slot = hooks
            .entry(event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let (Some(list), Some(add)) = (slot.as_array_mut(), groups.as_array()) {
            list.extend(add.iter().cloned());
        }
    }
    hooks.retain(|_, groups| !matches!(groups, Value::Array(list) if list.is_empty()));
    object.insert("hooks".into(), Value::Object(hooks));
    let changed = data != *existing;
    (data, changed)
}

/// Refuse settings whose hooks are not laid out as Claude and Codex lay them out, before anything
/// is merged into them.
fn check_layout(settings: &Value, path: &Path) -> Result<()> {
    let bad = |what: String| {
        fail(format!(
            "Cannot merge hooks into {}: {what}. Fix the file and rerun the installer.",
            path.display()
        ))
    };
    let Some(object) = settings.as_object() else {
        return bad("the top level must be an object".into());
    };
    let Some(hooks) = object.get("hooks") else {
        return Ok(());
    };
    let Some(events) = hooks.as_object() else {
        return bad("\"hooks\" must be an object".into());
    };
    for (event, groups) in events {
        let Some(groups) = groups.as_array() else {
            return bad(format!("\"hooks\" / \"{event}\" must be a list"));
        };
        for group in groups {
            let Some(group) = group.as_object() else {
                return bad(format!("every group under \"{event}\" must be an object"));
            };
            match group.get("hooks") {
                None => {}
                Some(Value::Array(list)) if list.iter().all(Value::is_object) => {}
                Some(_) => {
                    return bad(format!(
                        "the \"hooks\" of a group under \"{event}\" must be a list of objects"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Where a link leads, resolved: a link whose target is missing still leads somewhere.
fn leads_to(link: &Path) -> PathBuf {
    match link.canonicalize() {
        Ok(real) => real,
        Err(_) => match fs::read_link(link) {
            Ok(target) => resolve(&link.parent().unwrap_or(Path::new(".")).join(target)),
            Err(_) => resolve(link),
        },
    }
}

/// The link the Python version made for its hook: it leads to `integrations/agent-hook.py` of some clone.
fn old_hook_link(link: &Path) -> bool {
    is_symlink(link)
        && fs::read_link(link).is_ok_and(|target| target.ends_with("integrations/agent-hook.py"))
}

/// `mkdir(parents=True, exist_ok=True, mode=0o700)`: whatever is made is private.
fn make_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| cannot_write(dir, e))
}

/// What an installation acts on. Apart from the real machine (`Target::current`) a test supplies it.
#[derive(Clone, Debug)]
pub struct Target {
    pub home: PathBuf,
    /// The folder with `integrations/`: what the links lead to and the templates come from.
    pub share: PathBuf,
    /// Where Codex keeps `hooks.json`, when not at `~/.codex`.
    pub codex_home: Option<PathBuf>,
    /// `Darwin` or `Linux`, as `uname -s` says. Only Darwin gets the iTerm2 profile.
    pub system: String,
}

impl Target {
    /// This machine, for this `home` and `system`.
    pub fn current(home: &Path, system: &str) -> Target {
        Target {
            home: home.to_path_buf(),
            share: resolve(&share_root()),
            codex_home: Some(codex_dir(home)),
            system: system.to_string(),
        }
    }

    fn codex(&self) -> PathBuf {
        self.codex_home
            .clone()
            .unwrap_or_else(|| self.home.join(".codex"))
    }

    fn source(&self, name: &str) -> PathBuf {
        self.share.join(INTEGRATIONS).join(name)
    }

    fn claude_settings(&self) -> PathBuf {
        self.home.join(".claude/settings.json")
    }

    fn codex_hooks(&self) -> PathBuf {
        self.codex().join("hooks.json")
    }

    fn omp_extension(&self) -> PathBuf {
        self.home.join(".omp/agent/extensions/tmux-workspace.ts")
    }

    fn neovim_helper(&self) -> PathBuf {
        self.home.join(".config/nvim/lua/custom/tmux_workspace.lua")
    }

    fn iterm_profile(&self) -> PathBuf {
        self.home
            .join("Library/Application Support/iTerm2/DynamicProfiles/tmux-workspace.json")
    }

    /// The link of the Python version's hook, which nothing uses any more.
    fn old_hook_link(&self) -> PathBuf {
        self.home.join(".local/lib/tmux-workspace/agent-hook.py")
    }
}

struct Installer<'a> {
    target: &'a Target,
    backups: PathBuf,
    actions: Vec<String>,
}

impl<'a> Installer<'a> {
    fn new(target: &'a Target) -> Self {
        Installer {
            target,
            backups: target
                .home
                .join(".local/state/tmux-workspace/backups")
                .join(format!("integrations-{}", now_ns())),
            actions: Vec::new(),
        }
    }

    fn backup(&self, path: &Path) -> Result<()> {
        let bytes = fs::read(path).map_err(|e| cannot_read(path, e))?;
        make_dir(&self.backups)?;
        let copy = self.backups.join(path.file_name().unwrap_or_default());
        write_private(&copy, &bytes).map_err(|e| cannot_write(&copy, e))
    }

    /// Make `link` lead to `source`, atomically. A file that is in the way is backed up first.
    fn link(&mut self, link: &Path, source: &Path) -> Result<()> {
        if is_symlink(link) && resolve(link) == resolve(source) {
            return Ok(());
        }
        let parent = link.parent().unwrap_or(Path::new("."));
        make_dir(parent)?;
        if link.exists() && !is_symlink(link) {
            self.backup(link)?;
        }
        let temporary = parent.join(format!(".tmux-workspace-{}", now_ns()));
        let placed = symlink(source, &temporary).and_then(|()| fs::rename(&temporary, link));
        let _ = fs::remove_file(&temporary);
        placed.map_err(|e| cannot_write(link, e))?;
        self.actions
            .push(format!("link {} -> {}", link.display(), source.display()));
        Ok(())
    }

    /// `link`, and whether it changed anything.
    fn link_changed(&mut self, link: &Path, source: &Path) -> Result<bool> {
        let count = self.actions.len();
        self.link(link, source)?;
        Ok(self.actions.len() > count)
    }

    /// Replace `path` atomically, unless someone else changed it while we worked. A symlinked file
    /// (dotfile managers) is written at its target, so the link stays.
    fn write_json(&self, path: &Path, data: &Value, original: &[u8]) -> Result<()> {
        if path.exists() {
            self.backup(path)?;
        }
        let path = if is_symlink(path) {
            resolve(path)
        } else {
            path.to_path_buf()
        };
        let dir = path.parent().unwrap_or(Path::new("."));
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let (mut file, temporary) =
            create_temporary(dir, &name).map_err(|e| cannot_write(&path, e))?;
        let text = dumps(data, Style::Indent2, false) + "\n";
        let written = file
            .write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| cannot_write(&path, e));
        drop(file);
        let replaced = written.and_then(|()| {
            let now = if path.exists() {
                fs::read(&path).map_err(|e| cannot_read(&path, e))?
            } else {
                b"{}".to_vec()
            };
            if now != original {
                return fail(format!(
                    "{} changed during installation; rerun the installer.",
                    path.display()
                ));
            }
            fs::rename(&temporary, &path).map_err(|e| cannot_write(&path, e))
        });
        let _ = fs::remove_file(&temporary);
        replaced
    }

    /// Merge the hooks of `template` into the settings file `path`. Whether anything changed.
    fn hooks(&mut self, path: &Path, template: &Path, name: &str) -> Result<bool> {
        let original = read_settings(path)?;
        let existing = parse_settings(&original, path)?;
        let wanted = render(template, &self.target.home)?;
        let Some(hooks) = wanted.get("hooks").and_then(Value::as_object) else {
            return fail(format!("{} holds no hooks", template.display()));
        };
        let (mut data, mut changed) = merge_hooks(&existing, hooks);
        if let Some(line) = wanted.get("statusLine").filter(|line| truthy(line)) {
            let current = data.get("statusLine").cloned();
            let command = current.as_ref().and_then(command_of);
            if current.as_ref().is_none_or(|value| !truthy(value)) || command.is_some_and(ours) {
                changed = changed || current.as_ref() != Some(line);
                if let Some(object) = data.as_object_mut() {
                    object.insert("statusLine".into(), line.clone());
                }
            } else {
                self.actions.push(format!(
                    "{name} statusLine kept: {}",
                    command.unwrap_or_default()
                ));
            }
        }
        if changed {
            self.write_json(path, &data, &original)?;
            self.actions
                .push(format!("{name} hooks installed in {}", path.display()));
        }
        Ok(changed)
    }

    /// Remove the link the Python version made for its hook, once no settings file runs the helper
    /// through it: its hooks would fail in every session. Otherwise keep it and say why.
    fn drop_old_hook_link(&mut self) -> Result<()> {
        let target = self.target;
        let link = target.old_hook_link();
        if !old_hook_link(&link) {
            return Ok(());
        }
        let mut files = vec![target.codex_hooks(), target.claude_settings()];
        let default_codex = target.home.join(".codex/hooks.json");
        if !files.contains(&default_codex) {
            files.push(default_codex);
        }
        if let Some(file) = files
            .iter()
            .find(|file| hook_commands(file).iter().any(|c| old_mark(c)))
        {
            self.actions.push(format!(
                "link {} kept: {} still has old Python hook entries",
                link.display(),
                file.display()
            ));
            return Ok(());
        }
        fs::remove_file(&link).map_err(|e| cannot_write(&link, e))?;
        self.actions
            .push(format!("link {} removed", link.display()));
        Ok(())
    }

    fn run(mut self) -> Result<Vec<String>> {
        let target = self.target;
        if target.codex().is_dir() {
            let template = target.source("codex-hooks.json");
            if self.hooks(&target.codex_hooks(), &template, "Codex")? {
                // Trust belongs to Codex's /hooks review; never edit the trusted hashes.
                self.actions.push(
                    "In Codex, review the tmux-workspace agent-hook entries with /hooks, then restart running CLIs."
                        .into(),
                );
            }
        }
        if target.home.join(".claude").is_dir() {
            let template = target.source("claude-hooks.json");
            self.hooks(&target.claude_settings(), &template, "Claude")?;
        }
        if target.home.join(".omp").is_dir() {
            self.link(&target.omp_extension(), &target.source("tmux-workspace.ts"))?;
        }
        if target.home.join(".config/nvim").is_dir()
            && self.link_changed(
                &target.neovim_helper(),
                &target.source("tmux_workspace.lua"),
            )?
        {
            self.actions.push(
                "Neovim: add require('custom.tmux_workspace').setup() to init.lua".to_string(),
            );
        }
        if target.system == "Darwin"
            && self.link_changed(
                &target.iterm_profile(),
                &target.source("iterm2-profile.json"),
            )?
        {
            self.actions
                .push("iTerm2: select Profiles > tmux workspace for a new tab.".to_string());
        }
        // Last: the hooks that ran through it are rewritten by now.
        self.drop_old_hook_link()?;
        if self.backups.exists() {
            self.actions
                .push(format!("backups in {}", self.backups.display()));
        }
        Ok(self.actions)
    }

    /// Undo `run`: drop this project's hook entries and status line, and the links into the share folder.
    fn remove(mut self) -> Result<Vec<String>> {
        let target = self.target;
        for (path, name) in [
            (target.codex_hooks(), "Codex"),
            (target.claude_settings(), "Claude"),
        ] {
            if !path.is_file() {
                continue;
            }
            let original = read_settings(&path)?;
            let existing = parse_settings(&original, &path)?;
            let (mut data, mut changed) = merge_hooks(&existing, &Map::new());
            let line_is_ours = data
                .get("statusLine")
                .is_some_and(|line| line.is_object() && command_of(line).is_some_and(ours));
            if line_is_ours && let Some(object) = data.as_object_mut() {
                // Not `remove`: with preserve_order that moves the last key into this one's place.
                object.shift_remove("statusLine");
                changed = true;
            }
            if changed {
                self.write_json(&path, &data, &original)?;
                self.actions
                    .push(format!("{name} hooks removed from {}", path.display()));
            }
        }
        let share = resolve(&target.share);
        for link in [
            target.old_hook_link(),
            target.omp_extension(),
            target.neovim_helper(),
            target.iterm_profile(),
        ] {
            let ours_to_remove = is_symlink(&link) && {
                let leads = leads_to(&link);
                (leads != share && leads.starts_with(&share)) || old_hook_link(&link)
            };
            if ours_to_remove {
                fs::remove_file(&link).map_err(|e| cannot_write(&link, e))?;
                self.actions.push(format!("link {}", link.display()));
            }
        }
        Ok(self.actions)
    }
}

/// The bytes of a settings file, or `{}` for one that is not there yet.
fn read_settings(path: &Path) -> Result<Vec<u8>> {
    if path.exists() {
        fs::read(path).map_err(|e| cannot_read(path, e))
    } else {
        Ok(b"{}".to_vec())
    }
}

/// The settings in `bytes`. Invalid JSON, or hooks laid out unexpectedly, stops here and the file stays as it was.
fn parse_settings(bytes: &[u8], path: &Path) -> Result<Value> {
    let settings: Value = serde_json::from_slice(bytes).map_err(|e| cannot_read(path, e))?;
    check_layout(&settings, path)?;
    Ok(settings)
}

/// A new file that only its owner can read.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

/// Register the integrations for the tools found in `home`. Gives what was done, in the words to print.
pub fn install(home: &Path, system: &str) -> Result<Vec<String>> {
    install_on(&Target::current(home, system))
}

/// `install` for a given target.
pub fn install_on(target: &Target) -> Result<Vec<String>> {
    Installer::new(target).run()
}

/// Undo `install`. Gives what was done, in the words to print.
pub fn uninstall(home: &Path) -> Result<Vec<String>> {
    uninstall_on(&Target::current(home, system_name()))
}

/// `uninstall` for a given target.
pub fn uninstall_on(target: &Target) -> Result<Vec<String>> {
    Installer::new(target).remove()
}

/// `uname -s` for the systems this runs on.
pub fn system_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Darwin"
    } else {
        "Linux"
    }
}

/// `integrations install`, `integrations uninstall` and `integrations status`, which prints
/// `installed` or `not installed`.
pub fn main(args: &[String]) -> Result<i32> {
    let home = home();
    let actions = match args.first().map(String::as_str) {
        Some("status") => {
            let installed = installed_on(&Target::current(&home, system_name()));
            let status = if installed {
                "installed"
            } else {
                "not installed"
            };
            vec![status.to_string()]
        }
        Some("install") => {
            let done = install(&home, system_name())?;
            if done.is_empty() {
                vec!["Integrations already installed.".to_string()]
            } else {
                done
            }
        }
        Some("uninstall") => uninstall(&home)?,
        _ => {
            return Err(Error::Msg(
                "Usage: tmux-workspace integrations install|uninstall|status".into(),
            ));
        }
    };
    let mut out = std::io::stdout();
    for action in actions {
        let _ = writeln!(out, "{action}");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use serde_json::json;

    /// A target whose home is a folder of its own, so its backups stay out of the way.
    fn target(dir: &TempDir) -> Target {
        Target {
            home: dir.path().join("home"),
            share: dir.path().join("share"),
            codex_home: None,
            system: "Linux".into(),
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_file_that_changed_while_we_worked_is_not_overwritten() {
        let dir = TempDir::new();
        let path = dir.path().join("files/settings.json");
        fs::create_dir(dir.path().join("files")).unwrap();
        fs::write(&path, r#"{"model": "opus"}"#).unwrap();
        let target = target(&dir);

        let error = Installer::new(&target)
            .write_json(&path, &json!({"a": 1}), br#"{"model": "sonnet"}"#)
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "{} changed during installation; rerun the installer.",
                path.display()
            )
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"model": "opus"}"#);
        assert_eq!(
            names(&dir.path().join("files")),
            ["settings.json"],
            "no temporary file is left"
        );
    }

    #[test]
    fn a_file_that_appeared_while_we_worked_is_not_overwritten() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        let target = target(&dir);
        // The installer found no file and so started from `{}`; now there is one.
        fs::write(&path, r#"{"model": "opus"}"#).unwrap();

        let error = Installer::new(&target)
            .write_json(&path, &json!({"a": 1}), b"{}")
            .unwrap_err();

        assert!(
            error
                .to_string()
                .ends_with("changed during installation; rerun the installer.")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"model": "opus"}"#);
    }

    #[test]
    fn an_unchanged_file_is_written_as_python_json_dump_with_indent_2() {
        let dir = TempDir::new();
        fs::create_dir(dir.path().join("files")).unwrap();
        let path = dir.path().join("files/settings.json");
        let original = "{\"name\": \"caf\u{e9}\"}".as_bytes();
        fs::write(&path, original).unwrap();
        let target = target(&dir);
        let settings: Value = serde_json::from_slice(original).unwrap();

        Installer::new(&target)
            .write_json(
                &path,
                &json!({"name": settings["name"], "list": [1, 2.5], "none": {}}),
                original,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\n  \"name\": \"caf\\u00e9\",\n  \"list\": [\n    1,\n    2.5\n  ],\n  \"none\": {}\n}\n"
        );
        assert_eq!(names(&dir.path().join("files")), ["settings.json"]);
    }

    #[test]
    fn a_missing_file_is_created_from_an_empty_object() {
        let dir = TempDir::new();
        let path = dir.path().join("hooks.json");
        let target = target(&dir);

        Installer::new(&target)
            .write_json(&path, &json!({"hooks": {}}), b"{}")
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\n  \"hooks\": {}\n}\n"
        );
    }

    #[test]
    fn merging_never_touches_hooks_it_does_not_understand() {
        let wanted = json!({"Stop": [{"hooks": []}]});
        let wanted = wanted.as_object().unwrap();
        for odd in [
            json!([]),
            json!("x"),
            json!({"hooks": null}),
            json!({"hooks": []}),
        ] {
            assert_eq!(merge_hooks(&odd, wanted), (odd.clone(), false), "{odd}");
        }
    }

    #[test]
    fn groups_keep_their_other_keys_and_order_when_our_entry_goes() {
        let existing = json!({"hooks": {"PreToolUse": [
            {"matcher": "Bash", "hooks": [
                {"command": "/h/.local/bin/tmux-workspace agent-hook claude"},
                {"command": "mine"}], "extra": 1},
            {"hooks": [{"command": "x/tmux-workspace/integrations/agent-hook.py claude"}]},
        ], "Empty": []}, "model": "opus"});

        let (data, changed) = merge_hooks(&existing, &Map::new());

        assert!(changed);
        assert_eq!(
            data,
            json!({"hooks": {"PreToolUse": [
                {"matcher": "Bash", "hooks": [{"command": "mine"}], "extra": 1}]}, "model": "opus"})
        );
        let keys: Vec<&String> = data.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["hooks", "model"],
            "the key order of the file is kept"
        );
        let group = data["hooks"]["PreToolUse"][0].as_object().unwrap();
        let keys: Vec<&String> = group.keys().collect();
        assert_eq!(keys, ["matcher", "hooks", "extra"]);
    }

    #[test]
    fn the_old_hook_link_is_told_by_where_it_led() {
        let dir = TempDir::new();
        let link = dir.path().join("agent-hook.py");
        assert!(!old_hook_link(&link));
        symlink("/old/clone/integrations/agent-hook.py", &link).unwrap();
        assert!(old_hook_link(&link));
        fs::remove_file(&link).unwrap();
        symlink("/old/clone/integrations/other.py", &link).unwrap();
        assert!(!old_hook_link(&link));
        fs::remove_file(&link).unwrap();
        // A file of that name is not a link.
        fs::create_dir_all(dir.path().join("integrations")).unwrap();
        fs::write(dir.path().join("integrations/agent-hook.py"), "").unwrap();
        assert!(!old_hook_link(
            &dir.path().join("integrations/agent-hook.py")
        ));
    }

    #[test]
    fn hook_commands_are_read_from_hooks_and_the_status_line() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            json!({
                "hooks": {"Stop": [{"hooks": [{"command": "a"}, {"type": "command"}]}, "x", {"hooks": 3}],
                          "Other": "x"},
                "statusLine": {"command": "line"}
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(hook_commands(&path), ["a", "line"]);
        assert_eq!(
            hook_commands(&dir.path().join("missing.json")),
            Vec::<String>::new()
        );
        fs::write(&path, "{broken").unwrap();
        assert_eq!(hook_commands(&path), Vec::<String>::new());
        fs::write(&path, "[1]").unwrap();
        assert_eq!(hook_commands(&path), Vec::<String>::new());
    }

    #[test]
    fn the_old_mark_needs_the_projects_name_in_the_path() {
        for command in [
            "/usr/bin/python3 /Users/u/.local/lib/tmux-workspace/agent-hook.py claude",
            "/usr/bin/python3 /Users/u/Projects/tmux-workspace/integrations/agent-hook.py codex",
        ] {
            assert!(old_mark(command) && ours(command), "{command}");
        }
        for command in [
            "python3 /home/u/bin/my-agent-hook.py stop",
            "python3 /home/u/bin/agent-hook.py stop",
            "agent-hook.py",
            "tmux-workspace agent-hook.py",
        ] {
            assert!(!old_mark(command) && !ours(command), "{command}");
        }
        // The current form is ours, but is not the old one.
        assert!(ours("/h/.local/bin/tmux-workspace agent-hook claude"));
        assert!(!old_mark("/h/.local/bin/tmux-workspace agent-hook claude"));
    }
}
