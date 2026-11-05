//! The agent hook: publishes the identity and status of the agent that runs in a tmux pane.
//!
//! It runs on every Claude Code tool call, so it is exact, silent and fast. It is a port of
//! agent-hook.py, and keeps its order of checks: each check that returns early also prevents
//! process calls.

pub mod claude;
pub mod codex;
pub mod process;
pub mod tmuxcmd;

use crate::error::Result;
use crate::host::{Host, RealHost};
use crate::pyjson::{Style, dumps};
use crate::text::now_ms;
use crate::tmux::Tmux;
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;

pub const OPTION: &str = "@workspace_app";
pub const STATUS: &str = "@workspace_agent";
pub const WAIT: &str = "@workspace_agent_wait";
pub const FAILED: &str = "@workspace_agent_failed";
pub const CODEX: &str = "@workspace_codex";
/// Milliseconds within which a failed turn writes its own idle state.
pub const SETTLE: i64 = 5000;
/// tmux decides turn over when it writes. Without agent-status.conf the pane becomes idle.
pub const TURN_OVER: &str = "#{?#{@agent_turn_over},#{E:@agent_turn_over},idle}";
/// One read gives the pane's process, its status options, the turn-over value and its identity.
pub const PANE: &str = "#{pane_pid}\t#{pane_tty}\t#{@workspace_agent}\t#{@workspace_agent_wait}\t#{@workspace_agent_failed}\t#{E:@agent_turn_over}\t#{@workspace_app}";

/// What the hook does for each name it is started with.
const MODES: [&str; 4] = ["claude", "claude-status", "omp", "codex"];
const CODEX_EVENTS: [&str; 8] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "Interrupt",
];
/// Input beyond this is ignored.
const MAX_STDIN: usize = 1 << 20;

/// Where the hook runs: the pane it reports on, the tmux server it talks to, and the folders it reads.
#[derive(Clone, Debug)]
pub struct HookEnv {
    /// `TMUX_PANE`: `%` and digits.
    pub pane: String,
    /// The server's socket, from `TMUX`: an absolute path.
    pub socket: String,
    /// The command that runs tmux against that socket: `tmux -N -S <socket>`.
    pub tmux: Vec<String>,
    /// Claude Code's per-pid state files: `$CLAUDE_CONFIG_DIR` or `~/.claude`, plus `sessions`.
    pub sessions: PathBuf,
    /// `$CODEX_HOME` or `~/.codex`.
    pub codex_home: PathBuf,
}

impl HookEnv {
    /// `None` unless `TMUX` and `TMUX_PANE` name a pane of a server.
    pub fn from_env() -> Option<HookEnv> {
        let var = |name: &str| std::env::var(name).ok();
        Self::from_vars(
            var("TMUX").as_deref(),
            var("TMUX_PANE").as_deref(),
            std::env::var_os("CLAUDE_CONFIG_DIR"),
            std::env::var_os("CODEX_HOME"),
            crate::paths::home,
        )
    }

    /// `from_env` with the variables given. `home` is asked only when a folder needs it.
    pub fn from_vars(
        tmux: Option<&str>,
        pane: Option<&str>,
        claude_config_dir: Option<OsString>,
        codex_home: Option<OsString>,
        home: impl Fn() -> PathBuf,
    ) -> Option<HookEnv> {
        // `TMUX` is `<socket>,<server pid>,<session>`, and the socket path may hold commas.
        let mut parts = tmux?.rsplitn(3, ',');
        let (_session, _server, socket) = (parts.next()?, parts.next()?, parts.next()?);
        let pane = pane?;
        let digits = pane.strip_prefix('%')?;
        if !socket.starts_with('/')
            || digits.is_empty()
            || !digits.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        // An empty variable counts as unset.
        let folder = |value: Option<OsString>, default: &str| {
            value
                .filter(|value| !value.is_empty())
                .map_or_else(|| home().join(default), PathBuf::from)
        };
        Some(HookEnv {
            pane: pane.to_string(),
            socket: socket.to_string(),
            tmux: Tmux::new().socket(socket).argv(&[]),
            sessions: folder(claude_config_dir, ".claude").join("sessions"),
            codex_home: folder(codex_home, ".codex"),
        })
    }
}

