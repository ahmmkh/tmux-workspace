//! First-run setup: ask where the user keeps projects and save it in their config.

use crate::error::Result;
use crate::paths::{self, Locations, normalize};
use serde_json::{Map, Value};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

pub const WELCOME: &str = "
Welcome to tmux-workspace.

Each workspace is a folder in your projects folder: `work api` opens ~/Projects/api,
creating it the first time. You can change this later in {config}.
";

pub const NEXT: &str = "
tmux-workspace is set up.
  Projects folder  {projects}
  Config           {config}

Try:
  work my-project   open or create a workspace in {projects}/my-project
  Ctrl+a A          dashboard of every pane and agent
  Ctrl+a ?          every shortcut
  tmux-workspace doctor   check the install
";

const USAGE: &str = "usage: tmux-workspace setup [--projects DIR] [--yes]";

/// `template` with each `{name}` replaced in one pass, so a value never gets expanded again.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut text = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        rest = &rest[open..];
        match values.iter().find(|(name, _)| {
            rest[1..].starts_with(name) && rest[1 + name.len()..].starts_with('}')
        }) {
            Some((name, value)) => {
                text.push_str(value);
                rest = &rest[name.len() + 2..];
            }
            None => {
                text.push('{');
                rest = &rest[1..];
            }
        }
    }
    text.push_str(rest);
    text
}

/// `~/code` rather than `/home/me/code`, so the config moves between machines.
fn display(home: &Path, path: &Path) -> String {
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Ok(inside) => format!("~/{}", inside.display()),
        Err(_) => path.display().to_string(),
    }
}

/// What the user typed as a folder: `~` expanded, a relative path taken from home.
fn resolve(loc: &Locations, answer: &str) -> PathBuf {
    let path = loc.expand_user(answer.trim());
    if path.is_absolute() {
        path
    } else {
        normalize(&loc.home.join(path))
    }
}

/// Why this path cannot be the projects folder, if it cannot.
fn problem(home: &Path, path: &Path) -> Option<String> {
    (path.exists() && !path.is_dir())
        .then(|| format!("{} is a file, not a folder.", display(home, path)))
}

/// `tmux-workspace setup [--projects DIR] [--yes]`. Returns an exit status.
pub fn setup(args: &[String]) -> Result<i32> {
    // SAFETY: `isatty` only inspects file descriptor 0.
    let interactive = unsafe { libc::isatty(0) } == 1;
    setup_with(
        &Locations::current(),
        args,
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        interactive,
    )
}

/// `setup` with its world passed in: where things live, the streams, and whether input is a terminal.
pub fn setup_with(
    loc: &Locations,
    args: &[String],
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    errors: &mut dyn Write,
    interactive: bool,
) -> Result<i32> {
    let home = loc.home.as_path();
    let default = loc.projects_dir();
    let config = display(home, &loc.user_config_path());
    let chosen = if let Some(flag) = args.iter().position(|a| a == "--projects") {
        let Some(answer) = args.get(flag + 1) else {
            writeln!(errors, "{USAGE}")?;
            return Ok(2);
        };
        let chosen = resolve(loc, answer);
        if let Some(problem) = problem(home, &chosen) {
            writeln!(errors, "{problem}")?;
            return Ok(2);
        }
        chosen
    } else if args.iter().any(|a| a == "--yes") || !interactive {
        default
    } else {
        writeln!(output, "{}", fill(WELCOME, &[("config", &config)]))?;
        let Some(chosen) = ask(loc, &default, input, output)? else {
            return Ok(1);
        };
        chosen
    };
    if !chosen.exists() {
        fs::create_dir_all(&chosen).map_err(|e| paths::cannot_write(&chosen, e))?;
        writeln!(output, "Created {}", display(home, &chosen))?;
    }
    let shown = display(home, &chosen);
    let mut updates = Map::new();
    updates.insert("projects_dir".to_string(), Value::String(shown.clone()));
    loc.save_config(updates)?;
    let next = fill(NEXT, &[("projects", &shown), ("config", &config)]);
    writeln!(output, "{next}")?;
    Ok(0)
}

