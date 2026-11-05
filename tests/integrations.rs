//! `integrations install|uninstall`, ported from `tests/test_integrations.py::Install`, and the
//! command that wraps it. Every test works in a temporary `HOME`; none reads or writes the real
//! `~/.claude`, `~/.codex`, `~/.omp`, `~/.config/nvim` or `~/Library`.

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use tmux_workspace::integrations::{Target, install_on, merge_hooks, ours, render, uninstall_on};
use tmux_workspace::testing::TempDir;
use tmux_workspace::text::shlex_quote;

const EXE: &str = env!("CARGO_BIN_EXE_tmux-workspace");

/// The checkout: what the links lead to and where the templates are.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap()
}

/// A temporary home folder, with the folders of the tools that are installed in it.
struct World {
    _dir: TempDir,
    home: PathBuf,
}

impl World {
    fn new() -> World {
        let dir = TempDir::new();
        let home = dir.path().canonicalize().unwrap();
        World { _dir: dir, home }
    }

    /// A world whose home folder has a space in its name.
    fn with_a_space() -> World {
        let dir = TempDir::new();
        let home = dir.path().canonicalize().unwrap().join("a b");
        fs::create_dir(&home).unwrap();
        World { _dir: dir, home }
    }

    fn target(&self, system: &str) -> Target {
        Target {
            home: self.home.clone(),
            share: root(),
            codex_home: None,
            system: system.into(),
        }
    }

    fn install(&self, system: &str) -> Vec<String> {
        install_on(&self.target(system)).unwrap()
    }

    fn uninstall(&self) -> Vec<String> {
        uninstall_on(&self.target("Linux")).unwrap()
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.home.join(relative)
    }

    fn mkdir(&self, relative: &str) {
        fs::create_dir_all(self.path(relative)).unwrap();
    }

    fn write_json(&self, relative: &str, value: &Value) {
        fs::write(self.path(relative), value.to_string()).unwrap();
    }

    fn read_json(&self, relative: &str) -> Value {
        serde_json::from_slice(&fs::read(self.path(relative)).unwrap()).unwrap()
    }

    /// The installed binary as a hook command shows it.
    fn binary(&self) -> String {
        shlex_quote(&self.path(".local/bin/tmux-workspace").to_string_lossy())
    }

