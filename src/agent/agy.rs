//! Antigravity CLI (`agy`) specification, `title` callback handler, and status mapping.
//!
//! `agy` invokes `~/.local/bin/tmux-workspace agent-hook agy` on every state change through its
//! CLI-scoped `"title"` setting in `~/.gemini/antigravity-cli/settings.json`. The hook publishes
//! `@workspace_app` and `@workspace_agent` when inside tmux, and prints a clean single-line
//! terminal title on stdout (even outside tmux).

use super::{AgentKind, AgentSpec};
use crate::error::Result;
use crate::hook::{CODEX, FAILED, HookEnv, OPTION, STATUS, WAIT, process, tmuxcmd};
use crate::host::Host;
use crate::pyjson::{Style, dumps};
use serde_json::{Map, Value};

pub const SPEC: AgentSpec = AgentSpec {
    kind: AgentKind::Agy,
    id: "agy",
    display_name: "Antigravity CLI",
    is_ai_agent: true,
    resume_argv: &["agy", "--conversation"],
    checks_session_file: false,
    one_shot_flags: &["-p", "--print", "--prompt", "--bg-updater"],
    non_interactive_subcommands: &[
        "acp",
        "agent",
        "agents",
        "changelog",
        "help",
        "install",
        "mcp",
        "mic-serve",
        "models",
        "plugin",
        "plugins",
        "remote-control",
        "update",
    ],
    value_flags: &[
        "-c",
        "--conversation",
        "-m",
        "--model",
        "--cwd",
        "--profile",
        "--approval-mode",
    ],
    hook_modes: &["agy"],
};

/// Maximum length (in characters) of the conversation title shown in the terminal title bar.
const MAX_TITLE_CHARS: usize = 60;

/// Desired `@workspace_agent` update computed from an `agy` payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wanted {
    /// Set `@workspace_agent` directly to this status word.
    Direct(&'static str),
    /// Let tmux evaluate `@agent_turn_over` (`done` when hidden and previously active, else `idle`).
    TurnOver,
}

