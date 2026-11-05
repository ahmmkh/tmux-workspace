//! Claude Code's own view of a session: the state file it writes, and the pane status that view calls for.

use super::{HookEnv, SETTLE};
use serde_json::Value;

/// Map the state file to a pane status. `None` means the pane keeps what it has.
pub fn wanted_status(
    state: Option<&Value>,
    session_id: &str,
    wait: &str,
    failed: &str,
    turn_over: &str,
) -> Option<&'static str> {
    let state = state?.as_object()?;
    let status = state.get("status").and_then(Value::as_str);
    // A file without a usable time counts as newer, so a blocked set by a hook never sticks.
    let newer = |margin: i64| match (number(state.get("statusUpdatedAt")), digits(wait)) {
        (Some(updated), Some(wait)) => updated > wait + margin as f64,
        _ => true,
    };
    match status? {
        "waiting" => Some("blocked"),
        // A hook that set blocked wins until the file changes after that hook started.
        "busy" => newer(0).then_some("working"),
        "idle" | "shell" => {
            // A failed turn stays blocked. The once-a-second call can miss a later turn shorter
            // than a second, so an idle file that changed well after the failure also ends it.
            // ponytail: a later turn that is missed and ends within SETTLE of the failure keeps
            // blocked until the next prompt. A UserPromptSubmit hook would close that.
            // Only an idle file ends a failed turn. A shell command never turns it into done.
            if failed == session_id && (status == Some("shell") || !newer(SETTLE)) {
                return None;
            }
            // Turn over fails safe. Anything but done gives idle.
            Some(if turn_over == "done" { "done" } else { "idle" })
        }
        _ => None,
    }
}

/// A JSON number, as Python's `isinstance(x, (int, float))` sees it (a bool is an int there).
fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        _ => None,
    }
}

/// `int(wait)` when `wait.isdigit()`: the time a hook blocked the pane, in milliseconds.
fn digits(wait: &str) -> Option<f64> {
    let all_digits = !wait.is_empty() && wait.bytes().all(|b| b.is_ascii_digit());
    all_digits.then(|| wait.parse().ok()).flatten()
}

