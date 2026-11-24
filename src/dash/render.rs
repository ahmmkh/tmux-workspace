//! Pure renderer for the tmux-workspace dashboard: rows and state in, styled lines out.
//! No tmux calls and no terminal I/O, so layout and golden frames are tested directly.

use crate::model::{AGENT_ORDER, Row, summary};
use crate::paths;
use crate::text;
use std::collections::HashSet;
use std::path::Path;
use unicode_width::UnicodeWidthChar;

pub type Segment = (String, String);
pub type Line = Vec<Segment>;

pub const SELECTION_RGB: (u8, u8, u8) = (0x28, 0x34, 0x57);

pub const HINTS: &[(&str, &str)] = &[
    ("enter", "jump"),
    ("/", "filter"),
    ("a", "agents"),
    ("p", "park"),
    ("u", "unpark"),
    ("r", "rename"),
    ("x", "kill"),
    ("?", "help"),
    ("q", "quit"),
];

/// What a narrow footer keeps first.
pub const HINT_PRIORITY: &[&str] = &["enter", "q", "?", "/", "p", "u", "r", "x", "a"];

pub const HELP: &[(&str, &str)] = &[
    ("j k ↓ ↑", "Move the selection"),
    ("g G", "First or last row"),
    (
        "enter",
        "Jump to the pane; a parked pane comes back next to yours",
    ),
    ("/", "Filter by workspace, tab, label or agent; esc clears"),
    ("a", "Show only panes that run an agent"),
    ("p", "Park the pane: hide it, keep it running"),
    ("u", "Bring a parked pane back next to yours"),
    ("r", "Rename the pane label"),
    ("x", "Close the pane (asks first)"),
    ("?", "Show or hide this help"),
    ("q esc", "Close the dashboard"),
];

pub const STATUS_W: usize = 10;
pub const AGENT_W: usize = 8;
pub const PANE_MAX: usize = 28;
pub const PATH_MIN: usize = 12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Filter,
    Rename,
    ConfirmKill,
    Help,
}

#[derive(Clone, Debug, Default)]
pub struct State {
    /// Selected pane id.
    pub selected: Option<String>,
    /// The pane the dashboard was opened from.
    pub origin: String,
    pub mode: Mode,
    /// What the user is typing in filter or rename mode.
    pub text: String,
    /// The pane a rename or close question is about, fixed when it is asked.
    pub target: String,
    pub target_label: String,
    pub query: String,
    pub agents_only: bool,
    pub message: String,
    pub message_until: f64,
    pub quit: bool,
    pub top: usize,
}

/// Tokyo Night palette: `(rgb, bold)` for a base style name.
pub(crate) fn style_rgb(name: &str) -> ((u8, u8, u8), bool) {
    match name {
        "frame" => ((0x3b, 0x42, 0x61), false),
        "title" => ((0x7a, 0xa2, 0xf7), true),
        "header" => ((0x56, 0x5f, 0x89), true),
        "dim" => ((0x56, 0x5f, 0x89), false),
        "selected" => ((0xc0, 0xca, 0xf5), true),
        "blocked" => ((0xf7, 0x76, 0x8e), true),
        "done" => ((0x7d, 0xcf, 0xff), false),
        "working" => ((0xe0, 0xaf, 0x68), false),
        "idle" => ((0x9e, 0xce, 0x6a), false),
        "key" => ((0x7a, 0xa2, 0xf7), false),
        "error" => ((0xf7, 0x76, 0x8e), true),
        _ => ((0xc0, 0xca, 0xf5), false),
    }
}

fn mark(status: &str) -> &'static str {
    match status {
        "blocked" | "done" | "working" => "●",
        "idle" => "○",
        _ => "",
    }
}

pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

