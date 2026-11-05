//! The agent hook against tmux. `Helper` fakes tmux and asserts the exact commands the hook writes;
//! `HelperWrites` runs the hook against a scratch server. Only the process lookup and the state file
//! are stand-ins in both. The binary tests at the end check that the hook is silent.

use serde_json::{Map, Value, json};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tmux_workspace::error::Error;
use tmux_workspace::hook::{self, HookEnv, TURN_OVER, tmuxcmd};
use tmux_workspace::host::{Host, RealHost};
use tmux_workspace::pyjson::{Style, dumps};
use tmux_workspace::testing::{FakeHost, Proc, ScratchTmux, TempDir, TmuxMode};
use tmux_workspace::text::py_strip;
use tmux_workspace::tmux::Tmux;

const EXE: &str = env!("CARGO_BIN_EXE_tmux-workspace");
const SESSION: &str = "session-1";
const PID: i32 = 4242;
const BIRTH: &str = "Thu Oct  1 20:23:29 2026";
const CWD: &str = "/work";

fn app_for(session: &str) -> String {
    let identity = json!({"kind": "claude", "id": session, "pid": PID, "cwd": CWD, "birth": BIRTH});
    dumps(&identity, Style::Compact, false)
}

/// The payload of a hook call: a session and a folder, and whatever else `extra` says.
fn payload(extra: Value) -> Value {
    let mut payload = json!({"session_id": SESSION, "cwd": CWD});
    for (key, value) in extra.as_object().expect("an object") {
        payload[key] = value.clone();
    }
    payload
}

fn contains_wait_of_13_digits(write: &str) -> bool {
    let marker = "@workspace_agent_wait ";
    let Some(at) = write.find(marker) else {
        return false;
    };
    let rest = &write[at + marker.len()..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits == 13
        && rest[digits..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
}

// ---------------------------------------------------------------------------------------------
// test_agent_status.py::Helper: a fake tmux. It records every call that is not a read.

#[derive(Clone, Default)]
struct PaneFields {
    agent: String,
    wait: String,
    failed: String,
    turn_over: String,
    app: String,
}

struct Helper {
    sessions: TempDir,
    pane: Rc<RefCell<PaneFields>>,
    writes: Rc<RefCell<Vec<String>>>,
    host: FakeHost,
    env: HookEnv,
}

impl Helper {
    fn new() -> Helper {
        let pane = Rc::new(RefCell::new(PaneFields {
            turn_over: "done".into(),
            app: app_for(SESSION),
            ..PaneFields::default()
        }));
        let writes = Rc::new(RefCell::new(Vec::new()));
        let (seen_pane, seen_writes) = (Rc::clone(&pane), Rc::clone(&writes));
        let mut host = FakeHost::scripted(move |argv| {
            let pane = seen_pane.borrow();
            if argv.iter().any(|arg| arg == "display-message") {
                let fields = [
                    "99",
                    "/dev/ttys001",
                    &pane.agent,
                    &pane.wait,
                    &pane.failed,
                    &pane.turn_over,
                    &pane.app,
                ];
                return Ok(py_strip(&fields.join("\t")).to_string());
            }
            if argv.iter().any(|arg| arg == "show-option") {
                return Ok(pane.app.clone());
            }
            seen_writes.borrow_mut().push(argv.join(" "));
            Ok(String::new())
        });
        // The process tree that makes `owner` give (PID, BIRTH), as the Python mock does.
        host.parent = PID;
        host.procs.insert(
            PID,
            Proc {
                args: "claude".into(),
                pgid: PID,
                tpgid: PID,
                lstart: BIRTH.into(),
                ..Proc::new(99, "ttys001", "claude")
            },
        );
        host.procs.insert(99, Proc::new(1, "ttys001", "sleep"));
        let sessions = TempDir::new();
        let env = HookEnv {
            pane: "%7".into(),
            socket: "/tmp/scratch-socket".into(),
            tmux: ["/opt/homebrew/bin/tmux", "-N", "-S", "/tmp/scratch-socket"]
                .map(String::from)
                .to_vec(),
            sessions: sessions.path().to_path_buf(),
            codex_home: sessions.path().to_path_buf(),
        };
        Helper {
            sessions,
            pane,
            writes,
            host,
            env,
        }
    }

    fn set(&self, change: impl FnOnce(&mut PaneFields)) {
        change(&mut self.pane.borrow_mut());
    }

    fn call(&self, mode: &str, extra: Value) -> String {
        hook::run(mode, &payload(extra), &self.env, &self.host)
            .expect("the hook does not fail")
            .unwrap_or_default()
    }

    fn file(&self, state: Value) {
        std::fs::write(
            self.sessions.path().join(format!("{PID}.json")),
            state.to_string(),
        )
        .unwrap();
    }

    fn writes(&self) -> Vec<String> {
        self.writes.borrow().clone()
    }

    fn blocked(&self, event: &str, extra: Value) -> Vec<String> {
        self.file(json!({"status": "busy", "statusUpdatedAt": 5}));
        self.set(|pane| pane.agent = "working".into());
        let mut extra = extra;
        extra["hook_event_name"] = event.into();
        assert_eq!(self.call("claude", extra), "");
        self.writes()
    }
}

#[test]
fn status_call_writes_working_as_a_compare_and_set() {
    let helper = Helper::new();
    helper.file(json!({"status": "busy", "statusUpdatedAt": 5}));
    helper.set(|pane| pane.agent = "idle".into());
    assert_eq!(
        helper.call("claude-status", json!({})),
        "Claude | session-\n"
    );
    let writes = helper.writes();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].contains("if-shell"), "{}", writes[0]);
    assert!(
        writes[0].contains("#{==:#{@workspace_agent},idle}"),
        "{}",
        writes[0]
    );
    assert!(
        writes[0].contains("@workspace_agent 'working'"),
        "{}",
        writes[0]
    );
}