/// Read the live state that Claude Code writes for each interactive session: `<sessions>/<pid>.json`.
pub fn session_state(env: &HookEnv, pid: &str) -> Option<Value> {
    let bytes = std::fs::read(env.sessions.join(format!("{pid}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SESSION: &str = "session-1";

    fn wanted(state: Value, wait: &str, failed: &str, turn_over: &str) -> Option<&'static str> {
        wanted_status(Some(&state), SESSION, wait, failed, turn_over)
    }

    // test_agent_status.py::Mapping

    #[test]
    fn busy_is_working() {
        assert_eq!(
            wanted(
                json!({"status": "busy", "statusUpdatedAt": 5}),
                "",
                "",
                "done"
            ),
            Some("working")
        );
    }

    #[test]
    fn waiting_is_blocked() {
        assert_eq!(
            wanted(
                json!({"status": "waiting", "statusUpdatedAt": 5}),
                "",
                "",
                "done"
            ),
            Some("blocked")
        );
    }

    #[test]
    fn idle_and_shell_apply_turn_over() {
        for status in ["idle", "shell"] {
            assert_eq!(
                wanted(json!({"status": status}), "", "", "done"),
                Some("done")
            );
            assert_eq!(
                wanted(json!({"status": status}), "", "", "idle"),
                Some("idle")
            );
        }
    }

    #[test]
    fn failed_turn_stays_blocked_while_the_file_is_idle() {
        let state = json!({"status": "idle", "statusUpdatedAt": 1100});
        assert_eq!(wanted(state, "1000", SESSION, "done"), None);
    }

    #[test]
    fn failed_turn_ends_when_a_later_turn_finished_between_two_calls() {
        let state = json!({"status": "idle", "statusUpdatedAt": 1000 + SETTLE + 1});
        assert_eq!(wanted(state, "1000", SESSION, "done"), Some("done"));
        let busy = json!({"status": "busy", "statusUpdatedAt": 1001});
        assert_eq!(wanted(busy, "1000", SESSION, "done"), Some("working"));
    }

    #[test]
    fn failure_of_another_session_does_not_count() {
        assert_eq!(
            wanted(json!({"status": "idle"}), "", "older-session", "done"),
            Some("done")
        );
    }

    #[test]
    fn missing_file_or_missing_status_changes_nothing() {
        assert_eq!(wanted_status(None, SESSION, "", "", "done"), None);
        for state in [
            json!({}),
            json!({"pid": 4242}),
            json!({"status": "unknown"}),
            json!([]),
        ] {
            assert_eq!(wanted(state.clone(), "", "", "done"), None, "{state}");
        }
    }

    #[test]
    fn blocked_by_a_hook_returns_to_working_only_when_the_file_is_newer() {
        let at = |updated: i64| json!({"status": "busy", "statusUpdatedAt": updated});
        assert_eq!(wanted(at(100), "100", "", "done"), None);
        assert_eq!(wanted(at(99), "100", "", "done"), None);
        assert_eq!(wanted(at(101), "100", "", "done"), Some("working"));
    }

    #[test]
    fn turn_over_without_a_usable_value_gives_idle() {
        for turn_over in ["", "working", "blocked", "garbage"] {
            assert_eq!(
                wanted(json!({"status": "idle"}), "", "", turn_over),
                Some("idle"),
                "{turn_over}"
            );
        }
    }

    #[test]
    fn shell_never_ends_a_failed_turn() {
        let state = json!({"status": "shell", "statusUpdatedAt": 1000 + SETTLE + 1});
        assert_eq!(wanted(state, "1000", SESSION, "done"), None);
        assert_eq!(
            wanted(json!({"status": "shell"}), "1000", SESSION, "done"),
            None
        );
    }

    #[test]
    fn file_without_a_usable_time_counts_as_newer() {
        for extra in [
            json!({}),
            json!({"statusUpdatedAt": null}),
            json!({"statusUpdatedAt": "soon"}),
        ] {
            let with = |status: &str| {
                let mut state = extra.clone();
                state["status"] = json!(status);
                state
            };
            assert_eq!(
                wanted(with("busy"), "100", "", "done"),
                Some("working"),
                "{extra}"
            );
            assert_eq!(
                wanted(with("idle"), "100", SESSION, "done"),
                Some("done"),
                "{extra}"
            );
        }
    }

    // Beyond the Python tests.

    #[test]
    fn a_wait_that_is_not_a_number_counts_as_newer() {
        for wait in ["", "soon", "-5", "1.5"] {
            let state = json!({"status": "busy", "statusUpdatedAt": 1});
            assert_eq!(wanted(state, wait, "", "done"), Some("working"), "{wait:?}");
        }
    }

    #[test]
    fn a_fractional_time_is_compared_as_a_number() {
        let at = |updated: f64| json!({"status": "busy", "statusUpdatedAt": updated});
        assert_eq!(wanted(at(100.5), "100", "", "done"), Some("working"));
        assert_eq!(wanted(at(100.0), "100", "", "done"), None);
    }

    #[test]
    fn session_state_reads_the_file_of_the_pid() {
        use crate::testing::TempDir;
        let dir = TempDir::new();
        let env = HookEnv {
            pane: "%1".into(),
            socket: "/s".into(),
            tmux: vec![],
            sessions: dir.path().to_path_buf(),
            codex_home: dir.path().to_path_buf(),
        };
        assert_eq!(session_state(&env, "4242"), None, "no file");
        std::fs::write(
            dir.path().join("4242.json"),
            r#"{"status": "busy", "statusUpdatedAt": 7}"#,
        )
        .unwrap();
        assert_eq!(
            session_state(&env, "4242"),
            Some(json!({"status": "busy", "statusUpdatedAt": 7}))
        );
        for bad in ["", "{", "\u{feff}{}"] {
            std::fs::write(dir.path().join("4243.json"), bad).unwrap();
            assert_eq!(session_state(&env, "4243"), None, "{bad:?}");
        }
        std::fs::write(dir.path().join("4244.json"), [0xff, 0xfe]).unwrap();
        assert_eq!(session_state(&env, "4244"), None, "not UTF-8");
        std::fs::write(dir.path().join("4245.json"), "[1]").unwrap();
        assert_eq!(
            session_state(&env, "4245"),
            Some(json!([1])),
            "any JSON; the caller checks for an object"
        );
        assert_eq!(session_state(&env, "a\0b"), None);
    }
}