/// The hook: read the payload from stdin, publish, print what the mode prints, and exit 0 whatever
/// happens. `args` is what follows `agent-hook`: the mode.
pub fn main(args: &[String]) -> i32 {
    // Recovery metadata must never change hook approval decisions or agent output: not even a panic
    // may write a message.
    std::panic::set_hook(Box::new(|_| {}));
    let started = now_ms();
    let printed = std::panic::catch_unwind(|| publish(started, args));
    if let Ok(Some(text)) = printed {
        let _ = std::io::stdout().write_all(text.as_bytes());
        let _ = std::io::stdout().flush();
    }
    0
}

/// The text `main` prints, if any. Every failure along the way is none.
fn publish(started: i64, args: &[String]) -> Option<String> {
    let [mode] = args else {
        return None;
    };
    if !MODES.contains(&mode.as_str()) {
        return None;
    }
    let env = HookEnv::from_env()?;
    let payload = read_payload()?;
    run_at(started, mode, &payload, &env, &RealHost)
        .ok()
        .flatten()
}

/// The JSON object on stdin. Input over 1 MiB, or anything but one JSON value, is none.
fn read_payload() -> Option<Value> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_STDIN as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_STDIN {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// One hook call. `Ok(Some(text))` is what to print on stdout, newline included.
pub fn run(mode: &str, payload: &Value, env: &HookEnv, host: &dyn Host) -> Result<Option<String>> {
    run_at(now_ms(), mode, payload, env, host)
}

/// Whether a JSON value is true in Python's eyes.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(fields)) => !fields.is_empty(),
    }
}

