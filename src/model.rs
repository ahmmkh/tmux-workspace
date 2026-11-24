//! Pane rows and agent status, with the rules of agent-status.conf. No tmux calls.

use crate::error::{Error, Result};
use crate::tmux::Fields;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Most urgent first.
pub const AGENT_ORDER: [&str; 4] = ["blocked", "done", "working", "idle"];
pub const SHELLS: [&str; 11] = [
    "zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "nu", "elvish", "xonsh", "pwsh",
];
pub const AGENTS: [&str; 4] = crate::agent::AI_AGENT_IDS;
pub const PARKING: &str = "__parked";
/// Placeholder session that `work` starts a server with.
pub const BOOTSTRAP: &str = "__workspace_bootstrap";
/// Fields queried from `tmux list-panes -a` to build dashboard `Row`s.
pub const DASH_PANE_FIELDS: &[&str] = &[
    "session_name",
    "window_id",
    "window_index",
    "window_name",
    "pane_id",
    "pane_index",
    "pane_current_command",
    "pane_current_path",
    "@workspace_label",
    "@workspace_agent",
    "@workspace_app",
    "@workspace_home",
];

/// A login shell shows as -zsh; a full path can appear too.
pub fn is_shell(command: &str) -> bool {
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    SHELLS.contains(&name.strip_prefix('-').unwrap_or(name))
}

/// The base name of a command (without leading `-` for login shells).
pub(crate) fn command_name(command: &str) -> &str {
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    name.strip_prefix('-').unwrap_or(name)
}

/// Whether `@workspace_app` records an `agy` session whose pane is no longer running `agy`.
/// `agy` has no `SessionEnd` hook, so once the foreground command is no longer `agy`, any
/// leftover `@workspace_app`/`@workspace_agent` is stale and hidden.
pub(crate) fn stale_agy(row: &Fields) -> bool {
    let raw = row.get("@workspace_app");
    if raw.is_empty() {
        return false;
    }
    let is_agy = option_json(raw)
        .ok()
        .and_then(|v| v.get("kind").and_then(Value::as_str).map(|k| k == "agy"))
        .unwrap_or(false);
    is_agy && command_name(row.get("pane_current_command")) != "agy"
}

/// A pane at its shell prompt (or a post-exit `agy` pane) has no agent status, whatever its option still says.
pub fn pane_status(row: &Fields) -> &'static str {
    if is_shell(row.get("pane_current_command")) || stale_agy(row) {
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

/// One pane as the dashboard shows it. A parked pane belongs to its home workspace and has no tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub pane_id: String,
    pub workspace: String,
    pub window_index: i64,
    pub tab: String,
    pub pane_index: i64,
    pub label: String,
    pub agent: String,
    pub status: &'static str,
    pub parked: bool,
    pub path: String,
}

/// Counts shown in the dashboard top border.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub blocked: usize,
    pub done: usize,
    pub working: usize,
    pub idle: usize,
    pub panes: usize,
    pub workspaces: usize,
}

impl Summary {
    /// Count for one status name from `AGENT_ORDER`.
    pub fn for_status(&self, status: &str) -> usize {
        match status {
            "blocked" => self.blocked,
            "done" => self.done,
            "working" => self.working,
            "idle" => self.idle,
            _ => 0,
        }
    }
}

fn number(value: &str) -> i64 {
    value.trim().parse().unwrap_or(0)
}

/// The agent running in a pane: from `@workspace_app.kind` first, then `pane_current_command`.
/// Returns empty when an `agy` pane has exited to another command.
pub fn agent(row: &Fields) -> String {
    if stale_agy(row) {
        return String::new();
    }
    let raw = row.get("@workspace_app");
    if !raw.is_empty()
        && let Ok(val) = option_json(raw)
        && let Some(kind) = val.get("kind").and_then(Value::as_str)
        && !kind.is_empty()
    {
        return kind.to_string();
    }
    let cmd = Path::new(row.get("pane_current_command"))
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if AGENTS.contains(&cmd) {
        cmd.to_string()
    } else {
        String::new()
    }
}