    /// The backups of files named `name`, one per installation that made one.
    fn backups(&self, name: &str) -> Vec<PathBuf> {
        let dir = self.path(".local/state/tmux-workspace/backups");
        let mut found: Vec<PathBuf> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .map(|entry| entry.unwrap().path().join(name))
            .filter(|path| path.exists())
            .collect();
        found.sort();
        found
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Every hook command of an event in settings.
fn commands(settings: &Value, event: &str) -> Vec<String> {
    settings["hooks"][event]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["hooks"].as_array().unwrap())
        .map(|hook| hook["command"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn install_preserves_other_hooks_backs_up_and_is_idempotent() {
    let world = World::new();
    world.mkdir(".codex");
    let existing = json!({"description": "my hooks", "hooks": {"Stop": [{"hooks": [
        {"type": "command", "command": "existing-hook", "timeout": 10}]}]}});
    let original = existing.to_string().into_bytes();
    fs::write(world.path(".codex/hooks.json"), &original).unwrap();

    world.install("Darwin");

    let installed = fs::read(world.path(".codex/hooks.json")).unwrap();
    let data: Value = serde_json::from_slice(&installed).unwrap();
    assert_eq!(data["hooks"]["Stop"][0], existing["hooks"]["Stop"][0]);
    assert_eq!(data["description"], "my hooks");
    assert_eq!(data["hooks"]["Stop"].as_array().unwrap().len(), 2);
    let backups = world.backups("hooks.json");
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), original);
    assert_eq!(mode(&world.path(".codex/hooks.json")), 0o600);
    assert_eq!(
        world
            .path("Library/Application Support/iTerm2/DynamicProfiles/tmux-workspace.json")
            .canonicalize()
            .unwrap(),
        root().join("integrations/iterm2-profile.json")
    );

    let again = world.install("Darwin");
    assert_eq!(again, Vec::<String>::new());
    assert_eq!(
        fs::read(world.path(".codex/hooks.json")).unwrap(),
        installed
    );
    assert_eq!(world.backups("hooks.json"), backups);
    assert!(!world.path(".codex/config.toml").exists());
}

#[test]
fn invalid_existing_hooks_are_not_overwritten() {
    let world = World::new();
    world.mkdir(".codex");
    fs::write(world.path(".codex/hooks.json"), "not json").unwrap();
    let error = install_on(&world.target("Linux")).unwrap_err();
    assert!(
        error.to_string().contains("hooks.json"),
        "the error names the file: {error}"
    );
    assert_eq!(
        fs::read_to_string(world.path(".codex/hooks.json")).unwrap(),
        "not json"
    );
}

#[test]
fn rendered_command_names_the_installed_binary() {
    let data = render(
        &root().join("integrations/codex-hooks.json"),
        Path::new("/h"),
    )
    .unwrap();
    let command = data["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        command.contains("/h/.local/bin/tmux-workspace agent-hook codex"),
        "{command}"
    );
    assert_eq!(
        command,
        r#"[ -n "$TMUX_PANE" ] && /h/.local/bin/tmux-workspace agent-hook codex; true"#
    );
    assert!(!data.to_string().contains("{{"));
    assert!(!data.to_string().contains("python"));
}

#[test]
fn templates_name_no_author_paths() {
    for name in ["codex-hooks.json", "claude-hooks.json"] {
        let text = fs::read_to_string(root().join("integrations").join(name)).unwrap();
        assert!(!text.contains("/Users/"), "{name}");
    }
}

#[test]
fn merge_replaces_old_agent_hook_entries_instead_of_duplicating() {
    let python = json!({"type": "command", "command": "/usr/bin/python3 /old/agent-hook.py codex", "timeout": 3});
    let installed_python = json!({"type": "command",
        "command": "/usr/bin/python3 /h/.local/lib/tmux-workspace/agent-hook.py claude", "timeout": 3});
    let before = json!({"type": "command", "command": "/h/.local/bin/tmux-workspace agent-hook codex", "timeout": 3});
    let mine = json!({"type": "command", "command": "existing-hook", "timeout": 10});
    let existing =
        json!({"hooks": {"Stop": [{"hooks": [mine, python, installed_python, before]}]}});
    let wanted = json!({"Stop": [{"hooks": [
        {"type": "command", "command": "/h/.local/bin/tmux-workspace agent-hook codex", "timeout": 3}]}]});
    let wanted = wanted.as_object().unwrap();

    let (data, changed) = merge_hooks(&existing, wanted);

    assert!(changed);
    assert_eq!(
        commands(&data, "Stop"),
        [
            "existing-hook",
            "/h/.local/bin/tmux-workspace agent-hook codex"
        ]
    );
    assert_eq!(merge_hooks(&data, wanted), (data, false));
}

#[test]
fn ours_names_only_this_projects_commands() {
    for command in [
        "/usr/bin/python3 /h/.local/lib/tmux-workspace/agent-hook.py claude",
        "/h/.local/bin/tmux-workspace agent-hook claude",
        r#"[ -n "$TMUX_PANE" ] && '/t/a b/.local/bin/tmux-workspace' agent-hook codex; true"#,
    ] {
        assert!(ours(command), "{command}");
    }
    for command in [
        "existing-hook",
        "my-agent-hook run",
        "tmux-workspace save",
        "other-tool agent-hook claude",
        "",
    ] {
        assert!(!ours(command), "{command}");
    }
}

fn claude_install(settings: Option<Value>) -> (World, Value, Vec<String>) {
    let world = World::new();
    world.mkdir(".claude");
    if let Some(settings) = settings {
        world.write_json(".claude/settings.json", &settings);
    }
    let actions = world.install("Linux");
    let data = world.read_json(".claude/settings.json");
    (world, data, actions)
}

#[test]
fn claude_hooks_and_status_line_are_registered() {
    let (world, data, _) = claude_install(Some(json!({"model": "opus"})));
    let binary = world.binary();
    assert_eq!(data["model"], "opus");
    assert_eq!(
        data["statusLine"]["command"],
        format!("{binary} agent-hook claude-status")
    );
    for event in [
        "SessionStart",
        "SessionEnd",
        "CwdChanged",
        "PermissionRequest",
        "StopFailure",
        "PreToolUse",
    ] {
        assert!(
            commands(&data, event)
                .iter()
                .any(|command| command.contains(&format!("{binary} agent-hook claude"))),
            "{event}"
        );
    }
    assert_eq!(
        data["hooks"]["PreToolUse"][0]["matcher"],
        "AskUserQuestion|ExitPlanMode"
    );
}

#[test]
fn existing_claude_status_line_is_kept() {
    let (_, data, actions) = claude_install(Some(
        json!({"statusLine": {"type": "command", "command": "mine"}}),
    ));
    assert_eq!(data["statusLine"]["command"], "mine");
    assert!(actions.contains(&"Claude statusLine kept: mine".to_string()));
}

#[test]
fn old_python_entries_and_status_line_are_replaced() {
    let (world, data, _) = claude_install(Some(json!({
        "statusLine": {"type": "command", "refreshInterval": 1,
            "command": "/usr/bin/python3 /h/.local/lib/tmux-workspace/agent-hook.py claude-status"},
        "hooks": {"SessionStart": [{"hooks": [
            {"type": "command", "command": "/usr/bin/python3 /h/.local/lib/tmux-workspace/agent-hook.py claude", "timeout": 3},
            {"type": "command", "command": "mine", "timeout": 5}]}]}
    })));
    let text = data.to_string();
    assert!(!text.contains("agent-hook.py"), "{text}");
    assert_eq!(
        data["statusLine"]["command"],
        format!("{} agent-hook claude-status", world.binary())
    );
    let commands = commands(&data, "SessionStart");
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0], "mine");
}