/// `started` is the time, in milliseconds, at which the hook began: a hook that blocks a pane records it.
fn run_at(
    started: i64,
    mode: &str,
    payload: &Value,
    env: &HookEnv,
    host: &dyn Host,
) -> Result<Option<String>> {
    if !MODES.contains(&mode) {
        return Ok(None);
    }
    let Some(data) = payload.as_object() else {
        return Ok(None);
    };
    let status_line = mode == "claude-status";
    let kind = if status_line { "claude" } else { mode };
    let event = if status_line {
        Some("SessionStart")
    } else {
        data.get("hook_event_name").and_then(Value::as_str)
    };
    // A subagent's approval reaches the user through the pane's agent, so it counts for blocked.
    let blocking = kind == "claude"
        && matches!(
            event,
            Some("PermissionRequest" | "PreToolUse" | "StopFailure")
        );
    let allowed: &[&str] = if kind == "codex" {
        &CODEX_EVENTS
    } else {
        &["SessionStart", "SessionEnd", "CwdChanged"]
    };
    let event_allowed = event.is_some_and(|event| allowed.contains(&event));
    if !blocking && (truthy(data.get("agent_id")) || !event_allowed) {
        return Ok(None);
    }
    let Some(session_id) = data.get("session_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    if session_id.is_empty() || session_id.chars().count() > 4096 {
        return Ok(None);
    }
    if session_id.chars().any(|c| c < ' ') {
        return Ok(None);
    }
    let pane = env.pane.as_str();
    if blocking {
        return block(started, event, session_id, data, env, host);
    }
    let cwd = match data.get("cwd").and_then(Value::as_str) {
        Some(cwd) if cwd.starts_with('/') => cwd,
        _ => return Ok(None),
    };
    if cwd.chars().any(|c| c < ' ') {
        return Ok(None);
    }
    let mut explicit_pid = None;
    if kind == "omp" {
        // JSON integers only, and no pid 1 or below. A pid that does not fit i32 is no process.
        let named = match data.get("pid") {
            Some(Value::Number(number)) => number.as_i64().filter(|pid| *pid > 1),
            _ => None,
        };
        let Some(pid) = named.and_then(|pid| i32::try_from(pid).ok()) else {
            return Ok(None);
        };
        explicit_pid = Some(pid);
    }
    let mut state = tmuxcmd::pane_state(host, &env.tmux, pane)?;
    let pane_pid = process::py_int(&state.pid)?;
    // A shared Codex app-server's inherited TMUX_PANE does not identify its client: the title does.
    let mut routed = None;
    let (pid, birth) =
        match process::owner(host, kind, pane_pid, &state.tty, explicit_pid, None, None)? {
            Some(identity) => identity,
            None if kind == "codex" => match codex::detached_codex(host, &env.tmux, session_id)? {
                Some((resolved, identity)) => {
                    routed = Some(resolved);
                    identity
                }
                None => return Ok(None),
            },
            None => return Ok(None),
        };
    let pane = routed.as_deref().unwrap_or(pane);
    let mut metadata = Map::new();
    metadata.insert("kind".into(), kind.into());
    metadata.insert("id".into(), session_id.into());
    metadata.insert("pid".into(), pid.into());
    metadata.insert("cwd".into(), cwd.into());
    metadata.insert("birth".into(), birth.into());
    if kind == "codex" {
        codex::codex_event(host, &env.tmux, pane, payload, &Value::Object(metadata))?;
        return Ok(None);
    }
    if kind == "claude"
        && !status_line
        && event == Some("SessionStart")
        && data.get("source").and_then(Value::as_str) == Some("fork")
    {
        // /fork may copy a BACKGROUND conversation under the same PID. Invalidate
        // the old owner until the native status callback identifies the visible one.
        let read = tmuxcmd::run(
            host,
            &env.tmux,
            &["show-option", "-p", "-v", "-q", "-t", pane, OPTION],
        )?;
        let current = tmuxcmd::published(&tmuxcmd::option_value(&read));
        if ["kind", "pid", "birth"]
            .iter()
            .all(|key| current.get(*key) == metadata.get(*key))
        {
            tmuxcmd::clear_owned(host, &env.tmux, pane, &current)?;
        }
        return Ok(None);
    }
    if event == Some("SessionEnd") {
        tmuxcmd::clear_owned(host, &env.tmux, pane, &metadata)?;
    } else {
        let value = dumps(&Value::Object(metadata.clone()), Style::Compact, false);
        if value != state.app {
            let mut command = tmuxcmd::assign(pane, OPTION, &value);
            let old = tmuxcmd::published(&state.app);
            if ["id", "pid", "birth"]
                .iter()
                .any(|key| old.get(*key) != metadata.get(*key))
            {
                // A new session never inherits the status of the session before it.
                command += " ; ";
                command += &tmuxcmd::unset(pane, &[STATUS, WAIT, FAILED, CODEX]);
                state.status.clear();
                state.wait.clear();
                state.failed.clear();
            }
            tmuxcmd::set_if(host, &env.tmux, pane, OPTION, &state.app, &command, None)?;
        }
    }
    if !status_line {
        return Ok(None);
    }
    let short: String = session_id.chars().take(8).collect();
    let line = format!("Claude | {short}\n");
    // From here on the line is printed whatever else goes wrong, as it was when it was printed first.
    let file = claude::session_state(env, &pid.to_string());
    let wanted = claude::wanted_status(
        file.as_ref(),
        session_id,
        &state.wait,
        &state.failed,
        &state.turn_over,
    );
    if let Some(wanted) = wanted.filter(|wanted| *wanted != state.status) {
        // Any status the file gives ends a failed turn, so the mark goes with the write.
        // A hook that blocks an already blocked pane changes only the wait value.
        let write = if matches!(wanted, "idle" | "done") {
            tmuxcmd::turn_over(pane)
        } else {
            tmuxcmd::assign(pane, STATUS, wanted)
        };
        let command = format!("{write} ; {}", tmuxcmd::unset(pane, &[FAILED]));
        let also = Some((WAIT, state.wait.as_str()));
        let _ = tmuxcmd::set_if(host, &env.tmux, pane, STATUS, &state.status, &command, also);
    }
    Ok(Some(line))
}

