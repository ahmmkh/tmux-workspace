//! JSON hook/settings rendering, merging, layout validation, and `ours` detection.

use crate::error::{Result, fail};
use crate::paths::cannot_read;
use crate::text::{shlex_quote, truthy};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// The installed binary, which every hook command runs.
pub fn binary(home: &Path) -> PathBuf {
    home.join(".local/bin/tmux-workspace")
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

/// The `command` of a hook, statusLine, or title object.
pub(crate) fn command_of(value: &Value) -> Option<&str> {
    value.get("command").and_then(Value::as_str)
}

/// Whether an Antigravity CLI `title`/`statusLine` setting entry is disabled (`"enabled": false`).
pub(crate) fn is_disabled(value: &Value) -> bool {
    value.get("enabled") == Some(&Value::Bool(false))
}

fn is_ours(hook: &Value) -> bool {
    command_of(hook).is_some_and(ours)
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

/// Whether an Antigravity CLI `settings.json` invokes our `agent-hook` via `title` or `statusLine`
/// (and is not explicitly disabled).
pub fn hooked_agy(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(settings) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    ["title", "statusLine"].iter().any(|key| {
        settings
            .get(*key)
            .filter(|entry| entry.is_object() && !is_disabled(entry))
            .and_then(command_of)
            .is_some_and(ours)
    })
}

/// An `integrations/*.json` template with `{{BIN}}` filled in: the installed binary of this
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
pub(crate) fn check_layout(settings: &Value, path: &Path) -> Result<()> {
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

/// Merge an Antigravity CLI `"title"` callback into `existing`.
/// `agy` normalizes/strips default keys (`"type": "command"`, `"enabled": true`) on save; an
/// existing `"title"` whose `command` already equals the wanted command and is not disabled counts
/// as unchanged so repeated installs are idempotent.
pub(crate) fn merge_agy_title(
    existing: &Value,
    wanted_title: &Value,
) -> (Value, bool, Option<String>) {
    let mut data = existing.clone();
    let current = data.get("title").cloned();
    let current_cmd = current.as_ref().and_then(command_of);
    let wanted_cmd = command_of(wanted_title);

    if current.as_ref().is_none_or(|value| !truthy(value)) || current_cmd.is_some_and(ours) {
        let already_effective = current.as_ref().is_some_and(|entry| {
            entry.is_object() && !is_disabled(entry) && command_of(entry) == wanted_cmd
        });
        if already_effective {
            return (data, false, None);
        }
        if let Some(object) = data.as_object_mut() {
            object.insert("title".into(), wanted_title.clone());
        }
        return (data, true, None);
    }

    (
        data,
        false,
        Some(current_cmd.unwrap_or_default().to_string()),
    )
}
