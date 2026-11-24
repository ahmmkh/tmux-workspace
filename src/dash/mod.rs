//! Agent and workspace dashboard: one full-screen table of every pane, with live agent status.

pub mod render;
pub mod terminal;

use crate::dash::render::{Mode, State, frame_text, render, selected_index};
use crate::error::Result;
use crate::model::{DASH_PANE_FIELDS, Row, build_rows, filter_rows};
use crate::pane;
use crate::park;
use crate::text;
use crate::tmux::{Fields, Tmux};
use regex::Regex;
use std::io::Write;
use std::sync::OnceLock;

pub const MESSAGE_SECONDS: f64 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Escape,
    Up,
    Down,
    Home,
    End,
    Resize,
    Quit,
    Other,
}

/// Side-effect boundary for the dashboard so key handling and `--once` are tested without tmux.
pub trait DashHost {
    fn list_panes(&self) -> Result<Vec<Fields>>;
    fn capture_pane(&self, pane_id: &str) -> Result<String>;
    fn server_running(&self) -> bool;
    fn select_pane(&self, pane_id: &str) -> Result<()>;
    fn kill_pane(&self, pane_id: &str) -> Result<()>;
    fn park(&self, pane_id: &str) -> Result<()>;
    fn rejoin(&self, pane_id: &str, origin: &str) -> Result<()>;
    fn set_label(&self, pane_id: &str, label: &str) -> Result<()>;
}

pub struct TmuxDash<'a>(pub &'a Tmux);

impl DashHost for TmuxDash<'_> {
    fn list_panes(&self) -> Result<Vec<Fields>> {
        self.0.fields("list-panes", DASH_PANE_FIELDS, &["-a"])
    }

    fn capture_pane(&self, pane_id: &str) -> Result<String> {
        self.0.run(&["capture-pane", "-p", "-J", "-t", pane_id])
    }

    fn server_running(&self) -> bool {
        self.0
            .run(&["display-message", "-p", "#{socket_path}"])
            .is_ok()
    }

    fn select_pane(&self, pane_id: &str) -> Result<()> {
        self.0.run(&["select-window", "-t", pane_id])?;
        self.0.run(&["select-pane", "-t", pane_id])?;
        // No client when run outside a popup.
        self.0.quiet(&["switch-client", "-t", pane_id]);
        Ok(())
    }

    fn kill_pane(&self, pane_id: &str) -> Result<()> {
        self.0.run(&["kill-pane", "-t", pane_id]).map(|_| ())
    }

    fn park(&self, pane_id: &str) -> Result<()> {
        park::park(self.0, pane_id)
    }

    fn rejoin(&self, pane_id: &str, origin: &str) -> Result<()> {
        park::rejoin(self.0, pane_id, origin, "")
    }

    fn set_label(&self, pane_id: &str, label: &str) -> Result<()> {
        pane::set_label(self.0, pane_id, label)
    }
}

pub fn say(state: &mut State, message: &str) {
    state.message = message.trim().to_string();
    state.message_until = text::now() + MESSAGE_SECONDS;
}

fn act(state: &mut State, action: impl FnOnce() -> Result<()>) -> bool {
    match action() {
        Ok(()) => true,
        Err(err) => {
            say(state, &err.to_string());
            false
        }
    }
}

fn current<'a>(rows: &'a [Row], state: &State) -> &'a Row {
    &rows[selected_index(rows, state)]
}

fn bring_back(state: &mut State, row: &Row, host: &dyn DashHost) {
    if state.origin.is_empty() {
        say(
            state,
            "Open the dashboard from a pane to bring parked panes back",
        );
        return;
    }
    let pane_id = row.pane_id.clone();
    let origin = state.origin.clone();
    state.quit = act(state, || host.rejoin(&pane_id, &origin));
}

fn jump(state: &mut State, rows: &[Row], host: &dyn DashHost) {
    let row = current(rows, state).clone();
    if row.parked {
        bring_back(state, &row, host);
        return;
    }
    state.quit = act(state, || host.select_pane(&row.pane_id));
}

fn typing(key: Key, state: &mut State, host: &dyn DashHost) {
    match key {
        Key::Escape => {
            if state.mode == Mode::Filter {
                state.query.clear();
            }
            state.mode = Mode::Normal;
            state.text.clear();
        }
        Key::Enter => {
            if state.mode == Mode::Rename && !state.target.is_empty() {
                let target = state.target.clone();
                let label = state.text.clone();
                act(state, || host.set_label(&target, &label));
            }
            state.mode = Mode::Normal;
            state.text.clear();
        }
        Key::Backspace => {
            state.text.pop();
        }
        Key::Char(c) if text::is_printable(c) => {
            state.text.push(c);
        }
        _ => {}
    }
    if state.mode == Mode::Filter {
        state.query = state.text.clone();
    }
}

