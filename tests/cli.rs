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
        "Workspace error: Unknown workspace command: nope\nReport it: work bug\n"
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
    let mut command = scratch.bin(EXE);
    command.arg("nope").env("TMUX_WORKSPACE_TRACE", &trace);
    // Outside tmux, and with a tmux on `PATH` that goes to the scratch server: were one to run, it
    // would show in the trace and still not reach another server.
    common::outside_tmux(&mut command, dir.path(), scratch.socket());
    let out = command.output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Workspace error: Unknown workspace command: nope\nReport it: work bug\n"
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
        "Workspace error: Install fzf to pick a workspace, or run: work NAME\nReport it: work bug\n"
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

/// `dash`, `dashboard`, `ai` and `cleanup` stay Python until stages 2 and 3 port them: the binary
/// hands them to `legacy/bin/tmux-workspace`.
mod legacy {
    use super::*;
    use std::path::{Path, PathBuf};

    fn script() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("legacy/bin/tmux-workspace")
    }

    #[test]
    fn dash_is_handed_to_the_legacy_python() {
        let scratch = ScratchTmux::new();
        scratch.pane(&["new-window", "-t", "alpha", "-n", "two"]);
        let args = ["dash", "--once", "--width", "80", "--height", "24"];
        let ours = scratch.bin(EXE).args(args).output().unwrap();
        // The frame the Python itself draws for this server.
        let theirs = scratch
            .bin("python3")
            .arg(script())
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            ours.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&ours.stderr)
        );
        assert_eq!(ours.stderr, b"");
        let frame = String::from_utf8_lossy(&ours.stdout);
        assert!(frame.contains("0:one"), "{frame}");
        assert!(frame.contains("1:two"), "{frame}");
        assert_eq!(frame, String::from_utf8_lossy(&theirs.stdout));
    }

    #[test]
    fn legacy_command_without_python_explains() {
        let scratch = ScratchTmux::new();
        let empty = TempDir::new();
        let out = scratch
            .bin(EXE)
            .arg("dash")
            .env("PATH", empty.path())
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "tmux-workspace dash needs python3 3.9+ until it is ported (stage 2/3)\n"
        );
        assert_eq!(out.stdout, b"");
        assert_eq!(out.status.code(), Some(1));
    }

    #[test]
    fn legacy_command_with_an_old_python_explains() {
        let scratch = ScratchTmux::new();
        let fakes = common::Fakes::new();
        // Fails the one version check.
        fakes.add("python3", "exit 1");
        let out = scratch
            .bin(EXE)
            .arg("cleanup")
            .env("PATH", fakes.path())
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "tmux-workspace cleanup needs python3 3.9+ until it is ported (stage 2/3)\n"
        );
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(fakes.calls("python3"), 1, "one `python3 -c` call, no more");
        assert_eq!(fakes.args("python3", 1)[0], "-c");
    }

    #[test]
    fn a_share_folder_without_the_legacy_script_is_an_error_not_a_crash() {
        let scratch = ScratchTmux::new();
        let fakes = common::Fakes::new();
        fakes.add("python3", "exit 0");
        let share = TempDir::new();
        let out = scratch
            .bin(EXE)
            .arg("ai")
            .env("PATH", fakes.path())
            .env("TMUX_WORKSPACE_SHARE", share.path())
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!(
                "Workspace error: ai needs {}/legacy/bin/tmux-workspace, which is missing\nReport it: work bug\n",
                share.path().display()
            )
        );
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(fakes.calls("python3"), 1, "the script is never started");
    }

    #[test]
    fn all_four_commands_run_the_script_with_their_arguments() {
        for command in ["dash", "dashboard", "ai", "cleanup"] {
            let scratch = ScratchTmux::new();
            let fakes = common::Fakes::new();
            // Passes the version check; the second call is the script, whose status is the command's.
            fakes.add(
                "python3",
                r#"[ "$1" = -c ] && exit 0
exit 7"#,
            );
            let out = scratch
                .bin(EXE)
                .args([command, "--once", "a b"])
                .env("PATH", fakes.path())
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(7), "{command}");
            assert_eq!(out.stderr, b"", "{command}");
            assert_eq!(fakes.calls("python3"), 2, "{command}");
            let check = fakes.args("python3", 1);
            assert_eq!(check[0], "-c", "{command}");
            assert!(check[1].contains("(3, 9)"), "{command}: {check:?}");
            assert_eq!(
                fakes.args("python3", 2),
                [script().to_str().unwrap(), command, "--once", "a b"],
                "{command}"
            );
        }
    }
}

