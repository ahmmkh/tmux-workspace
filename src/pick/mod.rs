//! The workspace and tab pickers, and the commands behind them: `open` (`work NAME`, `prefix s`),
//! `pick-window` (`prefix w`) with its `window-preview`, `new-tab` (`prefix c`) and `kill-workspace`
//! (`prefix X`).

mod window;
mod workspace;

pub use window::{new_tab, pick_window, window_preview};
pub use workspace::{choose_or_create, delete_workspace, onboard, open_workspace, session_names};

use crate::cli::in_tmux;
use crate::error::{Error, Result};
use crate::model::{AGENT_ORDER, RollUp, roll_up};
use crate::paths::{Locations, resolve};
use crate::text::{is_py_space, ljust, py_splitlines, py_strip};
use crate::tmux::Tmux;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// The width of the widest status word, so that the words line up.
pub const STATUS_WIDTH: usize = {
    let mut width = 0;
    let mut at = 0;
    while at < AGENT_ORDER.len() {
        if AGENT_ORDER[at].len() > width {
            width = AGENT_ORDER[at].len();
        }
        at += 1;
    }
    width
};

/// The ANSI colour of a status word: red, cyan, yellow, green.
pub(super) fn colour(status: &str) -> Option<u8> {
    match status {
        "blocked" => Some(31),
        "done" => Some(36),
        "working" => Some(33),
        "idle" => Some(32),
        _ => None,
    }
}

/// A status as a coloured word of `STATUS_WIDTH` characters; nothing for a status that is not one.
pub fn status_word(status: &str) -> String {
    colour(status).map_or_else(String::new, |code| {
        format!("\x1b[{code}m{}\x1b[0m", ljust(status, STATUS_WIDTH))
    })
}

/// The status of every pane, read in one call. A failed read gives no status, so the pickers still
/// list their rows.
pub fn agent_status(tmux: &Tmux) -> RollUp {
    let names = [
        "session_name",
        "window_id",
        "pane_id",
        "pane_current_command",
        "@workspace_agent",
        "@workspace_app",
        "@workspace_home",
    ];
    tmux.fields("list-panes", &names, &["-a"])
        .map(|rows| roll_up(&rows))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// The surroundings

/// What the commands of this module ask of their surroundings.
#[derive(Clone, Debug)]
pub struct Setting {
    /// Where the home, the projects folder and the state are.
    pub loc: Locations,
    /// Whether the input is a terminal.
    pub stdin_tty: bool,
    /// Whether the output is a terminal.
    pub stdout_tty: bool,
    /// Whether this runs inside tmux, where the client can be switched to a workspace. Outside it
    /// the terminal is attached to one.
    pub inside_tmux: bool,
}

impl Setting {
    /// The real surroundings.
    pub fn current() -> Setting {
        // SAFETY: `isatty` only inspects file descriptors 0 and 1.
        let (stdin_tty, stdout_tty) = unsafe { (libc::isatty(0) == 1, libc::isatty(1) == 1) };
        Setting {
            loc: Locations::current(),
            stdin_tty,
            stdout_tty,
            inside_tmux: in_tmux(),
        }
    }

    /// Whether the user can be asked: both the input and the output are a terminal.
    fn terminal(&self) -> bool {
        self.stdin_tty && self.stdout_tty
    }

    /// The folder that holds one directory per project, with its links followed. It is worked out
    /// when it is needed, so that the folder a first-run setup has just chosen is the one used.
    fn projects(&self) -> PathBuf {
        resolve(&self.loc.projects_dir())
    }

    fn state(&self) -> PathBuf {
        self.loc.state_dir()
    }
}

/// `Path(text).expanduser()`, where an empty text is the current folder.
pub(super) fn py_path(loc: &Locations, text: &str) -> PathBuf {
    let path = loc.expand_user(text);
    if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path
    }
}

// ---------------------------------------------------------------------------------------------
// Rows

