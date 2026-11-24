//! Dashboard pure renderer, golden frames, key handling, and live scratch-tmux UI tests.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tmux_workspace::dash::render::{
    Line, Mode, State, cell_width, fit, frame_text, render, render_with,
};
use tmux_workspace::dash::{DashHost, Key, handle_key, main_with, read_preview};
use tmux_workspace::error::{Error, Result};
use tmux_workspace::model::{Row, build_rows};
use tmux_workspace::testing::{ScratchTmux, TempDir};
use tmux_workspace::text;
use tmux_workspace::tmux::Fields;

const EXE: &str = env!("CARGO_BIN_EXE_tmux-workspace");
const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");

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

fn fixture_fields() -> Vec<Fields> {
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

fn fixture_rows() -> Vec<Row> {
    build_rows(&fixture_fields())
}

fn preview() -> Vec<String> {
    ["$ make test", "Ran 235 tests in 21.8s", "", "OK"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn lines_text(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.iter().map(|(t, _)| t.as_str()).collect())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Width & ShortPath

#[test]
fn cell_width_counts_wide_characters_twice() {
    assert_eq!(cell_width("ab"), 2);
    assert_eq!(cell_width("日本"), 4);
    assert_eq!(cell_width("🙂"), 2);
    assert_eq!(cell_width("e\u{0301}"), 1);
}

#[test]
fn fit_truncates_with_an_ellipsis_and_pads() {
    assert_eq!(fit("terraform", 6), "terra…");
    assert_eq!(fit("ui", 4), "ui  ");
    assert_eq!(cell_width(&fit("日本語ラベル", 5)), 5);
}

#[test]
fn home_through_a_symlink_still_shortens() {
    let dir = TempDir::new();
    let real = dir.path().canonicalize().unwrap().join("real");
    std::fs::create_dir_all(real.join("code")).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        text::short_path(&real.join("code").to_string_lossy(), &link),
        "~/code"
    );
    assert_eq!(
        text::short_path(&link.join("code").to_string_lossy(), &link),
        "~/code"
    );
    assert_eq!(text::short_path("/srv/elsewhere", &link), "/srv/elsewhere");
}

// ---------------------------------------------------------------------------------------------
// Render & Golden frames

#[test]
fn every_line_fills_the_width() {
    let rows = fixture_rows();
    let prev = preview();
    for (w, h) in [(120, 40), (80, 24), (50, 20), (40, 12), (30, 6)] {
        let lines = render(&rows, &State::default(), w, h, &prev);
        assert_eq!(lines.len(), h, "size {w}x{h}");
        let widths: HashSet<usize> = lines_text(&lines).iter().map(|l| cell_width(l)).collect();
        assert_eq!(widths, HashSet::from([w]), "size {w}x{h}");
    }
}

#[test]
fn header_counts_and_zero_omission() {
    let rows = fixture_rows();
    let top = &lines_text(&render(&rows, &State::default(), 120, 40, &[]))[0];
    assert!(top.contains("tmux-workspace"));
    assert!(top.contains("1 blocked · 1 done · 1 working · 1 idle"));
    assert!(top.contains("6 panes · 4 workspaces"));

    let without_done: Vec<Row> = rows.into_iter().filter(|r| r.status != "done").collect();
    let top2 = &lines_text(&render(&without_done, &State::default(), 120, 40, &[]))[0];
    assert!(!top2.contains("done"));
}

#[test]
fn column_header_mockup_cells_and_selection() {
    let rows = fixture_rows();
    let out = lines_text(&render(&rows, &State::default(), 120, 40, &preview())).join("\n");
    for cell in [
        "WORKSPACE",
        "TAB",
        "PANE",
        "AGENT",
        "STATUS",
        "1:code",
        "1:claude",
        "● blocked",
        "○ idle",
        "⇣ parked",
    ] {
        assert!(out.contains(cell), "missing {cell}");
    }

    let lines = lines_text(&render(&rows, &State::default(), 120, 40, &[]));
    assert!(lines[2].starts_with("│ ▸ api"), "{}", lines[2]);

    let state = State {
        selected: Some("%5".into()),
        ..State::default()
    };
    let lines = lines_text(&render(&rows, &state, 120, 40, &[]));
    let sel_line = lines.iter().find(|l| l.contains('▸')).unwrap();
    assert_eq!(sel_line.split_whitespace().nth(2), Some("infra"));

    let scrolled = State {
        selected: Some("%4".into()),
        ..State::default()
    };
    let lines = lines_text(&render(&rows, &scrolled, 80, 8, &[]));
    assert!(lines.iter().any(|l| l.contains("▸ docs")));

    let parked_line = lines_text(&render(&rows, &State::default(), 120, 40, &[]))
        .into_iter()
        .find(|l| l.contains("⇣ parked"))
        .unwrap();
    assert!(
        parked_line
            .split("⇣ parked")
            .nth(1)
            .unwrap()
            .contains("api")
    );
}

#[test]
fn wide_labels_keep_columns_aligned() {
    let mut rows = fixture_rows();
    rows[0].label = "日本語ラベル🙂".into();
    let lines = lines_text(&render(&rows, &State::default(), 80, 24, &[]));
    let widths: HashSet<usize> = lines.iter().map(|l| cell_width(l)).collect();
    assert_eq!(widths, HashSet::from([80]));

    let table: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with('│') && (l.contains('●') || l.contains('○')))
        .collect();
    assert_eq!(table.len(), 4);
    let cols: HashSet<usize> = table
        .iter()
        .map(|l| {
            let pos = [l.find('●'), l.find('○')]
                .into_iter()
                .flatten()
                .min()
                .unwrap();
            cell_width(&l[..pos])
        })
        .collect();
    assert_eq!(cols.len(), 1);
}

#[test]
fn narrow_hides_agent_and_preview_tall_shows_preview() {
    let rows = fixture_rows();
    let narrow = lines_text(&render(&rows, &State::default(), 48, 18, &preview())).join("\n");
    assert!(!narrow.contains("AGENT"));
    assert!(!narrow.contains("preview"));

    let tall = lines_text(&render(&rows, &State::default(), 100, 30, &preview())).join("\n");
    assert!(tall.contains("preview"));
    assert!(tall.contains("Ran 235 tests"));
}

#[test]
fn footer_modes_empty_help_and_colour() {
    let rows = fixture_rows();
    let footer = lines_text(&render(&rows, &State::default(), 120, 40, &[]))
        .pop()
        .unwrap();
    assert!(footer.starts_with("└ enter jump"));
    assert!(footer.contains("q quit"));

    let filter_state = State {
        mode: Mode::Filter,
        text: "inf".into(),
        ..State::default()
    };
    let footer = lines_text(&render(&rows, &filter_state, 120, 40, &[]))
        .pop()
        .unwrap();
    assert!(footer.contains("/inf"));

    let msg_state = State {
        message: "can't find pane".into(),
        message_until: f64::INFINITY,
        ..State::default()
    };
    let footer = lines_text(&render(&rows, &msg_state, 120, 40, &[]))
        .pop()
        .unwrap();
    assert!(footer.contains("can't find pane"));

    let empty = lines_text(&render(&[], &State::default(), 80, 24, &[])).join("\n");
    assert!(empty.contains("No panes"));

    let help = lines_text(&render(
        &rows,
        &State {
            mode: Mode::Help,
            ..State::default()
        },
        100,
        30,
        &[],
    ))
    .join("\n");
    assert!(help.contains("Jump to the pane"));

    let coloured = frame_text(&render(&rows, &State::default(), 120, 40, &[]), true);
    assert!(coloured.contains("\x1b["));
    assert!(coloured.contains("38;2;247;118;142"));
    assert!(coloured.ends_with("\x1b[0m"));
}

#[test]
fn golden_frames_match_byte_for_byte() {
    let rows = fixture_rows();
    let prev = preview();
    let fake_home = Path::new("/nonexistent-home-for-golden");
    for (w, h) in [(120, 40), (80, 24), (50, 20)] {
        let lines = render_with(&rows, &State::default(), w, h, &prev, 0.0, fake_home);
        let frame = format!("{}\n", frame_text(&lines, false));
        let path = Path::new(GOLDEN_DIR).join(format!("dashboard-{w}x{h}.txt"));
        let expected = std::fs::read_to_string(&path).unwrap();
        assert_eq!(frame, expected, "golden mismatch for {w}x{h}");
    }
}

// ---------------------------------------------------------------------------------------------
// Keys & Main with FakeDashHost

#[derive(Default)]
struct FakeDash {
    running: bool,
    panes: Vec<Fields>,
    capture: String,
    capture_err: bool,
    select_err: Option<String>,
    calls: RefCell<Vec<String>>,
}

impl FakeDash {
    fn ok() -> Self {
        Self {
            running: true,
            panes: fixture_fields(),
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl DashHost for FakeDash {
    fn list_panes(&self) -> Result<Vec<Fields>> {
        Ok(self.panes.clone())
    }

    fn capture_pane(&self, pane_id: &str) -> Result<String> {
        self.calls
            .borrow_mut()
            .push(format!("capture-pane {pane_id}"));
        if self.capture_err {
            Err(Error::Msg("gone".into()))
        } else {
            Ok(self.capture.clone())
        }
    }

    fn server_running(&self) -> bool {
        self.running
    }

    fn select_pane(&self, pane_id: &str) -> Result<()> {
        if let Some(msg) = &self.select_err {
            return Err(Error::Msg(msg.clone()));
        }
        self.calls.borrow_mut().push(format!("select {pane_id}"));
        Ok(())
    }

    fn kill_pane(&self, pane_id: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("kill-pane {pane_id}"));
        Ok(())
    }

    fn park(&self, pane_id: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("park {pane_id}"));
        Ok(())
    }

    fn rejoin(&self, pane_id: &str, origin: &str) -> Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("rejoin {pane_id} {origin}"));
        Ok(())
    }

    fn set_label(&self, pane_id: &str, label: &str) -> Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("set_label {pane_id} {label}"));
        Ok(())
    }
}

