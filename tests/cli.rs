//! The binary's top level: dispatch, the error contract and the process trace.

mod common;

use tmux_workspace::testing::{ScratchTmux, TempDir};

const EXE: &str = common::EXE;

#[test]
fn unknown_command_reports_and_exits_1() {
    let scratch = ScratchTmux::new();
    let out = scratch.bin(EXE).arg("nope").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Workspace error: Unknown workspace command: nope\n"
    );
    assert_eq!(
        out.stdout, b"",
        "inside tmux the message goes to display-message, not stdout"
    );
    assert_eq!(out.status.code(), Some(1));
    // The display-message call is asserted by the trace test below: no client is attached to read it back.
}

#[test]
fn trace_records_each_child_process() {
    let scratch = ScratchTmux::new();
    let dir = TempDir::new();
    let trace = dir.path().join("trace");
    let out = scratch
        .bin(EXE)
        .arg("nope")
        .env("TMUX_WORKSPACE_TRACE", &trace)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        std::fs::read_to_string(&trace).unwrap(),
        "tmux display-message Workspace: Unknown workspace command: nope\n"
    );
}

#[test]
fn outside_tmux_the_message_goes_to_stdout_and_no_tmux_runs() {
    let scratch = ScratchTmux::new();
    let dir = TempDir::new();
    let trace = dir.path().join("trace");
    let out = scratch
        .bin(EXE)
        .arg("nope")
        .env_remove("TMUX")
        .env("TMUX_WORKSPACE_TRACE", &trace)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Workspace error: Unknown workspace command: nope\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Workspace: Unknown workspace command: nope\n"
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(!trace.exists(), "no child process was started");
}

#[test]
fn no_argument_dispatches_open() {
    // `open` without a workspace name is the picker, which needs fzf or sesh and a terminal: on a
    // `PATH` that has only tmux, it says what to install. (Not a terminal: stdin is closed.)
    let scratch = ScratchTmux::new();
    let fakes = common::Fakes::new();
    fakes.real_tmux();
    let out = common::quietly({
        let mut command = scratch.bin(EXE);
        command.env("PATH", fakes.path());
        command
    });
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Workspace error: Install fzf to pick a workspace, or run: work NAME\n"
    );
    assert_eq!(out.status.code(), Some(1));
}