#[test]
fn status_write_is_guarded_by_the_wait_value_that_was_read() {
    let helper = Helper::new();
    helper.file(json!({"status": "busy", "statusUpdatedAt": 9}));
    helper.set(|pane| {
        pane.agent = "blocked".into();
        pane.wait = "5".into();
    });
    helper.call("claude-status", json!({}));
    let writes = helper.writes();
    assert_eq!(writes.len(), 1);
    assert!(
        writes[0]
            .contains("#{&&:#{==:#{@workspace_agent},blocked},#{==:#{@workspace_agent_wait},5}}"),
        "{}",
        writes[0]
    );
}

#[test]
fn status_call_issues_no_write_when_nothing_changed() {
    let helper = Helper::new();
    helper.file(json!({"status": "busy", "statusUpdatedAt": 5}));
    helper.set(|pane| pane.agent = "working".into());
    assert_eq!(
        helper.call("claude-status", json!({})),
        "Claude | session-\n"
    );
    assert!(helper.writes().is_empty());
}

#[test]
fn status_call_without_a_state_file_leaves_the_status() {
    let helper = Helper::new();
    helper.set(|pane| pane.agent = "working".into());
    helper.call("claude-status", json!({}));
    assert!(helper.writes().is_empty());
}

#[test]
fn status_call_writes_the_identity_only_when_it_changed() {
    let helper = Helper::new();
    helper.set(|pane| pane.app = String::new());
    helper.call("claude-status", json!({}));
    let writes = helper.writes();
    assert_eq!(writes.len(), 1);
    assert!(
        writes[0].contains("#{==:#{@workspace_app},}"),
        "{}",
        writes[0]
    );
    assert!(writes[0].contains("@workspace_app"), "{}", writes[0]);
}

#[test]
fn turn_over_is_decided_by_tmux_inside_the_compare_and_set() {
    let helper = Helper::new();
    helper.file(json!({"status": "idle", "statusUpdatedAt": 5}));
    helper.set(|pane| pane.agent = "working".into());
    helper.call("claude-status", json!({}));
    let writes = helper.writes();
    assert_eq!(writes.len(), 1);
    assert!(
        writes[0].contains("#{==:#{@workspace_agent},working}"),
        "{}",
        writes[0]
    );
    assert!(
        writes[0].contains(&format!(
            "set-option -pF -t %7 @workspace_agent '{TURN_OVER}'"
        )),
        "{}",
        writes[0]
    );
}

#[test]
fn new_session_unsets_the_old_status_with_the_identity() {
    let helper = Helper::new();
    helper.file(json!({"status": "busy", "statusUpdatedAt": 5}));
    helper.set(|pane| {
        pane.agent = "blocked".into();
        pane.wait = "9".into();
        pane.failed = "older-session".into();
        pane.app = app_for("older-session");
    });
    helper.call("claude-status", json!({}));
    let writes = helper.writes();
    assert_eq!(writes.len(), 2);
    assert!(writes[0].contains("@workspace_app '"), "{}", writes[0]);
    for option in [
        "@workspace_agent",
        "@workspace_agent_wait",
        "@workspace_agent_failed",
    ] {
        assert!(
            writes[0].contains(&format!("set-option -pu -t %7 {option}")),
            "{option}: {}",
            writes[0]
        );
    }
    assert!(
        writes[1].contains("#{&&:#{==:#{@workspace_agent},},#{==:#{@workspace_agent_wait},}}"),
        "{}",
        writes[1]
    );
    assert!(
        writes[1].contains("@workspace_agent 'working'"),
        "{}",
        writes[1]
    );
}

#[test]
fn changed_folder_keeps_the_status() {
    let helper = Helper::new();
    helper.set(|pane| pane.agent = "working".into());
    helper.call("claude-status", json!({"cwd": "/elsewhere"}));
    let writes = helper.writes();
    assert_eq!(writes.len(), 1);
    assert!(!writes[0].contains("-pu"), "{}", writes[0]);
}

#[test]
fn identity_that_is_not_an_object_changes_nothing() {
    let helper = Helper::new();
    helper.file(json!({"status": "busy"}));
    for app in ["[1]", "\"text\"", "7", "not json"] {
        helper.set(|pane| pane.app = app.into());
        assert_eq!(
            helper.call("claude", json!({"hook_event_name": "PermissionRequest"})),
            ""
        );
        assert_eq!(
            helper.call("claude", json!({"hook_event_name": "SessionEnd"})),
            ""
        );
        assert_eq!(
            helper.call(
                "claude",
                json!({"hook_event_name": "SessionStart", "source": "fork"})
            ),
            ""
        );
        assert!(helper.writes().is_empty(), "{app}: {:?}", helper.writes());
    }
}

#[test]
fn hook_does_not_block_without_a_state_file_that_could_clear_it() {
    let helper = Helper::new();
    helper.set(|pane| pane.agent = "working".into());
    helper.call("claude", json!({"hook_event_name": "PermissionRequest"}));
    helper.file(json!({"pid": PID}));
    helper.call("claude", json!({"hook_event_name": "StopFailure"}));
    assert!(helper.writes().is_empty());
}

