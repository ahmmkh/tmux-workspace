//! Review inactive terminals before explicitly closing their processes.

use crate::error::{Error, Result};
use crate::model::BOOTSTRAP;
use crate::process::{self, Spawn};
use crate::text;
use crate::tmux::{Fields, Tmux};
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};

const CLEANUP_FIELDS: &[&str] = &[
    "pane_id",
    "pane_pid",
    "pane_current_command",
    "pane_dead",
    "pane_tty",
    "session_name",
    "window_index",
    "pane_index",
    "@workspace_label",
    "@workspace_agent",
    "@workspace_home",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub pane_pid: String,
    pub pane_current_command: String,
    pub agent: String,
    /// `(pid, ppid, comm)` — RSS changes constantly, so it is omitted from review identity.
    pub processes: Vec<(i32, i32, String)>,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub pane_id: String,
    pub pane_pid: String,
    pub pane_current_command: String,
    pub pane_dead: String,
    pub session_name: String,
    pub window_index: String,
    pub pane_index: String,
    pub label: String,
    pub agent: String,
    pub home: String,
    /// `(pid, ppid, rss_kib, comm)` sorted by pid.
    pub processes: Vec<(i32, i32, u64, String)>,
    pub memory_mib: f64,
    pub identity: Identity,
}

/// Side-effect boundary so cleanup is tested without tmux, `ps`, or `fzf`.
pub trait CleanupHost {
    fn list_panes(&self) -> Result<Vec<Fields>>;
    fn ps_table(&self) -> Result<String>;
    fn current_pane(&self) -> Option<String>;
    fn is_tty(&self) -> bool;
    fn has_fzf(&self) -> bool;
    fn pick_fzf(&self, lines: &str) -> Result<Option<String>>;
    fn confirm_close(&self) -> Result<String>;
    fn kill_pane_guarded(&self, pane: &str, guard: &str, kill_cmd: &str) -> Result<()>;
}

pub struct TmuxCleanup<'a>(pub &'a Tmux);

impl CleanupHost for TmuxCleanup<'_> {
    fn list_panes(&self) -> Result<Vec<Fields>> {
        self.0.fields("list-panes", CLEANUP_FIELDS, &["-a"])
    }

    fn ps_table(&self) -> Result<String> {
        let out = process::spawn(&Spawn {
            argv: ["ps", "-axo", "pid=,ppid=,rss=,comm="]
                .map(String::from)
                .to_vec(),
            ..Spawn::default()
        })?;
        if out.status != 0 {
            return Err(Error::Msg(out.stderr.trim().to_string()));
        }
        Ok(out.stdout)
    }

    fn current_pane(&self) -> Option<String> {
        std::env::var("TMUX_PANE").ok()
    }

    fn is_tty(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn has_fzf(&self) -> bool {
        process::which("fzf").is_some()
    }

    fn pick_fzf(&self, lines: &str) -> Result<Option<String>> {
        let out = process::spawn(&Spawn {
            argv: [
                "fzf",
                "--multi",
                "--delimiter=\t",
                "--with-nth=2..",
                "--layout=reverse",
                "--header=Review candidates, not proven stale | Tab: select | Enter: review closure | Esc: cancel",
                "--prompt=Clean terminals > ",
            ]
            .map(String::from)
            .to_vec(),
            stdin: Some(lines.to_string()),
            ..Spawn::default()
        })?;
        if out.status != 0 || out.stdout.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(out.stdout))
        }
    }

    fn confirm_close(&self) -> Result<String> {
        let mut stdout = std::io::stdout();
        write!(stdout, "Type close to confirm: ")?;
        stdout.flush()?;
        let mut answer = String::new();
        let n = std::io::stdin().read_line(&mut answer)?;
        if n == 0 {
            return Err(Error::Eof);
        }
        Ok(answer)
    }

    fn kill_pane_guarded(&self, pane: &str, guard: &str, kill_cmd: &str) -> Result<()> {
        self.0
            .run(&["if-shell", "-F", "-t", pane, guard, kill_cmd])
            .map(|_| ())
    }
}