/// `tmux-workspace setup`, run in a scratch world. A pipe is not a terminal, so these take the
/// non-interactive path, as Python does; the prompts are covered by the unit tests of `onboarding`.
mod setup {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Output, Stdio};

    const CONFIG: &str = "{\n  \"model\": \"\",\n  \"doppler\": null,\n  \"projects_dir\": \"~/";

    struct World {
        scratch: ScratchTmux,
        share: TempDir,
        trace: std::path::PathBuf,
        _trace_dir: TempDir,
    }

    impl World {
        fn new() -> World {
            let share = TempDir::new();
            std::fs::write(
                share.path().join("config.example.json"),
                "{\"model\": \"\", \"doppler\": null}\n",
            )
            .unwrap();
            let trace_dir = TempDir::new();
            World {
                scratch: ScratchTmux::new(),
                share,
                trace: trace_dir.path().join("trace"),
                _trace_dir: trace_dir,
            }
        }

        /// Run `setup` with these arguments and this text piped to its input.
        fn setup(&self, args: &[&str], piped: &str) -> Output {
            self.run(self.scratch.bin(EXE), args, piped)
        }

        /// Run `setup` as a caller whose umask is 022, which is what most shells start with.
        fn setup_under_umask_022(&self, args: &[&str], piped: &str) -> Output {
            let mut command = self.scratch.bin("/bin/sh");
            command.args(["-c", "umask 022; exec \"$0\" \"$@\"", EXE]);
            self.run(command, args, piped)
        }

        fn run(&self, mut command: std::process::Command, args: &[&str], piped: &str) -> Output {
            let mut child = command
                .arg("setup")
                .args(args)
                .env("TMUX_WORKSPACE_SHARE", self.share.path())
                .env("TMUX_WORKSPACE_TRACE", &self.trace)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(piped.as_bytes()).unwrap();
            drop(stdin);
            child.wait_with_output().unwrap()
        }

        fn config(&self) -> std::path::PathBuf {
            self.scratch
                .home()
                .join(".config/tmux-workspace/config.json")
        }
    }

    fn next_text(projects: &str) -> String {
        format!(
            "\ntmux-workspace is set up.\n  Projects folder  {projects}\n  Config           ~/.config/tmux-workspace/config.json\n\nTry:\n  work my-project   open or create a workspace in {projects}/my-project\n  Ctrl+a A          dashboard of every pane and agent\n  Ctrl+a ?          every shortcut\n  tmux-workspace doctor   check the install\n\n"
        )
    }

    #[test]
    fn without_a_terminal_the_default_folder_is_created_and_saved() {
        let w = World::new();
        // The piped answer is not read: a pipe is not a terminal.
        let out = w.setup(&[], "~/code\n");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("Created ~/Projects\n{}", next_text("~/Projects"))
        );
        assert_eq!(out.stderr, b"");
        assert!(w.scratch.home().join("Projects").is_dir());
        assert!(!w.scratch.home().join("code").exists());
        assert_eq!(
            std::fs::read_to_string(w.config()).unwrap(),
            format!("{CONFIG}Projects\"\n}}\n")
        );
        let mode = std::fs::metadata(w.config()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!w.trace.exists(), "setup starts no child process");
    }

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn what_setup_creates_is_private_whatever_the_callers_umask() {
        let w = World::new();
        let home = w.scratch.home();
        let out = w.setup_under_umask_022(&["--projects", "new/projects"], "");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Python runs with umask 077: the new folder and its new parent, the config's folders and file.
        assert_eq!(mode(&home.join("new/projects")), 0o700);
        assert_eq!(mode(&home.join("new")), 0o700);
        assert_eq!(mode(&home.join(".config")), 0o700);
        assert_eq!(mode(&home.join(".config/tmux-workspace")), 0o700);
        assert_eq!(mode(&w.config()), 0o600);
    }

    #[test]
    fn the_projects_flag_saves_that_folder() {
        let w = World::new();
        let out = w.setup(&["--projects", "dev"], "");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("Created ~/dev\n{}", next_text("~/dev"))
        );
        assert!(w.scratch.home().join("dev").is_dir());
        assert_eq!(
            std::fs::read_to_string(w.config()).unwrap(),
            format!("{CONFIG}dev\"\n}}\n")
        );
    }

    #[test]
    fn a_second_run_keeps_the_saved_folder_as_its_default() {
        let w = World::new();
        w.setup(&["--projects", "dev"], "");
        let out = w.setup(&[], "");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(String::from_utf8_lossy(&out.stdout), next_text("~/dev"));
    }

    #[test]
    fn the_projects_flag_without_a_folder_exits_2_quietly() {
        let w = World::new();
        let out = w.setup(&["--projects"], "");
        assert_eq!(out.status.code(), Some(2));
        assert_eq!(out.stdout, b"");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "usage: tmux-workspace setup [--projects DIR] [--yes]\n"
        );
        assert!(!w.config().exists());
        assert!(!w.trace.exists(), "a usage error is not a tmux message");
    }

    #[test]
    fn a_folder_that_cannot_be_made_follows_the_error_contract() {
        let w = World::new();
        std::fs::write(w.scratch.home().join("notes"), "x").unwrap();
        let out = w.setup(&["--projects", "notes/sub"], "");
        assert_eq!(out.status.code(), Some(1));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.starts_with("Workspace error: Cannot write "), "{err}");
        assert!(err.contains("notes/sub: "), "{err}");
        let trace = std::fs::read_to_string(&w.trace).unwrap();
        assert!(
            trace.starts_with("tmux display-message Workspace: Cannot write "),
            "{trace}"
        );
        assert!(!w.config().exists());
    }
}