pub fn cell_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// Exactly `width` cells: cut with an ellipsis when too long, pad with spaces when short.
pub fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let w = cell_width(text);
    if w <= width {
        return format!("{text}{}", " ".repeat(width - w));
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cw = char_width(ch);
        if used + cw > width - 1 {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    for _ in 0..(width - 1 - used) {
        out.push(' ');
    }
    out
}

/// Like `fit`, but keeps the end of the text: `…/deep/directory`.
pub fn fit_left(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if cell_width(text) <= width {
        return fit(text, width);
    }
    let mut rev = Vec::new();
    let mut used = 0;
    for ch in text.chars().rev() {
        let cw = char_width(ch);
        if used + cw > width - 1 {
            break;
        }
        rev.push(ch);
        used += cw;
    }
    let mut out = " ".repeat(width - 1 - used);
    out.push('…');
    for ch in rev.into_iter().rev() {
        out.push(ch);
    }
    out
}

/// `~/code` for a path under the home folder, also when reached through a symlink.
pub fn short_path(path: &str) -> String {
    text::short_path(path, &paths::home())
}

/// Cut or pad a styled line to exactly `width` cells.
pub fn fit_line(line: &[Segment], width: usize) -> Line {
    let mut out = Vec::new();
    let mut used = 0;
    for (seg_text, style) in line {
        let room = width.saturating_sub(used);
        if room == 0 {
            break;
        }
        let fitted = if cell_width(seg_text) > room {
            fit(seg_text, room)
        } else {
            seg_text.clone()
        };
        used += cell_width(&fitted);
        out.push((fitted, style.clone()));
    }
    if used < width {
        out.push((" ".repeat(width - used), String::new()));
    }
    out
}

pub fn selected_index(rows: &[Row], state: &State) -> usize {
    if let Some(sel) = &state.selected
        && let Some(i) = rows.iter().position(|r| r.pane_id == *sel)
    {
        return i;
    }
    0
}

pub fn workspace_cell(row: &Row) -> &str {
    if row.parked {
        "⇣ parked"
    } else {
        &row.workspace
    }
}

pub fn tab_cell(row: &Row) -> String {
    if row.parked {
        row.workspace.clone()
    } else {
        format!("{}:{}", row.window_index, row.tab)
    }
}

pub fn status_cell(row: &Row) -> String {
    if row.status.is_empty() {
        "—".to_string()
    } else {
        format!("{} {}", mark(row.status), row.status)
    }
}

/// Widths of workspace, tab, pane, agent and path; agent and path are 0 when hidden. STATUS is fixed.
pub fn columns(rows: &[Row], width: usize) -> (usize, usize, usize, usize, usize) {
    let inner = width as isize - 6; // '│ ', the two-cell selection marker, ' │'
    let agent = if width >= 50 { AGENT_W } else { 0 };
    let mut ws = rows
        .iter()
        .map(|r| cell_width(workspace_cell(r)))
        .fold(9usize, usize::max)
        .min(20);
    let mut tab = rows
        .iter()
        .map(|r| cell_width(&tab_cell(r)))
        .fold(3usize, usize::max)
        .min(16);
    if width < 80 {
        ws = ws.min(12);
        tab = tab.min(8);
    }
    let gaps: isize = if agent > 0 { 4 } else { 3 };
    let mut pane = inner - (ws + tab + agent + STATUS_W) as isize - gaps;
    let mut path = 0usize;
    let cap = rows
        .iter()
        .map(|r| cell_width(&format!("{}:{}", r.pane_index, r.label)))
        .fold(8usize, usize::max)
        .min(PANE_MAX) as isize;
    if pane > cap + 1 + PATH_MIN as isize {
        path = (pane - cap - 1) as usize;
        pane = cap;
    }
    while pane < 8 && tab > 4 {
        tab -= 1;
        pane += 1;
    }
    while pane < 8 && ws > 6 {
        ws -= 1;
        pane += 1;
    }
    (ws, tab, pane.max(1) as usize, agent, path)
}

fn seg(text: impl Into<String>, style: impl Into<String>) -> Segment {
    (text.into(), style.into())
}

fn line_width(line: &[Segment]) -> usize {
    line.iter().map(|(t, _)| cell_width(t)).sum()
}

pub fn top_border(rows: &[Row], width: usize) -> Line {
    let counts = summary(rows);
    let mut right: Line = Vec::new();
    for status in AGENT_ORDER {
        let n = counts.for_status(status);
        if n > 0 {
            if !right.is_empty() {
                right.push(seg(" · ", "dim"));
            }
            right.push(seg(format!("{n} {status}"), status));
        }
    }
    let totals = format!(
        "{} pane{} · {} workspace{}",
        counts.panes,
        if counts.panes == 1 { "" } else { "s" },
        counts.workspaces,
        if counts.workspaces == 1 { "" } else { "s" },
    );
    let mut full = right.clone();
    if !full.is_empty() {
        full.push(seg(" · ", "dim"));
    }
    full.push(seg(totals, "dim"));

    let prefix_w = cell_width("┌ tmux-workspace ");
    let suffix_w = cell_width("  ┐");
    for candidate in [full, right, Vec::new()] {
        let used = prefix_w + line_width(&candidate) + suffix_w;
        if used <= width {
            let fill = "─".repeat(width - used);
            if candidate.is_empty() {
                return vec![
                    seg("┌ ", "frame"),
                    seg("tmux-workspace", "title"),
                    seg(format!(" {fill}──┐"), "frame"),
                ];
            }
            let mut out = vec![
                seg("┌ ", "frame"),
                seg("tmux-workspace", "title"),
                seg(format!(" {fill} "), "frame"),
            ];
            out.extend(candidate);
            out.push(seg(" ┐", "frame"));
            return out;
        }
    }
    let mut out = fit_line(
        &[seg("┌ ", "frame"), seg("tmux-workspace", "title")],
        width.saturating_sub(1),
    );
    out.push(seg("┐", "frame"));
    out
}

pub fn boxed(content: &[Segment], width: usize) -> Line {
    let mut out = vec![seg("│ ", "frame")];
    out.extend(fit_line(content, width.saturating_sub(4)));
    out.push(seg(" │", "frame"));
    out
}

fn table_lines(rows: &[Row], state: &State, width: usize, height: usize, home: &Path) -> Vec<Line> {
    let (ws, tab, pane, agent, path) = columns(rows, width);
    let mut names: Vec<(&str, usize)> = vec![("WORKSPACE", ws), ("TAB", tab), ("PANE", pane)];
    if agent > 0 {
        names.push(("AGENT", agent));
    }
    names.push(("STATUS", STATUS_W));
    if path > 0 {
        names.push(("PATH", path));
    }
    let mut header_content = vec![seg("  ", "")];
    for (i, (name, w)) in names.iter().enumerate() {
        let gap = if i + 1 < names.len() { " " } else { "" };
        header_content.push(seg(format!("{}{gap}", fit(name, *w)), "header"));
    }
    let header = boxed(&header_content, width);
    let body_height = height.saturating_sub(1);
    let mut out = Vec::with_capacity(height);
    out.push(header);
    if body_height == 0 {
        return out;
    }

    if state.mode == Mode::Help {
        for (keys, what) in HELP.iter().take(body_height) {
            out.push(boxed(
                &[
                    seg("  ", ""),
                    seg(fit(keys, 10), "key"),
                    seg(format!(" {what}"), ""),
                ],
                width,
            ));
        }
        while out.len() < height {
            out.push(boxed(&[], width));
        }
        return out;
    }

    if rows.is_empty() {
        let message = if !state.query.is_empty() || state.agents_only {
            "No panes match the filter"
        } else {
            "No panes"
        };
        out.push(boxed(&[seg(format!("  {message}"), "dim")], width));
        while out.len() < height {
            out.push(boxed(&[], width));
        }
        return out;
    }

    let chosen = selected_index(rows, state);
    let mut first = state.top.min(chosen);
    if chosen >= first + body_height {
        first = chosen + 1 - body_height;
    }
    let end = (first + body_height).min(rows.len());
    for (i, row) in rows[first..end].iter().enumerate() {
        let idx = first + i;
        let is_sel = idx == chosen;
        let sel = if is_sel { " sel" } else { "" };
        let base = if is_sel { "selected sel" } else { "" };
        let ws_style = if row.parked {
            format!("dim{sel}")
        } else {
            base.to_string()
        };
        let mut cells = vec![
            seg(if is_sel { "▸ " } else { "  " }, base),
            seg(format!("{} ", fit(workspace_cell(row), ws)), ws_style),
            seg(format!("{} ", fit(&tab_cell(row), tab)), base),
            seg(
                format!(
                    "{} ",
                    fit(&format!("{}:{}", row.pane_index, row.label), pane)
                ),
                base,
            ),
        ];
        if agent > 0 {
            let agent_text = if row.agent.is_empty() {
                "—"
            } else {
                row.agent.as_str()
            };
            let agent_style = if is_sel { "selected sel" } else { "dim" };
            cells.push(seg(format!("{} ", fit(agent_text, agent)), agent_style));
        }
        let st_base = if row.status.is_empty() {
            "dim"
        } else {
            row.status
        };
        cells.push(seg(
            fit(&status_cell(row), STATUS_W),
            format!("{st_base}{sel}"),
        ));
        if path > 0 {
            let shortened = text::short_path(&row.path, home);
            cells.push(seg(
                format!(" {}", fit_left(&shortened, path)),
                format!("dim{sel}"),
            ));
        }
        out.push(boxed(&cells, width));
    }
    while out.len() < height {
        out.push(boxed(&[], width));
    }
    out
}

fn preview_lines(
    rows: &[Row],
    state: &State,
    width: usize,
    count: usize,
    preview: &[String],
) -> Vec<Line> {
    let label = if rows.is_empty() {
        ""
    } else {
        rows[selected_index(rows, state)].label.as_str()
    };
    let used = cell_width(&format!("├─ preview · {label} ┤")) + 1;
    let title = vec![
        seg("├─ ", "frame"),
        seg("preview", "title"),
        seg(format!(" · {label} "), "dim"),
    ];
    let separator = if used > width {
        let mut line = fit_line(&title, width.saturating_sub(1));
        line.push(seg("┤", "frame"));
        line
    } else {
        let mut line = title;
        line.push(seg("─".repeat(width - used + 1), "frame"));
        line.push(seg("┤", "frame"));
        line
    };
    let mut out = Vec::with_capacity(count + 1);
    out.push(separator);
    let shown: &[String] = if count == 0 {
        &[]
    } else if preview.len() > count {
        &preview[preview.len() - count..]
    } else {
        preview
    };
    for line in shown {
        out.push(boxed(&[seg(line.as_str(), "dim")], width));
    }
    for _ in shown.len()..count {
        out.push(boxed(&[seg("", "dim")], width));
    }
    out
}

fn footer(state: &State, width: usize, now: f64) -> Line {
    let mut content: Line = if !state.message.is_empty() && now < state.message_until {
        vec![seg(state.message.as_str(), "error")]
    } else {
        match state.mode {
            Mode::Filter => vec![seg("/", "key"), seg(format!("{}█", state.text), "")],
            Mode::Rename => vec![seg("rename: ", "key"), seg(format!("{}█", state.text), "")],
            Mode::ConfirmKill => vec![
                seg(format!("close {}? ", state.target_label), "error"),
                seg("y", "key"),
                seg(" / ", "dim"),
                seg("n", "key"),
            ],
            Mode::Normal | Mode::Help => {
                let mut c = Vec::new();
                if !state.query.is_empty() || state.agents_only {
                    let mut parts = Vec::new();
                    if !state.query.is_empty() {
                        parts.push(format!("/{}", state.query));
                    }
                    if state.agents_only {
                        parts.push("agents only".to_string());
                    }
                    c.push(seg(parts.join(" "), "title"));
                    c.push(seg(" · esc clears · ", "dim"));
                }
                let room = width.saturating_sub(4).saturating_sub(line_width(&c));
                let mut kept: HashSet<&str> = HashSet::new();
                for &key in HINT_PRIORITY {
                    let mut trial = kept.clone();
                    trial.insert(key);
                    let total: usize = HINTS
                        .iter()
                        .filter(|(k, _)| trial.contains(k))
                        .map(|(k, w)| cell_width(&format!("{k} {w}")) + 3)
                        .sum();
                    if total.saturating_sub(3) <= room {
                        kept = trial;
                    }
                }
                let mut first = true;
                for &(key, what) in HINTS {
                    if kept.contains(key) {
                        if !first {
                            c.push(seg(" · ", "dim"));
                        }
                        first = false;
                        c.push(seg(key, "key"));
                        c.push(seg(format!(" {what}"), "dim"));
                    }
                }
                c
            }
        }
    };
    let room = width.saturating_sub(4);
    if line_width(&content) > room {
        content = fit_line(&content, room);
    }
    let used = line_width(&content);
    let mut out = vec![seg("└ ", "frame")];
    out.extend(content);
    out.push(seg(
        format!(" {}┘", "─".repeat(room.saturating_sub(used))),
        "frame",
    ));
    out
}

/// Render exactly `height` lines of exactly `width` cells.
pub fn render(
    rows: &[Row],
    state: &State,
    width: usize,
    height: usize,
    preview: &[String],
) -> Vec<Line> {
    render_with(
        rows,
        state,
        width,
        height,
        preview,
        text::now(),
        &paths::home(),
    )
}

/// Pure render with explicit clock and home directory (used in tests).
pub fn render_with(
    rows: &[Row],
    state: &State,
    width: usize,
    height: usize,
    preview: &[String],
    now: f64,
    home: &Path,
) -> Vec<Line> {
    if width < 20 || height < 4 {
        return (0..height)
            .map(|_| {
                fit_line(
                    &[seg("tmux-workspace: make the window bigger", "dim")],
                    width,
                )
            })
            .collect();
    }
    let count = if height >= 20 {
        (height / 3).min(10)
    } else {
        0
    };
    let table_height = height - 2 - if count > 0 { count + 1 } else { 0 };
    let mut lines = Vec::with_capacity(height);
    lines.push(top_border(rows, width));
    lines.extend(table_lines(rows, state, width, table_height, home));
    if count > 0 {
        lines.extend(preview_lines(rows, state, width, count, preview));
    }
    lines.push(footer(state, width, now));
    lines
        .into_iter()
        .map(|line| fit_line(&line, width))
        .collect()
}

/// 24-bit truecolor ANSI SGR escape sequence matching `dashboard.py:sgr`.
pub fn sgr(style: &str) -> String {
    let names: Vec<&str> = style.split_whitespace().collect();
    let base = match names.first().copied() {
        Some(first) if first != "sel" => first,
        _ => "",
    };
    let ((r, g, b), bold) = style_rgb(base);
    let selected = names.contains(&"sel");
    let mut codes = Vec::with_capacity(3);
    if bold {
        codes.push("1".to_string());
    }
    codes.push(format!("38;2;{r};{g};{b}"));
    if selected {
        let (sr, sg, sb) = SELECTION_RGB;
        codes.push(format!("48;2;{sr};{sg};{sb}"));
    }
    format!("\x1b[0;{}m", codes.join(";"))
}

pub fn frame_text(lines: &[Line], color: bool) -> String {
    if !color {
        return lines
            .iter()
            .map(|line| line.iter().map(|(t, _)| t.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
    }
    lines
        .iter()
        .map(|line| {
            let mut buf = String::new();
            for (t, style) in line {
                buf.push_str(&sgr(style));
                buf.push_str(t);
            }
            buf.push_str("\x1b[0m");
            buf
        })
        .collect::<Vec<_>>()
        .join("\n")
}