#[test]
fn linux_skips_iterm_and_absent_tools() {
    let world = World::new();
    let actions = world.install("Linux");
    assert_eq!(actions, Vec::<String>::new());
    assert!(!world.path("Library").exists());
    assert!(!world.path(".claude").exists());
    assert!(!world.path(".codex").exists());
    assert!(!world.path(".omp").exists());
    // The Python hook is gone, so there is nothing to link in ~/.local either.
    assert!(!world.path(".local").exists());
}

#[test]
fn omp_and_neovim_are_linked_when_present() {
    let world = World::new();
    world.mkdir(".omp");
    world.mkdir(".config/nvim");
    let actions = world.install("Linux");
    assert_eq!(
        world
            .path(".omp/agent/extensions/tmux-workspace.ts")
            .canonicalize()
            .unwrap(),
        root().join("integrations/tmux-workspace.ts")
    );
    assert_eq!(
        world
            .path(".config/nvim/lua/custom/tmux_workspace.lua")
            .canonicalize()
            .unwrap(),
        root().join("integrations/tmux_workspace.lua")
    );
    assert!(
        actions.contains(
            &"Neovim: add require('custom.tmux_workspace').setup() to init.lua".to_string()
        ),
        "{actions:?}"
    );
}

#[test]
fn codex_home_is_respected() {
    let world = World::new();
    world.mkdir("custom-codex");
    let mut target = world.target("Linux");
    target.codex_home = Some(world.path("custom-codex"));
    let actions = install_on(&target).unwrap();
    let text = fs::read_to_string(world.path("custom-codex/hooks.json")).unwrap();
    assert!(text.contains("tmux-workspace agent-hook codex"), "{text}");
    assert!(!world.path(".codex").exists());
    assert!(
        actions
            .iter()
            .any(|a| a == "In Codex, review the tmux-workspace agent-hook entries with /hooks, then restart running CLIs."),
        "{actions:?}"
    );
}

