//! Pane rows and agent status, with the rules of agent-status.conf. No tmux calls.

use crate::error::{Error, Result};
use crate::tmux::Fields;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Most urgent first.
pub const AGENT_ORDER: [&str; 4] = ["blocked", "done", "working", "idle"];
pub const SHELLS: [&str; 11] = [
    "zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "nu", "elvish", "xonsh", "pwsh",
];
pub const AGENTS: [&str; 3] = ["claude", "codex", "omp"];
pub const PARKING: &str = "__parked";
/// Placeholder session that `work` starts a server with.
pub const BOOTSTRAP: &str = "__workspace_bootstrap";

/// A login shell shows as -zsh; a full path can appear too.
pub fn is_shell(command: &str) -> bool {
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    SHELLS.contains(&name.strip_prefix('-').unwrap_or(name))
}

/// A pane at its shell prompt has no agent status, whatever its option still says.
pub fn pane_status(row: &Fields) -> &'static str {
    if is_shell(row.get("pane_current_command")) {
        return "";
    }
    let status = row.get("@workspace_agent");
    AGENT_ORDER
        .iter()
        .copied()
        .find(|known| *known == status)
        .unwrap_or("")
}

/// Position in `AGENT_ORDER`: lower is more urgent.
fn rank(status: &str) -> usize {
    AGENT_ORDER
        .iter()
        .position(|known| *known == status)
        .unwrap_or(AGENT_ORDER.len())
}

/// Pane status rolled up per tab and per workspace: the most urgent status wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RollUp {
    pub by_pane: HashMap<String, &'static str>,
    pub by_tab: HashMap<String, &'static str>,
    pub by_workspace: HashMap<String, &'static str>,
}

/// Roll pane status up per tab and per workspace. A parked pane counts toward its home.
pub fn roll_up(rows: &[Fields]) -> RollUp {
    let mut up = RollUp::default();
    for row in rows {
        let status = pane_status(row);
        if status.is_empty() {
            continue;
        }
        up.by_pane.insert(row.get("pane_id").to_string(), status);
        let workspace = if row.get("session_name") == PARKING {
            row.get("@workspace_home")
        } else {
            row.get("session_name")
        };
        for (group, key) in [
            (&mut up.by_tab, row.get("window_id")),
            (&mut up.by_workspace, workspace),
        ] {
            let known = group.entry(key.to_string()).or_insert(status);
            if rank(status) < rank(known) {
                *known = status;
            }
        }
    }
    up
}

