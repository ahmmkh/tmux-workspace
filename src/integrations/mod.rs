//! `integrations install|uninstall|status`: register the agent hook with Claude Code, Codex, and
//! Antigravity CLI (`agy`), and link the omp extension, the Neovim helper, and the iTerm2 profile.
//!
//! Agent settings are merged, never replaced: this project's own hook/title entries are updated
//! in place, and everything else in the file is kept as it was. Every file that is changed or
//! replaced is backed up first, and running it again changes nothing.

mod json;
mod target;

pub use json::{binary, hook_commands, hooked_agy, merge_hooks, old_mark, ours, render};
pub use target::{Target, codex_dir};

use crate::error::{Error, Result, fail};
use crate::paths::{cannot_read, cannot_write, home, resolve};
use crate::pyjson::{Style, dumps};
use crate::snapshot::create_temporary;
use crate::text::{now_ns, truthy};
use json::{check_layout, command_of, merge_agy_title};
use serde_json::{Map, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, symlink};
use std::path::{Path, PathBuf};
use target::{is_symlink, old_hook_link};

/// Whether any integration (agent hook/title or link) is currently in place on `target`.
pub fn installed_on(target: &Target) -> bool {
    let each = target.each();
    // The iTerm2 profile counts as well, but is no integration of a report.
    each.iter().any(|(_, there)| *there) || target.made(&target.iterm_profile())
}