#[test]
fn stop_failure_of_a_subagent_changes_nothing() {
    let helper = Helper::new();
    assert!(
        helper
            .blocked("StopFailure", json!({"agent_id": "sub-1"}))
            .is_empty()
    );
}

#[test]
fn permission_request_blocks_and_records_the_time() {
    let helper = Helper::new();
    let writes = helper.blocked("PermissionRequest", json!({"tool_name": "Bash"}));
    assert_eq!(writes.len(), 1);
    assert!(
        writes[0].contains("@workspace_agent blocked"),
        "{}",
        writes[0]
    );
    assert!(contains_wait_of_13_digits(&writes[0]), "{}", writes[0]);
    assert!(
        !writes[0].contains("@workspace_agent_failed"),
        "{}",
        writes[0]
    );
}

#[test]
fn question_and_plan_block() {
    let helper = Helper::new();
    for tool in ["AskUserQuestion", "ExitPlanMode"] {
        helper.writes.borrow_mut().clear();
        let writes = helper.blocked("PreToolUse", json!({"tool_name": tool}));
        assert!(
            writes[0].contains("@workspace_agent blocked"),
            "{tool}: {}",
            writes[0]
        );
    }
}

#[test]
fn other_tools_do_not_block() {
    let helper = Helper::new();
    assert!(
        helper
            .blocked("PreToolUse", json!({"tool_name": "Bash"}))
            .is_empty()
    );
}

#[test]
fn stop_failure_blocks_and_marks_the_turn_failed() {
    let helper = Helper::new();
    let writes = helper.blocked("StopFailure", json!({}));
    assert!(
        writes[0].contains("@workspace_agent blocked"),
        "{}",
        writes[0]
    );
    assert!(
        writes[0].contains(&format!("@workspace_agent_failed {SESSION}")),
        "{}",
        writes[0]
    );
}

#[test]
fn subagent_approval_counts_for_blocked() {
    let helper = Helper::new();
    let writes = helper.blocked("PermissionRequest", json!({"agent_id": "sub-1"}));
    assert!(
        writes[0].contains("@workspace_agent blocked"),
        "{}",
        writes[0]
    );
}

#[test]
fn nested_claude_changes_nothing() {
    let helper = Helper::new();
    let nested = json!({"session_id": "nested-print-session"});
    assert!(
        helper
            .blocked("PermissionRequest", nested.clone())
            .is_empty()
    );
    assert!(helper.blocked("StopFailure", nested).is_empty());
}

#[test]
fn pane_without_an_identity_changes_nothing() {
    let helper = Helper::new();
    helper.set(|pane| pane.app = String::new());
    assert!(helper.blocked("PermissionRequest", json!({})).is_empty());
}

#[test]
fn session_end_clears_the_status_with_the_identity() {
    let helper = Helper::new();
    assert_eq!(
        helper.call("claude", json!({"hook_event_name": "SessionEnd"})),
        ""
    );
    let writes = helper.writes();
    assert_eq!(writes.len(), 1);
    for option in [
        "@workspace_app",
        "@workspace_agent",
        "@workspace_agent_wait",
        "@workspace_agent_failed",
    ] {
        assert!(
            writes[0].contains(&format!("set-option -pu -t %7 {option}")),
            "{option}: {}",
            writes[0]
        );
    }
}

// Beyond the Python tests.

#[test]
fn claude_status_prints_the_short_session_id() {
    let helper = Helper::new();
    helper.set(|pane| pane.app = app_for("session-123"));
    assert_eq!(
        helper.call("claude-status", json!({"session_id": "session-123"})),
        "Claude | session-\n"
    );
}

#[test]
fn the_short_session_id_counts_characters() {
    let helper = Helper::new();
    let session = "éééééééééé";
    helper.set(|pane| pane.app = app_for(session));
    assert_eq!(
        helper.call("claude-status", json!({"session_id": session})),
        "Claude | éééééééé\n"
    );
    assert_eq!(
        helper.call("claude-status", json!({"session_id": "abc"})),
        "Claude | abc\n",
        "a short id is printed whole (and its identity written)"
    );
}

#[test]
fn the_identity_is_written_as_python_would_dump_it() {
    let helper = Helper::new();
    helper.set(|pane| pane.app = String::new());
    helper.call(
        "claude-status",
        json!({"session_id": "é😀/\"", "cwd": "/w\u{e9}"}),
    );
    let writes = helper.writes();
    assert_eq!(writes.len(), 1);
    let identity = format!(
        r#"{{"kind":"claude","id":"\u00e9\ud83d\ude00/\"","pid":{PID},"cwd":"/w\u00e9","birth":"{BIRTH}"}}"#
    );
    assert!(
        writes[0].contains(&format!("@workspace_app '{identity}'")),
        "{}",
        writes[0]
    );
}

#[test]
fn only_the_status_mode_prints() {
    let helper = Helper::new();
    helper.file(json!({"status": "busy", "statusUpdatedAt": 5}));
    for event in ["SessionStart", "SessionEnd", "CwdChanged"] {
        assert_eq!(
            helper.call("claude", json!({"hook_event_name": event})),
            "",
            "{event}"
        );
    }
}