// fzf searches only the label and prints only the hidden name, or the typed text when no row matches.
pub const TAB_FZF: [&str; 4] = ["fzf", "--ansi", "--delimiter=\t", "--nth=1"];
// No --accept-nth (fzf 0.60+): picked_name keeps the first field, so fzf 0.38 works too.
// --print-query, not the accept-or-print-query action (fzf 0.47+): the query is the first line, a chosen row the last.
pub const WORKSPACE_FZF: [&str; 7] = [
    "fzf",
    "--delimiter=\t",
    "--with-nth=2..",
    "--nth=1",
    "--ansi",
    "--tiebreak=index",
    "--print-query",
];

/// fzf shows and searches the label, shows the status and prints only the name.
pub fn workspace_row(name: &str, label: &str, status: &str) -> String {
    [name, &ljust(label, 24), &status_word(status)].join("\t")
}

/// The workspace name from fzf's output: a chosen row's first field, or the typed query.
pub fn picked_name(output: &str) -> String {
    py_splitlines(output)
        .into_iter()
        .map(py_strip)
        .rfind(|line| !line.is_empty())
        .map_or_else(String::new, |line| {
            py_strip(line.split('\t').next().unwrap_or("")).to_string()
        })
}

/// What `input()` does: show the prompt, read a line. The end of input is [`Error::Eof`].
pub(super) fn ask(prompt: &str, input: &mut dyn BufRead, output: &mut dyn Write) -> Result<String> {
    write!(output, "{prompt}")?;
    output.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err(Error::Eof);
    }
    Ok(line)
}

/// `answer` as a position in a list of `len` items, when it is a number from 1 to `len`.
pub(super) fn position(answer: &str, len: usize) -> Option<usize> {
    if answer.is_empty() || !answer.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    answer
        .parse()
        .ok()
        .filter(|number| (1..=len).contains(number))
}