/// `mkdir(parents=True, exist_ok=True, mode=0o700)`: whatever is made is private.
fn make_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| cannot_write(dir, e))
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
                    "{}: changed during installation; rerun the installer.",
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
            return fail(format!("{}: no hooks in it", template.display()));
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

    /// Merge the `title` callback of `agy-settings.json` into `~/.gemini/antigravity-cli/settings.json`.
    fn agy_title(&mut self, path: &Path, template: &Path) -> Result<bool> {
        let original = read_settings(path)?;
        let existing: Value =
            serde_json::from_slice(&original).map_err(|e| cannot_read(path, e))?;
        if !existing.is_object() {
            return fail(format!(
                "Cannot merge hooks into {}: the top level must be an object. Fix the file and rerun the installer.",
                path.display()
            ));
        }
        let wanted = render(template, &self.target.home)?;
        let Some(wanted_title) = wanted.get("title").filter(|title| truthy(title)) else {
            return fail(format!("{}: no title in it", template.display()));
        };
        let (data, changed, kept) = merge_agy_title(&existing, wanted_title);
        if let Some(custom) = kept {
            self.actions
                .push(format!("Antigravity CLI title kept: {custom}"));
            return Ok(false);
        }
        if changed {
            self.write_json(path, &data, &original)?;
            self.actions.push(format!(
                "Antigravity CLI title installed in {}",
                path.display()
            ));
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
        if target.agy_dir().is_dir() {
            let template = target.source("agy-settings.json");
            self.agy_title(&target.agy_settings(), &template)?;
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

    /// Undo `run`: drop this project's hook entries, status line, agy title, and links it made.
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
        let agy_path = target.agy_settings();
        if agy_path.is_file() {
            let original = read_settings(&agy_path)?;
            let mut data: Value =
                serde_json::from_slice(&original).map_err(|e| cannot_read(&agy_path, e))?;
            let title_is_ours = data
                .get("title")
                .is_some_and(|title| title.is_object() && command_of(title).is_some_and(ours));
            if title_is_ours && let Some(object) = data.as_object_mut() {
                object.shift_remove("title");
                self.write_json(&agy_path, &data, &original)?;
                self.actions.push(format!(
                    "Antigravity CLI title removed from {}",
                    agy_path.display()
                ));
            }
        }
        let old = target.old_hook_link();
        for link in std::iter::once(old).chain(target.links()) {
            if target.made(&link) || old_hook_link(&link) {
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
    use crate::anonymize::{Anonymizer, Identity, Names};
    use crate::testing::TempDir;
    use serde_json::json;
    use target::is_version;

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
    fn each_integration_says_whether_it_is_in_place() {
        let dir = TempDir::new();
        let target = target(&dir);
        let all = |claude, codex, omp, agy, nvim| {
            [
                ("claude", claude),
                ("codex", codex),
                ("omp", omp),
                ("agy", agy),
                ("nvim", nvim),
            ]
        };
        assert_eq!(target.each(), all(false, false, false, false, false));
        assert!(!installed_on(&target));

        let hook = |binary: &str| {
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command",
                "command": format!("'{binary}' agent-hook codex")}]}]}})
            .to_string()
        };
        fs::create_dir_all(target.home.join(".claude")).unwrap();
        fs::write(
            target.home.join(".claude/settings.json"),
            hook("/h/.local/bin/tmux-workspace"),
        )
        .unwrap();
        assert_eq!(target.each(), all(true, false, false, false, false));

        // A hook that is somebody else's is not ours.
        fs::create_dir_all(target.home.join(".codex")).unwrap();
        fs::write(target.home.join(".codex/hooks.json"), hook("/h/bin/other")).unwrap();
        assert_eq!(target.each(), all(true, false, false, false, false));
        fs::write(
            target.home.join(".codex/hooks.json"),
            hook("/h/.local/bin/tmux-workspace"),
        )
        .unwrap();
        assert_eq!(target.each(), all(true, true, false, false, false));

        // The links count when they lead into the share folder, and not when they lead elsewhere.
        let source = dir.path().join("share/integrations/tmux-workspace.ts");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "").unwrap();
        let link = target.home.join(".omp/agent/extensions/tmux-workspace.ts");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&source, &link).unwrap();
        assert_eq!(target.each(), all(true, true, true, false, false));

        // Antigravity CLI title callback (even when agy normalized away default keys).
        fs::create_dir_all(target.agy_dir()).unwrap();
        fs::write(
            target.agy_settings(),
            json!({"title": {"command": "/h/.local/bin/tmux-workspace agent-hook agy"}})
                .to_string(),
        )
        .unwrap();
        assert_eq!(target.each(), all(true, true, true, true, false));

        let elsewhere = target.home.join("mine.lua");
        fs::write(&elsewhere, "").unwrap();
        let lua = target
            .home
            .join(".config/nvim/lua/custom/tmux_workspace.lua");
        fs::create_dir_all(lua.parent().unwrap()).unwrap();
        symlink(&elsewhere, &lua).unwrap();
        assert_eq!(target.each(), all(true, true, true, true, false));
        fs::remove_file(&lua).unwrap();
        symlink(&source, &lua).unwrap();
        assert_eq!(target.each(), all(true, true, true, true, true));
        assert!(installed_on(&target));
    }

    #[test]
    fn agy_title_install_is_idempotent_across_normalization_and_keeps_custom_title() {
        let dir = TempDir::new();
        let target = target(&dir);
        fs::create_dir_all(target.agy_dir()).unwrap();
        let template = target.source("agy-settings.json");
        fs::create_dir_all(template.parent().unwrap()).unwrap();
        fs::write(
            &template,
            r#"{"title": {"type": "command", "command": "{{BIN}} agent-hook agy"}}"#,
        )
        .unwrap();

        let first = install_on(&target).unwrap();
        assert!(
            first
                .iter()
                .any(|a| a.contains("Antigravity CLI title installed"))
        );
        assert!(target.each().iter().any(|(k, v)| *k == "agy" && *v));

        // Simulate agy stripping default "type": "command" on startup.
        let installed: Value =
            serde_json::from_str(&fs::read_to_string(target.agy_settings()).unwrap()).unwrap();
        let cmd = installed["title"]["command"].as_str().unwrap().to_string();
        fs::write(
            target.agy_settings(),
            json!({"title": {"command": cmd}, "theme": "dark"}).to_string(),
        )
        .unwrap();
        assert!(
            install_on(&target).unwrap().is_empty(),
            "normalized title is idempotent"
        );

        // Uninstall removes our title and preserves unrelated settings.
        let removed = uninstall_on(&target).unwrap();
        assert!(
            removed
                .iter()
                .any(|a| a.contains("Antigravity CLI title removed"))
        );
        let after: Value =
            serde_json::from_str(&fs::read_to_string(target.agy_settings()).unwrap()).unwrap();
        assert_eq!(after, json!({"theme": "dark"}));

        // Custom user title is never overwritten.
        fs::write(
            target.agy_settings(),
            json!({"title": {"command": "my-custom-title"}}).to_string(),
        )
        .unwrap();
        let kept = install_on(&target).unwrap();
        assert!(
            kept.iter()
                .any(|a| a.contains("Antigravity CLI title kept: my-custom-title"))
        );
        assert!(!hooked_agy(&target.agy_settings()));
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
                "{}: changed during installation; rerun the installer.",
                path.display()
            )
        );
        let shown =
            Anonymizer::new(&Identity::default(), &Names::default()).apply(&error.to_string());
        assert!(
            shown
                .as_str()
                .ends_with(": changed during installation; rerun the installer."),
            "{shown}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"model": "opus"}"#);
        assert_eq!(names(&dir.path().join("files")), ["settings.json"]);
    }

    #[test]
    fn a_template_without_hooks_says_so_after_its_path() {
        let dir = TempDir::new();
        let template = dir.path().join("share/integrations/codex-hooks.json");
        fs::create_dir_all(template.parent().unwrap()).unwrap();
        fs::write(&template, "{}").unwrap();
        let target = target(&dir);
        let error = Installer::new(&target)
            .hooks(&dir.path().join("hooks.json"), &template, "Codex")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("{}: no hooks in it", template.display())
        );
        let shown =
            Anonymizer::new(&Identity::default(), &Names::default()).apply(&error.to_string());
        assert!(shown.as_str().ends_with(": no hooks in it"), "{shown}");
    }

    #[test]
    fn a_file_that_appeared_while_we_worked_is_not_overwritten() {
        let dir = TempDir::new();
        let path = dir.path().join("settings.json");
        let target = target(&dir);
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
        assert_eq!(keys, ["hooks", "model"]);
        let group = data["hooks"]["PreToolUse"][0].as_object().unwrap();
        let keys: Vec<&String> = group.keys().collect();
        assert_eq!(keys, ["matcher", "hooks", "extra"]);
    }

    #[test]
    fn version_folders_are_named_as_install_sh_names_them() {
        for name in ["2.0.0", "10.20.30", "2.0.1-rc.1", "2.0.0+build-7"] {
            assert!(is_version(name), "{name}");
        }
        for name in [
            "current",
            "2.0",
            "2.0.0.1",
            "v2.0.0",
            "2.x.0",
            "2.0.0-",
            "2.0.0-a_b",
            ".incoming-1",
        ] {
            assert!(!is_version(name), "{name}");
        }
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
        assert!(ours("/h/.local/bin/tmux-workspace agent-hook claude"));
        assert!(!old_mark("/h/.local/bin/tmux-workspace agent-hook claude"));
    }
}