/// `update [--check]` hands over to the installer that came with the binary,
/// `bash <share>/install.sh --update`: the binary has no HTTP client, the installer has curl or wget.
mod update {
    use super::*;
    use std::path::Path;

    /// A share folder whose install.sh writes down how it ran, and under which umask.
    fn share() -> TempDir {
        let share = TempDir::new();
        std::fs::write(share.path().join("agent-status.conf"), "").unwrap();
        tmux_workspace::testing::write_script(
            &share.path().join("install.sh"),
            "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\" > \"${0%/*}/ran\"\numask > \"${0%/*}/umask\"\necho checked\nexit 4\n",
        );
        share
    }

    fn ran(share: &Path) -> String {
        std::fs::read_to_string(share.join("ran")).unwrap_or_default()
    }

    #[test]
    fn update_runs_the_installer_next_to_the_binary() {
        let scratch = ScratchTmux::new();
        let share = share();
        let out = scratch
            .bin(EXE)
            .arg("update")
            .env("TMUX_WORKSPACE_SHARE", share.path())
            .output()
            .unwrap();
        assert_eq!(
            ran(share.path()),
            format!("{}/install.sh\n--update\n", share.path().display())
        );
        // The installer's output and status are the command's own: it replaced the binary.
        assert_eq!(common::text(&out.stdout), "checked\n");
        assert_eq!(out.stderr, b"");
        assert_eq!(out.status.code(), Some(4));
    }

    #[test]
    fn update_check_only_asks() {
        let scratch = ScratchTmux::new();
        let share = share();
        scratch
            .bin(EXE)
            .args(["update", "--check"])
            .env("TMUX_WORKSPACE_SHARE", share.path())
            .output()
            .unwrap();
        assert_eq!(
            ran(share.path()),
            format!("{}/install.sh\n--update\n--check\n", share.path().display())
        );
    }

    #[test]
    fn the_installer_runs_with_the_callers_umask() {
        // Every other command makes its files private; the installer's links and folders are the
        // user's, as when they run install.sh themselves.
        let scratch = ScratchTmux::new();
        let share = share();
        let mut command = scratch.bin("/bin/sh");
        command
            .args(["-c", "umask 022; exec \"$0\" update", EXE])
            .env("TMUX_WORKSPACE_SHARE", share.path());
        command.output().unwrap();
        assert_eq!(
            std::fs::read_to_string(share.path().join("umask"))
                .unwrap()
                .trim(),
            "0022"
        );
    }

    #[test]
    fn another_argument_is_a_usage_error() {
        let scratch = ScratchTmux::new();
        let share = share();
        let out = scratch
            .bin(EXE)
            .args(["update", "--now"])
            .env("TMUX_WORKSPACE_SHARE", share.path())
            .output()
            .unwrap();
        assert_eq!(
            common::text(&out.stderr),
            "Workspace error: Usage: tmux-workspace update [--check]\nReport it: work bug\n"
        );
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(ran(share.path()), "", "the installer does not run");
    }

    #[test]
    fn without_the_installer_it_says_what_is_missing() {
        let scratch = ScratchTmux::new();
        let share = TempDir::new();
        let out = scratch
            .bin(EXE)
            .arg("update")
            .env("TMUX_WORKSPACE_SHARE", share.path())
            .output()
            .unwrap();
        assert_eq!(
            common::text(&out.stderr),
            format!(
                "Workspace error: update needs {}/install.sh, which is missing\nReport it: work bug\n",
                share.path().display()
            )
        );
        assert_eq!(out.status.code(), Some(1));
    }
}