fn press(host: &FakeDash, keys: &[Key], mut state: State, rows: &[Row]) -> State {
    for &key in keys {
        handle_key(key, &mut state, rows, host);
    }
    state
}

fn origin_state() -> State {
    State {
        origin: "%9".into(),
        ..State::default()
    }
}

#[test]
fn keys_navigation_jump_park_unpark_rename_kill_filter_help_and_errors() {
    let rows = fixture_rows();
    let parked = rows.iter().find(|r| r.parked).unwrap().clone();

    // Down then enter jumps to second row.
    let host = FakeDash::ok();
    let state = press(&host, &[Key::Down, Key::Enter], origin_state(), &rows);
    assert_eq!(host.calls(), [format!("select {}", rows[1].pane_id)]);
    assert!(state.quit);

    // j/k/g/G movement.
    let host = FakeDash::ok();
    assert_eq!(
        press(
            &host,
            &[Key::Char('j'), Key::Char('j')],
            origin_state(),
            &rows
        )
        .selected
        .as_deref(),
        Some(rows[2].pane_id.as_str())
    );
    assert_eq!(
        press(
            &host,
            &[Key::Char('j'), Key::Char('j'), Key::Char('k')],
            origin_state(),
            &rows
        )
        .selected
        .as_deref(),
        Some(rows[1].pane_id.as_str())
    );
    assert_eq!(
        press(&host, &[Key::Char('G')], origin_state(), &rows)
            .selected
            .as_deref(),
        Some(rows.last().unwrap().pane_id.as_str())
    );
    assert_eq!(
        press(
            &host,
            &[Key::Char('G'), Key::Char('g')],
            origin_state(),
            &rows
        )
        .selected
        .as_deref(),
        Some(rows[0].pane_id.as_str())
    );
    assert_eq!(
        press(&host, &[Key::Char('k')], origin_state(), &rows)
            .selected
            .as_deref(),
        Some(rows[0].pane_id.as_str())
    );

    // Enter on parked row rejoins next to origin.
    let host = FakeDash::ok();
    let state = press(
        &host,
        &[Key::Enter],
        State {
            origin: "%9".into(),
            selected: Some(parked.pane_id.clone()),
            ..State::default()
        },
        &rows,
    );
    assert_eq!(host.calls(), [format!("rejoin {} %9", parked.pane_id)]);
    assert!(state.quit);

    // Park refuses origin pane.
    let host = FakeDash::ok();
    let state = press(
        &host,
        &[Key::Char('p')],
        State {
            origin: rows[0].pane_id.clone(),
            ..State::default()
        },
        &rows,
    );
    assert!(host.calls().is_empty());
    assert!(
        state
            .message
            .contains("Cannot park the pane this dashboard was opened from")
    );

    // Park selected pane.
    let host = FakeDash::ok();
    press(
        &host,
        &[Key::Char('j'), Key::Char('p')],
        origin_state(),
        &rows,
    );
    assert_eq!(host.calls(), [format!("park {}", rows[1].pane_id)]);

    // Unpark requires a parked row.
    let host = FakeDash::ok();
    let state = press(&host, &[Key::Char('u')], origin_state(), &rows);
    assert!(state.message.contains("not parked"));
    press(
        &host,
        &[Key::Char('u')],
        State {
            origin: "%9".into(),
            selected: Some(parked.pane_id.clone()),
            ..State::default()
        },
        &rows,
    );
    assert_eq!(host.calls(), [format!("rejoin {} %9", parked.pane_id)]);

    // Rename sets label; Escape cancels.
    let host = FakeDash::ok();
    let mut keys = vec![Key::Char('r')];
    keys.extend(std::iter::repeat_n(Key::Backspace, 10));
    keys.extend(['l', 'o', 'g', 's'].map(Key::Char));
    keys.push(Key::Enter);
    let state = press(&host, &keys, origin_state(), &rows);
    assert_eq!(
        host.calls(),
        [format!("set_label {} logs", rows[0].pane_id)]
    );
    assert_eq!(state.mode, Mode::Normal);

    let host = FakeDash::ok();
    press(
        &host,
        &[Key::Char('r'), Key::Char('x'), Key::Escape],
        origin_state(),
        &rows,
    );
    assert!(host.calls().is_empty());

    // Kill requires y; targets the pane fixed when x was pressed; no-ops if gone.
    let host = FakeDash::ok();
    press(
        &host,
        &[Key::Char('x'), Key::Char('n')],
        origin_state(),
        &rows,
    );
    assert!(host.calls().is_empty());
    press(
        &host,
        &[Key::Char('x'), Key::Char('y')],
        origin_state(),
        &rows,
    );
    assert_eq!(host.calls(), [format!("kill-pane {}", rows[0].pane_id)]);

    let host = FakeDash::ok();
    let mut state = press(
        &host,
        &[Key::Char('j'), Key::Char('x')],
        origin_state(),
        &rows,
    );
    let mut reordered = vec![rows[2].clone(), rows[1].clone(), rows[0].clone()];
    reordered.extend(rows[3..].iter().cloned());
    state.selected = Some(reordered[0].pane_id.clone());
    handle_key(Key::Char('y'), &mut state, &reordered, &host);
    assert_eq!(host.calls(), [format!("kill-pane {}", rows[1].pane_id)]);

    let host = FakeDash::ok();
    let mut state = press(
        &host,
        &[Key::Char('j'), Key::Char('x')],
        origin_state(),
        &rows,
    );
    let remaining: Vec<Row> = rows
        .iter()
        .filter(|r| r.pane_id != rows[1].pane_id)
        .cloned()
        .collect();
    handle_key(Key::Char('y'), &mut state, &remaining, &host);
    assert!(host.calls().is_empty());
    assert!(state.message.contains("gone"));

    // Rename targets the pane chosen when r was pressed.
    let host = FakeDash::ok();
    let mut state = press(
        &host,
        &[Key::Char('j'), Key::Char('r'), Key::Char('x')],
        origin_state(),
        &rows,
    );
    state.selected = Some(rows[0].pane_id.clone());
    handle_key(Key::Enter, &mut state, &rows, &host);
    assert_eq!(
        host.calls(),
        [format!("set_label {} {}x", rows[1].pane_id, rows[1].label)]
    );

    // Kill prompt names the pane.
    let state = press(&host, &[Key::Char('x')], origin_state(), &rows);
    let footer = lines_text(&render(&rows, &state, 120, 40, &[]))
        .pop()
        .unwrap();
    assert!(footer.contains(&format!("close {}?", rows[0].label)));

    // Filter is live; Escape clears filter before quitting.
    let host = FakeDash::ok();
    let state = press(
        &host,
        &[
            Key::Char('/'),
            Key::Char('i'),
            Key::Char('n'),
            Key::Char('f'),
        ],
        origin_state(),
        &rows,
    );
    assert_eq!((state.mode, state.query.as_str()), (Mode::Filter, "inf"));
    assert_eq!(
        press(&host, &[Key::Enter], state.clone(), &rows).query,
        "inf"
    );
    assert_eq!(press(&host, &[Key::Escape], state, &rows).query, "");

    let state = press(
        &host,
        &[Key::Char('/'), Key::Char('x'), Key::Enter, Key::Escape],
        origin_state(),
        &rows,
    );
    assert_eq!((state.query.as_str(), state.quit), ("", false));
    assert!(press(&host, &[Key::Escape], state, &rows).quit);

    // Agents toggle and help mode.
    assert!(press(&host, &[Key::Char('a')], origin_state(), &rows).agents_only);
    assert!(
        !press(
            &host,
            &[Key::Char('a'), Key::Char('a')],
            origin_state(),
            &rows
        )
        .agents_only
    );
    assert_eq!(
        press(&host, &[Key::Char('?')], origin_state(), &rows).mode,
        Mode::Help
    );
    assert_eq!(
        press(
            &host,
            &[Key::Char('?'), Key::Char('j')],
            origin_state(),
            &rows
        )
        .mode,
        Mode::Normal
    );

    // Vanished pane error reaches footer without quitting.
    let mut host = FakeDash::ok();
    host.select_err = Some("can't find pane: %1\n".into());
    let state = press(&host, &[Key::Enter], origin_state(), &rows);
    assert!(state.message.contains("can't find pane"));
    assert!(!state.quit);

    // Keys on empty table do nothing.
    let host = FakeDash::ok();
    let state = press(
        &host,
        &[
            Key::Char('j'),
            Key::Enter,
            Key::Char('p'),
            Key::Char('x'),
            Key::Char('y'),
            Key::Char('r'),
            Key::Enter,
        ],
        origin_state(),
        &[],
    );
    assert!(host.calls().is_empty());
    assert!(!state.quit);
}