/// Ask until the answer can be a projects folder. `None` when the input ends first.
fn ask(
    loc: &Locations,
    default: &Path,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<Option<PathBuf>> {
    loop {
        write!(
            output,
            "Where do you keep your projects? [{}]: ",
            display(&loc.home, default)
        )?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            writeln!(
                output,
                "\nSetup cancelled; nothing was saved. Run tmux-workspace setup to finish."
            )?;
            return Ok(None);
        }
        let chosen = if line.trim().is_empty() {
            default.to_path_buf()
        } else {
            resolve(loc, &line)
        };
        match problem(&loc.home, &chosen) {
            None => return Ok(Some(chosen)),
            Some(problem) => writeln!(output, "{problem} Choose another folder.")?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use serde_json::json;
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    const CONFIG_SHOWN: &str = "~/.config/tmux-workspace/config.json";

    /// What `setup` printed and returned.
    struct Run {
        code: i32,
        out: String,
        err: String,
    }

    /// A temporary home whose legacy clone ships the example config, as in tests/test_onboarding.py.
    struct World {
        home: TempDir,
        loc: Locations,
        config: PathBuf,
    }

    impl World {
        fn new() -> World {
            let home = TempDir::new();
            let repo = home.path().join("repo");
            fs::create_dir(&repo).unwrap();
            fs::write(
                repo.join("config.example.json"),
                "{\"model\": \"\", \"doppler\": null}\n",
            )
            .unwrap();
            let loc = Locations {
                home: home.path().to_path_buf(),
                xdg_config_home: None,
                projects: None,
                state_dir: None,
                share: Some(repo),
            };
            let config = home.path().join(".config/tmux-workspace/config.json");
            World { home, loc, config }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.home.path().join(name)
        }

        fn run(&self, input: &str, args: &[&str], interactive: bool) -> Run {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            let mut input = Cursor::new(input.as_bytes());
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = setup_with(
                &self.loc,
                &args,
                &mut input,
                &mut out,
                &mut err,
                interactive,
            )
            .unwrap();
            Run {
                code,
                out: String::from_utf8(out).unwrap(),
                err: String::from_utf8(err).unwrap(),
            }
        }

        /// A user at a terminal who types these answers, one per line.
        fn terminal(&self, answers: &[&str], args: &[&str]) -> Run {
            let typed: String = answers.iter().map(|a| format!("{a}\n")).collect();
            self.run(&typed, args, true)
        }

        /// No terminal on input.
        fn pipe(&self, args: &[&str]) -> Run {
            self.run("", args, false)
        }

        fn saved(&self) -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(&self.config).unwrap()).unwrap()
        }
    }

    // tests/test_onboarding.py::NeedsOnboarding

    mod needs_onboarding {
        use super::*;

        fn write_config(w: &World, text: &str) {
            fs::create_dir_all(w.config.parent().unwrap()).unwrap();
            fs::write(&w.config, text).unwrap();
        }

        #[test]
        fn missing_config_needs_onboarding() {
            assert!(World::new().loc.needs_onboarding());
        }

        #[test]
        fn config_without_projects_dir_needs_onboarding() {
            let w = World::new();
            write_config(&w, r#"{"model": ""}"#);
            assert!(w.loc.needs_onboarding());
        }

        #[test]
        fn broken_config_is_left_alone() {
            let w = World::new();
            write_config(&w, "{broken");
            assert!(!w.loc.needs_onboarding());
        }

        #[test]
        fn save_config_keeps_other_keys_and_is_private() {
            let w = World::new();
            write_config(&w, r#"{"model": "typesafe/jev-1"}"#);
            let mut updates = serde_json::Map::new();
            updates.insert("projects_dir".into(), json!("~/code"));
            w.loc.save_config(updates).unwrap();
            assert_eq!(
                w.saved(),
                json!({"model": "typesafe/jev-1", "projects_dir": "~/code"})
            );
            assert_eq!(
                fs::metadata(&w.config).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert!(!w.loc.needs_onboarding());
        }

        #[test]
        fn save_config_starts_from_the_legacy_config() {
            let w = World::new();
            fs::write(
                w.path("repo/config.json"),
                r#"{"model": "typesafe/jev-2", "doppler": {"project": "p"}}"#,
            )
            .unwrap();
            let mut updates = serde_json::Map::new();
            updates.insert("projects_dir".into(), json!("~/Projects"));
            w.loc.save_config(updates).unwrap();
            assert_eq!(w.saved()["doppler"], json!({"project": "p"}));
        }

        // Beyond the Python cases.

        #[test]
        fn a_config_that_is_not_an_object_is_left_alone() {
            let w = World::new();
            write_config(&w, "[]");
            assert!(!w.loc.needs_onboarding());
        }

        #[test]
        fn a_config_that_names_a_projects_dir_is_done() {
            let w = World::new();
            write_config(&w, r#"{"projects_dir": ""}"#);
            assert!(!w.loc.needs_onboarding());
        }

        #[test]
        fn a_config_path_that_is_not_a_file_is_left_alone() {
            let w = World::new();
            fs::create_dir_all(&w.config).unwrap();
            assert!(!w.loc.needs_onboarding());
        }
    }

    // tests/test_onboarding.py::Setup

    mod setup {
        use super::*;

        #[test]
        fn enter_keeps_the_default_and_creates_it() {
            let w = World::new();
            let run = w.terminal(&[""], &[]);
            assert_eq!(run.code, 0);
            assert_eq!(w.saved()["projects_dir"], "~/Projects");
            assert!(w.path("Projects").is_dir());
            assert!(run.out.contains("Where do you keep your projects?"));
            assert!(run.out.contains("work my-project"));
        }

        #[test]
        fn typed_folder_is_saved_with_a_tilde() {
            let w = World::new();
            w.terminal(&["~/code"], &[]);
            assert_eq!(w.saved()["projects_dir"], "~/code");
            assert_eq!(w.loc.projects_dir(), w.path("code"));
            // The legacy example, then the answer, in Python's layout.
            assert_eq!(
                fs::read_to_string(&w.config).unwrap(),
                "{\n  \"model\": \"\",\n  \"doppler\": null,\n  \"projects_dir\": \"~/code\"\n}\n"
            );
        }

        #[test]
        fn relative_answer_is_under_home() {
            let w = World::new();
            w.terminal(&["src"], &[]);
            assert_eq!(w.loc.projects_dir(), w.path("src"));
        }

        #[test]
        fn folder_outside_home_is_kept_absolute() {
            let w = World::new();
            let elsewhere = TempDir::new();
            let target = elsewhere.path().join("work");
            let target_text = target.to_string_lossy().into_owned();
            w.terminal(&[&target_text], &[]);
            assert_eq!(w.saved()["projects_dir"], target_text.as_str());
            assert!(target.is_dir());
        }

        #[test]
        fn a_file_is_refused_and_asked_again() {
            let w = World::new();
            fs::write(w.path("notes"), "x").unwrap();
            let run = w.terminal(&["notes", "code"], &[]);
            assert_eq!(run.code, 0);
            assert!(run.out.contains("is a file"), "{}", run.out);
            assert_eq!(w.loc.projects_dir(), w.path("code"));
        }

        #[test]
        fn projects_flag_needs_no_terminal() {
            let w = World::new();
            let dev = w.path("dev").to_string_lossy().into_owned();
            let run = w.pipe(&["--projects", &dev]);
            assert_eq!(run.code, 0);
            assert_eq!(w.loc.projects_dir(), w.path("dev"));
            assert!(!run.out.contains("Where do you keep"));
        }

        #[test]
        fn without_a_terminal_the_default_is_used() {
            let w = World::new();
            assert_eq!(w.pipe(&[]).code, 0);
            assert_eq!(w.loc.projects_dir(), w.path("Projects"));
        }

        #[test]
        fn end_of_input_cancels_without_saving() {
            let w = World::new();
            let run = w.terminal(&[], &[]);
            assert_eq!(run.code, 1);
            assert!(!w.config.exists());
        }
    }

    // The exact copy and the paths Python does not test.

    mod transcript {
        use super::*;

        fn next_text(projects: &str) -> String {
            format!(
                "\ntmux-workspace is set up.\n  Projects folder  {projects}\n  Config           {CONFIG_SHOWN}\n\nTry:\n  work my-project   open or create a workspace in {projects}/my-project\n  Ctrl+a A          dashboard of every pane and agent\n  Ctrl+a ?          every shortcut\n  tmux-workspace doctor   check the install\n\n"
            )
        }

        const WELCOME_TEXT: &str = "\nWelcome to tmux-workspace.\n\nEach workspace is a folder in your projects folder: `work api` opens ~/Projects/api,\ncreating it the first time. You can change this later in ~/.config/tmux-workspace/config.json.\n\n";

        #[test]
        fn an_interactive_run_prints_the_welcome_the_prompt_and_what_is_next() {
            let w = World::new();
            let run = w.terminal(&[""], &[]);
            assert_eq!(
                run.out,
                format!(
                    "{WELCOME_TEXT}Where do you keep your projects? [~/Projects]: Created ~/Projects\n{}",
                    next_text("~/Projects")
                )
            );
            assert_eq!(run.err, "");
            assert_eq!(
                fs::read_to_string(&w.config).unwrap(),
                "{\n  \"model\": \"\",\n  \"doppler\": null,\n  \"projects_dir\": \"~/Projects\"\n}\n"
            );
        }

        #[test]
        fn a_refused_answer_asks_again_with_the_same_prompt() {
            let w = World::new();
            fs::write(w.path("notes"), "x").unwrap();
            let run = w.terminal(&["notes", "code"], &[]);
            assert_eq!(
                run.out,
                format!(
                    "{WELCOME_TEXT}Where do you keep your projects? [~/Projects]: ~/notes is a file, not a folder. Choose another folder.\nWhere do you keep your projects? [~/Projects]: Created ~/code\n{}",
                    next_text("~/code")
                )
            );
        }

        #[test]
        fn cancelling_says_so_and_exits_1() {
            let w = World::new();
            let run = w.terminal(&[], &[]);
            assert_eq!(
                run.out,
                format!(
                    "{WELCOME_TEXT}Where do you keep your projects? [~/Projects]: \nSetup cancelled; nothing was saved. Run tmux-workspace setup to finish.\n"
                )
            );
        }

        #[test]
        fn the_default_follows_the_config_when_the_user_has_one() {
            let w = World::new();
            w.terminal(&["~/code"], &[]);
            let run = w.terminal(&[""], &[]);
            assert!(
                run.out
                    .contains("Where do you keep your projects? [~/code]: "),
                "{}",
                run.out
            );
            assert!(!run.out.contains("Created"), "{}", run.out);
            assert_eq!(w.saved()["projects_dir"], "~/code");
        }

        #[test]
        fn a_run_without_a_terminal_prints_only_the_result() {
            let w = World::new();
            let run = w.pipe(&[]);
            assert_eq!(
                run.out,
                format!("Created ~/Projects\n{}", next_text("~/Projects"))
            );
            assert_eq!(run.err, "");
        }

        #[test]
        fn yes_skips_the_prompt_at_a_terminal() {
            let w = World::new();
            let run = w.terminal(&["~/ignored"], &["--yes"]);
            assert_eq!(run.code, 0);
            assert_eq!(
                run.out,
                format!("Created ~/Projects\n{}", next_text("~/Projects"))
            );
            assert!(!w.path("ignored").exists());
        }

        #[test]
        fn an_existing_folder_is_not_announced_as_created() {
            let w = World::new();
            fs::create_dir(w.path("dev")).unwrap();
            let run = w.pipe(&["--projects", "dev"]);
            assert_eq!(run.out, next_text("~/dev"));
        }

        #[test]
        fn projects_flag_wins_over_yes_and_a_terminal() {
            let w = World::new();
            let run = w.terminal(&["~/typed"], &["--yes", "--projects", "~/dev"]);
            assert_eq!(w.saved()["projects_dir"], "~/dev");
            assert!(!run.out.contains("Where do you keep"));
        }

        #[test]
        fn projects_flag_without_a_folder_is_a_usage_error() {
            let w = World::new();
            let run = w.pipe(&["--projects"]);
            assert_eq!(run.code, 2);
            assert_eq!(run.out, "");
            assert_eq!(
                run.err,
                "usage: tmux-workspace setup [--projects DIR] [--yes]\n"
            );
            assert!(!w.config.exists());
        }

        #[test]
        fn projects_flag_naming_a_file_is_refused() {
            let w = World::new();
            fs::write(w.path("notes"), "x").unwrap();
            let run = w.pipe(&["--projects", "notes"]);
            assert_eq!(run.code, 2);
            assert_eq!(run.out, "");
            assert_eq!(run.err, "~/notes is a file, not a folder.\n");
            assert!(!w.config.exists());
        }

        #[test]
        fn a_folder_that_cannot_be_made_is_an_error_naming_it() {
            let w = World::new();
            fs::write(w.path("notes"), "x").unwrap();
            let args = ["--projects".to_string(), "notes/sub".to_string()];
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let error = setup_with(
                &w.loc,
                &args,
                &mut Cursor::new(&b""[..]),
                &mut out,
                &mut err,
                false,
            )
            .unwrap_err();
            assert!(error.to_string().contains("notes/sub"), "{error}");
            assert!(!w.config.exists());
        }
    }
}