/// Convert raw tmux `list-panes -a` rows into sorted dashboard rows, skipping the bootstrap session.
pub fn build_rows(pane_rows: &[Fields]) -> Vec<Row> {
    let mut rows = Vec::new();
    for row in pane_rows {
        let session = row.get("session_name");
        if session == BOOTSTRAP {
            continue;
        }
        let parked = session == PARKING;
        let home = row.get("@workspace_home");
        let workspace = if parked {
            if home.is_empty() { PARKING } else { home }
        } else {
            session
        };
        let label = row.get("@workspace_label");
        let label = if label.is_empty() {
            row.get("pane_current_command")
        } else {
            label
        };
        rows.push(Row {
            pane_id: row.get("pane_id").to_string(),
            workspace: workspace.to_string(),
            window_index: number(row.get("window_index")),
            tab: if parked {
                String::new()
            } else {
                row.get("window_name").to_string()
            },
            pane_index: number(row.get("pane_index")),
            label: label.to_string(),
            agent: agent(row),
            status: pane_status(row),
            parked,
            path: row.get("pane_current_path").to_string(),
        });
    }
    sort_rows(rows)
}

/// Workspaces by their most urgent agent, then by name; parked panes last in their workspace.
pub fn sort_rows(mut rows: Vec<Row>) -> Vec<Row> {
    let none = AGENT_ORDER.len();
    let mut urgency: HashMap<String, usize> = HashMap::new();
    for row in &rows {
        let r = if row.status.is_empty() {
            none
        } else {
            rank(row.status)
        };
        let entry = urgency.entry(row.workspace.clone()).or_insert(none);
        if r < *entry {
            *entry = r;
        }
    }
    rows.sort_by(|a, b| {
        let ua = urgency[&a.workspace];
        let ub = urgency[&b.workspace];
        (
            ua,
            a.workspace.as_str(),
            a.parked,
            a.window_index,
            a.pane_index,
        )
            .cmp(&(
                ub,
                b.workspace.as_str(),
                b.parked,
                b.window_index,
                b.pane_index,
            ))
    });
    rows
}

/// Filter rows by case-insensitive substring match against workspace, tab, label, or agent,
/// and optionally restrict to panes running an agent.
pub fn filter_rows(rows: &[Row], query: &str, agents_only: bool) -> Vec<Row> {
    let needle = query.trim().to_lowercase();
    rows.iter()
        .filter(|r| {
            (!agents_only || !r.agent.is_empty())
                && (needle.is_empty()
                    || r.workspace.to_lowercase().contains(&needle)
                    || r.tab.to_lowercase().contains(&needle)
                    || r.label.to_lowercase().contains(&needle)
                    || r.agent.to_lowercase().contains(&needle))
        })
        .cloned()
        .collect()
}