#[test]
fn the_status_line_still_prints_when_the_status_write_fails() {
    let helper = Helper::new();
    helper.file(json!({"status": "busy", "statusUpdatedAt": 5}));
    helper.set(|pane| pane.agent = "idle".into());
    let host = FakeHost {
        procs: helper.host.procs.clone(),
        parent: PID,
        ..FakeHost::scripted(|argv| {
            if argv.iter().any(|arg| arg == "if-shell") {
                return Err(Error::Msg("tmux went away".into()));
            }
            Ok([
                "99",
                "/dev/ttys001",
                "idle",
                "",
                "",
                "done",
                &app_for(SESSION),
            ]
            .join("\t"))
        })
    };
    let printed = hook::run("claude-status", &payload(json!({})), &helper.env, &host).unwrap();
    assert_eq!(printed.as_deref(), Some("Claude | session-\n"));
}

#[test]
fn the_status_line_is_not_printed_when_the_identity_write_fails() {
    let helper = Helper::new();
    helper.set(|pane| pane.app = String::new());
    let host = FakeHost {
        procs: helper.host.procs.clone(),
        parent: PID,
        ..FakeHost::scripted(|argv| {
            if argv.iter().any(|arg| arg == "if-shell") {
                return Err(Error::Msg("tmux went away".into()));
            }
            Ok("99\t/dev/ttys001".to_string())
        })
    };
    assert!(hook::run("claude-status", &payload(json!({})), &helper.env, &host).is_err());
}

#[test]
fn the_hook_asks_tmux_for_the_pane_once_and_ps_only_for_ancestors() {
    let helper = Helper::new();
    helper.file(json!({"status": "busy", "statusUpdatedAt": 5}));
    helper.call("claude-status", json!({}));
    assert_eq!(
        helper
            .host
            .calls_to("tmux")
            .iter()
            .filter(|call| call.iter().any(|arg| arg == "display-message"))
            .count(),
        1
    );
    for call in helper.host.calls_to("ps") {
        assert!(!call.contains(&"-axo".to_string()), "{call:?}");
    }
}

#[test]
fn fork_clears_an_identity_of_the_same_process_and_leaves_another() {
    let helper = Helper::new();
    helper.set(|pane| pane.app = app_for("the-old-conversation"));
    assert_eq!(
        helper.call(
            "claude",
            json!({"hook_event_name": "SessionStart", "source": "fork"})
        ),
        ""
    );
    let writes = helper.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(
        writes[0].contains("set-option -pu -t %7 @workspace_app"),
        "{}",
        writes[0]
    );
    // An identity of another process stays.
    let stranger = json!({"kind": "claude", "id": "x", "pid": 1, "cwd": CWD, "birth": BIRTH});
    helper.set(|pane| pane.app = dumps(&stranger, Style::Compact, false));
    helper.writes.borrow_mut().clear();
    helper.call(
        "claude",
        json!({"hook_event_name": "SessionStart", "source": "fork"}),
    );
    assert!(helper.writes().is_empty());
}