/// Extract a valid conversation id (`conversation_id`, falling back to `session_id`).
/// Invalid or absent IDs return `""` so blank startup / `/new` still publishes process ownership.
pub fn extract_conversation_id(data: &Map<String, Value>) -> &str {
    let raw = data
        .get("conversation_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| data.get("session_id").and_then(Value::as_str))
        .unwrap_or("");
    if raw.is_empty()
        || raw.chars().count() > 4096
        || raw.starts_with('-')
        || raw.chars().any(|c| c < ' ' || c == '\x7f')
    {
        ""
    } else {
        raw
    }
}

/// Extract a validated working directory (`cwd`, falling back to `workspace.current_dir`).
pub fn extract_cwd(data: &Map<String, Value>) -> Option<&str> {
    let candidate = data.get("cwd").and_then(Value::as_str).or_else(|| {
        data.get("workspace")
            .and_then(Value::as_object)
            .and_then(|ws| ws.get("current_dir"))
            .and_then(Value::as_str)
    })?;
    if candidate.starts_with('/') && !candidate.chars().any(|c| c < ' ' || c == '\x7f') {
        Some(candidate)
    } else {
        None
    }
}

/// Format the single-line terminal title printed to stdout for `agy`'s `title` callback.
/// Returns `None` when `payload` is not a JSON object.
pub fn format_title(payload: &Value) -> Option<String> {
    let data = payload.as_object()?;
    if let Some(raw_title) = data.get("conversation_title").and_then(Value::as_str) {
        let cleaned = clean_title(raw_title);
        if !cleaned.is_empty() {
            return Some(format!("agy | {cleaned}\n"));
        }
    }
    let id = extract_conversation_id(data);
    if !id.is_empty() {
        let short: String = id.chars().take(8).collect();
        return Some(format!("agy | {short}\n"));
    }
    Some("agy\n".to_string())
}

/// Collapse whitespace, strip control characters, and cap to `MAX_TITLE_CHARS`.
fn clean_title(raw: &str) -> String {
    let collapsed: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed.chars().take(MAX_TITLE_CHARS).collect()
}

/// Map `agent_state` + `tool_confirmation_pending` to a desired `@workspace_agent` transition.
pub fn wanted_status(data: &Map<String, Value>, current_status: &str) -> Option<Wanted> {
    let pending = data
        .get("tool_confirmation_pending")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let agent_state = data.get("agent_state").and_then(Value::as_str);
    if pending || matches!(agent_state, Some("reviewing" | "authenticating" | "error")) {
        return Some(Wanted::Direct("blocked"));
    }
    match agent_state {
        Some("working" | "tool_use") => Some(Wanted::Direct("working")),
        // Startup/init is not an active user turn; never chime `done` on launch or restore.
        Some("initializing") => Some(Wanted::Direct("idle")),
        Some("idle") => {
            if current_status.is_empty() {
                Some(Wanted::Direct("idle"))
            } else {
                Some(Wanted::TurnOver)
            }
        }
        _ => None,
    }
}

/// Handle `agent-hook agy` inside tmux. Always returns the formatted title line when `payload`
/// is a JSON object, even if the pane is gone or not owned by an interactive foreground `agy`.
pub fn agy_event(payload: &Value, env: &HookEnv, host: &dyn Host) -> Result<Option<String>> {
    let Some(data) = payload.as_object() else {
        return Ok(None);
    };
    let title = format_title(payload);
    let Some(cwd) = extract_cwd(data) else {
        return Ok(title);
    };
    let pane = env.pane.as_str();
    let mut state = tmuxcmd::pane_state(host, &env.tmux, pane)?;
    if state.pid.is_empty() {
        return Ok(title);
    }
    let pane_pid = process::py_int(&state.pid)?;
    let Some((pid, birth)) = process::owner(host, "agy", pane_pid, &state.tty, None, None, None)?
    else {
        return Ok(title);
    };

    let conversation_id = extract_conversation_id(data);
    let mut metadata = Map::new();
    metadata.insert("kind".into(), "agy".into());
    metadata.insert("id".into(), conversation_id.into());
    metadata.insert("pid".into(), pid.into());
    metadata.insert("cwd".into(), cwd.into());
    metadata.insert("birth".into(), birth.into());

    let value = dumps(&Value::Object(metadata.clone()), Style::Compact, false);
    if value != state.app {
        let mut command = tmuxcmd::assign(pane, OPTION, &value);
        let old = tmuxcmd::published(&state.app);
        if ["kind", "id", "pid", "birth"]
            .iter()
            .any(|key| old.get(*key) != metadata.get(*key))
        {
            command += " ; ";
            command += &tmuxcmd::unset(pane, &[STATUS, WAIT, FAILED, CODEX]);
            state.status.clear();
            state.wait.clear();
            state.failed.clear();
        }
        tmuxcmd::set_if(host, &env.tmux, pane, OPTION, &state.app, &command, None)?;
    }

    if let Some(wanted) = wanted_status(data, &state.status) {
        match wanted {
            Wanted::Direct(target) if target != state.status => {
                let write = tmuxcmd::assign(pane, STATUS, target);
                let _ = tmuxcmd::set_if(host, &env.tmux, pane, STATUS, &state.status, &write, None);
            }
            Wanted::TurnOver => {
                let expected = if state.turn_over == "done" {
                    "done"
                } else {
                    "idle"
                };
                if expected != state.status {
                    let write = tmuxcmd::turn_over(pane);
                    let _ =
                        tmuxcmd::set_if(host, &env.tmux, pane, STATUS, &state.status, &write, None);
                }
            }
            _ => {}
        }
    }

    Ok(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeHost, Proc};
    use serde_json::json;
    use std::path::PathBuf;

    fn env() -> HookEnv {
        HookEnv {
            pane: "%7".into(),
            socket: "/tmp/sock".into(),
            tmux: vec!["tmux".into(), "-N".into(), "-S".into(), "/tmp/sock".into()],
            sessions: PathBuf::from("/tmp/claude/sessions"),
            codex_home: PathBuf::from("/tmp/codex"),
        }
    }

    fn agy_host(pane_state_line: &str) -> FakeHost {
        let state_line = pane_state_line.to_string();
        let mut host = FakeHost::scripted(move |argv| {
            if argv.iter().any(|a| a == "display-message") {
                return Ok(state_line.clone());
            }
            Ok(String::new())
        });
        host.parent = 200;
        host.procs.insert(
            200,
            Proc {
                args: "agy".into(),
                pgid: 200,
                tpgid: 200,
                lstart: "Mon Nov 24 10:00:00 2026".into(),
                ..Proc::new(100, "ttys001", "agy")
            },
        );
        host.procs.insert(100, Proc::new(1, "ttys001", "zsh"));
        host
    }

    #[test]
    fn title_formats_conversation_title_then_short_id_then_bare_agy() {
        assert_eq!(
            format_title(
                &json!({"conversation_title": "Fix  bug\nnow\t", "conversation_id": "123456789abc"})
            ),
            Some("agy | Fix bug now\n".into())
        );
        assert_eq!(
            format_title(&json!({"conversation_title": "a".repeat(80)})),
            Some(format!("agy | {}\n", "a".repeat(60)))
        );
        assert_eq!(
            format_title(&json!({"conversation_id": "c5ccc94d-bdaf-4613"})),
            Some("agy | c5ccc94d\n".into())
        );
        assert_eq!(
            format_title(&json!({"session_id": "abcdef123456"})),
            Some("agy | abcdef12\n".into())
        );
        assert_eq!(format_title(&json!({})), Some("agy\n".into()));
        assert_eq!(format_title(&json!([])), None);
    }

    #[test]
    fn conversation_id_rejects_control_chars_and_leading_dash() {
        for bad in ["-flag", "a\nb", "a\x7fb", &"x".repeat(4097)] {
            let obj = json!({"conversation_id": bad});
            assert_eq!(extract_conversation_id(obj.as_object().unwrap()), "");
        }
        let obj = json!({"conversation_id": "", "session_id": "sess-1234"});
        assert_eq!(
            extract_conversation_id(obj.as_object().unwrap()),
            "sess-1234"
        );
    }

    #[test]
    fn state_mapping_covers_all_agy_states_and_confirmation_pending() {
        let map = |v: Value, current: &str| wanted_status(v.as_object().unwrap(), current);
        assert_eq!(
            map(
                json!({"agent_state": "working", "tool_confirmation_pending": true}),
                "working"
            ),
            Some(Wanted::Direct("blocked"))
        );
        for state in ["reviewing", "authenticating", "error"] {
            assert_eq!(
                map(json!({"agent_state": state}), "working"),
                Some(Wanted::Direct("blocked")),
                "{state}"
            );
        }
        for state in ["working", "tool_use"] {
            assert_eq!(
                map(json!({"agent_state": state}), "idle"),
                Some(Wanted::Direct("working")),
                "{state}"
            );
        }
        assert_eq!(
            map(json!({"agent_state": "initializing"}), "working"),
            Some(Wanted::Direct("idle")),
            "initializing never chimes done"
        );
        assert_eq!(
            map(json!({"agent_state": "idle"}), ""),
            Some(Wanted::Direct("idle")),
            "initial idle never chimes done"
        );
        assert_eq!(
            map(json!({"agent_state": "idle"}), "working"),
            Some(Wanted::TurnOver),
            "turn completion uses tmux turn_over"
        );
        assert_eq!(map(json!({"agent_state": "future_state"}), "working"), None);
        assert_eq!(map(json!({}), "working"), None);
    }

    #[test]
    fn agy_event_publishes_metadata_and_resets_status_on_new_conversation() {
        let old_app = r#"{"kind":"agy","id":"conv-old","pid":200,"cwd":"/work","birth":"Mon Nov 24 10:00:00 2026"}"#;
        let line = format!("100\t/dev/ttys001\tworking\t\t\tdone\t{old_app}");
        let host = agy_host(&line);
        let payload = json!({
            "cwd": "/work",
            "conversation_id": "",
            "agent_state": "idle",
        });
        let title = agy_event(&payload, &env(), &host).unwrap();
        assert_eq!(title.as_deref(), Some("agy\n"));
        let tmux_calls = host.calls_to("tmux");
        // 1: display-message, 2: @workspace_app update with unset of stale status, 3: @workspace_agent = idle
        assert_eq!(tmux_calls.len(), 3, "{tmux_calls:?}");
        assert!(
            tmux_calls[1]
                .last()
                .unwrap()
                .contains("set-option -pu -t %7 @workspace_agent")
        );
        assert!(
            tmux_calls[2]
                .last()
                .unwrap()
                .contains("@workspace_agent 'idle'")
        );
    }

    #[test]
    fn agy_event_uses_workspace_current_dir_fallback() {
        let line = "100\t/dev/ttys001\t\t\t\tidle\t";
        let host = agy_host(line);
        let payload = json!({
            "workspace": {"current_dir": "/projects/api"},
            "conversation_id": "conv-12345678",
            "conversation_title": "Add agy support",
            "agent_state": "working",
        });
        let title = agy_event(&payload, &env(), &host).unwrap();
        assert_eq!(title.as_deref(), Some("agy | Add agy support\n"));
        let tmux_calls = host.calls_to("tmux");
        assert!(
            tmux_calls[1]
                .last()
                .unwrap()
                .contains(r#""cwd":"/projects/api""#)
        );
        assert!(
            tmux_calls[2]
                .last()
                .unwrap()
                .contains("@workspace_agent 'working'")
        );
    }

    #[test]
    fn agy_event_returns_title_even_when_pane_is_gone() {
        let host = FakeHost::scripted(|_| Ok(String::new()));
        let payload = json!({
            "cwd": "/work",
            "conversation_title": "Gone pane",
            "agent_state": "working",
        });
        assert_eq!(
            agy_event(&payload, &env(), &host).unwrap().as_deref(),
            Some("agy | Gone pane\n")
        );
        assert_eq!(host.calls_to("tmux").len(), 1);
    }
}
