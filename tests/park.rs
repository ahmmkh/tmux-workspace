//! Parking a pane out of its window and back, and the help screens. The Python behind these is
//! bin/tmux-workspace at 30b1401 (`park`, `unpark`, `choose-park`, `help`, `help-topic`).
//!
//! Parking runs against a scratch tmux server. The help topics are compared with what the Python
//! prints (scripts/gen_help.py loads it). The fzf and pager paths run on a pseudo-terminal with fake
//! `fzf` and `less` programs that record how they were started.

mod common;

use common::{EXE, Fakes, on_a_terminal, self_path, shell_quote, text};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use tmux_workspace::testing::{ScratchTmux, TempDir};

/// A scratch server with two panes side by side in `alpha:one`: `first`, and `second`.
struct World {
    scratch: ScratchTmux,
    first: String,
    second: String,
}

impl World {
    fn new() -> World {
        let scratch = ScratchTmux::new();
        let first = scratch.expand("alpha:one", "#{pane_id}");
        let second = scratch.pane(&["split-window", "-t", "alpha:one"]);
        World {
            scratch,
            first,
            second,
        }
    }

    /// One more pane in `alpha:one`.
    fn split(&self) -> String {
        self.scratch.pane(&["split-window", "-t", "alpha:one"])
    }

    fn label(&self, pane: &str, label: &str) {
        self.set(pane, "@workspace_label", label);
    }

    fn set(&self, pane: &str, name: &str, value: &str) {
        self.scratch
            .tmux(&["set-option", "-p", "-t", pane, name, value]);
    }

    fn option(&self, pane: &str, name: &str) -> String {
        self.scratch
            .tmux(&["show-options", "-pqv", "-t", pane, name])
    }

    fn session(&self, pane: &str) -> String {
        self.scratch.expand(pane, "#{session_name}")
    }

    fn label_of(&self, pane: &str) -> String {
        self.option(pane, "@workspace_label")
    }

    fn number(&self, pane: &str, format: &str) -> u32 {
        self.scratch.expand(pane, format).parse().unwrap()
    }

    fn parking_exists(&self) -> bool {
        self.scratch
            .run(&["has-session", "-t", "=__parked"])
            .status
            .success()
    }