#[test]
fn omp_publishes_its_identity_for_the_pid_it_names() {
    let mut helper = Helper::new();
    helper.host.procs.get_mut(&PID).unwrap().comm = "omp".into();
    helper.host.procs.get_mut(&PID).unwrap().args = "omp".into();
    helper.set(|pane| pane.app = String::new());
    let omp = json!({"hook_event_name": "SessionStart", "pid": PID});
    assert_eq!(helper.call("omp", omp.clone()), "");
    let writes = helper.writes();
    assert_eq!(writes.len(), 1);
    let identity =
        format!(r#"{{"kind":"omp","id":"{SESSION}","pid":{PID},"cwd":"{CWD}","birth":"{BIRTH}"}}"#);
    assert!(
        writes[0].contains(&format!("@workspace_app '{identity}'")),
        "{}",
        writes[0]
    );
    // A pid that is not the agent's owner publishes nothing.
    helper.writes.borrow_mut().clear();
    assert_eq!(
        helper.call("omp", json!({"hook_event_name": "SessionStart", "pid": 77})),
        ""
    );
    assert!(helper.writes().is_empty());
}

// ---------------------------------------------------------------------------------------------
// test_agent_status_tmux.py::HelperWrites: the real hook against a scratch server.

struct World {
    scratch: ScratchTmux,
    hidden: String,
    sessions: TempDir,
    env: HookEnv,
}

impl World {
    fn new() -> World {
        let scratch = ScratchTmux::new();
        scratch.tmux(&[
            "source-file",
            concat!(env!("CARGO_MANIFEST_DIR"), "/agent-status.conf"),
        ]);
        scratch.tmux(&["set-option", "-g", "@agent_sound_cmd", "true"]); // Tests never play a real sound.
        let hidden = scratch.pane(&["new-window", "-t", "alpha:", "-n", "two"]);
        let sessions = TempDir::new();
        // The hook addresses the scratch socket by its path and never the default server.
        let tmux = Tmux::new().socket(scratch.socket()).argv(&[]);
        let env = HookEnv {
            pane: hidden.clone(),
            socket: scratch.socket().to_string(),
            tmux,
            sessions: sessions.path().to_path_buf(),
            codex_home: sessions.path().to_path_buf(),
        };
        World {
            scratch,
            hidden,
            sessions,
            env,
        }
    }

    /// A host whose process tree makes `owner` give (PID, BIRTH): the pane's process runs `sleep`
    /// and PID, its child, runs Claude.
    fn host(&self, tmux: TmuxMode) -> FakeHost {
        self.host_of("claude", tmux)
    }

    /// The same tree with `kind` (`claude` or `codex`) as the agent that runs in the pane.
    fn host_of(&self, kind: &str, tmux: TmuxMode) -> FakeHost {
        let pane_pid: i32 = self
            .scratch
            .expand(&self.hidden, "#{pane_pid}")
            .parse()
            .unwrap();
        let tty = self.scratch.expand(&self.hidden, "#{pane_tty}");
        let tty = tty.strip_prefix("/dev/").unwrap().to_string();
        let mut host = FakeHost {
            tmux,
            parent: PID,
            ..FakeHost::new()
        };
        host.procs.insert(
            PID,
            Proc {
                args: kind.into(),
                pgid: PID,
                tpgid: PID,
                lstart: BIRTH.into(),
                ..Proc::new(pane_pid, &tty, kind)
            },
        );
        host.procs.insert(pane_pid, Proc::new(1, &tty, "sleep"));
        host
    }

    /// A host in which no agent runs in the pane: `owner` gives none.
    fn host_without_agent(&self, tmux: TmuxMode) -> FakeHost {
        let pane_pid: i32 = self
            .scratch
            .expand(&self.hidden, "#{pane_pid}")
            .parse()
            .unwrap();
        let tty = self.scratch.expand(&self.hidden, "#{pane_tty}");
        let mut host = FakeHost {
            tmux,
            parent: pane_pid,
            ..FakeHost::new()
        };
        host.procs.insert(
            pane_pid,
            Proc::new(1, tty.strip_prefix("/dev/").unwrap(), "sleep"),
        );
        host
    }

    fn call(&self, mode: &str, extra: Value) {
        self.call_on(&self.host(TmuxMode::Real), mode, extra);
    }

    fn call_on(&self, host: &FakeHost, mode: &str, extra: Value) {
        let printed =
            hook::run(mode, &payload(extra), &self.env, host).expect("the hook does not fail");
        assert!(printed.is_none_or(|text| text.starts_with("Claude | ")));
    }

    fn file(&self, state: Value) {
        std::fs::write(
            self.sessions.path().join(format!("{PID}.json")),
            state.to_string(),
        )
        .unwrap();
    }

    fn option(&self, name: &str) -> String {
        self.scratch
            .tmux(&["show-options", "-pqv", "-t", &self.hidden, name])
    }

    fn status(&self) -> String {
        self.option("@workspace_agent")
    }

    fn set_status(&self, value: &str) {
        if value.is_empty() {
            self.scratch
                .tmux(&["set-option", "-pu", "-t", &self.hidden, "@workspace_agent"]);
        } else {
            self.scratch.tmux(&[
                "set-option",
                "-p",
                "-t",
                &self.hidden,
                "@workspace_agent",
                value,
            ]);
        }
    }

    fn identity(&self) -> Map<String, Value> {
        tmuxcmd::published(&self.option("@workspace_app"))
    }
}

#[test]
fn turn_over_without_the_conf_gives_idle() {
    let world = World::new();
    world
        .scratch
        .tmux(&["set-option", "-gu", "@agent_turn_over"]);
    world.call("claude-status", json!({}));
    world.set_status("working");
    world.file(json!({"status": "idle", "statusUpdatedAt": 5}));
    world.call("claude-status", json!({}));
    assert_eq!(world.status(), "idle");
}

#[test]
fn turn_over_with_the_conf_and_no_client_gives_done() {
    let world = World::new();
    world.call("claude-status", json!({}));
    world.set_status("working");
    world.file(json!({"status": "idle", "statusUpdatedAt": 5}));
    world.call("claude-status", json!({}));
    assert_eq!(world.status(), "done");
}

#[test]
fn write_does_nothing_when_the_status_or_the_wait_changed_underneath() {
    let world = World::new();
    world.call("claude-status", json!({}));
    world.file(json!({"status": "idle", "statusUpdatedAt": 5}));
    for (option, value) in [
        ("@workspace_agent", "blocked"),
        ("@workspace_agent_wait", "7"),
    ] {
        world.set_status("working");
        // Another writer changes the pane right after the hook has read it.
        let (socket, hidden) = (world.scratch.socket().to_string(), world.hidden.clone());
        let fired = Cell::new(false);
        let host = world.host(TmuxMode::Scripted(Box::new(move |argv| {
            let out = RealHost.run(argv)?;
            if argv.iter().any(|arg| arg == "display-message") && !fired.replace(true) {
                let change = [
                    "tmux",
                    "-S",
                    &socket,
                    "set-option",
                    "-p",
                    "-t",
                    &hidden,
                    option,
                    value,
                ];
                RealHost.run(&change.map(String::from))?;
            }
            Ok(out)
        })));
        world.call_on(&host, "claude-status", json!({}));
        assert_eq!(world.option(option), value);
        assert!(
            ["working", "blocked"].contains(&world.status().as_str()),
            "{}",
            world.status()
        );
    }
}

#[test]
fn hook_blocks_and_a_newer_file_returns_to_working() {
    let world = World::new();
    world.call("claude-status", json!({}));
    world.file(json!({"status": "busy", "statusUpdatedAt": 5}));
    world.call("claude", json!({"hook_event_name": "StopFailure"}));
    assert_eq!(world.status(), "blocked");
    assert_eq!(world.option("@workspace_agent_failed"), SESSION);
    world.call("claude-status", json!({}));
    assert_eq!(world.status(), "blocked");
    let wait: i64 = world.option("@workspace_agent_wait").parse().unwrap();
    world.file(json!({"status": "busy", "statusUpdatedAt": wait + 1}));
    world.call("claude-status", json!({}));
    assert_eq!(world.status(), "working");
    assert_eq!(world.option("@workspace_agent_failed"), "");
}

#[test]
fn identity_with_special_characters_survives_the_write_and_the_guard() {
    let world = World::new();
    let session = "it's #1, {a} \"b\" #{pane_id} $HOME ; kill-server";
    world.call("claude-status", json!({"session_id": session}));
    let expected =
        json!({"kind": "claude", "id": session, "pid": PID, "cwd": "/work", "birth": BIRTH});
    assert_eq!(Value::Object(world.identity()), expected);
    world.set_status("working");
    world.call(
        "claude-status",
        json!({"session_id": session, "cwd": "/else, where}"}),
    );
    let mut moved = expected.clone();
    moved["cwd"] = json!("/else, where}");
    assert_eq!(Value::Object(world.identity()), moved);
    assert_eq!(world.status(), "working");
    world.call(
        "claude",
        json!({"session_id": session, "cwd": "/else, where}", "hook_event_name": "SessionEnd"}),
    );
    assert_eq!(
        (world.option("@workspace_app"), world.status()),
        (String::new(), String::new())
    );
}

#[test]
fn new_session_ends_up_idle_and_never_done() {
    let world = World::new();
    world.call("claude-status", json!({"session_id": "killed-session"}));
    world.set_status("working");
    world.scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &world.hidden,
        "@workspace_agent_failed",
        "killed-session",
    ]);
    world.file(json!({"status": "idle", "statusUpdatedAt": 5}));
    world.call("claude-status", json!({"session_id": "fresh-session"}));
    assert_eq!(world.status(), "idle");
    assert_eq!(world.option("@workspace_agent_failed"), "");
}