/// The picker for machines without fzf: a numbered list, or type a new name.
pub fn numbered_pick(
    rows: &[String],
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<String> {
    for (number, row) in rows.iter().enumerate() {
        let mut fields = row.split('\t').skip(1);
        let label = fields.next().unwrap_or("");
        let status = fields.next().unwrap_or("");
        let line = format!(
            "{:>2}. {}  {status}",
            number + 1,
            label.trim_end_matches(is_py_space)
        );
        writeln!(output, "{}", line.trim_end_matches(is_py_space))?;
    }
    let answer = ask(
        "\nNumber, or a new workspace name (Enter cancels): ",
        input,
        output,
    )?;
    let answer = py_strip(&answer);
    if !answer.is_empty() && answer.chars().all(|c| c.is_ascii_digit()) {
        return Ok(match position(answer, rows.len()) {
            Some(number) => rows[number - 1]
                .split('\t')
                .next()
                .unwrap_or("")
                .to_string(),
            None => {
                writeln!(output, "No workspace {answer}")?;
                String::new()
            }
        });
    }
    Ok(answer.to_string())
}

/// fzf searches only the index and the name, which stand before the tab character.
pub fn tab_row(
    index: &str,
    name: &str,
    count: &str,
    labels: &str,
    active: bool,
    status: &str,
) -> String {
    let word = match status_word(status) {
        word if word.is_empty() => " ".repeat(STATUS_WIDTH),
        word => word,
    };
    let plural = if count.trim().parse::<u64>().is_ok_and(|panes| panes > 1) {
        "s"
    } else {
        ""
    };
    let marker = if active { "  ● active" } else { "" };
    format!(
        "{index}: {}\t{word} [{count} pane{plural}: {labels}]{marker}",
        ljust(name, 16)
    )
}

/// The index of a tab from its row, or from the first field of it.
pub fn tab_index(row: &str) -> &str {
    py_strip(row.split(':').next().unwrap_or(""))
}

#[cfg(test)]
#[allow(unused_imports)]
use {
    crate::model::{BOOTSTRAP, PARKING},
    crate::process::{Spawn, spawn, which},
    crate::tmux::SEP,
    std::collections::HashMap,
    std::path::Path,
    window::*,
    workspace::*,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ScratchTmux, ScriptedTmux, TempDir};
    use std::io::Cursor;

    #[test]
    fn a_status_word_is_coloured_and_padded_to_the_widest() {
        assert_eq!(status_word("done"), "\x1b[36mdone   \x1b[0m");
        assert_eq!(status_word("blocked"), "\x1b[31mblocked\x1b[0m");
        assert_eq!(status_word("working"), "\x1b[33mworking\x1b[0m");
        assert_eq!(status_word("idle"), "\x1b[32midle   \x1b[0m");
    }

    #[test]
    fn something_that_is_not_a_status_has_no_word() {
        for status in ["", "busy", "Done", " done"] {
            assert_eq!(status_word(status), "", "{status:?}");
        }
    }

    #[test]
    fn every_status_has_a_colour_and_fits_the_width() {
        assert_eq!(STATUS_WIDTH, 7);
        for status in AGENT_ORDER {
            assert!(colour(status).is_some(), "{status}");
            assert!(status.len() <= STATUS_WIDTH, "{status}");
        }
    }

    #[test]
    fn a_failed_read_gives_no_status() {
        // A socket that does not exist: the read fails and the answer is empty, not an error.
        let missing = Tmux::new().socket("/nonexistent/tmux-workspace-test-socket");
        assert_eq!(agent_status(&missing), RollUp::default());
    }

    // ---------------------------------------------------------------------------------------
    // tests/test_agent_status.py::RollUp, the pickers' read of the status

    // RollUp::test_failing_read_gives_no_status
    #[test]
    fn failing_read_gives_no_status() {
        let failing = ScriptedTmux::new("", 1);
        let status = agent_status(&failing.client());
        assert_eq!(status, RollUp::default());
        // The rows are still listed, with no status word.
        let row = workspace_row(
            "alpha",
            "● alpha",
            status.by_workspace.get("alpha").copied().unwrap_or(""),
        );
        assert_eq!(
            row.split('\t').collect::<Vec<_>>(),
            ["alpha", &ljust("● alpha", 24), ""]
        );
    }

    // RollUp::test_read_asks_tmux_once
    #[test]
    fn read_asks_tmux_once() {
        let line = ["alpha", "@1", "%1", "omp", "blocked", "", ""].join(&SEP.to_string());
        let scripted = ScriptedTmux::new(&format!("{line}\n"), 0);
        let status = agent_status(&scripted.client());
        assert_eq!(
            status.by_pane,
            HashMap::from([("%1".to_string(), "blocked")])
        );
        assert_eq!(
            status.by_tab,
            HashMap::from([("@1".to_string(), "blocked")])
        );
        assert_eq!(
            status.by_workspace,
            HashMap::from([("alpha".to_string(), "blocked")])
        );
        let calls = scripted.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][..2], ["list-panes", "-a"]);
    }

    // RollUp::test_window_preview_shows_home_as_a_tilde
    #[test]
    fn window_preview_shows_home_as_a_tilde() {
        let scratch = ScratchTmux::new();
        let home = TempDir::new();
        let api = home.path().join("Projects/api");
        std::fs::create_dir_all(&api).unwrap();
        scratch.tmux(&[
            "new-window",
            "-d",
            "-t",
            "alpha:",
            "-n",
            "code",
            "-c",
            api.to_str().unwrap(),
            "exec sleep 300",
        ]);
        scratch.tmux(&[
            "set-option",
            "-p",
            "-t",
            "alpha:code",
            "@workspace_label",
            "api",
        ]);
        let tmux = Tmux::new().socket(scratch.socket());
        let shown = preview_of(&tmux, home.path(), "alpha", "code");
        assert!(
            shown.contains("Pane 0: api (sleep) in ~/Projects/api\n"),
            "{shown}"
        );
        assert!(
            !shown.contains(&format!("{}/", home.path().display())),
            "{shown}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // tests/test_agent_status.py::Rows

    // Rows::test_workspace_row_hides_the_name_in_the_first_field
    #[test]
    fn workspace_row_hides_the_name_in_the_first_field() {
        let row = workspace_row("odoo-project", "● odoo-project", "blocked");
        let parts: Vec<&str> = row.split('\t').collect();
        assert_eq!(parts.len(), 3);
        let (name, label, status) = (parts[0], parts[1], parts[2]);
        assert_eq!((name, label.trim_end()), ("odoo-project", "● odoo-project"));
        assert!(status.contains("blocked"));
        assert!(status.starts_with("\x1b["));
        let plain = workspace_row("notes", "● notes", "");
        assert_eq!(
            plain.split('\t').collect::<Vec<_>>(),
            ["notes", &ljust("● notes", label.chars().count()), ""]
        );
    }

    // Rows::test_picked_name_is_the_last_non_empty_line
    #[test]
    fn picked_name_is_the_last_non_empty_line() {
        assert_eq!(picked_name("odoo\nodoo-project\n"), "odoo-project");
        assert_eq!(picked_name("odoo-project\n"), "odoo-project");
        assert_eq!(picked_name("new-client\n\n"), "new-client");
        assert_eq!(picked_name("done\n"), "done");
        assert_eq!(picked_name(" \n"), "");
        assert_eq!(picked_name(""), "");
    }

    // Rows::test_picked_name_is_the_selection_after_the_query
    #[test]
    fn picked_name_is_the_selection_after_the_query() {
        assert_eq!(
            picked_name("odo\nodoo-project\t● odoo-project\t\n"),
            "odoo-project"
        );
        assert_eq!(picked_name("new-client\n"), "new-client");
    }

    // Rows::test_picked_name_drops_the_label_and_status_fields
    #[test]
    fn picked_name_drops_the_label_and_status_fields() {
        let row = workspace_row("odoo-project", "● odoo-project", "blocked");
        assert_eq!(picked_name(&format!("{row}\n")), "odoo-project");
    }

    // Rows::test_fzf_options_work_with_fzf_0_38
    #[test]
    fn fzf_options_work_with_fzf_0_38() {
        // Debian 12 ships fzf 0.38 and Ubuntu 24.04 ships 0.44; --accept-nth needs 0.60.
        let known = [
            "--ansi",
            "--delimiter",
            "--nth",
            "--with-nth",
            "--tiebreak",
            "--bind",
            "--header",
            "--prompt",
            "--height",
            "--layout",
            "--border",
            "--info",
            "--preview",
            "--preview-window",
            "--filter",
            "--multi",
            "--print-query",
        ];
        for option in WORKSPACE_FZF[1..].iter().chain(&TAB_FZF[1..]) {
            let name = option.split('=').next().unwrap();
            assert!(known.contains(&name), "{option}");
        }
        // The options the pickers rely on, as the Python had them.
        assert_eq!(TAB_FZF, ["fzf", "--ansi", "--delimiter=\t", "--nth=1"]);
        assert_eq!(
            WORKSPACE_FZF,
            [
                "fzf",
                "--delimiter=\t",
                "--with-nth=2..",
                "--nth=1",
                "--ansi",
                "--tiebreak=index",
                "--print-query"
            ]
        );
    }

    // Rows::test_numbered_picker_without_fzf
    #[test]
    fn numbered_picker_without_fzf() {
        let rows = [
            workspace_row("api", "● api", "blocked"),
            workspace_row("docs", "📁 docs", ""),
        ];
        for (answer, expected) in [
            ("2", "docs"),
            ("new-client", "new-client"),
            ("", ""),
            ("9", ""),
        ] {
            let mut shown = Vec::new();
            let picked =
                numbered_pick(&rows, &mut Cursor::new(format!("{answer}\n")), &mut shown).unwrap();
            assert_eq!(picked, expected, "{answer:?}");
            let shown = String::from_utf8(shown).unwrap();
            assert!(shown.contains(" 1. ● api"), "{shown}");
            assert!(
                shown.contains(" 2. 📁 docs\n"),
                "no trailing spaces: {shown:?}"
            );
            assert!(
                shown.ends_with("\nNumber, or a new workspace name (Enter cancels): ")
                    || shown.ends_with(&format!("No workspace {answer}\n")),
                "{shown:?}"
            );
        }
    }

    #[test]
    fn numbered_picker_lines_show_the_status_and_end_of_input_cancels() {
        let rows = [workspace_row("api", "● api", "blocked")];
        let mut shown = Vec::new();
        let ended = numbered_pick(&rows, &mut Cursor::new(""), &mut shown);
        assert!(matches!(ended, Err(Error::Eof)));
        assert!(
            String::from_utf8(shown)
                .unwrap()
                .starts_with(&format!(" 1. ● api  {}\n", status_word("blocked")))
        );
    }

    // Rows::test_status_word_never_reaches_the_returned_name
    #[test]
    fn status_word_never_reaches_the_returned_name() {
        if which("fzf").is_none() {
            return;
        }
        let rows = [
            workspace_row("odoo-project", "● odoo-project", "blocked"),
            workspace_row("notes", "● notes", ""),
        ]
        .join("\n");
        let filtered = |query: &str| {
            let mut argv: Vec<String> = WORKSPACE_FZF.map(String::from).to_vec();
            argv.push(format!("--filter={query}"));
            spawn(&Spawn {
                argv,
                stdin: Some(rows.clone()),
                ..Spawn::default()
            })
            .unwrap()
            .stdout
        };
        assert_eq!(picked_name(&filtered("odoo")), "odoo-project");
        // Only the query comes back: no row matched on its status word. Enter would create 'blocked'.
        assert_eq!(filtered("blocked").lines().collect::<Vec<_>>(), ["blocked"]);
        assert!(WORKSPACE_FZF.contains(&"--print-query"));
    }

    // Rows::test_tab_index_is_the_same_with_and_without_a_status_word
    #[test]
    fn tab_index_is_the_same_with_and_without_a_status_word() {
        let plain = tab_row("3", "editor", "2", "1:omp, 2:shell", false, "");
        let marked = tab_row("3", "editor", "2", "1:omp, 2:shell", true, "working");
        assert!(!plain.contains("working"));
        assert!(marked.find("editor").unwrap() < marked.find("working").unwrap());
        assert_eq!((tab_index(&plain), tab_index(&marked)), ("3", "3"));
        let bare = |row: &str| row.replace("\x1b[33m", "").replace("\x1b[0m", "");
        assert_eq!(plain.find('[').unwrap(), bare(&marked).find('[').unwrap());
        let idle = tab_row("3", "editor", "2", "1:omp, 2:shell", false, "idle");
        assert_eq!(idle.find(" [").unwrap(), marked.find(" [").unwrap());
    }

    #[test]
    fn a_tab_row_counts_panes_and_marks_the_active_tab() {
        let row = tab_row("1", "one", "1", "1:omp", true, "");
        assert_eq!(
            row,
            "1: one             \t        [1 pane: 1:omp]  ● active"
        );
        let row = tab_row("2", "two", "3", "1:a, 2:b, 3:c", false, "done");
        assert_eq!(
            row,
            format!(
                "2: two             \t{} [3 panes: 1:a, 2:b, 3:c]",
                status_word("done")
            )
        );
    }

    // Rows::test_tab_picker_searches_only_the_index_and_the_name
    #[test]
    fn tab_picker_searches_only_the_index_and_the_name() {
        if which("fzf").is_none() {
            return;
        }
        let rows = [
            tab_row("3", "editor", "2", "1:omp, 2:shell", false, "working"),
            tab_row("4", "logs", "1", "1:tail", true, ""),
        ]
        .join("\n");
        let filtered = |query: &str| {
            let mut argv: Vec<String> = TAB_FZF.map(String::from).to_vec();
            argv.push(format!("--filter={query}"));
            spawn(&Spawn {
                argv,
                stdin: Some(rows.clone()),
                ..Spawn::default()
            })
            .unwrap()
            .stdout
        };
        assert_eq!(tab_index(&filtered("editor")), "3");
        assert_eq!(tab_index(&filtered("4")), "4");
        for query in ["working", "omp", "active"] {
            assert_eq!(filtered(query), "", "{query}");
        }
        // The preview receives the first field, and window_preview reads the index from it.
        let first = rows.lines().next().unwrap().split('\t').next().unwrap();
        assert_eq!(first.split(':').next().unwrap(), "3");
    }

    // ---------------------------------------------------------------------------------------
    // tests/test_performance.py::Pickers, the order and the contents of the workspace list

    // Pickers::test_workspace_picker_lists_recent_sessions_before_other_directories
    #[test]
    fn workspace_picker_lists_recent_sessions_before_other_directories() {
        let sep = SEP.to_string();
        let listing = [
            ["alphabetical-first", "100", "1"],
            ["recent", "300", "1"],
            ["__parked", "999", "1"],
            ["previous", "200", "1"],
            ["never-attached", "", "1"],
        ]
        .map(|row| row.join(&sep))
        .join("\n");
        let folder = TempDir::new();
        let projects = folder.path().join("projects");
        std::fs::create_dir_all(projects.join("new-project")).unwrap();
        std::fs::create_dir(projects.join(".hidden")).unwrap();
        std::fs::write(projects.join("a-file"), "").unwrap();
        let other = folder.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let loc = Locations {
            home: folder.path().to_path_buf(),
            xdg_config_home: None,
            projects: None,
            state_dir: None,
            share: None,
        };

        let live = recent_sessions(&listing).unwrap();
        let names = project_names(&projects);
        assert_eq!(names, ["new-project"]);
        let zoxide = zoxide_folders(
            &loc,
            &format!("{}\n", other.display()),
            &live,
            &names,
            &projects,
        );
        let by_workspace = HashMap::from([("recent".to_string(), "blocked")]);
        let rows = workspace_rows(&live, &zoxide, &names, &by_workspace);
        let order: Vec<&str> = rows
            .iter()
            .map(|row| row.split('\t').next().unwrap())
            .collect();
        assert_eq!(
            order,
            [
                "recent",
                "previous",
                "alphabetical-first",
                "never-attached",
                other.to_str().unwrap(),
                "new-project"
            ]
        );
        assert!(rows.join("\n").contains("blocked"));
        assert!(WORKSPACE_FZF.contains(&"--tiebreak=index"));
    }

    #[test]
    fn sessions_with_the_same_time_keep_tmuxs_order_and_a_bad_time_is_an_error() {
        let sep = SEP.to_string();
        let row = |name: &str, when: &str| [name, when, "1"].join(&sep);
        let listing = [row("b", "5"), row("a", "5"), row("c", " 7 ")].join("\n");
        assert_eq!(recent_sessions(&listing).unwrap(), ["c", "b", "a"]);
        assert_eq!(recent_sessions("").unwrap(), Vec::<String>::new());
        assert_eq!(
            recent_sessions(&row("a", "soon")).unwrap_err().to_string(),
            "invalid literal for int() with base 10: 'soon'"
        );
        // The parking session and the bootstrap placeholder are never workspaces.
        let hidden = [
            row("__parked", "9"),
            row("__workspace_bootstrap", "8"),
            row("w", ""),
        ]
        .join("\n");
        assert_eq!(recent_sessions(&hidden).unwrap(), ["w"]);
    }

    #[test]
    fn the_zoxide_folders_leave_out_what_the_list_shows_already() {
        let folder = TempDir::new();
        let projects = folder.path().join("Projects");
        std::fs::create_dir_all(projects.join("inside")).unwrap();
        // Shares only a string prefix with the projects folder, which is enough to leave it out.
        let sibling = folder.path().join("Projects-old");
        let (live_dir, listed, plain) = (
            folder.path().join("live-one"),
            folder.path().join("listed"),
            folder.path().join("plain"),
        );
        for dir in [&sibling, &live_dir, &listed, &plain] {
            std::fs::create_dir(dir).unwrap();
        }
        let loc = Locations {
            home: folder.path().to_path_buf(),
            xdg_config_home: None,
            projects: None,
            state_dir: None,
            share: None,
        };
        let output = [
            projects.join("inside").display().to_string(),
            sibling.display().to_string(),
            live_dir.display().to_string(),
            listed.display().to_string(),
            folder.path().join("gone").display().to_string(),
            format!("  {}  ", plain.display()),
            "~/plain".to_string(),
        ]
        .join("\n");
        let found = zoxide_folders(
            &loc,
            &output,
            &["live-one".to_string()],
            &["listed".to_string()],
            &projects,
        );
        // `~/plain` is the same folder as the line before it, and zoxide may list it twice.
        assert_eq!(
            found,
            [plain.display().to_string(), plain.display().to_string()]
        );
    }

    #[test]
    fn project_names_are_the_visible_folders_sorted() {
        let folder = TempDir::new();
        for name in ["b", "a", ".git", "Z"] {
            std::fs::create_dir(folder.path().join(name)).unwrap();
        }
        std::fs::write(folder.path().join("file"), "").unwrap();
        std::os::unix::fs::symlink(folder.path().join("a"), folder.path().join("link")).unwrap();
        assert_eq!(project_names(folder.path()), ["Z", "a", "b", "link"]);
        assert_eq!(
            project_names(&folder.path().join("missing")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn workspace_rows_label_each_kind_and_skip_projects_that_are_live() {
        let by_workspace = HashMap::from([("api".to_string(), "done")]);
        let rows = workspace_rows(
            &["api".to_string()],
            &["/x/other".to_string()],
            &["api".to_string(), "docs".to_string()],
            &by_workspace,
        );
        assert_eq!(
            rows,
            [
                workspace_row("api", "● api", "done"),
                workspace_row("/x/other", "⚡ /x/other", ""),
                workspace_row("docs", "📁 docs", ""),
            ]
        );
    }

    // ---------------------------------------------------------------------------------------
    // kill-workspace, the questions it asks

    /// Surroundings for a command that does not look at the terminal or the environment itself.
    fn setting(home: &TempDir, stdin_tty: bool) -> Setting {
        Setting {
            loc: Locations {
                home: home.path().to_path_buf(),
                xdg_config_home: None,
                projects: None,
                state_dir: None,
                share: None,
            },
            stdin_tty,
            stdout_tty: true,
            // The client is not switched: a scratch server has none.
            inside_tmux: false,
        }
    }

    /// A scratch server with the sessions `alpha`, `beta` and `gamma`.
    fn three_workspaces() -> (ScratchTmux, Tmux) {
        let scratch = ScratchTmux::new();
        for name in ["beta", "gamma"] {
            scratch.tmux(&["new-session", "-d", "-s", name, "exec sleep 300"]);
        }
        let tmux = Tmux::new().socket(scratch.socket());
        (scratch, tmux)
    }

    fn delete(
        setting: &Setting,
        tmux: &Tmux,
        name: Option<&str>,
        typed: &str,
    ) -> (Result<()>, String) {
        let mut shown = Vec::new();
        let result = delete_with(
            setting,
            tmux,
            name,
            &mut Cursor::new(typed.to_string()),
            &mut shown,
        );
        (result, String::from_utf8(shown).unwrap())
    }

    #[test]
    fn a_terminal_is_asked_before_a_workspace_is_closed() {
        let (home, (_scratch, tmux)) = (TempDir::new(), three_workspaces());
        let asking = setting(&home, true);
        let question = "Are you sure you want to close workspace 'beta'? (y/N): ";
        // No, anything else, and the end of input keep the workspace; only the first two say so.
        for (typed, said) in [
            ("n\n", "Cancelled.\n"),
            ("maybe\n", "Cancelled.\n"),
            ("", ""),
        ] {
            let (result, shown) = delete(&asking, &tmux, Some("beta"), typed);
            assert!(result.is_ok(), "{typed:?}");
            assert_eq!(shown, format!("{question}{said}"), "{typed:?}");
            assert_eq!(
                session_names(&tmux),
                ["alpha", "beta", "gamma"],
                "{typed:?}"
            );
        }
        for typed in ["y\n", "  YES \n"] {
            let (_scratch, tmux) = three_workspaces();
            let (result, shown) = delete(&asking, &tmux, Some("beta"), typed);
            assert!(result.is_ok(), "{typed:?}");
            assert!(shown.starts_with(question), "{typed:?}");
            assert_eq!(session_names(&tmux), ["alpha", "gamma"], "{typed:?}");
        }
    }

    #[test]
    fn without_a_terminal_nothing_is_asked_and_a_name_or_number_picks_the_workspace() {
        let (home, (_scratch, tmux)) = (TempDir::new(), three_workspaces());
        let piped = setting(&home, false);
        // A name given is closed at once.
        let (result, shown) = delete(&piped, &tmux, Some(" gamma "), "");
        assert!(result.is_ok());
        assert_eq!(shown, "");
        assert_eq!(session_names(&tmux), ["alpha", "beta"]);
        // Without one the workspaces are listed; a number picks, then a name.
        let (result, shown) = delete(&piped, &tmux, None, "2\n");
        assert!(result.is_ok());
        assert_eq!(
            shown,
            "Active workspaces:\n\n  1. alpha\n  2. beta\n\nNumber or name of workspace to delete (Enter cancels): "
        );
        assert_eq!(session_names(&tmux), ["alpha"]);
        let (_, _) = delete(&piped, &tmux, None, "alpha\n");
        assert_eq!(
            session_names(&tmux),
            Vec::<String>::new(),
            "no workspace is left"
        );
    }

    #[test]
    fn an_empty_answer_a_number_out_of_range_and_the_end_of_input_close_nothing() {
        let (home, (_scratch, tmux)) = (TempDir::new(), three_workspaces());
        let piped = setting(&home, false);
        for typed in ["\n", "   \n", "7\n", "0\n", "nobody\n"] {
            let (result, _) = delete(&piped, &tmux, None, typed);
            assert!(result.is_ok(), "{typed:?}");
            assert_eq!(
                session_names(&tmux),
                ["alpha", "beta", "gamma"],
                "{typed:?}"
            );
        }
        let (result, _) = delete(&piped, &tmux, None, "");
        assert!(matches!(result, Err(Error::Eof)));
        assert_eq!(session_names(&tmux).len(), 3);
    }

    // ---------------------------------------------------------------------------------------
    // Names the user types

    #[test]
    fn a_typed_name_loses_unprintable_characters_and_one_icon_of_each_kind() {
        assert_eq!(clean_name("  api  "), "api");
        assert_eq!(clean_name("● api"), "api");
        assert_eq!(clean_name("📁 docs"), "docs");
        assert_eq!(clean_name("⚡ /x/other"), "/x/other");
        assert_eq!(clean_name("● 📁 both"), "both");
        assert_eq!(clean_name("📁 ● wrong order"), "● wrong order");
        assert_eq!(clean_name("a\x07b\u{200b}c\n"), "abc");
        assert_eq!(
            clean_name("●api"),
            "●api",
            "the icon needs the space after it"
        );
        assert_eq!(clean_name("   "), "");
    }
}
