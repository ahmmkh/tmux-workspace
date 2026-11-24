//! Cleanup candidate filtering, invalidation on change, and atomic tmux close guard tests.

use std::cell::RefCell;
use tmux_workspace::cleanup::{CleanupHost, candidates, cleanup_with};
use tmux_workspace::error::Result;
use tmux_workspace::tmux::Fields;

struct FakeCleanup {
    rows: RefCell<Vec<Fields>>,
    ps: String,
    current_pane: Option<String>,
    tty: bool,
    fzf: bool,
    fzf_selection: Option<String>,
    confirm_answer: String,
    mutate_on_confirm: bool,
    kills: RefCell<Vec<(String, String, String)>>,
}

impl FakeCleanup {
    fn sample() -> Self {
        let rows: Vec<Fields> = ["idle", "working", "blocked", ""]
            .into_iter()
            .enumerate()
            .map(|(idx, status)| {
                let i = idx + 1;
                let mut f = Fields::default();
                f.set("pane_id", format!("%{i}"));
                f.set("pane_pid", format!("{}", i * 100));
                f.set("pane_current_command", "codex");
                f.set("pane_dead", "0");
                f.set("pane_tty", "/dev/ttys001");
                f.set("session_name", "work");
                f.set("window_index", format!("{i}"));
                f.set("pane_index", "1");
                f.set("@workspace_label", "task");
                f.set("@workspace_agent", status);
                f.set("@workspace_home", "");
                f
            })
            .collect();
        Self {
            rows: RefCell::new(rows),
            ps: "100 1 1024 codex\n".into(),
            current_pane: Some("%99".into()),
            tty: true,
            fzf: true,
            fzf_selection: Some("%1\ttask\n".into()),
            confirm_answer: "close".into(),
            mutate_on_confirm: false,
            kills: RefCell::new(Vec::new()),
        }
    }
}

impl CleanupHost for FakeCleanup {
    fn list_panes(&self) -> Result<Vec<Fields>> {
        Ok(self.rows.borrow().clone())
    }

    fn ps_table(&self) -> Result<String> {
        Ok(self.ps.clone())
    }

    fn current_pane(&self) -> Option<String> {
        self.current_pane.clone()
    }

    fn is_tty(&self) -> bool {
        self.tty
    }

    fn has_fzf(&self) -> bool {
        self.fzf
    }

    fn pick_fzf(&self, _lines: &str) -> Result<Option<String>> {
        Ok(self.fzf_selection.clone())
    }

    fn confirm_close(&self) -> Result<String> {
        if self.mutate_on_confirm {
            self.rows.borrow_mut()[0].set("@workspace_agent", "working");
        }
        Ok(self.confirm_answer.clone())
    }

    fn kill_pane_guarded(&self, pane: &str, guard: &str, kill_cmd: &str) -> Result<()> {
        self.kills
            .borrow_mut()
            .push((pane.to_string(), guard.to_string(), kill_cmd.to_string()));
        Ok(())
    }
}

#[test]
fn lists_unknown_but_excludes_busy_and_counts_child_memory() {
    let mut host = FakeCleanup::sample();
    host.ps = "100 1 1024 codex\n101 100 2048 helper\n400 1 4096 zsh\n".into();
    let rows = candidates(&host).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.pane_id.as_str()).collect();
    assert_eq!(ids, ["%1", "%4"]);
    assert_eq!(rows[0].memory_mib, 3.0);
    assert_eq!(rows[0].processes.len(), 2);
}

#[test]
fn cancel_closes_nothing_and_new_work_invalidates_selection() {
    for answer in ["", "close"] {
        let mut host = FakeCleanup::sample();
        host.confirm_answer = answer.into();
        host.mutate_on_confirm = true;
        let mut out = Vec::new();
        cleanup_with(&host, &mut out).unwrap();
        assert!(host.kills.borrow().is_empty(), "answer={answer:?}");
    }
}

#[test]
fn confirmed_selection_uses_exact_pane_and_atomic_status_guard() {
    let host = FakeCleanup::sample();
    let mut out = Vec::new();
    cleanup_with(&host, &mut out).unwrap();
    let kills = host.kills.borrow();
    assert_eq!(kills.len(), 1);
    assert_eq!(kills[0].0, "%1");
    assert!(kills[0].1.contains("blocked"));
    assert!(kills[0].1.contains("working"));
    assert!(kills[0].1.contains("#{==:#{pane_pid},100}"));
    assert_eq!(kills[0].2, "kill-pane -t %1");
}

#[test]
fn non_tty_prints_candidates_and_guidance_without_killing() {
    let mut host = FakeCleanup::sample();
    host.tty = false;
    let mut out = Vec::new();
    cleanup_with(&host, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("%1\twork:1.1"));
    assert!(text.contains("Run tmux-workspace cleanup in a terminal with fzf"));
    assert!(host.kills.borrow().is_empty());
}