/// Summarize status counts, total panes, and unique workspaces.
pub fn summary(rows: &[Row]) -> Summary {
    let mut s = Summary {
        panes: rows.len(),
        ..Summary::default()
    };
    let mut workspaces: HashSet<&str> = HashSet::new();
    for row in rows {
        workspaces.insert(row.workspace.as_str());
        match row.status {
            "blocked" => s.blocked += 1,
            "done" => s.done += 1,
            "working" => s.working += 1,
            "idle" => s.idle += 1,
            _ => {}
        }
    }
    s.workspaces = workspaces.len();
    s
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
            assert_eq!(AGENTS, ["claude", "codex", "omp", "agy"]);
            assert_eq!(PARKING, "__parked");
            assert_eq!(BOOTSTRAP, "__workspace_bootstrap");
            assert_eq!(SHELLS.len(), 11);
            assert!(SHELLS.contains(&"pwsh") && SHELLS.contains(&"elvish"));
        }

        #[test]
        fn stale_agy_status_is_hidden_once_agy_exits() {
            let mut row = Fields::default();
            row.set("pane_current_command", "agy");
            row.set("@workspace_agent", "working");
            row.set("@workspace_app", r#"{"kind":"agy","id":"conv-1"}"#);
            assert_eq!(pane_status(&row), "working");

            row.set("pane_current_command", "vim");
            assert_eq!(pane_status(&row), "");

            // Non-agy apps running a child command keep their status.
            row.set("@workspace_app", r#"{"kind":"claude","id":"s-1"}"#);
            assert_eq!(pane_status(&row), "working");
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

        #[test]
        fn agent_from_an_escaped_identity() {
            let mut row = fixture()[0].clone();
            row.set("@workspace_app", r#"{"kind": "claude", "id": "\$x"}"#);
            row.set("pane_current_command", "node");
            assert_eq!(build_rows(&[row])[0].agent, "claude");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn fixture_pane(
        pane_id: &str,
        session: &str,
        window: usize,
        index: usize,
        name: &str,
        pane_index: usize,
        label: &str,
        command: &str,
        status: &str,
        app: &str,
        home: &str,
        path: &str,
    ) -> Fields {
        let mut row = Fields::default();
        row.set("session_name", session);
        row.set("window_id", format!("@{window}"));
        row.set("window_index", index.to_string());
        row.set("window_name", name);
        row.set("pane_id", pane_id);
        row.set("pane_index", pane_index.to_string());
        row.set("pane_current_command", command);
        let fallback_path = format!(
            "/srv/projects/{}",
            if home.is_empty() { session } else { home }
        );
        row.set(
            "pane_current_path",
            if path.is_empty() {
                &fallback_path
            } else {
                path
            },
        );
        row.set("@workspace_label", label);
        row.set("@workspace_agent", status);
        row.set(
            "@workspace_app",
            if app.is_empty() {
                String::new()
            } else {
                format!(r#"{{"kind": "{app}", "id": "x"}}"#)
            },
        );
        row.set("@workspace_home", home);
        row
    }

    fn fixture() -> Vec<Fields> {
        vec![
            fixture_pane(
                "%4", "docs", 4, 1, "write", 1, "guide", "omp", "idle", "omp", "", "",
            ),
            fixture_pane(
                "%2", "api", 2, 2, "servers", 1, "logs", "zsh", "", "", "", "",
            ),
            fixture_pane(
                "%3", "frontend", 3, 1, "code", 2, "ui", "codex", "done", "", "", "",
            ),
            fixture_pane(
                "%1", "api", 1, 1, "code", 1, "claude", "node", "blocked", "claude", "", "",
            ),
            fixture_pane(
                "%5",
                "infra",
                5,
                1,
                "deploy",
                1,
                "terraform",
                "omp",
                "working",
                "",
                "",
                "",
            ),
            fixture_pane(
                "%6", "__parked", 6, 1, "tests", 1, "tests", "zsh", "", "", "api", "",
            ),
        ]
    }

    // tests/test_workspace_model.py::Rows

    mod rows {
        use super::*;

        #[test]
        fn rows_sort_by_workspace_urgency_then_position() {
            let rows = build_rows(&fixture());
            let ws: Vec<&str> = rows.iter().map(|r| r.workspace.as_str()).collect();
            assert_eq!(ws, ["api", "api", "api", "frontend", "infra", "docs"]);
            let top3: Vec<&str> = rows[..3].iter().map(|r| r.pane_id.as_str()).collect();
            assert_eq!(top3, ["%1", "%2", "%6"]);
        }

        #[test]
        fn parked_pane_sits_last_under_its_home() {
            let rows = build_rows(&fixture());
            let parked = &rows[2];
            assert!(parked.parked);
            assert_eq!(
                (parked.workspace.as_str(), parked.tab.as_str()),
                ("api", "")
            );
        }

        #[test]
        fn agent_comes_from_workspace_app_then_command() {
            let rows = build_rows(&fixture());
            let agents: HashMap<&str, &str> = rows
                .iter()
                .map(|r| (r.pane_id.as_str(), r.agent.as_str()))
                .collect();
            assert_eq!(agents["%1"], "claude");
            assert_eq!(agents["%3"], "codex");
            assert_eq!(agents["%2"], "");
        }

        #[test]
        fn shell_pane_has_no_status() {
            let rows = build_rows(&fixture());
            let statuses: HashMap<&str, &str> = rows
                .iter()
                .map(|r| (r.pane_id.as_str(), r.status))
                .collect();
            assert_eq!(statuses["%2"], "");
        }

        #[test]
        fn label_falls_back_to_the_command() {
            let rows = build_rows(&[fixture_pane(
                "%9", "w", 9, 1, "t", 1, "", "htop", "", "", "", "",
            )]);
            assert_eq!(rows[0].label, "htop");
        }

        #[test]
        fn broken_app_json_is_ignored() {
            let mut row = fixture()[0].clone();
            row.set("@workspace_app", "{not json");
            assert_eq!(build_rows(&[row])[0].agent, "omp");
        }

        #[test]
        fn bootstrap_session_is_hidden() {
            let rows = build_rows(&[fixture_pane(
                "%9",
                "__workspace_bootstrap",
                9,
                1,
                "t",
                1,
                "",
                "sleep",
                "",
                "",
                "",
                "",
            )]);
            assert!(rows.is_empty());
        }

        #[test]
        fn filter_matches_label_case_insensitively() {
            let rows = build_rows(&fixture());
            let ids: Vec<String> = filter_rows(&rows, "TERRA", false)
                .into_iter()
                .map(|r| r.pane_id)
                .collect();
            assert_eq!(ids, ["%5"]);
        }

        #[test]
        fn filter_matches_workspace_tab_and_agent() {
            let rows = build_rows(&fixture());
            let ids = |q| -> Vec<String> {
                filter_rows(&rows, q, false)
                    .into_iter()
                    .map(|r| r.pane_id)
                    .collect()
            };
            assert_eq!(ids("front"), ["%3"]);
            assert_eq!(ids("servers"), ["%2"]);
            assert_eq!(ids("claude"), ["%1"]);
        }

        #[test]
        fn agents_only_drops_rows_without_agent() {
            let rows = build_rows(&fixture());
            let ids: Vec<String> = filter_rows(&rows, "", true)
                .into_iter()
                .map(|r| r.pane_id)
                .collect();
            assert_eq!(ids, ["%1", "%3", "%5", "%4"]);
        }

        #[test]
        fn summary_counts() {
            let rows = build_rows(&fixture());
            assert_eq!(
                summary(&rows),
                Summary {
                    blocked: 1,
                    done: 1,
                    working: 1,
                    idle: 1,
                    panes: 6,
                    workspaces: 4,
                }
            );
        }

        #[test]
        fn stale_agy_hides_agent_and_status_in_rows() {
            let active = fixture_pane(
                "%7",
                "api",
                7,
                1,
                "agy",
                1,
                "antigravity",
                "agy",
                "working",
                "agy",
                "",
                "",
            );
            let exited = fixture_pane(
                "%8",
                "api",
                8,
                2,
                "agy",
                1,
                "antigravity",
                "vim",
                "working",
                "agy",
                "",
                "",
            );
            let rows = build_rows(&[active, exited]);
            let by_id: HashMap<&str, &Row> = rows.iter().map(|r| (r.pane_id.as_str(), r)).collect();
            assert_eq!(by_id["%7"].agent, "agy");
            assert_eq!(by_id["%7"].status, "working");
            assert_eq!(by_id["%8"].agent, "");
            assert_eq!(by_id["%8"].status, "");
        }
    }
}
