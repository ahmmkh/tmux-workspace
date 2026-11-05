//! How a pane starts (tests/test_recovery.py: `ShellFallback` and `PaneStart`) and pane labels.
//!
//! The choice of shell is a pure function, so `ShellFallback` calls it with the four candidates.
//! Everything else runs the binary against a scratch tmux server. A fake shell and fake `claude`,
//! `codex`, `omp` and `nvim` programs record how they were started, so no test ever execs a real
//! login shell.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tmux_workspace::pane::{choose_shell, server_shell, shell_argv};
use tmux_workspace::pyjson::{Style, dumps};
use tmux_workspace::testing::{ScratchTmux, TempDir};

const EXE: &str = env!("CARGO_BIN_EXE_tmux-workspace");
const UID: &str = "uuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuu";

/// Executable files in a temporary folder that record how they were started.
struct Fakes {
    dir: TempDir,
}

impl Fakes {
    fn new() -> Fakes {
        let dir = TempDir::new();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::fs::create_dir(dir.path().join("shells")).unwrap();
        Fakes { dir }
    }

    fn write(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A shell named `name` that appends `<program>|<arguments>|<$SHELL>` to the shell log and exits.
    fn shell(&self, name: &str) -> String {
        let path = self.dir.path().join("shells").join(name);
        let log = self.dir.path().join("shell.log");
        if !path.exists() {
            Self::write(
                &path,
                &format!(
                    r#"printf '%s|%s|%s\n' "$0" "$*" "$SHELL" >> '{}'"#,
                    log.display()
                ),
            );
        }
        path.to_str().unwrap().to_string()
    }

    /// A program on `PATH` that appends `<folder it ran in>|<arguments>` to the apps log, and exits
    /// with `status`.
    fn app(&self, name: &str, status: i32) {
        let log = self.dir.path().join("apps.log");
        Self::write(
            &self.dir.path().join("bin").join(name),
            &format!(
                r#"printf '%s|%s\n' "$(pwd -P)" "$*" >> '{}'; exit {status}"#,
                log.display()
            ),
        );
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join(file))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// What the shells were started as, one line per start.
    fn shell_log(&self) -> Vec<String> {
        self.lines("shell.log")
    }

    /// What the apps were started as, one line per start.
    fn apps_log(&self) -> Vec<String> {
        self.lines("apps.log")
    }

    fn bin(&self) -> String {
        format!(
            "{}:{}",
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }
}

/// A scratch server with one pane, the fakes, and the binary set up to run in that pane.
struct World {
    scratch: ScratchTmux,
    fakes: Fakes,
    pane: String,
}

impl World {
    fn new() -> World {
        let scratch = ScratchTmux::new();
        let pane = scratch.expand("alpha:one", "#{pane_id}");
        World {
            scratch,
            fakes: Fakes::new(),
            pane,
        }
    }

    /// A pane that has its label, so `pane-start` has nothing to ask.
    fn labelled() -> World {
        let world = World::new();
        world.set_pane("@workspace_label", "main");
        world
    }

    fn set_pane(&self, name: &str, value: &str) {
        self.scratch
            .tmux(&["set-option", "-p", "-t", &self.pane, name, value]);
    }

    fn pane_option(&self, name: &str) -> String {
        self.scratch
            .tmux(&["show-options", "-pqv", "-t", &self.pane, name])
    }

    /// The binary in the pane, with the shell `sh` of the fakes and the fake apps on `PATH`. Its input
    /// is empty, so a prompt ends at once.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = self.scratch.bin(EXE);
        command
            .args(args)
            .env("TMUX_PANE", &self.pane)
            .env("TMUX_WORKSPACE_SHELL", self.fakes.shell("sh"))
            .env("PATH", self.fakes.bin())
            .env("TZ", "UTC")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Write a snapshot of one pane `record` into the fakes' folder and give the path.
    fn snapshot(&self, record: Value) -> String {
        let dir = self.fakes.dir.path();
        let snapshot = json!({"version": 1, "directory": dir, "saved_at": 0, "panes": [record]});
        let path = dir.join("snapshot.json");
        std::fs::write(&path, snapshot.to_string()).unwrap();
        path.to_str().unwrap().to_string()
    }

    fn record(&self, app: Value) -> Value {
        json!({"id": UID, "label": "agent", "parked": "", "home": "",
               "cwd": self.fakes.dir.path(), "app": app})
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn wait_for_exit(child: &mut Child, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

// ---------------------------------------------------------------------------------------------
// ShellFallback: a new pane execs the user's real shell, and never ends up dead.

/// What `exec` is given: the shell and its arguments, for the four places a shell can come from.
/// `server` is the `SHELL` the tmux server started with, as `show-environment` prints it.
fn start(
    environment: Option<&str>,
    server: &str,
    account: &str,
    override_shell: Option<&str>,
) -> Vec<String> {
    let shown = if server.is_empty() {
        "-SHELL".to_string()
    } else {
        format!("SHELL={server}")
    };
    let shell = choose_shell(
        override_shell.unwrap_or(""),
        server_shell(&shown),
        account,
        environment.unwrap_or(""),
    );
    shell_argv(&shell)
}

#[test]
fn server_shell_wins_over_the_pane_shell() {
    let fakes = Fakes::new();
    let (sh, bash) = (fakes.shell("sh"), fakes.shell("bash"));
    assert_eq!(start(Some(&sh), &bash, &sh, None), [bash.as_str(), "-l"]);
}

#[test]
fn override_wins() {
    let fakes = Fakes::new();
    let (sh, bash) = (fakes.shell("sh"), fakes.shell("bash"));
    assert_eq!(
        start(None, &bash, "", Some(&sh)),
        [sh.as_str(), "-l"],
        "the override beats the server"
    );
}

#[test]
fn account_shell_when_the_server_has_none() {
    let fakes = Fakes::new();
    let (sh, bash) = (fakes.shell("sh"), fakes.shell("bash"));
    assert_eq!(start(Some(&sh), "", &bash, None), [bash.as_str(), "-l"]);
}

#[test]
fn pane_start_execs_shell_from_environment() {
    let fakes = Fakes::new();
    let bash = fakes.shell("bash");
    assert_eq!(start(Some(&bash), "", "", None), [bash.as_str(), "-l"]);
}

#[test]
fn pane_start_falls_back_to_bin_sh() {
    for value in [None, Some(""), Some("/nonexistent/zsh"), Some("/etc")] {
        assert_eq!(
            start(value, "/nonexistent/fish", "/etc", None),
            ["/bin/sh", "-l"],
            "{value:?}"
        );
    }
}

#[test]
fn shells_without_a_login_flag_get_none() {
    let fakes = Fakes::new();
    let elvish = fakes.shell("elvish");
    assert_eq!(start(None, &elvish, "", None), [elvish.as_str()]);
}

#[test]
fn every_shell_that_takes_a_login_flag_gets_it() {
    for name in [
        "sh", "bash", "zsh", "ksh", "mksh", "dash", "fish", "tcsh", "csh", "nu", "xonsh", "pwsh",
    ] {
        let path = format!("/opt/{name}");
        assert_eq!(shell_argv(&path), [path.as_str(), "-l"], "{name}");
    }
    for name in ["elvish", "bashful", "zsh5", "rc", ""] {
        let path = format!("/opt/{name}");
        assert_eq!(shell_argv(&path), [path.as_str()], "{name}");
    }
}

#[test]
fn the_server_reports_its_shell_only_when_it_has_one() {
    assert_eq!(server_shell("SHELL=/bin/zsh"), "/bin/zsh");
    assert_eq!(server_shell("-SHELL"), "");
    assert_eq!(server_shell(""), "");
}

/// The binary: the server's shell is exec'd with `-l`, and the programs it starts are told its name.
#[test]
fn pane_start_execs_the_servers_shell_and_names_it_in_shell() {
    let world = World::labelled();
    let bash = world.fakes.shell("bash");
    world
        .scratch
        .tmux(&["set-environment", "-g", "SHELL", &bash]);
    let out = world
        .command(&["pane-start"])
        .env_remove("TMUX_WORKSPACE_SHELL")
        .env("SHELL", "/bin/sh")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(world.fakes.shell_log(), [format!("{bash}|-l|{bash}")]);
}

#[test]
fn pane_start_prefers_the_override_to_the_server() {
    let world = World::labelled();
    let (sh, bash) = (world.fakes.shell("sh"), world.fakes.shell("bash"));
    world
        .scratch
        .tmux(&["set-environment", "-g", "SHELL", &bash]);
    let out = world.run(&["pane-start"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(world.fakes.shell_log(), [format!("{sh}|-l|{sh}")]);
}

#[test]
fn exec_shell_starts_the_shell_with_the_login_flag() {
    let world = World::new();
    let out = world.run(&["exec-shell"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let sh = world.fakes.shell("sh");
    assert_eq!(world.fakes.shell_log(), [format!("{sh}|-l|{sh}")]);
}

#[test]
fn exec_shell_outside_tmux_still_finds_a_shell() {
    let world = World::new();
    let out = world
        .command(&["exec-shell"])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(world.fakes.shell_log().len(), 1);
}

// ---------------------------------------------------------------------------------------------
// PaneStart: reopen exactly the saved conversation, or explain why it cannot.

/// Run `pane-start` on a snapshot with this app record; give what the apps recorded, and what was printed.
fn start_app(world: &World, app: Value) -> (Vec<String>, String) {
    let snapshot = world.snapshot(world.record(app));
    let out = world.run(&["pane-start", &snapshot, UID]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let sh = world.fakes.shell("sh");
    assert_eq!(
        world.fakes.shell_log(),
        [format!("{sh}|-l|{sh}")],
        "the shell starts last, whatever happened to the app"
    );
    (world.fakes.apps_log(), text(&out.stdout))
}

#[test]
fn resume_commands() {
    for (kind, arguments) in [("claude", "--resume abc"), ("codex", "resume abc")] {
        let world = World::new();
        world.fakes.app(kind, 0);
        let work = world.fakes.dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let (apps, out) = start_app(&world, json!({"kind": kind, "id": "abc", "cwd": work}));
        let work = work.canonicalize().unwrap();
        assert_eq!(apps, [format!("{}|{arguments}", work.display())], "{kind}");
        assert!(out.contains(&format!("Reopening {kind}")), "{kind}: {out}");
    }
}

#[test]
fn omp_and_nvim_resume_commands() {
    for (kind, arguments) in [("omp", "--resume abc"), ("nvim", "-S abc")] {
        let world = World::new();
        world.fakes.app(kind, 0);
        let (apps, out) = start_app(&world, json!({"kind": kind, "id": "abc"}));
        // No cwd in the app: the pane's folder from the record.
        let here = world.fakes.dir.path().canonicalize().unwrap();
        assert_eq!(apps, [format!("{}|{arguments}", here.display())], "{kind}");
        assert!(
            out.contains(&format!(
                "Reopening {kind} for pane agent (no prompt replay)."
            )),
            "{kind}: {out}"
        );
    }
}

#[test]
fn missing_omp_session_file_needs_attention() {
    let world = World::new();
    world.fakes.app("omp", 0);
    let (apps, out) = start_app(
        &world,
        json!({"kind": "omp", "id": "/nonexistent/session.jsonl"}),
    );
    assert!(apps.is_empty(), "{apps:?}");
    assert!(out.contains("Saved session file no longer exists"), "{out}");
}

#[test]
fn an_existing_session_file_is_passed_on() {
    let world = World::new();
    world.fakes.app("nvim", 0);
    let session = world.fakes.dir.path().join("Session.vim");
    std::fs::write(&session, "").unwrap();
    let (apps, _) = start_app(
        &world,
        json!({"kind": "nvim", "id": session.to_str().unwrap()}),
    );
    assert_eq!(apps.len(), 1);
    assert!(
        apps[0].ends_with(&format!("|-S {}", session.display())),
        "{apps:?}"
    );
}

#[test]
fn identity_starting_with_dash_is_refused() {
    let world = World::new();
    world.fakes.app("claude", 0);
    let (apps, out) = start_app(
        &world,
        json!({"kind": "claude", "id": "--dangerously-skip-permissions"}),
    );
    assert!(apps.is_empty(), "{apps:?}");
    assert!(
        out.contains("Missing or invalid exact recovery identity"),
        "{out}"
    );
}

#[test]
fn recorded_error_is_shown_instead_of_guessing() {
    let world = World::new();
    world.fakes.app("codex", 0);
    let (apps, out) = start_app(
        &world,
        json!({"kind": "codex", "error": "No exact conversation identity was recorded"}),
    );
    assert!(apps.is_empty(), "{apps:?}");
    assert!(
        out.contains("Recovery needs attention: No exact conversation identity was recorded"),
        "{out}"
    );
}

#[test]
fn an_identity_that_is_not_a_recoverable_app_is_refused() {
    for app in [
        json!({"kind": "vim", "id": "abc"}),
        json!({"kind": "claude", "id": ""}),
        json!({"kind": "claude", "id": 7}),
        json!({"kind": "claude"}),
        json!({"id": "abc"}),
        json!({"kind": ["claude"], "id": "abc"}),
    ] {
        let world = World::new();
        world.fakes.app("claude", 0);
        let (apps, out) = start_app(&world, app.clone());
        assert!(apps.is_empty(), "{app}: {apps:?}");
        assert!(
            out.contains("Missing or invalid exact recovery identity"),
            "{app}: {out}"
        );
    }
}

#[test]
fn a_record_without_an_app_starts_the_shell_only() {
    for app in [json!(null), json!({}), json!("")] {
        let world = World::new();
        world.fakes.app("claude", 0);
        let (apps, out) = start_app(&world, app);
        assert!(apps.is_empty());
        assert_eq!(out, "");
    }
}

#[test]
fn an_app_that_fails_is_reported_and_no_other_conversation_is_substituted() {
    let world = World::new();
    world.fakes.app("claude", 3);
    let (apps, out) = start_app(&world, json!({"kind": "claude", "id": "abc"}));
    assert_eq!(apps.len(), 1);
    assert!(
        out.contains("claude exited with status 3; no other conversation was substituted.\n"),
        "{out}"
    );
}

#[test]
fn an_app_that_is_not_installed_needs_attention_and_the_shell_still_starts() {
    let world = World::new(); // no fake `omp`
    let (apps, out) = start_app(&world, json!({"kind": "omp", "id": "abc"}));
    assert!(apps.is_empty());
    assert!(out.contains("Recovery needs attention: "), "{out}");
}

#[test]
fn an_app_in_a_folder_that_is_gone_needs_attention() {
    let world = World::new();
    world.fakes.app("claude", 0);
    let (apps, out) = start_app(
        &world,
        json!({"kind": "claude", "id": "abc", "cwd": "/nonexistent/folder"}),
    );
    assert!(apps.is_empty());
    assert!(out.contains("Recovery needs attention: "), "{out}");
}

#[test]
fn ctrl_c_during_recovery_reaches_the_app_but_does_not_end_the_pane() {
    let world = World::new();
    // The app interrupts its parent, as the terminal does to the whole foreground group.
    let app = world.fakes.dir.path().join("bin").join("claude");
    Fakes::write(&app, r#"kill -INT "$PPID"; sleep 0.3; exit 130"#);
    let (_, out) = start_app(&world, json!({"kind": "claude", "id": "abc"}));
    assert!(out.contains("claude exited with status 130;"), "{out}");
}

#[test]
fn an_app_starts_with_the_default_interrupt_handling() {
    let world = World::new();
    // If the child inherited an ignored SIGINT, `kill -INT $$` would do nothing and the script would reach `exit 7`.
    let app = world.fakes.dir.path().join("bin").join("claude");
    Fakes::write(&app, r#"kill -INT $$; sleep 1; exit 7"#);
    let (_, out) = start_app(&world, json!({"kind": "claude", "id": "abc"}));
    assert!(out.contains("claude exited with status -2;"), "{out}");
}

#[test]
fn pane_start_with_a_snapshot_writes_the_identity_label_parked_and_home() {
    let world = World::new();
    let mut record = world.record(json!(null));
    record["label"] = json!("  my\u{7} agent  ");
    record["parked"] = json!("1700000000");
    record["home"] = json!("alpha:one");
    let snapshot = world.snapshot(record);
    let out = world.run(&["pane-start", &snapshot, UID]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(world.pane_option("@workspace_id"), UID);
    assert_eq!(world.pane_option("@workspace_label"), "my agent");
    assert_eq!(world.pane_option("@workspace_parked"), "1700000000");
    assert_eq!(world.pane_option("@workspace_home"), "alpha:one");
}

#[test]
fn pane_start_with_a_snapshot_leaves_an_empty_parked_and_home_unset() {
    let world = World::new();
    world.set_pane("@workspace_home", "stale");
    let snapshot = world.snapshot(world.record(json!(null)));
    assert!(world.run(&["pane-start", &snapshot, UID]).status.success());
    assert_eq!(world.pane_option("@workspace_parked"), "");
    assert_eq!(world.pane_option("@workspace_home"), "stale");
}

#[test]
fn saved_terminal_text_is_replayed_without_its_control_characters() {
    let world = World::new();
    let history = "plain\ttab\n\u{1b}[31mred\u{7}\r\nnext\rlast\ncaf\u{e9}\u{200b}\n";
    std::fs::write(world.fakes.dir.path().join(format!("{UID}.txt")), history).unwrap();
    let snapshot = world.snapshot(world.record(json!(null)));
    let out = world.run(&["pane-start", &snapshot, UID]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "── Saved terminal text · 1970-01-01 00:00 ──\n\
         plain\ttab\n[31mred\nnext\nlast\ncafé\n\
         ── End saved text · prior processes are not running ──\n"
    );
}

#[test]
fn saved_text_that_does_not_end_in_a_newline_runs_into_the_closing_line() {
    let world = World::new();
    std::fs::write(world.fakes.dir.path().join(format!("{UID}.txt")), "abc").unwrap();
    let snapshot = world.snapshot(world.record(json!(null)));
    let out = world.run(&["pane-start", &snapshot, UID]);
    assert!(
        text(&out.stdout)
            .ends_with("──\nabc── End saved text · prior processes are not running ──\n"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn no_saved_text_means_no_banner() {
    let world = World::new();
    let snapshot = world.snapshot(world.record(json!(null)));
    assert_eq!(text(&world.run(&["pane-start", &snapshot, UID]).stdout), "");
}

#[test]
fn pane_start_holds_the_app_until_restoring_is_over() {
    let world = World::new();
    world.fakes.app("claude", 0);
    world
        .scratch
        .tmux(&["set-option", "-g", "@workspace_restoring", "1"]);
    let snapshot = world.snapshot(world.record(json!({"kind": "claude", "id": "abc"})));
    let mut child = world
        .command(&["pane-start", &snapshot, UID])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    assert!(
        !wait_for_exit(&mut child, Duration::from_millis(500)),
        "the pane started while the workspace was being rebuilt"
    );
    assert!(world.fakes.apps_log().is_empty());
    world
        .scratch
        .tmux(&["set-option", "-g", "@workspace_restoring", "0"]);
    assert!(
        wait_for_exit(&mut child, Duration::from_secs(5)),
        "never started"
    );
    assert_eq!(world.fakes.apps_log().len(), 1);
    assert_eq!(world.fakes.shell_log().len(), 1);
}

#[test]
fn pane_start_for_a_pane_that_is_not_in_the_snapshot_fails() {
    let world = World::new();
    let snapshot = world.snapshot(world.record(json!(null)));
    let out = world.run(&["pane-start", &snapshot, "other"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("Workspace error: "),
        "{}",
        text(&out.stderr)
    );
    assert!(world.fakes.shell_log().is_empty());
}

#[test]
fn pane_start_for_a_snapshot_that_is_not_there_fails() {
    let world = World::new();
    let out = world.run(&["pane-start", "/nonexistent/snapshot.json", UID]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("Workspace error: Cannot read /nonexistent/snapshot.json: "),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn pane_start_outside_a_pane_fails() {
    let world = World::new();
    let out = world
        .command(&["pane-start"])
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "Workspace error: pane-start must run inside tmux\n"
    );
    assert!(world.fakes.shell_log().is_empty());
}

// ---------------------------------------------------------------------------------------------
// A pane without a snapshot: identity and label.

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn a_new_pane_gets_an_identity_and_keeps_its_label() {
    let world = World::labelled();
    assert!(world.run(&["pane-start"]).status.success());
    let id = world.pane_option("@workspace_id");
    assert!(valid_id(&id), "{id}");
    assert_eq!(world.pane_option("@workspace_label"), "main");
    // Started again, the pane keeps what it has.
    assert!(world.run(&["pane-start"]).status.success());
    assert_eq!(world.pane_option("@workspace_id"), id);
}

#[test]
fn an_identity_that_is_not_32_lowercase_hex_digits_is_replaced() {
    for bad in ["zzz", "", &"A".repeat(32), &"a".repeat(31), &"a".repeat(33)] {
        let world = World::labelled();
        if !bad.is_empty() {
            world.set_pane("@workspace_id", bad);
        }
        assert!(world.run(&["pane-start"]).status.success());
        let id = world.pane_option("@workspace_id");
        assert!(valid_id(&id) && id != bad, "{bad}: {id}");
    }
}

#[test]
fn a_valid_identity_is_kept() {
    let world = World::labelled();
    world.set_pane("@workspace_id", UID.replace('u', "a").as_str());
    assert!(world.run(&["pane-start"]).status.success());
    assert_eq!(world.pane_option("@workspace_id"), "a".repeat(32));
}

#[test]
fn an_unlabelled_pane_asks_for_its_label() {
    let world = World::new();
    let mut child = world
        .command(&["pane-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"  build server \n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(text(&out.stdout), "Pane label [shell]: ");
    assert_eq!(world.pane_option("@workspace_label"), "build server");
    assert_eq!(world.fakes.shell_log().len(), 1);
}

#[test]
fn an_empty_answer_or_the_end_of_input_gives_the_default_label() {
    for (input, shown) in [
        ("\n", "Pane label [shell]: "),
        ("", "Pane label [shell]: \n"),
    ] {
        let world = World::new();
        let mut child = world
            .command(&["pane-start"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(text(&out.stdout), shown, "{input:?}");
        assert_eq!(world.pane_option("@workspace_label"), "shell", "{input:?}");
    }
}

#[test]
fn an_answer_is_cleaned_like_any_label() {
    let world = World::new();
    let long = "x".repeat(100);
    let mut child = world
        .command(&["pane-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("a\u{1b}[1mb{long}\n").as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    let label = world.pane_option("@workspace_label");
    assert_eq!(label.chars().count(), 80);
    assert!(label.starts_with("a[1mbxxx"), "{label}");
}

#[test]
fn ctrl_c_at_the_label_prompt_gives_the_default_label_and_starts_the_shell() {
    let world = World::new();
    let mut child = world
        .command(&["pane-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let _input = child.stdin.take(); // stays open: the prompt waits
    let mut stdout = child.stdout.take().unwrap();
    let mut prompt = vec![0u8; "Pane label [shell]: ".len()];
    stdout.read_exact(&mut prompt).unwrap();
    assert_eq!(text(&prompt), "Pane label [shell]: ");
    // SAFETY: signals a child of this test that is still running.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");
    assert_eq!(rest, "\n");
    assert_eq!(world.pane_option("@workspace_label"), "shell");
    assert_eq!(world.fakes.shell_log().len(), 1);
}

#[test]
fn the_label_command_asks_with_the_current_label_as_the_default() {
    let world = World::labelled();
    let mut child = world
        .command(&["label"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(text(&out.stdout), "Pane label [main]: ");
    assert_eq!(world.pane_option("@workspace_label"), "main");
    assert!(world.fakes.shell_log().is_empty(), "label starts no shell");

    let mut child = world
        .command(&["label"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"renamed\n").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(world.pane_option("@workspace_label"), "renamed");
}

#[test]
fn the_label_command_names_a_pane_by_argument() {
    let world = World::new();
    let other = world.scratch.pane(&["split-window", "-t", "alpha:one"]);
    let mut child = world
        .command(&["label", &other])
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"logs\n").unwrap();
    assert!(child.wait().unwrap().success());
    let label = |pane: &str| {
        world
            .scratch
            .tmux(&["show-options", "-pqv", "-t", pane, "@workspace_label"])
    };
    assert_eq!(label(&other), "logs");
    assert_eq!(label(&world.pane), "");
}

#[test]
fn the_label_command_without_a_label_proposes_shell() {
    let world = World::new();
    let mut child = world
        .command(&["label"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert_eq!(text(&out.stdout), "Pane label [shell]: \n");
    assert_eq!(world.pane_option("@workspace_label"), "shell");
}

/// A label with a `$`, a `#` or a `;` in it is read back as it is, not expanded or split.
#[test]
fn a_label_reads_back_unchanged() {
    for label in ["a$b", "#{pane_id}", "50% $HOME ~", "x#[fg=red]y", "a'b\"c"] {
        let world = World::new();
        world.set_pane("@workspace_label", label);
        let out = world.run(&["pane-start"]);
        assert!(out.status.success());
        assert_eq!(text(&out.stdout), "", "{label}: asked for a label");
        assert_eq!(world.pane_option("@workspace_label"), label);
    }
}

#[test]
fn a_pane_start_that_cannot_write_fails_with_the_error_contract() {
    let world = World::labelled();
    let out = world
        .command(&["pane-start"])
        .env("TMUX_PANE", "%999")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("Workspace error: "),
        "{}",
        text(&out.stderr)
    );
    assert!(world.fakes.shell_log().is_empty());
}

// ---------------------------------------------------------------------------------------------
// SnapshotRestore: save and restore on a scratch server with an empty state directory.

const SLEEP: &str = "exec sleep 300";

/// A scratch server with session `alpha` and session `beta`: tab `code` with the panes `api` and `logs`
/// (30% wide, so that a tiled layout would be wrong), and tab `db` with the pane `psql`.
/// Restored panes exec `/bin/sh`, never the developer's shell.
struct Saved {
    scratch: ScratchTmux,
    /// The first pane of tab `code`.
    api: String,
}

impl Saved {
    fn new() -> Saved {
        let scratch = ScratchTmux::new();
        scratch.tmux(&["set-option", "-g", "default-shell", "/bin/sh"]);
        scratch.tmux(&["set-environment", "-g", "SHELL", "/bin/sh"]);
        scratch.tmux(&[
            "new-session",
            "-d",
            "-s",
            "beta",
            "-n",
            "code",
            "-x",
            "120",
            "-y",
            "40",
            SLEEP,
        ]);
        let api = scratch.expand("beta:code", "#{pane_id}");
        let logs = scratch.pane(&["split-window", "-t", &api, "-h", "-l", "30%"]);
        let psql = scratch.pane(&["new-window", "-t", "beta:", "-n", "db"]);
        for (pane, label) in [(&api, "api"), (&logs, "logs"), (&psql, "psql")] {
            scratch.tmux(&["set-option", "-p", "-t", pane, "@workspace_label", label]);
        }
        Saved { scratch, api }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = self.scratch.bin(EXE);
        command.args(args).stdin(Stdio::null());
        command
    }

    /// Run the binary; it must succeed.
    fn controller(&self, args: &[&str]) -> Output {
        let out = self.command(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        out
    }

    /// Pane geometry, not `window_layout`: the layout string names pane ids, which a restore renews.
    fn layout(&self) -> Vec<String> {
        let format = "#{window_index}:#{window_name}:#{pane_index}:#{pane_left},#{pane_top},#{pane_width}x#{pane_height}";
        self.beta_panes(format)
    }

    fn labels(&self) -> Vec<String> {
        self.beta_panes("#{window_name}:#{@workspace_label}")
    }

    fn beta_panes(&self, format: &str) -> Vec<String> {
        let out = self
            .scratch
            .run(&["list-panes", "-s", "-t", "=beta", "-F", format]);
        text(&out.stdout)
            .split_whitespace()
            .map(String::from)
            .collect()
    }

    fn has_beta(&self) -> bool {
        self.scratch
            .run(&["has-session", "-t", "=beta"])
            .status
            .success()
    }

    fn state(&self) -> &Path {
        self.scratch.state_dir()
    }

    /// The snapshot `latest.json` points at.
    fn latest(&self) -> (std::path::PathBuf, Value) {
        let latest: Value =
            serde_json::from_slice(&std::fs::read(self.state().join("latest.json")).unwrap())
                .unwrap();
        let path = std::path::PathBuf::from(latest["path"].as_str().unwrap());
        let data = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        (path, data)
    }

    /// The names of the snapshot folders, oldest first.
    fn snapshots(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.state().join("snapshots"))
            .map(|dir| {
                dir.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

/// Poll until `read` gives `expected`, for up to 10 s, then assert that it does.
fn eventually<T: PartialEq + std::fmt::Debug>(read: impl Fn() -> T, expected: T) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while read() != expected && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(read(), expected);
}

#[test]
fn round_trip_restores_sessions_tabs_labels_and_layout() {
    let saved = Saved::new();
    let (layout, labels) = (saved.layout(), saved.labels());
    assert_eq!(labels, ["code:api", "code:logs", "db:psql"]);
    saved.controller(&["save"]);
    saved.scratch.tmux(&["kill-session", "-t", "=beta"]);
    saved.controller(&["restore"]);
    eventually(|| saved.labels(), labels);
    assert_eq!(saved.layout(), layout);
}

#[test]
fn initialize_restores_once_per_server() {
    let saved = Saved::new();
    saved.controller(&["save"]);
    saved.scratch.tmux(&["kill-session", "-t", "=beta"]);
    saved.controller(&["init"]);
    eventually(|| saved.has_beta(), true);
    saved.scratch.tmux(&["kill-session", "-t", "=beta"]);
    saved.controller(&["init"]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !saved.has_beta(),
        "a second init restored a workspace the user closed"
    );
}

#[test]
fn snapshot_files_are_private() {
    let saved = Saved::new();
    saved.controller(&["save"]);
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let (path, _) = saved.latest();
    let directory = path.parent().unwrap();
    assert_eq!(mode(saved.state()), 0o700);
    assert_eq!(mode(&saved.state().join("snapshots")), 0o700);
    assert_eq!(mode(directory), 0o700);
    assert_eq!(mode(&saved.state().join("latest.json")), 0o600);
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(mode(&entry.path()), 0o600, "{:?}", entry.file_name());
    }
}

#[test]
fn save_writes_python_shaped_json() {
    let saved = Saved::new();
    saved.controller(&["save"]);
    let (path, data) = saved.latest();
    let file = std::fs::read_to_string(&path).unwrap();
    let keys =
        |value: &Value| -> Vec<String> { value.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys(&data),
        [
            "version",
            "saved_at",
            "directory",
            "active_session",
            "windows",
            "panes"
        ]
    );
    assert_eq!(data["version"], json!(1));
    assert!(data["saved_at"].is_f64(), "{}", data["saved_at"]);
    assert_eq!(data["directory"], json!(path.parent().unwrap()));
    // The text is what json.dumps(indent=2) writes, with no newline at the end.
    assert_eq!(file, dumps(&data, Style::Indent2, false));
    assert!(!file.ends_with('\n'));

    let panes = data["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 4, "alpha:one and the three of beta");
    for pane in panes {
        assert_eq!(
            keys(pane),
            [
                "id", "session", "window", "index", "label", "cwd", "active", "parked", "home",
                "app"
            ]
        );
        let id = pane["id"].as_str().unwrap();
        assert!(
            id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "{id}"
        );
        assert!(pane["window"].is_u64() && pane["index"].is_u64());
        assert!(pane["active"].is_boolean());
        assert_eq!(
            (pane["parked"].as_str(), pane["home"].as_str()),
            (Some(""), Some(""))
        );
        assert!(pane["app"].is_null());
        assert!(path.parent().unwrap().join(format!("{id}.txt")).is_file());
    }
    let labelled: Vec<(&str, &str)> = panes
        .iter()
        .map(|p| (p["session"].as_str().unwrap(), p["label"].as_str().unwrap()))
        .collect();
    assert_eq!(
        labelled,
        [
            ("alpha", "shell"),
            ("beta", "api"),
            ("beta", "logs"),
            ("beta", "psql")
        ]
    );
    // The panes' ids are the ones now stored in tmux.
    let stored = saved
        .scratch
        .tmux(&["show-options", "-pqv", "-t", &saved.api, "@workspace_id"]);
    assert_eq!(panes[1]["id"], json!(stored));

    let windows = data["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 3);
    for window in windows {
        assert_eq!(
            keys(window),
            [
                "session_name",
                "window_index",
                "window_name",
                "window_layout",
                "window_active",
                "window_width",
                "window_height",
                "window_zoomed_flag"
            ]
        );
        assert!(window.as_object().unwrap().values().all(Value::is_string));
    }
    assert_eq!(windows[1]["window_name"], json!("code"));
    assert_eq!(
        (
            windows[1]["window_width"].as_str(),
            windows[1]["window_height"].as_str()
        ),
        (Some("120"), Some("40"))
    );

    // latest.json is json.dumps with the default separators.
    let latest = std::fs::read_to_string(saved.state().join("latest.json")).unwrap();
    let parsed: Value = serde_json::from_str(&latest).unwrap();
    assert_eq!(keys(&parsed), ["saved_at", "path"]);
    assert_eq!(parsed["saved_at"], data["saved_at"]);
    assert_eq!(parsed["path"], json!(path));
    assert_eq!(latest, dumps(&parsed, Style::Default, false));
    assert!(latest.starts_with("{\"saved_at\": "), "{latest}");
}

#[test]
fn a_panes_terminal_text_is_saved_with_one_newline_after_it() {
    let saved = Saved::new();
    let pane = saved.scratch.tmux(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "beta:",
        "-n",
        "text",
        "printf 'first line\\nsecond line\\n\\n\\n'; exec sleep 300",
    ]);
    eventually(
        || {
            saved
                .scratch
                .tmux(&["capture-pane", "-p", "-t", &pane])
                .starts_with("first line")
        },
        true,
    );
    saved.controller(&["save"]);
    let (path, data) = saved.latest();
    let record = data["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["window"] == json!(2))
        .unwrap();
    let file = path
        .parent()
        .unwrap()
        .join(format!("{}.txt", record["id"].as_str().unwrap()));
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        "first line\nsecond line\n"
    );
}

#[test]
fn the_bootstrap_session_and_unparked_parking_panes_are_not_saved() {
    let saved = Saved::new();
    saved
        .scratch
        .tmux(&["new-session", "-d", "-s", "__workspace_bootstrap", SLEEP]);
    saved
        .scratch
        .tmux(&["new-session", "-d", "-s", "__parked", "-n", "stash", SLEEP]);
    let parked = saved
        .scratch
        .pane(&["new-window", "-t", "__parked:", "-n", "kept"]);
    saved.scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &parked,
        "@workspace_parked",
        "1700000000",
    ]);
    saved.scratch.tmux(&[
        "set-option",
        "-p",
        "-t",
        &parked,
        "@workspace_home",
        "beta:code",
    ]);
    saved.controller(&["save"]);
    let (_, data) = saved.latest();
    let sessions: Vec<&str> = data["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["session"].as_str().unwrap())
        .collect();
    assert_eq!(sessions, ["__parked", "alpha", "beta", "beta", "beta"]);
    let kept = &data["panes"][0];
    assert_eq!(kept["parked"], json!("1700000000"));
    assert_eq!(kept["home"], json!("beta:code"));
    let windows: Vec<&str> = data["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["session_name"].as_str().unwrap())
        .collect();
    assert!(!windows.contains(&"__workspace_bootstrap"), "{windows:?}");
    assert_eq!(
        windows.iter().filter(|name| **name == "__parked").count(),
        1
    );
}

#[test]
fn a_save_while_restoring_does_nothing() {
    let saved = Saved::new();
    saved
        .scratch
        .tmux(&["set-option", "-g", "@workspace_restoring", "1"]);
    saved.controller(&["save"]);
    assert!(!saved.state().join("latest.json").exists());
    assert!(saved.snapshots().is_empty());
}

#[test]
fn save_keeps_the_three_newest_snapshots() {
    let saved = Saved::new();
    let mut made = Vec::new();
    for _ in 0..5 {
        saved.controller(&["save"]);
        made.push(
            saved
                .latest()
                .0
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
    }
    assert_eq!(saved.snapshots(), made[2..]);
    // What is not a snapshot folder is never pruned, whatever its place in the order.
    std::fs::create_dir(saved.state().join("snapshots/keep-me")).unwrap();
    std::fs::write(saved.state().join("snapshots/0"), "x").unwrap();
    saved.controller(&["save"]);
    let left = saved.snapshots();
    assert!(
        left.contains(&"keep-me".to_string()) && left.contains(&"0".to_string()),
        "{left:?}"
    );
}

#[test]
fn save_records_the_time_and_clears_the_error() {
    let saved = Saved::new();
    saved
        .scratch
        .tmux(&["set-option", "-g", "@workspace_save_error", "old"]);
    saved.controller(&["save"]);
    let option = |name: &str| saved.scratch.tmux(&["show-options", "-gqv", name]);
    let shown = option("@workspace_saved");
    assert!(
        shown.len() == 5
            && shown.as_bytes()[2] == b':'
            && shown.bytes().filter(u8::is_ascii_digit).count() == 4,
        "{shown}"
    );
    assert_eq!(option("@workspace_save_error"), "");
}

#[test]
fn save_after_a_recent_save_is_not_skipped_but_tick_is() {
    let saved = Saved::new();
    saved.controller(&["tick"]);
    assert_eq!(saved.snapshots().len(), 1);
    saved.controller(&["tick"]);
    assert_eq!(
        saved.snapshots().len(),
        1,
        "an autosave within five minutes of the last save"
    );
    saved.controller(&["save"]);
    assert_eq!(saved.snapshots().len(), 2, "a save by hand always saves");
    // An old latest.json lets the next tick save again.
    let latest = saved.state().join("latest.json");
    let mut old: Value = serde_json::from_slice(&std::fs::read(&latest).unwrap()).unwrap();
    old["saved_at"] = json!(1.0);
    std::fs::write(&latest, old.to_string()).unwrap();
    saved.controller(&["tick"]);
    assert_eq!(saved.snapshots().len(), 3);
}

#[test]
fn tick_and_init_schedule_the_next_tick_in_five_minutes() {
    let saved = Saved::new();
    let dir = TempDir::new();
    let trace = dir.path().join("trace");
    let run = |command: &str| {
        let out = saved
            .command(&[command])
            .env("TMUX_WORKSPACE_TRACE", &trace)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    };
    let scheduled = || -> Vec<String> {
        std::fs::read_to_string(&trace)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("tmux run-shell -b -d 300 "))
            .map(String::from)
            .collect()
    };
    run("tick");
    let ticks = scheduled();
    assert_eq!(ticks.len(), 1, "{ticks:?}");
    assert!(ticks[0].ends_with(" tick"), "{ticks:?}");
    let program = ticks[0]
        .trim_start_matches("tmux run-shell -b -d 300 ")
        .trim_end_matches(" tick");
    assert!(Path::new(program).is_file(), "{program}");
    saved.scratch.tmux(&["kill-session", "-t", "=beta"]);
    run("init");
    assert_eq!(scheduled().len(), 2);
    assert_eq!(scheduled()[1], ticks[0]);
    assert_eq!(
        saved
            .scratch
            .tmux(&["show-options", "-gqv", "@workspace_initialized"]),
        "1"
    );
    run("init");
    assert_eq!(scheduled().len(), 2, "a second init starts no second timer");
}

/// A folder to put first on `PATH`: its `tmux` fails `capture-pane` and runs the real one otherwise.
fn tmux_without_capture(dir: &Path) -> String {
    let real = tmux_workspace::process::which("tmux").expect("tmux on PATH");
    let wrapper = dir.join("tmux");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do [ \"$arg\" = capture-pane ] && exit 1; done\nexec '{}' \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// test_performance.py::Saves, second half: a save that fails changes nothing the next restore reads.
#[test]
fn a_failed_capture_aborts_the_save_and_keeps_the_last_snapshot() {
    let saved = Saved::new();
    saved.controller(&["save"]);
    let before = std::fs::read(saved.state().join("latest.json")).unwrap();
    let folders = saved.snapshots();
    let dir = TempDir::new();
    let out = saved
        .command(&["save"])
        .env("PATH", tmux_without_capture(dir.path()))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("Workspace error: Command '["),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        std::fs::read(saved.state().join("latest.json")).unwrap(),
        before
    );
    assert_eq!(
        saved.snapshots(),
        folders,
        "a failed save leaves no half snapshot behind"
    );
}

#[test]
fn a_failed_autosave_is_reported_and_the_timer_goes_on() {
    let saved = Saved::new();
    let dir = TempDir::new();
    let trace = dir.path().join("trace");
    let out = saved
        .command(&["tick"])
        .env("PATH", tmux_without_capture(dir.path()))
        .env("TMUX_WORKSPACE_TRACE", &trace)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let error = saved
        .scratch
        .tmux(&["show-options", "-gqv", "@workspace_save_error"]);
    assert!(error.starts_with("Command '["), "{error}");
    assert!(error.contains("capture-pane"), "{error}");
    assert!(
        std::fs::read_to_string(&trace)
            .unwrap()
            .contains("run-shell -b -d 300 "),
        "the next autosave must be scheduled after a failure too"
    );
    assert!(!saved.state().join("latest.json").exists());
}

#[test]
fn concurrent_saves_take_the_lock() {
    let saved = Saved::new();
    let children: Vec<Child> = (0..2)
        .map(|_| {
            saved
                .command(&["save"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    }
    let folders = saved.snapshots();
    assert_eq!(folders.len(), 2, "one snapshot per call: {folders:?}");
    for folder in &folders {
        let names: Vec<String> = std::fs::read_dir(saved.state().join("snapshots").join(folder))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"workspace.json".to_string()), "{names:?}");
        assert!(
            names.iter().all(|name| !name.starts_with('.')),
            "a partial file: {names:?}"
        );
        assert_eq!(
            names.len(),
            5,
            "workspace.json and the text of four panes: {names:?}"
        );
    }
}

#[test]
fn save_waits_while_another_process_holds_the_lock() {
    let saved = Saved::new();
    let held = tmux_workspace::snapshot::lock_in(saved.state()).unwrap();
    let mut child = saved
        .command(&["save"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(
        !wait_for_exit(&mut child, Duration::from_millis(700)),
        "save ignored the lock"
    );
    assert!(!saved.state().join("latest.json").exists());
    drop(held);
    assert!(wait_for_exit(&mut child, Duration::from_secs(10)));
    assert!(saved.state().join("latest.json").exists());
}

#[test]
fn history_opens_the_saved_text_of_the_pane_in_less() {
    let saved = Saved::new();
    let fakes = Fakes::new();
    let log = fakes.dir.path().join("less.log");
    Fakes::write(
        &fakes.dir.path().join("bin/less"),
        &format!("printf '%s\\n' \"$*\" >> '{}'", log.display()),
    );
    let run = |pane: &str| {
        saved
            .command(&["history", pane])
            .env("PATH", fakes.bin())
            .output()
            .unwrap()
    };
    let out = run(&saved.api);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "Workspace error: No saved history yet; use prefix Ctrl+s first\n"
    );
    saved.controller(&["save"]);
    let (path, data) = saved.latest();
    let id = data["panes"][1]["id"].as_str().unwrap();
    assert!(run(&saved.api).status.success());
    assert_eq!(
        fakes.lines("less.log"),
        [format!(
            "-- {}",
            path.parent().unwrap().join(format!("{id}.txt")).display()
        )]
    );
    // A pane made after the save is not in it.
    let newer = saved
        .scratch
        .pane(&["new-window", "-t", "beta:", "-n", "newer"]);
    let out = run(&newer);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        "Workspace error: This pane is not in the latest snapshot\n"
    );
    assert_eq!(fakes.lines("less.log").len(), 1);
}

#[test]
fn a_damaged_latest_snapshot_is_an_error_and_nothing_is_restored() {
    let saved = Saved::new();
    std::fs::write(saved.state().join("latest.json"), "{").unwrap();
    for command in ["restore", "init"] {
        let out = saved.command(&[command]).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{command}");
        assert!(
            text(&out.stderr).starts_with("Workspace error: "),
            "{}",
            text(&out.stderr)
        );
    }
    for option in ["@workspace_restoring", "@workspace_initialized"] {
        assert_eq!(
            saved.scratch.tmux(&["show-options", "-gqv", option]),
            "",
            "{option}"
        );
    }
}