/// A hook of the pane's own Claude session that blocks the pane: an approval, a question, a plan or
/// a failed turn.
fn block(
    started: i64,
    event: Option<&str>,
    session_id: &str,
    data: &Map<String, Value>,
    env: &HookEnv,
    host: &dyn Host,
) -> Result<Option<String>> {
    let pane = env.pane.as_str();
    if event == Some("PreToolUse")
        && !matches!(
            data.get("tool_name").and_then(Value::as_str),
            Some("AskUserQuestion" | "ExitPlanMode")
        )
    {
        return Ok(None);
    }
    // A subagent that fails does not end the turn of the pane's agent.
    if event == Some("StopFailure") && truthy(data.get("agent_id")) {
        return Ok(None);
    }
    let current = tmuxcmd::published(&tmuxcmd::pane_state(host, &env.tmux, pane)?.app);
    // Only the pane's own Claude session may block it, never a nested claude -p.
    if current.get("kind").and_then(Value::as_str) != Some("claude")
        || current.get("id").and_then(Value::as_str) != Some(session_id)
    {
        return Ok(None);
    }
    // Only the status call clears blocked, and it needs the state file to do so.
    let file = current
        .get("pid")
        .and_then(pid_text)
        .and_then(|pid| claude::session_state(env, &pid));
    if !file
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|file| truthy(file.get("status")))
    {
        return Ok(None);
    }
    let started = started.to_string();
    let mut args = vec![
        "set-option",
        "-p",
        "-t",
        pane,
        STATUS,
        "blocked",
        ";",
        "set-option",
        "-p",
        "-t",
        pane,
        WAIT,
        &started,
    ];
    if event == Some("StopFailure") {
        args.extend([";", "set-option", "-p", "-t", pane, FAILED, session_id]);
    }
    tmuxcmd::run(host, &env.tmux, &args)?;
    Ok(None)
}