#[test]
fn main_once_error_outside_tmux_and_preview_stripping() {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let down = FakeDash::default();
    let code = main_with(&down, &["--once".into()], &mut out, &mut err).unwrap();
    assert_eq!(code, 1);
    assert_eq!(
        String::from_utf8(err).unwrap().trim(),
        "tmux-workspace dash: no tmux server is running (start one with: work)"
    );

    let host = FakeDash::ok();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = main_with(
        &host,
        &["--once", "--width", "90", "--height", "12"].map(String::from),
        &mut out,
        &mut err,
    )
    .unwrap();
    let stdout = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = stdout.trim_end_matches('\n').split('\n').collect();
    assert_eq!((code, lines.len()), (0, 12));
    let widths: HashSet<usize> = lines.iter().map(|l| cell_width(l)).collect();
    assert_eq!(widths, HashSet::from([90]));
    assert!(stdout.contains("infra"));

    // --height 19 never captures preview.
    let host = FakeDash::ok();
    let mut out = Vec::new();
    let mut err = Vec::new();
    main_with(
        &host,
        &["--once", "--height", "19"].map(String::from),
        &mut out,
        &mut err,
    )
    .unwrap();
    assert!(host.calls().is_empty());

    // Preview strips ANSI control sequences and trailing blanks.
    let mut host = FakeDash::ok();
    host.capture = "one\n\x1b[31mtwo\x07\n\n\n".into();
    assert_eq!(read_preview(&host, "%1", 5), ["one", "two"]);
    host.capture_err = true;
    assert!(read_preview(&host, "%1", 5).is_empty());
}