#[test]
fn the_status_line_command_runs_in_the_world_and_prints_the_short_id() {
    let world = World::new();
    let printed = hook::run(
        "claude-status",
        &payload(json!({"session_id": "session-123"})),
        &world.env,
        &world.host(TmuxMode::Real),
    )
    .unwrap();
    assert_eq!(printed.as_deref(), Some("Claude | session-\n"));
}

// ---------------------------------------------------------------------------------------------
// test_agent_status_tmux.py::HelperWrites, Codex: the real hook against a scratch server.

impl World {
    /// A Codex hook call from the pane's own Codex.
    fn codex(&self, extra: Value) {
        self.codex_on(&self.host_of("codex", TmuxMode::Real), extra);
    }

    fn codex_on(&self, host: &FakeHost, extra: Value) {
        let printed =
            hook::run("codex", &payload(extra), &self.env, host).expect("the hook does not fail");
        assert_eq!(printed, None, "only the status line of Claude prints");
    }

    fn status_of(&self, pane: &str) -> String {
        self.scratch
            .tmux(&["show-options", "-pqv", "-t", pane, "@workspace_agent"])
    }
}

/// A control-mode client of the session. tmux counts it as a terminal with focus.
struct ControlClient {
    child: std::process::Child,
}

impl ControlClient {
    fn attach(scratch: &ScratchTmux, session: &str) -> ControlClient {
        // `-S` names the scratch socket, so this tmux reaches no other server; `TMUX` goes because a
        // client that has it set refuses to attach.
        let child = scratch
            .bin("tmux")
            .env_remove("TMUX")
            .args([
                "-S",
                scratch.socket(),
                "-C",
                "attach-session",
                "-t",
                session,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start a control client");
        let focused = || {
            scratch
                .tmux(&["list-clients", "-t", session, "-F", "#{client_flags}"])
                .matches("focused")
                .count()
                .to_string()
        };
        tmux_workspace::testing::wait_for(focused, "1");
        ControlClient { child }
    }
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

#[test]
fn detached_codex_routes_by_thread_title_not_inherited_pane() {
    let world = World::new();
    let session = "01a128f8-8621-7561-8972-8d02a1234567";
    let title = format!("codex | {}...", &session[..29]);
    let actual = world
        .scratch
        .pane(&["new-window", "-t", "alpha:", "-n", "actual"]);
    world
        .scratch
        .tmux(&["select-pane", "-t", &actual, "-T", &title]);
    // Codex runs in `actual`; the hook inherited TMUX_PANE of `hidden`, so `owner` finds nothing
    // there and the title finds the pane.
    let pane_pid: i32 = world
        .scratch
        .expand(&actual, "#{pane_pid}")
        .parse()
        .unwrap();
    let tty = world.scratch.expand(&actual, "#{pane_tty}");
    let tty = tty.strip_prefix("/dev/").unwrap();
    let mut host = FakeHost {
        parent: PID,
        ..FakeHost::new()
    };
    host.procs.insert(
        PID,
        Proc {
            args: "codex".into(),
            pgid: PID,
            tpgid: PID,
            lstart: "birth".into(),
            ..Proc::new(pane_pid, tty, "codex")
        },
    );
    host.procs.insert(pane_pid, Proc::new(1, tty, "sleep"));
    let call = |event: &str| {
        world.codex_on(
            &host,
            json!({"session_id": session, "hook_event_name": event, "turn_id": "turn"}),
        )
    };
    call("UserPromptSubmit");
    call("PermissionRequest");
    assert_eq!(world.status_of(&actual), "blocked");
    assert_eq!(world.status(), "");
    // Two panes with the title are ambiguous: nothing is published.
    world
        .scratch
        .tmux(&["select-pane", "-t", &world.hidden, "-T", &title]);
    call("Stop");
    assert_eq!(world.status_of(&actual), "blocked");
    assert_eq!(world.status(), "");
}

#[test]
fn codex_lifecycle_and_unrelated_tool_cannot_clear_approval() {
    let world = World::new();
    world.codex(json!({"hook_event_name": "SessionStart"}));
    assert_eq!(world.status(), "idle");
    world.codex(json!({"hook_event_name": "UserPromptSubmit", "turn_id": "turn-1"}));
    assert_eq!(world.status(), "working");
    world.codex(
        json!({"hook_event_name": "PermissionRequest", "turn_id": "turn-1", "tool_name": "Bash"}),
    );
    assert_eq!(world.status(), "blocked");
    world
        .codex(json!({"hook_event_name": "PostToolUse", "turn_id": "turn-1", "tool_name": "Read"}));
    assert_eq!(world.status(), "blocked");
    world.codex(json!({"hook_event_name": "PostToolUse", "turn_id": "turn-1", "tool_name": "Bash", "tool_use_id": "call-1"}));
    assert_eq!(world.status(), "working");
    world.codex(json!({"hook_event_name": "Stop", "turn_id": "turn-1"}));
    assert_eq!(world.status(), "done");
    world
        .codex(json!({"hook_event_name": "PostToolUse", "turn_id": "turn-1", "tool_name": "Bash"}));
    assert_eq!(world.status(), "done");
    world.codex(json!({"hook_event_name": "SessionEnd"}));
    assert_eq!(world.option("@workspace_app"), "");
    assert_eq!(world.option("@workspace_codex"), "");
    assert_eq!(world.status(), "");
}

#[test]
fn codex_question_and_interrupt_and_old_turn() {
    let world = World::new();
    world.codex(json!({"hook_event_name": "UserPromptSubmit", "turn_id": "turn-2"}));
    world.codex(json!({"hook_event_name": "Stop", "turn_id": "turn-1"}));
    assert_eq!(world.status(), "working");
    world.codex(json!({"hook_event_name": "PreToolUse", "turn_id": "turn-2",
                       "tool_name": "request_user_input", "tool_use_id": "question"}));
    assert_eq!(world.status(), "blocked");
    world.codex(
        json!({"hook_event_name": "PostToolUse", "turn_id": "turn-2",
                       "tool_name": "request_user_input", "tool_use_id": "question"}),
    );
    assert_eq!(world.status(), "working");
    world.codex(json!({"hook_event_name": "Interrupt", "turn_id": "turn-2"}));
    assert_eq!(world.status(), "idle");
}

#[test]
fn codex_compaction_preserves_turn_and_subagents_cannot_publish() {
    let world = World::new();
    world.codex(json!({"hook_event_name": "UserPromptSubmit", "turn_id": "turn-1"}));
    world.codex(json!({"hook_event_name": "SessionStart", "source": "compact"}));
    assert_eq!(world.status(), "working");
    world.codex(json!({"hook_event_name": "Stop", "turn_id": "turn-1", "agent_id": "child"}));
    assert_eq!(world.status(), "working");
    world.codex(
        json!({"hook_event_name": "Stop", "turn_id": "turn-1", "session_id": "other-session"}),
    );
    assert_eq!(world.status(), "working");
    world.codex(json!({"hook_event_name": "SessionEnd", "session_id": "other-session"}));
    assert_eq!(world.status(), "working");
    // No Codex owns the pane any more.
    world.codex_on(
        &world.host_without_agent(TmuxMode::Real),
        json!({"hook_event_name": "Stop", "turn_id": "turn-1"}),
    );
    assert_eq!(world.status(), "working");
}

#[test]
fn codex_multiple_pending_questions_and_visible_completion() {
    let world = World::new();
    world.codex(json!({"hook_event_name": "UserPromptSubmit", "turn_id": "turn-1"}));
    for key in ["a", "b"] {
        world.codex(json!({"hook_event_name": "PreToolUse", "turn_id": "turn-1",
                           "tool_name": "request_user_input", "tool_use_id": key}));
    }
    world.codex(
        json!({"hook_event_name": "PostToolUse", "turn_id": "turn-1",
                       "tool_name": "request_user_input", "tool_use_id": "a"}),
    );
    assert_eq!(world.status(), "blocked");
    world.codex(
        json!({"hook_event_name": "PostToolUse", "turn_id": "turn-1",
                       "tool_name": "request_user_input", "tool_use_id": "b"}),
    );
    assert_eq!(world.status(), "working");
    let _client = ControlClient::attach(&world.scratch, "alpha");
    world.scratch.tmux(&["select-window", "-t", &world.hidden]);
    world.codex(json!({"hook_event_name": "Stop", "turn_id": "turn-1"}));
    assert_eq!(world.status(), "idle");
}

#[test]
fn codex_parallel_approvals_match_arguments_without_recording_them() {
    let world = World::new();
    world.codex(json!({"hook_event_name": "UserPromptSubmit", "turn_id": "turn-1"}));
    for command in ["private command one", "private command two"] {
        world.codex(
            json!({"hook_event_name": "PermissionRequest", "turn_id": "turn-1",
                           "tool_name": "Bash", "tool_input": {"command": command}}),
        );
    }
    assert!(!world.option("@workspace_codex").contains("private command"));
    world.codex(
        json!({"hook_event_name": "PostToolUse", "turn_id": "turn-1", "tool_name": "Bash",
                       "tool_use_id": "first", "tool_input": {"command": "private command one"}}),
    );
    assert_eq!(world.status(), "blocked");
    world.codex(
        json!({"hook_event_name": "PostToolUse", "turn_id": "turn-1", "tool_name": "Bash",
                       "tool_use_id": "second", "tool_input": {"command": "private command two"}}),
    );
    assert_eq!(world.status(), "working");
}

#[test]
fn codex_retries_when_another_hook_wins_its_compare_and_set() {
    let world = World::new();
    world.codex(json!({"hook_event_name": "UserPromptSubmit", "turn_id": "turn-1"}));
    let raced = Rc::new(Cell::new(false));
    let seen = Rc::clone(&raced);
    let (socket, hidden) = (world.scratch.socket().to_string(), world.hidden.clone());
    let host = world.host_of(
        "codex",
        TmuxMode::Scripted(Box::new(move |argv| {
            if argv.iter().any(|arg| arg == "if-shell") && !seen.replace(true) {
                // Another hook writes the record right after this one has read it.
                let tmux = |args: &[&str]| {
                    let mut argv = vec!["tmux".to_string(), "-S".into(), socket.clone()];
                    argv.extend(args.iter().map(|arg| arg.to_string()));
                    RealHost.run(&argv)
                };
                let read = tmux(&["show-options", "-t", &hidden, "-pqv", "@workspace_codex"])?;
                let mut record: Value = serde_json::from_str(&read).unwrap();
                record["pending"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("other-approval"));
                let record = dumps(&record, Style::Default, false);
                tmux(&[
                    "set-option",
                    "-t",
                    &hidden,
                    "-p",
                    "@workspace_codex",
                    &record,
                ])?;
            }
            RealHost.run(argv)
        })),
    );
    world.codex_on(
        &host,
        json!({"hook_event_name": "PermissionRequest", "turn_id": "turn-1", "tool_name": "Bash"}),
    );
    assert!(raced.get());
    let record: Value = serde_json::from_str(&world.option("@workspace_codex")).unwrap();
    assert_eq!(record["pending"], json!(["other-approval", "Bash"]));
    assert_eq!(world.status(), "blocked");
}

// ---------------------------------------------------------------------------------------------
// The binary: silent whatever happens.

/// `tmux-workspace agent-hook <mode>` with `stdin`, in a world of its own.
fn hook_binary(mode: &str, tmux: &str, pane: &str, stdin: Vec<u8>) -> (Output, Duration) {
    let home = TempDir::new();
    let mut command = Command::new(EXE);
    command
        .args(["agent-hook", mode])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("TMUX", tmux)
        .env("TMUX_PANE", pane)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    // The hook may stop reading, or never read, so a failed write is no failure.
    let writer = std::thread::spawn(move || {
        let _ = pipe.write_all(&stdin);
    });
    let out = child.wait_with_output().unwrap();
    let _ = writer.join();
    (out, started.elapsed())
}

fn assert_silent(out: &Output, what: &str) {
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{what}: stdout");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "{what}: stderr");
    assert_eq!(out.status.code(), Some(0), "{what}: status");
}

#[test]
fn hook_is_silent_when_the_server_is_gone() {
    let payload = json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/tmp"});
    let (out, took) = hook_binary(
        "claude",
        "/nonexistent/sock,1,0",
        "%1",
        payload.to_string().into_bytes(),
    );
    assert_silent(&out, "server gone");
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[test]
fn hook_is_silent_on_malformed_input() {
    let big = vec![b'a'; 2 << 20];
    let inputs: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("array", b"[]".to_vec()),
        ("unterminated", b"{".to_vec()),
        ("session id is a number", br#"{"session_id": 5}"#.to_vec()),
        ("not UTF-8", vec![0xff, 0xfe, b'{', b'}']),
        ("2 MiB", big),
    ];
    for mode in ["claude", "bogus"] {
        for (what, stdin) in &inputs {
            let (out, _) = hook_binary(mode, "/nonexistent/sock,1,0", "%1", stdin.clone());
            assert_silent(&out, &format!("{mode} {what}"));
        }
    }
}

#[test]
fn hook_ignores_an_environment_that_names_no_pane() {
    let payload = json!({"hook_event_name": "SessionStart", "session_id": "s", "cwd": "/tmp"});
    for (tmux, pane) in [
        ("", "%1"),
        ("/nonexistent/sock,1,0", ""),
        ("/nonexistent/sock", "%1"),
    ] {
        let (out, _) = hook_binary("claude", tmux, pane, payload.to_string().into_bytes());
        assert_silent(&out, &format!("{tmux:?} {pane:?}"));
    }
}

#[test]
fn hook_exits_zero_whatever_the_arguments() {
    let home = TempDir::new();
    for args in [
        vec!["agent-hook"],
        vec!["agent-hook", "claude", "extra"],
        vec!["agent-hook", ""],
    ] {
        let out = Command::new(EXE)
            .args(&args)
            .env_clear()
            .env("HOME", home.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_silent(&out, &format!("{args:?}"));
    }
}