#[test]
fn symlinked_settings_stay_symlinked() {
    let world = World::new();
    world.mkdir("dotfiles");
    world.mkdir(".claude");
    let target = world.path("dotfiles/claude-settings.json");
    fs::write(&target, r#"{"model": "opus"}"#).unwrap();
    symlink(&target, world.path(".claude/settings.json")).unwrap();

    world.install("Linux");

    assert!(
        fs::symlink_metadata(world.path(".claude/settings.json"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .contains("tmux-workspace agent-hook")
    );
}

#[test]
fn uninstall_removes_only_what_install_added() {
    let world = World::new();
    for name in [".claude", ".codex", ".omp", ".config/nvim"] {
        world.mkdir(name);
    }
    let mine = json!({"type": "command", "command": "existing-hook", "timeout": 10});
    world.write_json(
        ".codex/hooks.json",
        &json!({"hooks": {"Stop": [{"hooks": [mine]}]}}),
    );
    world.write_json(".claude/settings.json", &json!({"model": "opus"}));

    world.install("Darwin");
    let actions = world.uninstall();

    let codex = world.read_json(".codex/hooks.json");
    assert_eq!(codex["hooks"], json!({"Stop": [{"hooks": [mine]}]}));
    let claude = world.read_json(".claude/settings.json");
    assert_eq!(claude, json!({"model": "opus", "hooks": {}}));
    for link in [
        ".omp/agent/extensions/tmux-workspace.ts",
        ".config/nvim/lua/custom/tmux_workspace.lua",
        "Library/Application Support/iTerm2/DynamicProfiles/tmux-workspace.json",
    ] {
        assert!(fs::symlink_metadata(world.path(link)).is_err(), "{link}");
    }
    assert_eq!(actions.len(), 5, "{actions:?}");
}

#[test]
fn install_removes_the_old_python_hook_link() {
    let world = World::new();
    world.mkdir(".local/lib/tmux-workspace");
    let link = world.path(".local/lib/tmux-workspace/agent-hook.py");
    // The clone it led to is gone.
    symlink("/old/clone/integrations/agent-hook.py", &link).unwrap();

    let actions = world.install("Linux");

    assert!(fs::symlink_metadata(&link).is_err(), "the link is removed");
    assert_eq!(actions, [format!("link {} removed", link.display())]);
}

#[test]
fn install_keeps_a_hook_link_that_is_not_the_old_one() {
    let world = World::new();
    world.mkdir(".local/lib/tmux-workspace");
    let link = world.path(".local/lib/tmux-workspace/agent-hook.py");
    symlink("/somewhere/else/hook.py", &link).unwrap();
    let file = world.path(".local/lib/tmux-workspace/notes.txt");
    fs::write(&file, "mine").unwrap();

    let actions = world.install("Linux");

    assert!(fs::symlink_metadata(&link).is_ok());
    assert!(file.exists());
    assert_eq!(actions, Vec::<String>::new());
}

#[test]
fn home_with_a_space_is_quoted_in_hook_commands() {
    let data = render(
        &root().join("integrations/claude-hooks.json"),
        Path::new("/t/a b"),
    )
    .unwrap();
    let command = data["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert_eq!(
        command,
        "'/t/a b/.local/bin/tmux-workspace' agent-hook claude"
    );
    assert!(ours(command));
    assert_eq!(
        data["statusLine"]["command"],
        "'/t/a b/.local/bin/tmux-workspace' agent-hook claude-status"
    );

    // Installed for real, in such a home.
    let world = World::with_a_space();
    world.mkdir(".claude");
    world.install("Linux");
    let settings = world.read_json(".claude/settings.json");
    let installed = commands(&settings, "SessionStart")[0].clone();
    assert!(
        installed.starts_with(&format!(
            "'{}/a b/.local/bin/tmux-workspace' agent-hook claude",
            world.home.parent().unwrap().display()
        )),
        "{installed}"
    );
    assert!(ours(&installed));
}

#[test]
fn settings_with_an_unexpected_layout_are_refused_and_left_alone() {
    for layout in [
        r#"{"hooks": []}"#,
        r#"{"hooks": {"Stop": "x"}}"#,
        r#"{"hooks": {"Stop": ["x"]}}"#,
        r#"{"hooks": {"Stop": [{"hooks": {}}]}}"#,
        r#"{"hooks": {"Stop": [{"hooks": ["x"]}]}}"#,
        "[]",
    ] {
        let world = World::new();
        world.mkdir(".claude");
        fs::write(world.path(".claude/settings.json"), layout).unwrap();
        let error = install_on(&world.target("Linux")).unwrap_err();
        assert!(
            error.to_string().contains("settings.json"),
            "{layout}: {error}"
        );
        assert_eq!(
            fs::read_to_string(world.path(".claude/settings.json")).unwrap(),
            layout
        );
        assert!(world.backups("settings.json").is_empty(), "{layout}");
    }
}

#[test]
fn uninstall_removes_the_old_hook_link_and_dangling_links_into_the_share() {
    let world = World::new();
    world.mkdir(".local/lib/tmux-workspace");
    world.mkdir(".omp/agent/extensions");
    symlink(
        "/old/clone/integrations/agent-hook.py",
        world.path(".local/lib/tmux-workspace/agent-hook.py"),
    )
    .unwrap();
    // A link into this share whose file is gone.
    symlink(
        root().join("integrations/gone.ts"),
        world.path(".omp/agent/extensions/tmux-workspace.ts"),
    )
    .unwrap();

    let actions = world.uninstall();

    assert_eq!(actions.len(), 2, "{actions:?}");
    assert!(fs::symlink_metadata(world.path(".local/lib/tmux-workspace/agent-hook.py")).is_err());
    assert!(fs::symlink_metadata(world.path(".omp/agent/extensions/tmux-workspace.ts")).is_err());
}

#[test]
fn uninstall_leaves_links_that_lead_elsewhere() {
    let world = World::new();
    world.mkdir(".omp/agent/extensions");
    world.mkdir(".config/nvim/lua/custom");
    let mine = world.path("mine.ts");
    fs::write(&mine, "mine").unwrap();
    symlink(&mine, world.path(".omp/agent/extensions/tmux-workspace.ts")).unwrap();
    // A regular file of that name is not ours either.
    fs::write(
        world.path(".config/nvim/lua/custom/tmux_workspace.lua"),
        "mine",
    )
    .unwrap();

    let actions = world.uninstall();

    assert_eq!(actions, Vec::<String>::new());
    assert!(fs::symlink_metadata(world.path(".omp/agent/extensions/tmux-workspace.ts")).is_ok());
    assert!(
        world
            .path(".config/nvim/lua/custom/tmux_workspace.lua")
            .is_file()
    );
}

#[test]
fn a_regular_file_in_the_way_of_a_link_is_backed_up() {
    let world = World::new();
    world.mkdir(".omp/agent/extensions");
    let file = world.path(".omp/agent/extensions/tmux-workspace.ts");
    fs::write(&file, "my own extension").unwrap();

    let actions = world.install("Linux");

    assert_eq!(
        file.canonicalize().unwrap(),
        root().join("integrations/tmux-workspace.ts")
    );
    let backups = world.backups("tmux-workspace.ts");
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read_to_string(&backups[0]).unwrap(), "my own extension");
    assert_eq!(mode(&backups[0]), 0o600);
    assert!(
        actions.last().unwrap().starts_with("backups in "),
        "{actions:?}"
    );
}

#[test]
fn the_omp_extension_runs_the_installed_binary() {
    let text = fs::read_to_string(root().join("integrations/tmux-workspace.ts")).unwrap();
    assert!(text.contains(r#"join(homedir(), ".local/bin/tmux-workspace")"#));
    assert!(text.contains(r#"spawnSync(helper, ["agent-hook", "omp"]"#));
    assert!(!text.contains("python"));
    assert!(!text.contains("agent-hook.py"));
}

/// The binary, in a world of its own: nothing of the caller's environment, and `TMUX_WORKSPACE_SHARE`
/// at this checkout.
fn run(world: &World, args: &[&str], codex_home: Option<&Path>) -> std::process::Output {
    let mut command = Command::new(EXE);
    command
        .env_clear()
        .env("HOME", &world.home)
        .env("PATH", "/usr/bin:/bin")
        .env("TMUX_WORKSPACE_SHARE", root())
        .args(args);
    if let Some(codex) = codex_home {
        command.env("CODEX_HOME", codex);
    }
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn the_command_prints_the_actions_and_then_says_there_is_nothing_to_do() {
    let world = World::new();
    world.mkdir(".claude");
    world.mkdir(".omp");

    let first = run(&world, &["integrations", "install"], None);
    assert!(first.status.success(), "{}", text(&first.stderr));
    let out = text(&first.stdout);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        format!(
            "Claude hooks installed in {}",
            world.path(".claude/settings.json").display()
        )
    );
    // The omp extension is linked; on macOS the iTerm2 profile is too.
    assert!(
        lines[1].starts_with("link ") && lines[1].contains("tmux-workspace.ts"),
        "{out}"
    );
    assert!(lines.len() == 2 || lines.len() == 4, "{out}");
    assert_eq!(mode(&world.path(".claude/settings.json")), 0o600);
    // The command runs with umask 077: what it makes in the home is private.
    assert_eq!(mode(&world.path(".omp/agent")), 0o700);

    let second = run(&world, &["integrations", "install"], None);
    assert!(second.status.success());
    assert_eq!(text(&second.stdout), "Integrations already installed.\n");
}

#[test]
fn the_command_uninstalls_and_prints_the_actions() {
    let world = World::new();
    world.mkdir(".claude");
    run(&world, &["integrations", "install"], None);

    let out = run(&world, &["integrations", "uninstall"], None);

    assert!(out.status.success(), "{}", text(&out.stderr));
    let printed = text(&out.stdout);
    let mut lines = printed.lines();
    assert_eq!(
        lines.next().unwrap(),
        format!(
            "Claude hooks removed from {}",
            world.path(".claude/settings.json").display()
        )
    );
    // On macOS, the iTerm2 profile link install made goes too.
    assert!(lines.all(|line| line.starts_with("link ")), "{printed}");
    assert_eq!(
        world.read_json(".claude/settings.json"),
        json!({"hooks": {}})
    );
    let again = run(&world, &["integrations", "uninstall"], None);
    assert_eq!(text(&again.stdout), "");
}

#[test]
fn the_command_reads_codex_home() {
    let world = World::new();
    world.mkdir("elsewhere");
    // Without CODEX_HOME this home has no Codex at all.
    let out = run(
        &world,
        &["integrations", "install"],
        Some(&world.path("elsewhere")),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(world.path("elsewhere/hooks.json").is_file());
    assert!(!world.path(".codex").exists());
    assert!(text(&out.stdout).contains("In Codex, review the tmux-workspace agent-hook entries"));
}

#[test]
fn the_command_needs_install_or_uninstall() {
    let world = World::new();
    for args in [vec!["integrations"], vec!["integrations", "sideways"]] {
        let out = run(&world, &args, None);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            text(&out.stderr),
            "Workspace error: Usage: tmux-workspace integrations install|uninstall\n"
        );
        // Outside tmux the error contract's message goes to stdout.
        assert_eq!(
            text(&out.stdout),
            "Workspace: Usage: tmux-workspace integrations install|uninstall\n"
        );
    }
}