/// `integrations/agent-hook.py` is a shim now: v1 hook entries still run
/// `python3 ~/.local/lib/tmux-workspace/agent-hook.py <kind>`, a link to it, until
/// `install.sh --agents` replaces them.
mod agent_hook_shim {
    use super::*;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};

    fn shim() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("integrations/agent-hook.py")
    }

    /// Run the shim the way a v1 hook entry does, under this `HOME`, with this input.
    fn run(home: &Path, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new("python3")
            .arg(shim())
            .args(args)
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }

    #[test]
    fn it_hands_its_arguments_and_input_to_the_installed_binary() {
        let home = TempDir::new();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("tmux-workspace");
        tmux_workspace::testing::write_script(
            &fake,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{0}/argv'\n/bin/cat > '{0}/stdin'\necho out\necho err >&2\nexit 3\n",
                home.path().display()
            ),
        );
        // Not text: the bytes pass through untouched.
        let input = b"{\"session_id\": \"s\"}\xff\x00 and no newline";
        let out = run(home.path(), &["claude"], input);
        assert_eq!(
            std::fs::read_to_string(home.path().join("argv")).unwrap(),
            "agent-hook\nclaude\n"
        );
        assert_eq!(std::fs::read(home.path().join("stdin")).unwrap(), input);
        // Output and status are the binary's own: the shim replaced itself with it.
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn without_the_binary_it_exits_0_and_says_nothing() {
        let home = TempDir::new();
        let out = run(home.path(), &["claude"], b"{}");
        assert_eq!(out.stdout, b"");
        assert_eq!(out.stderr, b"");
        assert_eq!(out.status.code(), Some(0));
    }
}

/// The shared helpers keep a test away from the developer's tmux and from the programs they installed.
mod harness {
    use super::*;
    use std::process::Command;

    #[test]
    #[should_panic(expected = "Fakes::add refuses tmux")]
    fn fakes_do_not_take_the_name_tmux() {
        common::Fakes::new().add("tmux", "exit 0");
    }

    #[test]
    fn a_run_outside_tmux_gets_a_tmux_that_goes_to_the_scratch_server_only() {
        let scratch = ScratchTmux::new();
        let fakes = common::Fakes::new();
        fakes.real_tmux();
        let real = common::find_in_path("tmux");
        let before = std::fs::metadata(&real).unwrap();
        let list = |socket: &str| {
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "tmux list-sessions -F '#{session_name}'; echo \"[$TMUX|$TMUX_PANE]\"",
                ])
                .env("PATH", fakes.path())
                // What the developer's own tmux window would have set.
                .env("TMUX", "/not/a/socket,1,0")
                .env("TMUX_PANE", "%9");
            common::outside_tmux(&mut command, fakes.path(), socket);
            common::quietly(command)
        };
        let out = list(scratch.socket());
        assert_eq!(common::text(&out.stdout), "alpha\n[|]\n");
        // The link to the real tmux was replaced by the wrapper, not written through.
        assert!(std::fs::read_link(fakes.path().join("tmux")).is_err());
        let after = std::fs::metadata(&real).unwrap();
        assert_eq!(
            (before.len(), before.modified().unwrap()),
            (after.len(), after.modified().unwrap())
        );
        // A socket where nobody listens: the command fails, whatever server the developer runs.
        let nobody = fakes.path().join("nobody-listens");
        let out = list(nobody.to_str().unwrap());
        assert_eq!(common::text(&out.stdout), "[|]\n");
        // tmux names the socket it could not reach.
        assert!(
            common::text(&out.stderr).contains("nobody-listens"),
            "{}",
            common::text(&out.stderr)
        );
    }
}