    /// The binary with these arguments and an empty input, so that a prompt ends at once.
    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, "")
    }

    /// The binary with these arguments, which is typed `input`.
    fn run_with(&self, args: &[&str], input: &str) -> Output {
        use std::io::Write;
        let mut child = self
            .scratch
            .bin(EXE)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// Run a command that must succeed quietly.
    fn ok(&self, args: &[&str]) {
        let out = self.run(args);
        assert_eq!(text(&out.stderr), "", "{args:?}");
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(
            text(&out.stdout),
            "",
            "inside tmux the message goes to display-message"
        );
    }

    /// Run a command that must fail with this message.
    fn fails(&self, args: &[&str], input: &str, message: &str) {
        let out = self.run_with(args, input);
        assert_eq!(
            text(&out.stderr),
            format!("Workspace error: {message}\nReport it: work bug\n"),
            "{args:?}"
        );
        assert_eq!(out.status.code(), Some(1), "{args:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// park and unpark

#[test]
fn park_then_unpark_restores_the_pane_beside_the_target() {
    let world = World::new();
    let (a, b) = (&world.first, &world.second);
    world.label(b, "logs");
    world.ok(&["park", b]);

    assert_eq!(world.session(b), "__parked");
    assert_eq!(world.option(b, "@workspace_home"), "alpha");
    let stamp = world.option(b, "@workspace_parked");
    assert!(
        stamp.parse::<u128>().is_ok(),
        "a nanosecond stamp: {stamp:?}"
    );
    // The parking session holds the pane in a window named after its label; the placeholder that
    // kept the session alive is gone.
    assert_eq!(
        world.scratch.tmux(&[
            "list-panes",
            "-s",
            "-t",
            "=__parked",
            "-F",
            "#{pane_id} #{window_name}"
        ]),
        format!("{b} logs")
    );
    assert_eq!(
        world
            .scratch
            .tmux(&["list-panes", "-t", "alpha:one", "-F", "#{pane_id}"]),
        *a
    );

    world.ok(&["unpark", a, "right"]);
    assert_eq!(world.session(b), "alpha");
    assert_eq!(world.scratch.expand(b, "#{window_name}"), "one");
    assert_eq!(world.option(b, "@workspace_parked"), "");
    assert_eq!(world.option(b, "@workspace_home"), "");
    assert!(!world.parking_exists(), "the last parked pane left");
    // To the right of the target, at the same height.
    assert!(world.number(b, "#{pane_left}") > world.number(a, "#{pane_left}"));
    assert_eq!(
        world.number(b, "#{pane_top}"),
        world.number(a, "#{pane_top}")
    );
    assert_eq!(world.label_of(b), "logs", "the label stays");
}

#[test]
fn unpark_below_puts_the_pane_under_the_target() {
    let world = World::new();
    let (a, b) = (&world.first, &world.second);
    world.ok(&["park", b]);
    world.ok(&["unpark", a, "below"]);
    assert_eq!(world.session(b), "alpha");
    assert!(world.number(b, "#{pane_top}") > world.number(a, "#{pane_top}"));
    assert_eq!(
        world.number(b, "#{pane_left}"),
        world.number(a, "#{pane_left}")
    );
}

#[test]
fn park_without_a_pane_parks_the_current_one() {
    let world = World::new();
    // Outside a terminal tmux takes the active pane of the session for the current one.
    world.scratch.tmux(&["select-pane", "-t", &world.second]);
    world.ok(&["park"]);
    assert_eq!(world.session(&world.second), "__parked");
    assert_eq!(world.session(&world.first), "alpha");
}

#[test]
fn park_refuses_the_only_pane() {
    let scratch = ScratchTmux::new();
    let only = scratch.expand("alpha:one", "#{pane_id}");
    let out = scratch.bin(EXE).args(["park", &only]).output().unwrap();
    assert_eq!(
        text(&out.stderr),
        "Workspace error: This is the only pane. Create a split first, or switch windows.\nReport it: work bug\n"
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        !scratch
            .run(&["has-session", "-t", "=__parked"])
            .status
            .success(),
        "nothing was created"
    );
    assert_eq!(scratch.expand(&only, "#{session_name}"), "alpha");
}

#[test]
fn park_refuses_a_pane_that_is_already_parked() {
    let world = World::new();
    world.ok(&["park", &world.second]);
    world.fails(&["park", &world.second], "", "This pane is already parked");
}

#[test]
fn park_refuses_a_pane_that_does_not_exist() {
    let world = World::new();
    world.fails(&["park", "%999"], "", "No such pane: %999");
    assert!(!world.parking_exists());
}

#[test]
fn unpark_restores_the_newest_parked_pane_first() {
    let world = World::new();
    let (a, b) = (&world.first, &world.second);
    let c = world.split();
    world.ok(&["park", b]);
    world.ok(&["park", &c]);
    assert_eq!(world.session(&c), "__parked");
    world.ok(&["unpark", a]);
    assert_eq!(world.session(&c), "alpha", "the newest came back");
    assert_eq!(world.session(b), "__parked");
    world.ok(&["unpark", a]);
    assert_eq!(world.session(b), "alpha");
    world.fails(&["unpark", a], "", "No parked panes");
}

#[test]
fn unpark_needs_a_parked_pane() {
    let world = World::new();
    world.fails(&["unpark", &world.first], "", "No parked panes");
}

#[test]
fn unpark_does_not_restore_next_to_a_parked_pane() {
    let world = World::new();
    world.ok(&["park", &world.second]);
    world.fails(
        &["unpark", &world.second],
        "",
        "Switch to a project window before restoring a parked pane",
    );
    assert_eq!(world.session(&world.second), "__parked");
}

#[test]
fn the_library_lists_the_parked_panes_and_rejoins_one() {
    use tmux_workspace::park;
    use tmux_workspace::tmux::Tmux;
    let (world, logs, build) = two_parked();
    // On the scratch server's socket only, never the default server.
    let tmux = Tmux::new().socket(world.scratch.socket());
    let parked = park::parked(&tmux).unwrap();
    let ids: Vec<_> = parked.iter().map(|pane| pane.get("pane_id")).collect();
    assert_eq!(ids, [build.as_str(), logs.as_str()], "newest first");
    assert_eq!(
        (parked[0].get("label"), parked[0].get("home")),
        ("build", "alpha")
    );
    assert!(
        parked[0].get("stamp").parse::<u128>().unwrap() > parked[1].get("stamp").parse().unwrap()
    );

    let refused = park::rejoin(&tmux, &logs, &build, "right").unwrap_err();
    assert_eq!(
        refused.to_string(),
        "Switch to a project window before restoring a parked pane"
    );
    assert_eq!(world.session(&logs), "__parked");
    park::rejoin(&tmux, &logs, &world.first, "below").unwrap();
    assert_eq!(world.session(&logs), "alpha");
    assert!(world.number(&logs, "#{pane_top}") > world.number(&world.first, "#{pane_top}"));
    assert_eq!(world.option(&logs, "@workspace_parked"), "");
    assert_eq!(world.option(&logs, "@workspace_home"), "");
    assert_eq!(park::parked(&tmux).unwrap().len(), 1);
}

// ---------------------------------------------------------------------------------------------
// choose-park

/// Three panes in `alpha:one`; `logs` parked first, then `build`, which an agent marked done.
fn two_parked() -> (World, String, String) {
    let world = World::new();
    let (logs, build) = (world.second.clone(), world.split());
    world.label(&logs, "logs");
    world.label(&build, "build");
    world.set(&build, "@workspace_agent", "done");
    world.ok(&["park", &logs]);
    world.ok(&["park", &build]);
    (world, logs, build)
}

#[test]
fn choose_park_lists_the_panes_newest_first_and_restores_the_chosen_one() {
    let (world, logs, build) = two_parked();
    let a = world.first.clone();
    let out = world.run_with(&["choose-park", &a, "right"], "2\n");
    assert_eq!(
        text(&out.stdout),
        [
            "Parked panes — restore right of the selected pane\n\n",
            " 1. build  [alpha]  \x1b[36mdone   \x1b[0m\n",
            " 2. logs  [alpha]\n",
            "\nNumber (Enter cancels): ",
        ]
        .concat()
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(world.session(&logs), "alpha", "the second row was chosen");
    assert_eq!(world.session(&build), "__parked");
    assert!(world.number(&logs, "#{pane_left}") > world.number(&a, "#{pane_left}"));
}

#[test]
fn choose_park_below_lists_the_direction_and_restores_under_the_target() {
    let (world, _, build) = two_parked();
    let out = world.run_with(&["choose-park", &world.first, "below"], "1\n");
    assert!(text(&out.stdout).starts_with("Parked panes — restore below of the selected pane\n\n"));
    assert_eq!(world.session(&build), "alpha");
    assert!(world.number(&build, "#{pane_top}") > world.number(&world.first, "#{pane_top}"));
}

#[test]
fn choose_park_refuses_a_number_that_is_not_listed() {
    for answer in [
        "0\n",
        "3\n",
        "x\n",
        "-1\n",
        "1.5\n",
        "99999999999999999999999\n",
    ] {
        let (world, logs, build) = two_parked();
        world.fails(
            &["choose-park", &world.first, "right"],
            answer,
            "Choose a listed pane number",
        );
        assert_eq!(world.session(&logs), "__parked", "{answer:?}");
        assert_eq!(world.session(&build), "__parked", "{answer:?}");
    }
}

#[test]
fn choose_park_does_nothing_on_an_empty_answer_or_the_end_of_input() {
    for answer in ["\n", "  \n", ""] {
        let (world, logs, build) = two_parked();
        let out = world.run_with(&["choose-park", &world.first, "right"], answer);
        assert_eq!(text(&out.stderr), "", "{answer:?}");
        assert_eq!(out.status.code(), Some(0), "{answer:?}");
        assert_eq!(world.session(&logs), "__parked");
        assert_eq!(world.session(&build), "__parked");
    }
}

#[test]
fn choose_park_needs_a_parked_pane_before_it_asks() {
    let world = World::new();
    let out = world.run_with(&["choose-park", &world.first], "1\n");
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        "Workspace error: No parked panes\nReport it: work bug\n"
    );
}

// ---------------------------------------------------------------------------------------------
// help-topic and help without a terminal

/// What scripts/gen_help.py prints: the Python's answer.
fn python(args: &[&str]) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/gen_help.py");
    let out = Command::new("python3")
        .arg("-I")
        .arg(script)
        .args(args)
        .output()
        .expect("run python3");
    assert!(out.status.success(), "{}", text(&out.stderr));
    text(&out.stdout)
}

/// Make `command` a run outside tmux whose `tmux`, were it to run one, finds no server: the one in `bin`
/// asks a socket nobody listens on.
fn no_tmux(command: &mut Command, bin: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    let nobody = bin.join("no-server");
    common::outside_tmux(command, bin, nobody.to_str().unwrap());
}

/// The binary with a home of its own, outside tmux, and no terminal.
fn plain(home: &Path) -> Command {
    let mut command = Command::new(EXE);
    command
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_tmux(&mut command, &home.join(".outside-bin"));
    command
}

fn topic_ids() -> Vec<String> {
    python(&["--list"])
        .lines()
        .map(|line| line.split('\t').next().unwrap().to_string())
        .collect()
}

#[test]
fn every_help_topic_matches_python() {
    let home = TempDir::new();
    let ids = topic_ids();
    assert_eq!(ids.len(), 34);
    for id in &ids {
        let out = plain(home.path())
            .args(["help-topic", id])
            .output()
            .unwrap();
        assert_eq!(text(&out.stderr), "", "{id}");
        assert_eq!(text(&out.stdout), python(&["--print", id]), "{id}");
    }
}

#[test]
fn unknown_help_topic_says_so() {
    let home = TempDir::new();
    for (args, shown) in [
        (vec!["help-topic", "x"], "Topic 'x' not found.\n"),
        (vec!["help-topic"], "Topic '' not found.\n"),
        (
            vec!["help-topic", "Dashboard"],
            "Topic 'Dashboard' not found.\n",
        ),
    ] {
        let out = plain(home.path()).args(&args).output().unwrap();
        assert_eq!(text(&out.stdout), shown, "{args:?}");
        assert_eq!(out.status.code(), Some(0), "{args:?}");
    }
}

#[test]
fn help_without_a_terminal_or_guide_prints_every_topic() {
    let home = TempDir::new();
    let ids = topic_ids();
    let mut args = vec!["--print"];
    args.extend(ids.iter().map(String::as_str));
    let out = plain(home.path()).arg("help").output().unwrap();
    assert_eq!(text(&out.stderr), "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), python(&args));
}

#[test]
fn help_without_a_terminal_prints_the_guide_when_there_is_one() {
    let home = TempDir::new();
    let dir = home.path().join(".config/tmux");
    std::fs::create_dir_all(&dir).unwrap();
    // Python reads the guide as text: line ends become \n, and print adds one more.
    std::fs::write(
        dir.join("WORKSPACE_GUIDE.md"),
        "# Guide\r\nline two\rthree\n",
    )
    .unwrap();
    let out = plain(home.path()).arg("help").output().unwrap();
    assert_eq!(text(&out.stdout), "# Guide\nline two\nthree\n\n");
    assert_eq!(text(&out.stderr), "");
}

// ---------------------------------------------------------------------------------------------
// help on a terminal

impl Fakes {
    /// `help` with only these fakes on `PATH`, in a home with a guide when `guide` says so.
    fn help(&self, home: &Path, pick: &str) -> Command {
        let mut command = Command::new(EXE);
        command
            .arg("help")
            .env("HOME", home)
            .env("PATH", self.dir.path())
            .env("PICK", pick);
        no_tmux(&mut command, self.dir.path());
        command
    }
}

/// A home that has the guide.
fn home_with_guide() -> (TempDir, String) {
    let home = TempDir::new();
    let dir = home.path().join(".config/tmux");
    std::fs::create_dir_all(&dir).unwrap();
    let guide = dir.join("WORKSPACE_GUIDE.md");
    std::fs::write(&guide, "the guide\n").unwrap();
    let shown = guide.to_str().unwrap().to_string();
    (home, shown)
}

#[test]
fn help_on_a_terminal_lists_the_topics_in_fzf_and_pages_the_chosen_one() {
    let fakes = Fakes::new();
    fakes.fzf();
    fakes.less();
    let home = TempDir::new();
    let neovim = python(&["--list"])
        .lines()
        .find(|line| line.starts_with("topic-neovim\t"))
        .unwrap()
        .to_string();
    let (status, stderr, shown) = on_a_terminal(fakes.help(home.path(), &neovim), "");
    assert_eq!((status, stderr.as_str(), shown.as_str()), (0, "", ""));

    // fzf got every topic, an id and its display text on a line, and the options of the Python.
    assert_eq!(fakes.calls("fzf"), 2, "it asks again after the pager");
    assert_eq!(
        fakes.args("fzf", 1),
        [
            "--delimiter=\t".to_string(),
            "--with-nth=2".into(),
            format!("--preview={} help-topic {{1}}", shell_quote(&self_path())),
            "--preview-window=right:58%:wrap".into(),
            "--header=Type to fuzzy search commands | Enter: view full help | Esc: quit".into(),
            "--prompt=Help > ".into(),
            "--info=inline".into(),
            "--height=100%".into(),
            "--layout=reverse".into(),
            "--border=rounded".into(),
        ]
    );
    assert_eq!(
        fakes.input("fzf", 1).unwrap(),
        python(&["--list"]).trim_end_matches('\n')
    );
    // The pager got the topic and a hint, and no file name.
    assert_eq!(fakes.calls("less"), 1);
    assert_eq!(
        fakes.args("less", 1),
        ["-R", "-P", " Press q to return to search..."]
    );
    assert_eq!(
        fakes.input("less", 1).unwrap(),
        python(&["--print", "topic-neovim"]) + "[Press 'q' or 'Enter' to return to search...]"
    );
}

#[test]
fn choosing_the_guide_pages_the_guide_file() {
    let fakes = Fakes::new();
    fakes.fzf();
    fakes.less();
    let (home, guide) = home_with_guide();
    let (status, _, _) = on_a_terminal(
        fakes.help(home.path(), "topic-guide\tTopic: Full Manual"),
        "",
    );
    assert_eq!(status, 0);
    assert_eq!(fakes.args("less", 1), ["-R", "--", &guide]);
    assert_eq!(
        fakes.input("less", 1),
        None,
        "less reads the file, not a pipe"
    );
    assert_eq!(fakes.calls("fzf"), 2);
}

#[test]
fn choosing_the_guide_without_one_goes_back_to_the_list() {
    let fakes = Fakes::new();
    fakes.fzf();
    fakes.less();
    let home = TempDir::new();
    let (status, _, shown) = on_a_terminal(
        fakes.help(home.path(), "topic-guide\tTopic: Full Manual"),
        "",
    );
    assert_eq!((status, shown.as_str()), (0, ""));
    assert_eq!(fakes.calls("less"), 0);
    assert_eq!(fakes.calls("fzf"), 2);
}

#[test]
fn a_cancelled_or_empty_choice_ends_help() {
    for pick in ["", " \t "] {
        let fakes = Fakes::new();
        fakes.fzf();
        fakes.less();
        let home = TempDir::new();
        let (status, stderr, shown) = on_a_terminal(fakes.help(home.path(), pick), "");
        assert_eq!(
            (status, stderr.as_str(), shown.as_str()),
            (0, "", ""),
            "{pick:?}"
        );
        assert_eq!(fakes.calls("fzf"), 1, "{pick:?}");
        assert_eq!(fakes.calls("less"), 0, "{pick:?}");
    }
}

#[test]
fn an_id_that_is_not_a_topic_goes_back_to_the_list() {
    let fakes = Fakes::new();
    fakes.fzf();
    fakes.less();
    let home = TempDir::new();
    let (status, _, _) = on_a_terminal(fakes.help(home.path(), "nonsense\tx"), "");
    assert_eq!(status, 0);
    assert_eq!(fakes.calls("fzf"), 2);
    assert_eq!(fakes.calls("less"), 0);
}

#[test]
fn without_a_pager_the_topic_is_printed_until_enter() {
    let fakes = Fakes::new();
    fakes.fzf();
    let home = TempDir::new();
    // Enter is typed ahead.
    let (status, _, shown) = on_a_terminal(fakes.help(home.path(), "topic-neovim\tx"), "\n");
    assert_eq!(status, 0);
    let expected =
        python(&["--print", "topic-neovim"]) + "[Press 'q' or 'Enter' to return to search...]\n";
    assert_eq!(shown, expected);
    assert_eq!(fakes.calls("fzf"), 2);
}

#[test]
fn without_fzf_on_a_terminal_the_guide_goes_to_the_pager() {
    let fakes = Fakes::new();
    fakes.less();
    let (home, guide) = home_with_guide();
    let (status, _, shown) = on_a_terminal(fakes.help(home.path(), ""), "");
    assert_eq!((status, shown.as_str()), (0, ""));
    assert_eq!(fakes.args("less", 1), ["-R", "--", &guide]);
}

#[test]
fn without_fzf_or_pager_on_a_terminal_the_guide_is_printed() {
    let fakes = Fakes::new();
    let (home, _) = home_with_guide();
    let (status, _, shown) = on_a_terminal(fakes.help(home.path(), ""), "");
    assert_eq!(status, 0);
    assert_eq!(shown, "the guide\n\n");
}

#[test]
fn without_fzf_or_guide_on_a_terminal_every_topic_is_printed() {
    let fakes = Fakes::new();
    fakes.less();
    let home = TempDir::new();
    let (status, _, shown) = on_a_terminal(fakes.help(home.path(), ""), "");
    assert_eq!(status, 0);
    let ids = topic_ids();
    let mut args = vec!["--print"];
    args.extend(ids.iter().map(String::as_str));
    assert_eq!(shown, python(&args));
    assert_eq!(fakes.calls("less"), 0);
}

#[test]
fn fzf_on_a_pipe_is_not_used() {
    let fakes = Fakes::new();
    fakes.fzf();
    let home = TempDir::new();
    let out = fakes
        .help(home.path(), "topic-neovim\tx")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(fakes.calls("fzf"), 0);
    assert!(text(&out.stdout).starts_with("=== Dashboard ===\n\n"));
}
