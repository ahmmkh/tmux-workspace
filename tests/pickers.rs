//! Workspaces and tabs: `open`, the pickers, `new-tab` and `kill-workspace`. The Python behind these
//! is bin/tmux-workspace at 30b1401 (`open_workspace`, `choose_or_create_workspace`, `pick_window`,
//! `window_preview`, `new_tab`, `delete_workspace`).
//!
//! Every run is the real binary against a scratch tmux server, with a `PATH` of its own that holds
//! the real tmux and fake `fzf`, `zoxide` and `sesh`: the developer's own are never run, and their
//! zoxide database is never touched. The pickers that need a terminal run on a pseudo-terminal.

mod common;

use common::{EXE, Fakes, on_a_terminal, outside_tmux, quietly, started, text};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use tmux_workspace::testing::{ScratchTmux, TempDir, wait_for};

/// A scratch server (session `alpha`), a home, and the fake programs the binary finds on its `PATH`.
struct World {
    scratch: ScratchTmux,
    fakes: Fakes,
    /// The shipped files, so that no config of this checkout is read.
    share: TempDir,
    /// Folders the tests make and the trace files.
    work: TempDir,
}

impl World {
    /// A world whose user has chosen a projects folder already, so that nothing asks for one.
    fn new() -> World {
        let world = World::fresh();
        let config = world.home().join(".config/tmux-workspace");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.json"),
            "{\"projects_dir\": \"~/Projects\"}\n",
        )
        .unwrap();
        world
    }

    /// A world of a first-time user: there is no config.
    fn fresh() -> World {
        let fakes = Fakes::new();
        fakes.real_tmux();
        fakes.zoxide("", &[]);
        let share = TempDir::new();
        std::fs::write(
            share.path().join("config.example.json"),
            "{\"model\": \"\", \"doppler\": null}\n",
        )
        .unwrap();
        World {
            scratch: ScratchTmux::new(),
            fakes,
            share,
            work: TempDir::new(),
        }
    }

    /// The home the binary sees, as the files in it are named once links are followed.
    fn home(&self) -> PathBuf {
        self.scratch.home().canonicalize().unwrap()
    }

    /// The default projects folder, which is made when a test needs it.
    fn projects(&self) -> PathBuf {
        let projects = self.home().join("Projects");
        std::fs::create_dir_all(&projects).unwrap();
        projects
    }

    /// A folder of the tests' own, outside the projects folder.
    fn folder(&self, name: &str) -> PathBuf {
        let folder = self.work.path().canonicalize().unwrap().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// The binary inside tmux, with these arguments, only the fakes on `PATH`, and a folder of the
    /// test's own as the working folder: a name like `docs` is not the folder of this checkout.
    fn bin(&self, args: &[&str]) -> Command {
        let mut command = self.scratch.bin(EXE);
        command
            .args(args)
            .current_dir(self.work.path())
            .env("PATH", self.fakes.path())
            .env("TMUX_WORKSPACE_SHARE", self.share.path());
        command
    }

    /// The binary run outside tmux, as `bin`: the `tmux` it finds sends everything to the scratch server and
    /// only records `attach-session`. It is installed here every time, so no run outside tmux can
    /// reach another server, nor leave a link to the real tmux in the way.
    fn outside_bin(&self, args: &[&str]) -> Command {
        let mut command = self.bin(args);
        outside_tmux(&mut command, self.fakes.path(), self.scratch.socket());
        command
    }

    /// The binary run with no terminal.
    fn run(&self, args: &[&str]) -> Output {
        quietly(self.bin(args))
    }

    /// A command that must succeed without a word.
    fn ok(&self, args: &[&str]) {
        let out = self.run(args);
        assert_eq!(text(&out.stderr), "", "{args:?}");
        assert_eq!(text(&out.stdout), "", "{args:?}");
        assert_eq!(out.status.code(), Some(0), "{args:?}");
    }

    /// Run with a trace of every child process: the output, and the lines of the trace.
    fn traced(&self, mut command: Command) -> (Output, Vec<String>) {
        let trace = self.work.path().join(format!("trace-{}", unique()));
        command.env("TMUX_WORKSPACE_TRACE", &trace);
        let out = quietly(command);
        (out, lines_of(&trace))
    }

    /// The sessions of the server, sorted.
    fn sessions(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .scratch
            .tmux(&["list-sessions", "-F", "#{session_name}"])
            .lines()
            .map(String::from)
            .collect();
        names.sort();
        names
    }

    /// A session of its own, with a pane that sleeps; give its pane once it runs `sleep` for good: a
    /// pane that still starts up shows as `tmux` or the shell, which has no agent status.
    fn session(&self, name: &str, dir: &Path) -> String {
        self.scratch.tmux(&[
            "new-session",
            "-d",
            "-s",
            name,
            "-n",
            name,
            "-c",
            dir.to_str().unwrap(),
            "exec sleep 300",
        ]);
        let pane = self.scratch.expand(&format!("={name}:"), "#{pane_id}");
        self.scratch.settle(&pane);
        pane
    }

    fn alpha(&self) -> String {
        self.scratch.expand("alpha:one", "#{pane_id}")
    }

    /// Save, then lose the server: closing its only session ends it. What is left is a save that a
    /// new server would be started with.
    fn lose_the_server(&self) {
        let mut command = self.scratch.bin(EXE);
        command.arg("save");
        let saved = quietly(command);
        assert_eq!(saved.status.code(), Some(0), "{}", text(&saved.stderr));
        assert!(self.scratch.state_dir().join("latest.json").is_file());
        self.scratch.tmux(&["kill-session", "-t", "alpha"]);
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.scratch.run(&["has-session"]).status.success() {
            assert!(Instant::now() < deadline, "the server still runs");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn option(&self, target: &str, name: &str) -> String {
        self.scratch
            .tmux(&["show-options", "-pqv", "-t", target, name])
    }
}

fn unique() -> u128 {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let step = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    u128::from(std::process::id()) * 1000 + u128::from(step)
}

fn lines_of(trace: &Path) -> Vec<String> {
    std::fs::read_to_string(trace)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// The first line of the trace that starts with this text.
fn traced_line<'a>(trace: &'a [String], start: &str) -> &'a str {
    trace
        .iter()
        .find(|line| line.starts_with(start))
        .unwrap_or_else(|| panic!("no {start:?} in {trace:#?}"))
}

fn names_of(rows: &str) -> Vec<&str> {
    rows.lines()
        .map(|row| row.split('\t').next().unwrap())
        .collect()
}

/// Wait until every pane of the server runs `sleep`, with the rule of `wait_for`: the same listing on
/// three reads in a row. For panes that one tmux command made, which `ScratchTmux::pane` cannot wait for
/// one by one. A new pane shows as `tmux`, then as the shell, and only then as `sleep`.
fn wait_for_sleep_everywhere(scratch: &ScratchTmux) {
    let commands = || scratch.tmux(&["list-panes", "-a", "-F", "#{pane_current_command}"]);
    let panes = commands().lines().count();
    wait_for(commands, &vec!["sleep"; panes].join("\n"));
}

// ---------------------------------------------------------------------------------------------
// tests/test_performance.py::Pickers

// Pickers::test_tab_picker_uses_constant_tmux_reads
#[test]
fn tab_picker_uses_constant_tmux_reads() {
    let world = World::new();
    // 80 tabs, each with an agent that waits for the user.
    let mut chain: Vec<String> = Vec::new();
    for index in 1..80 {
        chain.extend(
            [
                "new-window",
                "-d",
                "-t",
                "alpha:",
                "-n",
                &format!("tab{index}"),
                "exec sleep 300",
                ";",
            ]
            .map(String::from),
        );
    }
    chain.pop();
    let chain: Vec<&str> = chain.iter().map(String::as_str).collect();
    world.scratch.tmux(&chain);
    wait_for_sleep_everywhere(&world.scratch);
    let mut mark: Vec<String> = Vec::new();
    for pane in world
        .scratch
        .tmux(&["list-panes", "-s", "-t", "alpha", "-F", "#{pane_id}"])
        .lines()
    {
        mark.extend(
            [
                "set-option",
                "-p",
                "-t",
                pane,
                "@workspace_agent",
                "blocked",
                ";",
            ]
            .map(String::from),
        );
    }
    mark.pop();
    let mark: Vec<&str> = mark.iter().map(String::as_str).collect();
    world.scratch.tmux(&mark);

    world.fakes.fzf_exiting(130);
    let trace = world.work.path().join("trace");
    let mut command = world.bin(&["pick-window"]);
    command
        .env("TMUX_PANE", world.alpha())
        .env("TMUX_WORKSPACE_TRACE", &trace);
    let (status, stderr, shown) = on_a_terminal(command, "");
    assert_eq!((status, stderr.as_str(), shown.as_str()), (0, "", ""));

    // One call for the session and one for every pane; the Python allowed three.
    let lines = lines_of(&trace);
    assert!(started(&trace, "tmux") <= 3, "{lines:#?}");
    assert_eq!(started(&trace, "tmux"), 2, "{lines:#?}");
    let rows = world.fakes.input("fzf", 1).unwrap();
    assert_eq!(rows.lines().count(), 80);
    assert!(rows.contains("tab79"));
    assert!(rows.contains("blocked"));
}

// Pickers::test_workspace_picker_lists_recent_sessions_before_other_directories, here the whole
// run: the order is tmux's (nothing has attached to a scratch server), the rows and the options
// are what fzf gets. The sort by the time of the last attach is in the unit tests of src/pick.rs.
#[test]
fn the_workspace_picker_gives_fzf_the_live_workspaces_then_other_folders_then_projects() {
    let world = World::new();
    let projects = world.projects();
    for name in ["new-project", "beta", ".hidden"] {
        std::fs::create_dir(projects.join(name)).unwrap();
    }
    std::fs::write(projects.join("a-file"), "").unwrap();
    let other = world.folder("other");
    let beta = world.session("beta", &projects.join("beta"));
    world.scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &beta,
        "@workspace_agent",
        "blocked",
    ]);
    world.scratch.tmux(&["new-session", "-d", "-s", "__parked"]);
    world.fakes.zoxide(
        &format!(
            "{}\n{}\n",
            other.display(),
            projects.join("new-project").display()
        ),
        &[],
    );
    world.fakes.fzf_exiting(130);

    let (status, stderr, shown) = on_a_terminal(world.bin(&["open"]), "");
    assert_eq!((status, stderr.as_str(), shown.as_str()), (0, "", ""));

    let rows = world.fakes.input("fzf", 1).unwrap();
    assert_eq!(
        names_of(&rows),
        ["alpha", "beta", other.to_str().unwrap(), "new-project"]
    );
    let beta_row: Vec<&str> = rows.lines().nth(1).unwrap().split('\t').collect();
    assert_eq!(beta_row[1], format!("{:<24}", "● beta"));
    assert_eq!(beta_row[2], "\x1b[31mblocked\x1b[0m");
    assert_eq!(rows.lines().next().unwrap().split('\t').nth(2), Some(""));
    assert!(rows.contains(&format!("⚡ {}", other.display())));
    assert!(rows.contains("📁 new-project"));
    assert!(
        !rows.contains("📁 beta"),
        "a live workspace is not listed twice"
    );
    assert!(!rows.contains("__parked"));
    assert_eq!(
        world.fakes.args("fzf", 1),
        [
            "--delimiter=\t",
            "--with-nth=2..",
            "--nth=1",
            "--ansi",
            "--tiebreak=index",
            "--print-query",
            "--header=Recent workspaces first | Select or type a NEW name | Esc: cancel",
            "--prompt=Workspace > ",
            "--height=50%",
            "--layout=reverse",
            "--border=rounded",
            "--info=inline",
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// tests/test_agent_status.py::Rows::test_picker_without_fzf_or_sesh_asks_for_a_number

#[test]
fn picker_without_fzf_or_sesh_asks_for_a_number() {
    let world = World::new();
    let projects = world.projects();
    std::fs::create_dir(projects.join("docs")).unwrap();
    // An empty answer cancels; nothing was opened and sesh, which is not there, was not tried.
    let (status, stderr, shown) = on_a_terminal(world.bin(&["open"]), "\n");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(shown.contains(" 1. ● alpha"), "{shown:?}");
    assert!(shown.contains(" 2. 📁 docs\n"), "{shown:?}");
    assert!(
        shown.ends_with("Number, or a new workspace name (Enter cancels): "),
        "{shown:?}"
    );
    assert_eq!(world.sessions(), ["alpha"]);
}

#[test]
fn the_numbered_picker_opens_the_chosen_row_or_a_new_name() {
    let world = World::new();
    let projects = world.projects();
    std::fs::create_dir(projects.join("docs")).unwrap();
    for (answer, created) in [("2", "docs"), ("brand-new", "brand-new")] {
        let mut command = world.bin(&["open"]);
        command.env("TMUX_PANE", world.alpha());
        let (status, stderr, _) = on_a_terminal(command, &format!("{answer}\n"));
        assert_eq!((status, stderr.as_str()), (0, ""), "{answer}");
        assert!(world.sessions().contains(&created.to_string()), "{answer}");
    }
}

#[test]
fn a_number_that_is_not_listed_opens_nothing() {
    let world = World::new();
    let (status, stderr, shown) = on_a_terminal(world.bin(&["open"]), "9\n");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(shown.contains("No workspace 9"), "{shown:?}");
    assert_eq!(world.sessions(), ["alpha"]);
}

#[test]
fn sesh_takes_over_when_there_is_no_fzf() {
    let world = World::new();
    world.fakes.add("sesh", "exit 0");
    let (status, stderr, _) = on_a_terminal(world.bin(&["open"]), "");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(world.fakes.all_args("sesh"), [["picker"]]);
}

#[test]
fn sesh_takes_over_without_a_terminal_too() {
    let world = World::new();
    world.fakes.add("sesh", "exit 0");
    world.fakes.fzf_exiting(0);
    let out = world.run(&["open"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(world.fakes.all_args("sesh"), [["picker"]]);
    assert_eq!(world.fakes.calls("fzf"), 0, "fzf needs a terminal");
}

#[test]
fn without_fzf_sesh_or_a_terminal_the_error_says_what_to_install() {
    let world = World::new();
    let out = world.run(&["open"]);
    assert_eq!(
        text(&out.stderr),
        "Workspace error: Install fzf to pick a workspace, or run: work NAME\nReport it: work bug\n"
    );
    assert_eq!(out.status.code(), Some(1));
    // Without an argument, `open` is what runs.
    let mut command = world.scratch.bin(EXE);
    command.env("PATH", world.fakes.path());
    let out = quietly(command);
    assert_eq!(
        text(&out.stderr),
        "Workspace error: Install fzf to pick a workspace, or run: work NAME\nReport it: work bug\n"
    );
}

// ---------------------------------------------------------------------------------------------
// the fzf pick

#[test]
fn a_chosen_row_opens_that_workspace() {
    let world = World::new();
    world.session("beta", &world.folder("beta"));
    world.fakes.fzf_exiting(0);
    let row = "beta\t● beta                 \t";
    let mut command = world.bin(&["open"]);
    command.env("PICK", format!("be\n{row}"));
    let trace = world.work.path().join("trace");
    command.env("TMUX_WORKSPACE_TRACE", &trace);
    let (status, stderr, _) = on_a_terminal(command, "");
    assert_eq!((status, stderr.as_str()), (0, ""));
    let lines = lines_of(&trace);
    // Inside tmux the client switches; here there is none, which is no error.
    assert_eq!(
        traced_line(&lines, "tmux switch-client"),
        "tmux switch-client -t =beta"
    );
    assert!(
        lines
            .iter()
            .all(|line| !line.starts_with("tmux new-session")),
        "{lines:?}"
    );
}

#[test]
fn a_typed_name_that_matches_no_row_opens_a_new_workspace() {
    let world = World::new();
    let projects = world.projects();
    // fzf exits with 1 when no row matched; the query is the only line.
    world.fakes.fzf_exiting(1);
    let mut command = world.bin(&["open"]);
    command.env("PICK", "new-client");
    let (status, stderr, _) = on_a_terminal(command, "");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(world.sessions().contains(&"new-client".to_string()));
    assert!(projects.join("new-client").is_dir());
}

#[test]
fn a_cancelled_fzf_opens_nothing() {
    let world = World::new();
    world.fakes.fzf_exiting(130);
    let (status, stderr, shown) = on_a_terminal(world.bin(&["open"]), "");
    assert_eq!((status, stderr.as_str(), shown.as_str()), (0, "", ""));
    assert_eq!(world.sessions(), ["alpha"]);
    // An fzf that fails does not open what it printed either.
    let world = World::new();
    world.fakes.fzf_exiting(2);
    let mut command = world.bin(&["open"]);
    command.env("PICK", "oops");
    let (status, _, _) = on_a_terminal(command, "");
    assert_eq!(status, 0);
    assert_eq!(world.sessions(), ["alpha"]);
}

// ---------------------------------------------------------------------------------------------
// tests/test_onboarding.py::FirstWork

// FirstWork::test_first_interactive_work_runs_onboarding_once
#[test]
fn first_interactive_work_runs_onboarding_once() {
    let world = World::fresh();
    let home = world.home();
    std::fs::create_dir_all(home.join("dev/seeded")).unwrap();
    world.fakes.fzf_exiting(130);
    let config = home.join(".config/tmux-workspace/config.json");
    assert!(!config.exists());

    let (status, stderr, shown) = on_a_terminal(world.bin(&["open"]), "~/dev\n");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(
        shown.contains("Where do you keep your projects?"),
        "{shown:?}"
    );
    let saved = std::fs::read_to_string(&config).unwrap();
    assert!(saved.contains("\"projects_dir\": \"~/dev\""), "{saved}");

    // The second time nothing is asked.
    let (status, stderr, shown) = on_a_terminal(world.bin(&["open"]), "");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(!shown.contains("Where do you keep"), "{shown:?}");

    // The folder chosen is the one the picker lists, from the first run on, as PROJECTS_ROOT was
    // reread after the setup.
    assert_eq!(world.fakes.calls("fzf"), 2);
    for call in 1..=2 {
        let rows = world.fakes.input("fzf", call).unwrap();
        assert!(rows.contains("📁 seeded"), "call {call}: {rows}");
    }
}

#[test]
fn a_workspace_opened_by_name_after_onboarding_goes_into_the_chosen_folder() {
    let world = World::fresh();
    let (status, stderr, _) = on_a_terminal(world.bin(&["open", "api"]), "src\n");
    assert_eq!((status, stderr.as_str()), (0, ""));
    // A relative answer is taken from home.
    assert!(world.home().join("src/api").is_dir());
    assert!(world.sessions().contains(&"api".to_string()));
    assert_eq!(
        world.scratch.expand("=api:", "#{session_path}"),
        world.home().join("src/api").to_str().unwrap()
    );
}

// FirstWork::test_new_workspace_names_its_first_tab
#[test]
fn new_workspace_names_its_first_tab() {
    // Without -n, tmux names the tab after the quoted default command, which shows as \\.
    let world = World::new();
    let projects = world.projects();
    let (out, trace) = world.traced(world.bin(&["open", "api"]));
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    let created = traced_line(&trace, "tmux new-session");
    assert_eq!(
        created,
        format!(
            "tmux new-session -d -s api -n api -c {}",
            projects.join("api").display()
        )
    );
    assert_eq!(
        world
            .scratch
            .tmux(&["list-windows", "-t", "=api:", "-F", "#{window_name}"]),
        "api"
    );
}

// FirstWork::test_non_interactive_work_never_prompts
#[test]
fn non_interactive_work_never_prompts() {
    let world = World::fresh();
    let config = world.home().join(".config/tmux-workspace/config.json");
    // Without a name the picker has nothing to run, and says so; it did not ask anything first.
    let out = world.run(&["open"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!text(&out.stdout).contains("Where do you keep"));
    // With one, the workspace opens in the default folder.
    let out = world.run(&["open", "api"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "");
    assert!(!config.exists(), "nothing was saved");
    assert!(world.home().join("Projects/api").is_dir());
}

// ---------------------------------------------------------------------------------------------
// open NAME

#[test]
fn a_live_workspace_is_switched_to_and_not_created_again() {
    let world = World::new();
    let (out, trace) = world.traced(world.bin(&["open", "alpha"]));
    assert_eq!(
        (text(&out.stderr).as_str(), out.status.code()),
        ("", Some(0))
    );
    assert_eq!(
        traced_line(&trace, "tmux switch-client"),
        "tmux switch-client -t =alpha"
    );
    assert!(
        trace
            .iter()
            .all(|line| !line.starts_with("tmux new-session")),
        "{trace:?}"
    );
    assert_eq!(world.sessions(), ["alpha"]);
    // The server was marked as set up, once.
    assert_eq!(
        world
            .scratch
            .tmux(&["show-options", "-gqv", "@workspace_initialized"]),
        "1"
    );
}

#[test]
fn the_icons_of_the_picker_do_not_belong_to_the_name() {
    let world = World::new();
    let projects = world.projects();
    let (out, trace) = world.traced(world.bin(&["open", "● alpha"]));
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        traced_line(&trace, "tmux switch-client"),
        "tmux switch-client -t =alpha"
    );
    world.ok(&["open", "📁 docs"]);
    world.ok(&["open", "  ⚡ \u{7}tools  "]);
    assert_eq!(world.sessions(), ["alpha", "docs", "tools"]);
    assert!(projects.join("docs").is_dir() && projects.join("tools").is_dir());
}

#[test]
fn a_new_name_makes_a_folder_in_the_projects_folder_and_tells_zoxide() {
    let world = World::new();
    let projects = world.projects();
    let (out, _) = world.traced(world.bin(&["open", "api"]));
    assert_eq!(out.status.code(), Some(0));
    let api = projects.join("api");
    assert!(api.is_dir());
    assert_eq!(
        world.fakes.all_args("zoxide"),
        [
            vec!["query".to_string(), "api".into()],
            vec!["add".to_string(), api.to_str().unwrap().into()],
        ]
    );
    assert_eq!(
        world.scratch.expand("=api:", "#{session_path}"),
        api.to_str().unwrap()
    );
    // Opening it again connects, and makes nothing.
    world.ok(&["open", "api"]);
    assert_eq!(world.sessions(), ["alpha", "api"]);
}

#[test]
fn a_folder_zoxide_knows_gives_the_workspace_its_name_and_place() {
    let world = World::new();
    let proj = world.folder("elsewhere/proj");
    world.fakes.zoxide("", &[("pro", proj.to_str().unwrap())]);
    world.ok(&["open", "pro"]);
    assert_eq!(world.sessions(), ["alpha", "proj"]);
    assert_eq!(
        world.scratch.expand("=proj:", "#{session_path}"),
        proj.to_str().unwrap()
    );
    assert!(!world.home().join("Projects/pro").exists());
    assert!(
        world
            .fakes
            .all_args("zoxide")
            .contains(&vec!["add".to_string(), proj.to_str().unwrap().into()])
    );
}

#[test]
fn a_zoxide_answer_that_is_not_a_folder_is_ignored() {
    let world = World::new();
    world.fakes.zoxide("", &[("ghost", "/nonexistent/ghost")]);
    world.ok(&["open", "ghost"]);
    assert!(world.home().join("Projects/ghost").is_dir());
    assert_eq!(world.sessions(), ["alpha", "ghost"]);
}

#[test]
fn a_dot_is_the_current_folder_and_zoxide_is_not_asked_about_it() {
    let world = World::new();
    let here = world.folder("here-i-am");
    let mut command = world.bin(&["open", "."]);
    command.current_dir(&here);
    let out = quietly(command);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(world.sessions(), ["alpha", "here-i-am"]);
    assert_eq!(
        world.scratch.expand("=here-i-am:", "#{session_path}"),
        here.to_str().unwrap()
    );
    let asked: Vec<_> = world.fakes.all_args("zoxide");
    assert_eq!(
        asked,
        [vec!["add".to_string(), here.to_str().unwrap().into()]]
    );
}

#[test]
fn a_path_to_a_folder_opens_that_folder() {
    let world = World::new();
    let folder = world.folder("not-a-project");
    world.ok(&["open", folder.to_str().unwrap()]);
    assert_eq!(world.sessions(), ["alpha", "not-a-project"]);
    assert_eq!(
        world.scratch.expand("=not-a-project:", "#{session_path}"),
        folder.to_str().unwrap()
    );
    // A path that is not a folder is a new name under the projects folder.
    let mut command = world.bin(&["open", "nested/name"]);
    command.current_dir(world.work.path());
    let out = quietly(command);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(world.home().join("Projects/nested/name").is_dir());
}

#[test]
fn a_projects_folder_that_cannot_be_made_is_an_error() {
    let world = World::new();
    let projects = world.projects();
    std::fs::write(projects.join("taken"), "a file").unwrap();
    let out = world.run(&["open", "taken"]);
    assert_eq!(out.status.code(), Some(1));
    let error = text(&out.stderr);
    assert!(
        error.starts_with(&format!(
            "Workspace error: Cannot write {}: ",
            projects.join("taken").display()
        )),
        "{error}"
    );
    assert_eq!(world.sessions(), ["alpha"]);
}

#[test]
fn outside_tmux_the_workspace_is_attached_with_exec() {
    let world = World::new();
    let out = quietly(world.outside_bin(&["open", "alpha"]));
    assert_eq!(
        (text(&out.stderr).as_str(), out.status.code()),
        ("", Some(0))
    );
    let attached = std::fs::read_to_string(world.fakes.path().join("attach.args")).unwrap();
    assert_eq!(attached, "attach-session\n-t\n=alpha\n");
    // A new workspace is made first.
    let out = quietly(world.outside_bin(&["open", "fresh"]));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(world.sessions(), ["alpha", "fresh"]);
    let attached = std::fs::read_to_string(world.fakes.path().join("attach.args")).unwrap();
    assert_eq!(attached, "attach-session\n-t\n=fresh\n");
}

#[test]
fn a_server_that_is_gone_is_started_with_the_saved_workspaces() {
    let world = World::new();
    world.lose_the_server();

    world.ok(&["open", "beta"]);

    // The saved workspace is back, the new one is there, and the placeholder that started the
    // server is gone.
    assert_eq!(world.sessions(), ["alpha", "beta"]);
    assert_eq!(
        world
            .scratch
            .tmux(&["show-options", "-gqv", "@workspace_initialized"]),
        "1"
    );
}

// ---------------------------------------------------------------------------------------------
// open update, open -NAME and open -- NAME
//
// A shell that was open before the upgrade to 2.0 still has the 1.x `work` function, which sends
// `work update` to `open update` and `work --update` to `open --update`.

/// What `open update` says when no workspace has that name.
const RELOAD_THE_SHELL: &str = "No workspace named update is open, so this looks like work update from a terminal opened before tmux-workspace 2.0.
Run exec $SHELL (or open a new terminal), then work update again.
To make a workspace named update, run: work ./update
";

/// What the refusal of a name that starts with a dash says.
fn dash_refusal(name: &str) -> String {
    format!("Workspace error: A workspace name cannot start with -: {name}\n")
}

#[test]
fn a_bare_update_with_no_update_workspace_says_to_reload_the_shell() {
    let world = World::new();
    let projects = world.projects();
    let out = world.run(&["open", "update"]);
    assert_eq!(text(&out.stderr), RELOAD_THE_SHELL);
    assert_eq!(text(&out.stdout), "");
    assert_eq!(out.status.code(), Some(2));
    // Nothing was made: no session, no folder, nothing told to zoxide, and none of what a
    // workspace that opens does first.
    assert_eq!(world.sessions(), ["alpha"]);
    assert!(!projects.join("update").exists());
    assert_eq!(world.fakes.calls("zoxide"), 0);
    assert_eq!(
        world
            .scratch
            .tmux(&["show-options", "-gqv", "@workspace_initialized"]),
        ""
    );
}

#[test]
fn the_same_holds_when_no_tmux_server_runs() {
    let world = World::new();
    let projects = world.projects();
    world.lose_the_server();

    let out = world.run(&["open", "update"]);
    assert_eq!(text(&out.stderr), RELOAD_THE_SHELL);
    assert_eq!(out.status.code(), Some(2));
    assert!(!projects.join("update").exists());
    assert_eq!(world.fakes.calls("zoxide"), 0);
    // No server was started for it, so the save was not restored either.
    assert!(!world.scratch.run(&["has-session"]).status.success());
}

#[test]
fn a_bare_update_asks_no_first_run_questions() {
    let world = World::fresh();
    let config = world.home().join(".config/tmux-workspace/config.json");
    let (status, stderr, shown) = on_a_terminal(world.bin(&["open", "update"]), "");
    assert_eq!((status, stderr.as_str()), (2, RELOAD_THE_SHELL));
    assert_eq!(shown, "");
    assert!(!config.exists());
}

#[test]
fn open_dash_dash_update_makes_the_workspace_named_update() {
    let world = World::new();
    let projects = world.projects();
    world.ok(&["open", "--", "update"]);
    assert_eq!(world.sessions(), ["alpha", "update"]);
    assert!(projects.join("update").is_dir());
    assert_eq!(
        world.scratch.expand("=update:", "#{session_path}"),
        projects.join("update").to_str().unwrap()
    );
}

#[test]
fn a_bare_update_opens_the_update_workspace_that_is_open() {
    let world = World::new();
    world.session("update", &world.folder("update"));
    let (out, trace) = world.traced(world.bin(&["open", "update"]));
    assert_eq!(
        (text(&out.stderr).as_str(), out.status.code()),
        ("", Some(0))
    );
    assert_eq!(
        traced_line(&trace, "tmux switch-client"),
        "tmux switch-client -t =update"
    );
    assert!(
        trace
            .iter()
            .all(|line| !line.starts_with("tmux new-session")),
        "{trace:?}"
    );
    assert_eq!(world.sessions(), ["alpha", "update"]);
}

#[test]
fn a_name_that_starts_with_a_dash_is_refused() {
    let world = World::new();
    let projects = world.projects();
    for name in ["--update", "-x", "-"] {
        let out = world.run(&["open", name]);
        assert_eq!(text(&out.stderr), dash_refusal(name), "{name}");
        assert_eq!(text(&out.stdout), "", "{name}");
        assert_eq!(out.status.code(), Some(2), "{name}");
    }
    assert_eq!(world.sessions(), ["alpha"]);
    assert_eq!(std::fs::read_dir(&projects).unwrap().count(), 0);
    assert_eq!(world.fakes.calls("zoxide"), 0);
    assert_eq!(
        world
            .scratch
            .tmux(&["show-options", "-gqv", "@workspace_initialized"]),
        ""
    );
}

#[test]
fn a_refused_dash_name_starts_no_server_and_asks_no_first_run_questions() {
    let world = World::fresh();
    world.lose_the_server();
    let config = world.home().join(".config/tmux-workspace/config.json");
    let (status, stderr, shown) = on_a_terminal(world.bin(&["open", "--update"]), "");
    assert_eq!(
        (status, stderr.as_str()),
        (2, dash_refusal("--update").as_str())
    );
    assert_eq!(shown, "");
    assert!(!config.exists());
    assert!(!world.scratch.run(&["has-session"]).status.success());
}

#[test]
fn a_name_after_dash_dash_is_taken_literally_even_with_a_dash() {
    let world = World::new();
    let projects = world.projects();
    world.ok(&["open", "--", "--update"]);
    assert_eq!(world.sessions(), ["--update", "alpha"]);
    assert!(projects.join("--update").is_dir());
    assert_eq!(
        world.scratch.expand("=--update:", "#{session_path}"),
        projects.join("--update").to_str().unwrap()
    );
    // zoxide takes the name as a name, not as one of its options.
    assert_eq!(
        world.fakes.all_args("zoxide")[0],
        ["query", "--", "--update"]
    );
}

#[test]
fn open_dash_dash_alone_picks_like_open_alone() {
    let world = World::new();
    let projects = world.projects();
    std::fs::create_dir(projects.join("docs")).unwrap();
    let (status, stderr, shown) = on_a_terminal(world.bin(&["open", "--"]), "\n");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(shown.contains(" 1. ● alpha"), "{shown:?}");
    assert!(shown.contains(" 2. 📁 docs\n"), "{shown:?}");
    assert!(
        shown.ends_with("Number, or a new workspace name (Enter cancels): "),
        "{shown:?}"
    );
    assert_eq!(world.sessions(), ["alpha"]);
}

// ---------------------------------------------------------------------------------------------
// pick-window

#[test]
fn a_chosen_tab_is_selected() {
    let world = World::new();
    let one = world.alpha();
    for name in ["two", "three"] {
        world
            .scratch
            .pane(&["new-window", "-t", "alpha:", "-n", name]);
    }
    world.scratch.tmux(&["select-window", "-t", "alpha:one"]);
    world.fakes.fzf_exiting(0);
    let mut command = world.bin(&["pick-window"]);
    command.env("TMUX_PANE", &one).env(
        "PICK",
        format!("2: {:<16}\t{:7} [1 pane: 0:sleep]", "three", ""),
    );
    let (status, stderr, shown) = on_a_terminal(command, "");
    assert_eq!((status, stderr.as_str(), shown.as_str()), (0, "", ""));
    assert_eq!(world.scratch.expand("alpha:", "#{window_name}"), "three");

    // The rows: the index and the name first, then the panes and a marker for the active tab.
    let rows = world.fakes.input("fzf", 1).unwrap();
    let rows: Vec<&str> = rows.lines().collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0],
        format!("0: {:<16}\t{:7} [1 pane: 0:sleep]  ● active", "one", "")
    );
    assert_eq!(
        rows[1],
        format!("1: {:<16}\t{:7} [1 pane: 0:sleep]", "two", "")
    );
    // The preview command runs this program on the first field.
    let args = world.fakes.args("fzf", 1);
    let preview = format!(
        "--preview={} window-preview {{1}}",
        common::shell_quote(&common::self_path())
    );
    assert_eq!(
        args[..4],
        ["--ansi", "--delimiter=\t", "--nth=1", preview.as_str()]
    );
    assert_eq!(
        args[4..],
        [
            "--preview-window=right:55%:wrap",
            "--header=Select window to switch | Esc: cancel",
            "--prompt=Window > ",
            "--height=100%",
            "--layout=reverse",
            "--border=rounded",
            "--info=inline",
        ]
    );
}

#[test]
fn the_tab_rows_show_the_labels_the_panes_and_the_status_of_each_tab() {
    let world = World::new();
    let one = world.alpha();
    let second = world.scratch.pane(&["split-window", "-t", "alpha:one"]);
    world
        .scratch
        .tmux(&["set-option", "-p", "-t", &one, "@workspace_label", "main"]);
    world.scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &second,
        "@workspace_agent",
        "done",
    ]);
    world.fakes.fzf_exiting(130);
    let mut command = world.bin(&["pick-window"]);
    command.env("TMUX_PANE", &one);
    let (status, _, _) = on_a_terminal(command, "");
    assert_eq!(status, 0);
    let rows = world.fakes.input("fzf", 1).unwrap();
    assert_eq!(
        rows,
        format!(
            "0: {:<16}\t\x1b[36mdone   \x1b[0m [2 panes: 0:main, 1:sleep]  ● active",
            "one"
        )
    );
}

#[test]
fn a_cancelled_tab_picker_selects_nothing() {
    let world = World::new();
    let one = world.alpha();
    world
        .scratch
        .pane(&["new-window", "-t", "alpha:", "-n", "two"]);
    world.scratch.tmux(&["select-window", "-t", "alpha:one"]);
    world.fakes.fzf_exiting(130);
    let mut command = world.bin(&["pick-window"]);
    command.env("TMUX_PANE", &one);
    let (status, stderr, _) = on_a_terminal(command, "");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert_eq!(world.scratch.expand("alpha:", "#{window_name}"), "one");
}

#[test]
fn without_a_terminal_the_tab_picker_is_tmuxs_own_tree() {
    let world = World::new();
    let mut command = world.bin(&["pick-window"]);
    command.env("TMUX_PANE", world.alpha());
    let (out, trace) = world.traced(command);
    // There is no client on a scratch server to show the tree to, so tmux refuses; what matters is
    // the command that was sent.
    assert_eq!(
        traced_line(&trace, "tmux choose-tree"),
        "tmux choose-tree -Zw -t =alpha -f #{==:#{session_name},alpha}"
    );
    assert_eq!(world.fakes.calls("fzf"), 0);
    assert!(
        text(&out.stderr).starts_with("Workspace error: Command '["),
        "{}",
        text(&out.stderr)
    );
}

// ---------------------------------------------------------------------------------------------
// window-preview

#[test]
fn the_preview_shows_the_window_its_panes_and_the_screen() {
    let world = World::new();
    let mut command = world.bin(&["window-preview", "0: one             "]);
    command.env("TMUX_PANE", world.alpha());
    let out = quietly(command);
    assert_eq!(out.status.code(), Some(0));
    let shown = text(&out.stdout);
    assert!(
        shown.starts_with("=== Window 0: one (1 panes) ===\n\n  • Pane 0: sleep (sleep) in "),
        "{shown}"
    );
    assert!(shown.contains("\n\n--- Terminal View ---\n\n"), "{shown}");
    assert!(shown.ends_with('\n'));
}

#[test]
fn a_preview_without_an_index_is_of_window_one_and_a_missing_window_is_said() {
    let world = World::new();
    let one = world.alpha();
    let run = |args: &[&str]| {
        let mut command = world.bin(args);
        command.env("TMUX_PANE", &one);
        text(&quietly(command).stdout)
    };
    // The first window is number 0 here, so there is no window 1: the listing of its panes fails,
    // and the preview says so after its heading.
    let shown = run(&["window-preview"]);
    assert!(shown.starts_with("=== Window 1: "), "{shown}");
    assert!(
        shown.contains(" ===\n\nPreview unavailable: Command '["),
        "{shown}"
    );
    assert!(!shown.contains("Terminal View"), "{shown}");
    let shown = run(&["window-preview", "9: nine"]);
    assert!(shown.starts_with("=== Window 9: "), "{shown}");
    assert!(shown.contains(" ===\n\nPreview unavailable: "), "{shown}");
}

// ---------------------------------------------------------------------------------------------
// new-tab

#[test]
fn a_new_tab_is_named_and_labelled_and_starts_in_the_folder() {
    let world = World::new();
    let folder = world.folder("tab-folder");
    let mut command = world.bin(&["new-tab", "My tab", folder.to_str().unwrap()]);
    command.env("TMUX_PANE", world.alpha());
    let out = quietly(command);
    assert_eq!(
        (text(&out.stderr).as_str(), out.status.code()),
        ("", Some(0))
    );
    assert_eq!(
        world
            .scratch
            .tmux(&["list-windows", "-t", "alpha:", "-F", "#{window_name}"]),
        "one\nMy tab"
    );
    let pane = world.scratch.expand("alpha:My tab", "#{pane_id}");
    assert_eq!(world.option(&pane, "@workspace_label"), "My tab");
    assert_eq!(
        world.scratch.expand(&pane, "#{pane_current_path}"),
        folder.to_str().unwrap()
    );
}

#[test]
fn a_tab_without_a_usable_name_is_called_tab() {
    let world = World::new();
    let folder = world.folder("tab-folder");
    for name in ["", "   ", "\u{7}\u{200b}"] {
        let mut command = world.bin(&["new-tab", name, folder.to_str().unwrap()]);
        command.env("TMUX_PANE", world.alpha());
        assert_eq!(quietly(command).status.code(), Some(0), "{name:?}");
    }
    let mut command = world.bin(&["new-tab"]);
    command.env("TMUX_PANE", world.alpha());
    assert_eq!(quietly(command).status.code(), Some(0));
    assert_eq!(
        world
            .scratch
            .tmux(&["list-windows", "-t", "alpha:", "-F", "#{window_name}"]),
        "one\ntab\ntab\ntab\ntab"
    );
    let pane = world.scratch.expand("alpha:{end}", "#{pane_id}");
    assert_eq!(world.option(&pane, "@workspace_label"), "tab");
}

#[test]
fn a_tab_whose_folder_is_missing_starts_in_the_folder_of_the_session() {
    let world = World::new();
    let home = world.folder("session-folder");
    let pane = world.session("beta", &home);
    let mut command = world.bin(&["new-tab", "x", "/nonexistent/folder"]);
    command.env("TMUX_PANE", &pane);
    assert_eq!(quietly(command).status.code(), Some(0));
    let tab = world.scratch.expand("=beta:x", "#{pane_current_path}");
    assert_eq!(tab, home.to_str().unwrap());

    // When the folder of the session is gone too, the projects folder is the place.
    let projects = world.projects();
    let doomed = world.folder("doomed");
    let pane = world.session("gone", &doomed);
    std::fs::remove_dir(&doomed).unwrap();
    let mut command = world.bin(&["new-tab", "y", ""]);
    command.env("TMUX_PANE", &pane);
    assert_eq!(quietly(command).status.code(), Some(0));
    assert_eq!(
        world.scratch.expand("=gone:y", "#{pane_current_path}"),
        projects.to_str().unwrap()
    );
}

// ---------------------------------------------------------------------------------------------
// kill-workspace

#[test]
fn a_workspace_named_on_the_command_line_is_closed() {
    let world = World::new();
    let beta = world.session("beta", &world.folder("beta"));
    let _ = beta;
    for command in ["kill-workspace", "delete", "rm"] {
        world.session("victim", &world.folder("victim"));
        let mut run = world.bin(&[command, "  victim "]);
        run.env("TMUX_PANE", world.alpha());
        let (out, trace) = world.traced(run);
        assert_eq!(
            (text(&out.stderr).as_str(), out.status.code()),
            ("", Some(0)),
            "{command}"
        );
        assert_eq!(world.sessions(), ["alpha", "beta"], "{command}");
        assert!(
            trace.contains(&"tmux display-message Workspace 'victim' closed.".to_string()),
            "{command}: {trace:#?}"
        );
    }
}

#[test]
fn outside_tmux_the_closing_is_told_on_stdout() {
    let world = World::new();
    world.session("beta", &world.folder("beta"));
    let out = quietly(world.outside_bin(&["kill-workspace", "beta"]));
    assert_eq!(text(&out.stdout), "Workspace 'beta' closed.\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(world.sessions(), ["alpha"]);

    let out = quietly(world.outside_bin(&["kill-workspace", "nope"]));
    assert_eq!(text(&out.stdout), "Workspace 'nope' not found.\n");
    assert_eq!(world.sessions(), ["alpha"]);
    // The parking session is not a workspace.
    world.scratch.tmux(&["new-session", "-d", "-s", "__parked"]);
    assert_eq!(
        text(&quietly(world.outside_bin(&["kill-workspace", "__parked"])).stdout),
        "Workspace '__parked' not found.\n"
    );
    assert_eq!(world.sessions(), ["__parked", "alpha"]);
}

#[test]
fn closing_the_current_workspace_moves_to_another_one_first() {
    let world = World::new();
    let beta = world.session("beta", &world.folder("beta"));
    let mut command = world.bin(&["kill-workspace", "beta"]);
    command.env("TMUX_PANE", &beta);
    let (out, trace) = world.traced(command);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let switch = trace
        .iter()
        .position(|l| l == "tmux switch-client -t =alpha")
        .unwrap();
    let kill = trace
        .iter()
        .position(|l| l == "tmux kill-session -t =beta")
        .unwrap();
    assert!(switch < kill, "{trace:#?}");
    assert_eq!(world.sessions(), ["alpha"]);
}

#[test]
fn closing_the_only_workspace_leaves_one_called_main_in_the_projects_folder() {
    let world = World::new();
    let projects = world.projects();
    let alpha = world.alpha();
    let mut command = world.bin(&["kill-workspace", "alpha"]);
    command.env("TMUX_PANE", &alpha);
    let (out, trace) = world.traced(command);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(world.sessions(), ["main"]);
    assert_eq!(
        traced_line(&trace, "tmux new-session"),
        format!(
            "tmux new-session -d -s main -n main -c {}",
            projects.display()
        )
    );
    assert_eq!(
        traced_line(&trace, "tmux switch-client"),
        "tmux switch-client -t =main"
    );
}

#[test]
fn with_no_workspace_there_is_nothing_to_close() {
    let world = World::new();
    world.scratch.tmux(&["new-session", "-d", "-s", "__parked"]);
    world.scratch.tmux(&["kill-session", "-t", "alpha"]);
    let out = quietly(world.outside_bin(&["kill-workspace", "alpha"]));
    assert_eq!(text(&out.stdout), "No active workspaces to delete.\n");
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn on_a_terminal_closing_is_confirmed_first() {
    let world = World::new();
    world.session("beta", &world.folder("beta"));
    let outside = |args: &[&str]| world.outside_bin(args);
    // No, and an empty answer, keep it. (The end of input is a unit test: a terminal has none.)
    for typed in ["n\n", "\n"] {
        let (status, stderr, shown) = on_a_terminal(outside(&["kill-workspace", "beta"]), typed);
        assert_eq!((status, stderr.as_str()), (0, ""), "{typed:?}");
        assert!(
            shown.starts_with("Are you sure you want to close workspace 'beta'? (y/N): "),
            "{shown:?}"
        );
        assert!(shown.ends_with("Cancelled.\n"), "{typed:?}: {shown:?}");
        assert_eq!(world.sessions(), ["alpha", "beta"], "{typed:?}");
    }
    let (status, _, shown) = on_a_terminal(outside(&["kill-workspace", "beta"]), " YES \n");
    assert_eq!(status, 0);
    assert!(shown.ends_with("Workspace 'beta' closed.\n"), "{shown:?}");
    assert_eq!(world.sessions(), ["alpha"]);
}

#[test]
fn without_a_name_a_workspace_is_chosen_from_a_numbered_list_or_fzf() {
    let world = World::new();
    world.session("beta", &world.folder("beta"));
    world.session("gamma", &world.folder("gamma"));
    let outside = |args: &[&str]| world.outside_bin(args);
    // No fzf: a list. The number is asked for, then the confirmation.
    let (status, stderr, shown) = on_a_terminal(outside(&["kill-workspace"]), "2\ny\n");
    assert_eq!((status, stderr.as_str()), (0, ""));
    assert!(shown.starts_with("Active workspaces:\n\n  1. alpha\n  2. beta\n  3. gamma\n\nNumber or name of workspace to delete (Enter cancels): "), "{shown:?}");
    assert_eq!(world.sessions(), ["alpha", "gamma"]);
    // A name works as well as a number.
    let (status, _, _) = on_a_terminal(outside(&["kill-workspace"]), "gamma\ny\n");
    assert_eq!(status, 0);
    assert_eq!(world.sessions(), ["alpha"]);

    // With fzf: the names, and the one picked is closed.
    world.session("delta", &world.folder("delta"));
    world.fakes.fzf_exiting(0);
    let mut command = outside(&["kill-workspace"]);
    command.env("PICK", "delta");
    let (status, _, _) = on_a_terminal(command, "y\n");
    assert_eq!(status, 0);
    assert_eq!(world.sessions(), ["alpha"]);
    assert_eq!(world.fakes.input("fzf", 1).unwrap(), "alpha\ndelta");
    assert_eq!(
        world.fakes.args("fzf", 1),
        [
            "--header=Select workspace to close/delete | Esc: cancel",
            "--prompt=Delete Workspace > ",
            "--height=40%",
            "--layout=reverse",
            "--border=rounded",
        ]
    );
    // A cancelled fzf closes nothing.
    world.session("delta", &world.folder("delta"));
    let mut command = outside(&["kill-workspace"]);
    command.env("PICK", "");
    let (status, _, _) = on_a_terminal(command, "y\n");
    assert_eq!(status, 0);
    assert_eq!(world.sessions(), ["alpha", "delta"]);
}