pub fn candidates(host: &dyn CleanupHost) -> Result<Vec<Candidate>> {
    let rows = host.list_panes()?;
    let ps_output = host.ps_table()?;
    let mut processes: HashMap<i32, (i32, u64, String)> = HashMap::new();
    for line in text::py_splitlines(&ps_output) {
        let parts = text::py_split_ws(line, 3);
        if parts.len() == 4
            && let (Ok(pid), Ok(ppid), Ok(rss)) = (
                parts[0].parse::<i32>(),
                parts[1].parse::<i32>(),
                parts[2].parse::<u64>(),
            )
        {
            processes.insert(pid, (ppid, rss, parts[3].to_string()));
        }
    }

    let current_pane = host.current_pane();
    let mut out = Vec::new();
    for row in rows {
        let pane_id = row.get("pane_id");
        let session = row.get("session_name");
        let agent = row.get("@workspace_agent");
        if current_pane.as_deref() == Some(pane_id)
            || session == BOOTSTRAP
            || matches!(agent, "working" | "blocked")
        {
            continue;
        }
        let pane_pid_str = row.get("pane_pid");
        let root_pid: i32 = pane_pid_str.trim().parse().unwrap_or(0);
        let mut members: HashSet<i32> = HashSet::new();
        members.insert(root_pid);
        loop {
            let children: Vec<i32> = processes
                .iter()
                .filter_map(|(&pid, &(ppid, _, _))| {
                    (members.contains(&ppid) && !members.contains(&pid)).then_some(pid)
                })
                .collect();
            if children.is_empty() {
                break;
            }
            members.extend(children);
        }
        let mut procs: Vec<(i32, i32, u64, String)> = members
            .into_iter()
            .filter_map(|pid| {
                processes
                    .get(&pid)
                    .map(|(ppid, rss, comm)| (pid, *ppid, *rss, comm.clone()))
            })
            .collect();
        procs.sort_by_key(|(pid, _, _, _)| *pid);
        let memory_mib = procs.iter().map(|(_, _, rss, _)| *rss as f64).sum::<f64>() / 1024.0;
        let identity = Identity {
            pane_pid: pane_pid_str.to_string(),
            pane_current_command: row.get("pane_current_command").to_string(),
            agent: agent.to_string(),
            processes: procs
                .iter()
                .map(|(pid, ppid, _, comm)| (*pid, *ppid, comm.clone()))
                .collect(),
        };
        out.push(Candidate {
            pane_id: pane_id.to_string(),
            pane_pid: pane_pid_str.to_string(),
            pane_current_command: row.get("pane_current_command").to_string(),
            pane_dead: row.get("pane_dead").to_string(),
            session_name: session.to_string(),
            window_index: row.get("window_index").to_string(),
            pane_index: row.get("pane_index").to_string(),
            label: row.get("@workspace_label").to_string(),
            agent: agent.to_string(),
            home: row.get("@workspace_home").to_string(),
            processes: procs,
            memory_mib,
            identity,
        });
    }
    Ok(out)
}

fn format_candidate(row: &Candidate) -> String {
    let status = if row.pane_dead == "1" {
        "dead"
    } else if !row.agent.is_empty() {
        row.agent.as_str()
    } else {
        "unverified"
    };
    let location = format!(
        "{}:{}.{}",
        row.session_name, row.window_index, row.pane_index
    );
    let label = if row.label.is_empty() {
        row.pane_current_command.as_str()
    } else {
        row.label.as_str()
    };
    let mut desc = format!(
        "{location}  {label}  [{status}]  {:.0} MiB / {} processes",
        row.memory_mib,
        row.processes.len()
    );
    if !row.home.is_empty() {
        desc.push_str("  home: ");
        desc.push_str(&row.home);
    }
    format!("{}\t{}", row.pane_id, text::printable(&desc))
}

pub fn cleanup_with(host: &dyn CleanupHost, out: &mut dyn Write) -> Result<()> {
    let mut rows = candidates(host)?;
    rows.sort_by(|a, b| b.memory_mib.total_cmp(&a.memory_mib));
    if rows.is_empty() {
        writeln!(out, "No inactive terminal candidates.")?;
        return Ok(());
    }
    let lines: Vec<String> = rows.iter().map(format_candidate).collect();
    if !host.is_tty() || !host.has_fzf() {
        for line in &lines {
            writeln!(out, "{line}")?;
        }
        writeln!(
            out,
            "Run tmux-workspace cleanup in a terminal with fzf to select and close panes."
        )?;
        return Ok(());
    }
    let Some(selected) = host.pick_fzf(&lines.join("\n"))? else {
        return Ok(());
    };
    let ids: HashSet<&str> = text::py_splitlines(&selected)
        .into_iter()
        .filter_map(|line| line.split('\t').next())
        .collect();
    let chosen: Vec<&Candidate> = rows
        .iter()
        .filter(|r| ids.contains(r.pane_id.as_str()))
        .collect();
    if chosen.is_empty() {
        return Ok(());
    }
    let matching_lines: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.split('\t').next().is_some_and(|id| ids.contains(id)))
        .collect();
    writeln!(
        out,
        "\nClose these terminals:\n{}",
        matching_lines.join("\n")
    )?;
    writeln!(
        out,
        "This terminates their jobs and may lose unsaved work. Shared daemon memory is not included."
    )?;
    if host.confirm_close()?.trim() != "close" {
        return Ok(());
    }
    let current: HashMap<String, Candidate> = candidates(host)?
        .into_iter()
        .map(|r| (r.pane_id.clone(), r))
        .collect();
    for row in chosen {
        let pane = &row.pane_id;
        match current.get(pane) {
            Some(now) if now.identity == row.identity => {}
            _ => {
                writeln!(
                    out,
                    "Skipped {pane}: processes or status changed; review again."
                )?;
                continue;
            }
        }
        // Check status again in the same tmux command queue as the close.
        let guard = format!(
            "#{{&&:#{{==:#{{pane_pid}},{}}},#{{&&:#{{!=:#{{@workspace_agent}},working}},#{{!=:#{{@workspace_agent}},blocked}}}}}}",
            row.pane_pid
        );
        let kill_cmd = format!("kill-pane -t {}", text::shlex_quote(pane));
        host.kill_pane_guarded(pane, &guard, &kill_cmd)?;
    }
    Ok(())
}

pub fn main(tmux: &Tmux) -> Result<i32> {
    cleanup_with(&TmuxCleanup(tmux), &mut std::io::stdout()).map(|()| 0)
}