/// `json.loads` for a tmux option value. tmux 3.4 prints `$` as `\$`, which JSON rejects.
pub fn option_json(raw: &str) -> Result<Value> {
    match serde_json::from_str(raw) {
        Err(_) if raw.contains("\\$") => serde_json::from_str(&raw.replace("\\$", "$")),
        other => other,
    }
    .map_err(|e| Error::Json(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pane(
        pane_id: &str,
        status: &str,
        window: &str,
        session: &str,
        command: &str,
        home: &str,
    ) -> Fields {
        let mut row = Fields::default();
        row.set("session_name", session);
        row.set("window_id", window);
        row.set("pane_id", pane_id);
        row.set("pane_current_command", command);
        row.set("@workspace_agent", status);
        row.set("@workspace_home", home);
        row
    }

    fn command_and_status(command: &str, status: &str) -> Fields {
        let mut row = Fields::default();
        row.set("pane_current_command", command);
        row.set("@workspace_agent", status);
        row
    }

    fn map(pairs: &[(&str, &'static str)]) -> HashMap<String, &'static str> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    // tests/test_workspace_model.py::Status

    mod status {
        use super::*;

        #[test]
        fn every_supported_shell_hides_the_status() {
            for shell in [
                "zsh",
                "bash",
                "fish",
                "sh",
                "dash",
                "ksh",
                "tcsh",
                "nu",
                "elvish",
                "xonsh",
                "pwsh",
                "-zsh",
                "/bin/bash",
            ] {
                assert_eq!(
                    pane_status(&command_and_status(shell, "working")),
                    "",
                    "{shell}"
                );
            }
        }

        #[test]
        fn agent_keeps_its_status() {
            assert_eq!(
                pane_status(&command_and_status("claude", "blocked")),
                "blocked"
            );
        }

        #[test]
        fn unknown_status_is_ignored() {
            assert_eq!(pane_status(&command_and_status("omp", "busy")), "");
        }

        #[test]
        fn programs_named_like_shells_are_not_shells() {
            for command in ["bashtop", "fisher", "zshdb", "shellcheck"] {
                assert!(!is_shell(command), "{command}");
            }
        }
    }

    // tests/test_workspace_model.py::RollUp

    mod roll_up {
        use super::*;

        #[test]
        fn bash_pane_does_not_count() {
            let rows = [
                pane("%1", "blocked", "@1", "alpha", "bash", ""),
                pane("%2", "idle", "@1", "alpha", "omp", ""),
            ];
            assert_eq!(
                roll_up(&rows),
                RollUp {
                    by_pane: map(&[("%2", "idle")]),
                    by_tab: map(&[("@1", "idle")]),
                    by_workspace: map(&[("alpha", "idle")]),
                }
            );
        }

        #[test]
        fn order_is_blocked_done_working_idle() {
            assert_eq!(AGENT_ORDER, ["blocked", "done", "working", "idle"]);
        }

        #[test]
        fn parked_pane_counts_toward_its_home() {
            let rows = [pane("%2", "done", "@9", PARKING, "omp", "beta")];
            assert_eq!(roll_up(&rows).by_workspace, map(&[("beta", "done")]));
        }
    }

    // Beyond the Python cases.

    mod beyond {
        use super::*;

        #[test]
        fn the_most_urgent_status_wins_per_tab_and_workspace() {
            let rows = [
                pane("%1", "idle", "@1", "alpha", "omp", ""),
                pane("%2", "blocked", "@1", "alpha", "claude", ""),
                pane("%3", "working", "@2", "alpha", "codex", ""),
                pane("%4", "done", "@3", "beta", "omp", ""),
            ];
            let up = roll_up(&rows);
            assert_eq!(
                up.by_pane,
                map(&[
                    ("%1", "idle"),
                    ("%2", "blocked"),
                    ("%3", "working"),
                    ("%4", "done")
                ])
            );
            assert_eq!(
                up.by_tab,
                map(&[("@1", "blocked"), ("@2", "working"), ("@3", "done")])
            );
            assert_eq!(
                up.by_workspace,
                map(&[("alpha", "blocked"), ("beta", "done")])
            );
        }

        #[test]
        fn a_row_without_fields_has_no_status() {
            assert_eq!(pane_status(&Fields::default()), "");
            assert_eq!(roll_up(&[Fields::default()]), RollUp::default());
        }

        #[test]
        fn the_shell_name_is_the_last_path_component_without_one_leading_dash() {
            assert!(is_shell("/usr/bin/zsh"));
            assert!(is_shell("-bash"));
            assert!(is_shell("bash/"));
            assert!(!is_shell("--bash"));
            assert!(!is_shell(""));
            assert!(!is_shell("/bin/"));
            assert!(!is_shell("-"));
        }

        #[test]
        fn a_broken_value_with_an_escaped_dollar_still_raises() {
            assert!(option_json(r#"{"id": \$HOME}"#).is_err());
        }

        #[test]
        fn constants_match_the_python_model() {
            assert_eq!(AGENTS, ["claude", "codex", "omp"]);
            assert_eq!(PARKING, "__parked");
            assert_eq!(BOOTSTRAP, "__workspace_bootstrap");
            assert_eq!(SHELLS.len(), 11);
            assert!(SHELLS.contains(&"pwsh") && SHELLS.contains(&"elvish"));
        }
    }

    // tests/test_workspace_model.py::OptionJson

    mod option_json {
        use super::*;

        #[test]
        fn plain_json() {
            assert_eq!(
                option_json(r#"{"kind": "claude"}"#).unwrap(),
                json!({"kind": "claude"})
            );
        }

        #[test]
        fn tmux_3_4_escaped_dollar() {
            // tmux 3.4 prints $ in an option value as \$, which JSON rejects.
            assert_eq!(
                option_json(r#"{"id": "\$HOME and \\"}"#).unwrap(),
                json!({"id": "$HOME and \\"})
            );
        }

        #[test]
        fn broken_value_still_raises() {
            assert!(option_json("{broken").is_err());
        }
    }
}