/// `str(pid)` for the identity a pane holds: its digits, or the text of a string. Anything else
/// names no state file.
fn pid_text(pid: &Value) -> Option<String> {
    match pid {
        Value::Number(number) => number.as_i64().map(|pid| pid.to_string()),
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use serde_json::json;

    fn vars(tmux: &str, pane: &str) -> Option<HookEnv> {
        HookEnv::from_vars(Some(tmux), Some(pane), None, None, || {
            PathBuf::from("/home/me")
        })
    }

    #[test]
    fn the_environment_names_a_pane_of_a_server() {
        let env = vars("/tmp/tmux-501/default,1234,0", "%7").unwrap();
        assert_eq!(env.pane, "%7");
        assert_eq!(env.socket, "/tmp/tmux-501/default");
        assert!(env.tmux[0].ends_with("tmux"), "{:?}", env.tmux);
        assert_eq!(env.tmux[1..], ["-N", "-S", "/tmp/tmux-501/default"]);
    }

    #[test]
    fn a_socket_path_may_hold_commas() {
        let env = vars("/tmp/a,b,c,1234,0", "%0").unwrap();
        assert_eq!(env.socket, "/tmp/a,b,c");
    }

    #[test]
    fn the_environment_is_rejected_unless_tmux_and_the_pane_are_well_formed() {
        for (tmux, pane) in [
            ("", "%1"),
            ("/tmp/s", "%1"),
            ("/tmp/s,1", "%1"),
            ("tmp/s,1,0", "%1"),
            (",1,0", "%1"),
            ("/tmp/s,1,0", ""),
            ("/tmp/s,1,0", "1"),
            ("/tmp/s,1,0", "%"),
            ("/tmp/s,1,0", "%1a"),
            ("/tmp/s,1,0", "x%1"),
            ("/tmp/s,1,0", "%1\n"),
            ("/tmp/s,1,0", "%-1"),
        ] {
            assert!(vars(tmux, pane).is_none(), "{tmux:?} {pane:?}");
        }
        assert!(HookEnv::from_vars(None, Some("%1"), None, None, PathBuf::new).is_none());
        assert!(HookEnv::from_vars(Some("/s,1,0"), None, None, None, PathBuf::new).is_none());
        assert!(vars("/s,1,0", "%123456").is_some());
    }

    // test_agent_status.py::Helper::test_sessions_folder_follows_claude_config_dir

    #[test]
    fn sessions_folder_follows_claude_config_dir() {
        let env = |dir: Option<&str>| {
            HookEnv::from_vars(
                Some("/s,1,0"),
                Some("%1"),
                dir.map(OsString::from),
                None,
                || PathBuf::from("/home/me"),
            )
            .unwrap()
        };
        assert_eq!(
            env(Some("/elsewhere")).sessions,
            PathBuf::from("/elsewhere/sessions")
        );
        assert_eq!(
            env(None).sessions,
            PathBuf::from("/home/me/.claude/sessions")
        );
        assert_eq!(
            env(Some("")).sessions,
            PathBuf::from("/home/me/.claude/sessions"),
            "an empty variable counts as unset"
        );
    }

    #[test]
    fn codex_home_follows_codex_home() {
        let env = |dir: Option<&str>| {
            HookEnv::from_vars(
                Some("/s,1,0"),
                Some("%1"),
                None,
                dir.map(OsString::from),
                || PathBuf::from("/home/me"),
            )
            .unwrap()
        };
        assert_eq!(env(Some("/codex")).codex_home, PathBuf::from("/codex"));
        assert_eq!(env(None).codex_home, PathBuf::from("/home/me/.codex"));
        assert_eq!(env(Some("")).codex_home, PathBuf::from("/home/me/.codex"));
    }

    #[test]
    fn home_is_not_asked_when_both_folders_are_given() {
        let env = HookEnv::from_vars(
            Some("/s,1,0"),
            Some("%1"),
            Some("/a".into()),
            Some("/b".into()),
            || panic!("home asked"),
        );
        assert!(env.is_some());
    }

    fn env() -> HookEnv {
        vars("/tmp/scratch-socket,1,0", "%7").unwrap()
    }

    /// Everything the hook decides before it starts a process: none of these may start one.
    #[test]
    fn a_payload_the_hook_rejects_starts_no_process() {
        let long = "x".repeat(4097);
        let cases = [
            (
                "bogus",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w"}),
            ),
            ("claude", json!([])),
            ("claude", json!("text")),
            ("claude", json!({})),
            ("claude", json!({"session_id": "s", "cwd": "/w"})),
            (
                "claude",
                json!({"hook_event_name": 5, "session_id": "s", "cwd": "/w"}),
            ),
            (
                "claude",
                json!({"hook_event_name": ["SessionStart"], "session_id": "s", "cwd": "/w"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "UserPromptSubmit", "session_id": "s", "cwd": "/w"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "SessionStart", "agent_id": "sub", "session_id": "s", "cwd": "/w"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "SessionStart", "session_id": 5, "cwd": "/w"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "SessionStart", "session_id": "", "cwd": "/w"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "SessionStart", "session_id": long, "cwd": "/w"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "SessionStart", "session_id": "a\nb", "cwd": "/w"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "SessionStart", "session_id": "s"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": 5}),
            ),
            (
                "claude",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "work"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w\tx"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "session_id": "s", "cwd": "/w"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "PreToolUse", "tool_name": 7, "session_id": "s"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "PreToolUse", "session_id": "s"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "StopFailure", "agent_id": "sub", "session_id": "s"}),
            ),
            (
                "claude",
                json!({"hook_event_name": "PermissionRequest", "session_id": ""}),
            ),
            (
                "codex",
                json!({"hook_event_name": "StopFailure", "session_id": "s", "cwd": "/w"}),
            ),
            (
                "codex",
                json!({"hook_event_name": "CwdChanged", "session_id": "s", "cwd": "/w"}),
            ),
            (
                "codex",
                json!({"hook_event_name": "Stop", "agent_id": "sub", "session_id": "s", "cwd": "/w"}),
            ),
            (
                "codex",
                json!({"hook_event_name": "PreToolUse", "session_id": "", "cwd": "/w"}),
            ),
            (
                "codex",
                json!({"hook_event_name": "PermissionRequest", "session_id": "s"}),
            ),
            (
                "codex",
                json!({"hook_event_name": "UserPromptSubmit", "session_id": "s", "cwd": "w"}),
            ),
            (
                "omp",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w"}),
            ),
            (
                "omp",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w", "pid": 1}),
            ),
            (
                "omp",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w", "pid": 0}),
            ),
            (
                "omp",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w", "pid": -5}),
            ),
            (
                "omp",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w", "pid": 7.0}),
            ),
            (
                "omp",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w", "pid": true}),
            ),
            (
                "omp",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w", "pid": "7"}),
            ),
            (
                "omp",
                json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/w", "pid": 99999999999u64}),
            ),
        ];
        for (mode, payload) in cases {
            let host = FakeHost::new();
            assert_eq!(
                run(mode, &payload, &env(), &host).unwrap(),
                None,
                "{mode} {payload}"
            );
            assert!(
                host.calls.borrow().is_empty(),
                "{mode} {payload}: {:?}",
                host.calls.borrow()
            );
        }
    }
}