// ---------------------------------------------------------------------------------------------
// Live scratch-tmux dashboard tests (ported from tests/test_dashboard_tmux.py)

fn wait_until(cond: impl Fn() -> bool, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(cond(), "condition did not hold within {timeout:?}");
}

fn dash_in_window(scratch: &ScratchTmux) -> String {
    let cmd = format!(
        "TMUX_WORKSPACE_CONFIG_DIR={} TMUX_WORKSPACE_STATE_DIR={} {} dash",
        text::shlex_quote(&scratch.home().join("config").to_string_lossy()),
        text::shlex_quote(&scratch.home().join("state").to_string_lossy()),
        text::shlex_quote(EXE),
    );
    let pane = scratch.tmux(&[
        "new-window",
        "-t",
        "alpha:",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-n",
        "dash",
        &cmd,
    ]);
    wait_until(
        || {
            scratch
                .tmux(&["capture-pane", "-t", &pane, "-p"])
                .contains("WORKSPACE")
        },
        Duration::from_secs(10),
    );
    pane
}

#[test]
fn scratch_tmux_once_lists_every_pane() {
    let scratch = ScratchTmux::new();
    scratch.pane(&["new-window", "-t", "alpha:", "-n", "two"]);
    let out = Command::new(EXE)
        .args(["dash", "--once", "--width", "100", "--height", "30"])
        .env("TMUX", format!("{},1,0", scratch.socket()))
        .env("TMUX_WORKSPACE_CONFIG_DIR", scratch.home().join("config"))
        .env("TMUX_WORKSPACE_STATE_DIR", scratch.home().join("state"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("0:one"), "{stdout}");
    assert!(stdout.contains("1:two"), "{stdout}");

    // Also works in a bare POSIX/C locale where tmux would otherwise replace \x1f with `_`.
    let out_c = Command::new(EXE)
        .args(["dash", "--once", "--width", "100", "--height", "30"])
        .env("TMUX", format!("{},1,0", scratch.socket()))
        .env("TMUX_WORKSPACE_CONFIG_DIR", scratch.home().join("config"))
        .env("TMUX_WORKSPACE_STATE_DIR", scratch.home().join("state"))
        .env("LC_ALL", "C")
        .env_remove("LANG")
        .env_remove("LC_CTYPE")
        .output()
        .unwrap();
    assert!(
        out_c.status.success(),
        "LC_ALL=C failed: {}",
        String::from_utf8_lossy(&out_c.stderr)
    );
    let stdout_c = String::from_utf8_lossy(&out_c.stdout);
    assert!(stdout_c.contains("0:one"), "{stdout_c}");
    assert!(stdout_c.contains("1:two"), "{stdout_c}");
}

#[test]
fn scratch_tmux_interactive_ui_draws_and_enter_selects() {
    let scratch = ScratchTmux::new();
    scratch.pane(&["new-window", "-t", "alpha:", "-n", "two"]);
    let dash = dash_in_window(&scratch);
    scratch.tmux(&["send-keys", "-t", &dash, "j"]);
    scratch.tmux(&["send-keys", "-t", &dash, "Enter"]);
    wait_until(
        || scratch.expand("alpha", "#{window_name}") == "two",
        Duration::from_secs(10),
    );
}

#[test]
fn scratch_tmux_p_parks_the_selected_pane() {
    let scratch = ScratchTmux::new();
    let first = scratch.pane(&["new-window", "-t", "alpha:", "-n", "two"]);
    scratch.pane(&["split-window", "-t", &first]);
    let dash = dash_in_window(&scratch);
    scratch.tmux(&["send-keys", "-t", &dash, "j"]);
    scratch.tmux(&["send-keys", "-t", &dash, "p"]);
    wait_until(
        || scratch.expand(&first, "#{session_name}") == "__parked",
        Duration::from_secs(10),
    );
}