/// Apply one key to `state`. `rows` are the visible rows, in table order.
pub fn handle_key(key: Key, state: &mut State, rows: &[Row], host: &dyn DashHost) {
    if state.mode == Mode::Help {
        state.mode = Mode::Normal;
        return;
    }
    if matches!(state.mode, Mode::Filter | Mode::Rename) {
        typing(key, state, host);
        return;
    }
    if state.mode == Mode::ConfirmKill {
        state.mode = Mode::Normal;
        if matches!(key, Key::Char('y' | 'Y')) {
            if !rows.iter().any(|r| r.pane_id == state.target) {
                let msg = format!("{} is gone already", state.target_label);
                say(state, &msg);
            } else {
                let target = state.target.clone();
                act(state, || host.kill_pane(&target));
            }
        }
        return;
    }
    match key {
        Key::Char('q') | Key::Quit => state.quit = true,
        Key::Escape => {
            if !state.query.is_empty() || state.agents_only {
                state.query.clear();
                state.agents_only = false;
            } else {
                state.quit = true;
            }
        }
        Key::Char('/') => {
            state.mode = Mode::Filter;
            state.text = state.query.clone();
        }
        Key::Char('a') => state.agents_only = !state.agents_only,
        Key::Char('?') => state.mode = Mode::Help,
        _ if rows.is_empty() => {}
        Key::Char('j') | Key::Down => {
            let idx = (selected_index(rows, state) + 1).min(rows.len() - 1);
            state.selected = Some(rows[idx].pane_id.clone());
        }
        Key::Char('k') | Key::Up => {
            let idx = selected_index(rows, state).saturating_sub(1);
            state.selected = Some(rows[idx].pane_id.clone());
        }
        Key::Char('g') | Key::Home => state.selected = Some(rows[0].pane_id.clone()),
        Key::Char('G') | Key::End => state.selected = Some(rows[rows.len() - 1].pane_id.clone()),
        Key::Enter => jump(state, rows, host),
        Key::Char('p') => {
            let row = current(rows, state).clone();
            if row.pane_id == state.origin {
                say(state, "Cannot park the pane this dashboard was opened from");
            } else if row.parked {
                say(state, "That pane is already parked");
            } else {
                act(state, || host.park(&row.pane_id));
            }
        }
        Key::Char('u') => {
            let row = current(rows, state).clone();
            if !row.parked {
                say(state, "That pane is not parked");
            } else {
                bring_back(state, &row, host);
            }
        }
        Key::Char('r') => {
            let row = current(rows, state);
            state.mode = Mode::Rename;
            state.text = row.label.clone();
            state.target = row.pane_id.clone();
            state.target_label = row.label.clone();
        }
        Key::Char('x') => {
            let row = current(rows, state);
            state.mode = Mode::ConfirmKill;
            state.target = row.pane_id.clone();
            state.target_label = row.label.clone();
        }
        _ => {}
    }
}

fn csi_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").unwrap()
    })
}

fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let spaces = 8 - (col % 8);
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

pub fn read_rows(host: &dyn DashHost) -> Result<Vec<Row>> {
    Ok(build_rows(&host.list_panes()?))
}

pub fn read_preview(host: &dyn DashHost, pane_id: &str, lines: usize) -> Vec<String> {
    let Ok(captured) = host.capture_pane(pane_id) else {
        return Vec::new();
    };
    let csi = csi_regex();
    let mut out: Vec<String> = text::py_splitlines(&captured)
        .into_iter()
        .map(|line| {
            let stripped = csi.replace_all(line, "");
            let clean: String = stripped
                .chars()
                .filter(|&c| text::is_printable(c) || c == '\t')
                .collect();
            expand_tabs(&clean)
        })
        .collect();
    while out.last().is_some_and(|line| line.trim().is_empty()) {
        out.pop();
    }
    if out.len() > lines {
        out.drain(..out.len() - lines);
    }
    out
}

pub fn visible_rows(rows: &[Row], state: &State) -> Vec<Row> {
    filter_rows(rows, &state.query, state.agents_only)
}

/// Follow the same pane across refreshes; if it is gone, stay at the same height.
pub fn keep_selection(rows: &[Row], state: &mut State, previous_index: usize) {
    if !rows.is_empty()
        && !state
            .selected
            .as_deref()
            .is_some_and(|sel| rows.iter().any(|r| r.pane_id == sel))
    {
        state.selected = Some(rows[previous_index.min(rows.len() - 1)].pane_id.clone());
    }
}

fn flag_number(args: &[String], flag: &str, default: usize) -> usize {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

pub fn main_with(
    host: &dyn DashHost,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    if !host.server_running() {
        writeln!(
            err,
            "tmux-workspace dash: no tmux server is running (start one with: work)"
        )?;
        return Ok(1);
    }
    if args.iter().any(|a| a == "--once") {
        let width = flag_number(args, "--width", 100);
        let height = flag_number(args, "--height", 30);
        let color = args.iter().any(|a| a == "--color");
        let state = State {
            origin: std::env::var("TMUX_PANE").unwrap_or_default(),
            ..State::default()
        };
        let rows = read_rows(host)?;
        let preview = if !rows.is_empty() && height >= 20 {
            read_preview(host, &rows[0].pane_id, 10)
        } else {
            Vec::new()
        };
        writeln!(
            out,
            "{}",
            frame_text(&render(&rows, &state, width, height, &preview), color)
        )?;
        return Ok(0);
    }
    terminal::run_loop(host)
}

pub fn main(tmux: &Tmux, args: &[String]) -> Result<i32> {
    main_with(
        &TmuxDash(tmux),
        args,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}
