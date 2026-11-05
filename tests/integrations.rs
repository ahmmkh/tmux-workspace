//! `integrations install|uninstall`, ported from `tests/test_integrations.py::Install`, and the
//! command that wraps it. Every test works in a temporary `HOME`; none reads or writes the real
//! `~/.claude`, `~/.codex`, `~/.omp`, `~/.config/nvim` or `~/Library`.

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use tmux_workspace::integrations::{
    Target, install_on, installed_on, merge_hooks, ours, render, uninstall_on,
};
use tmux_workspace::testing::{ScratchTmux, TempDir};
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
    let python = json!({"type": "command",
        "command": "/usr/bin/python3 /old/clone/tmux-workspace/integrations/agent-hook.py codex", "timeout": 3});
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
        // Every form this project ever registered.
        "/usr/bin/python3 /h/.local/lib/tmux-workspace/agent-hook.py claude",
        "/usr/bin/python3 /Users/u/.local/lib/tmux-workspace/agent-hook.py claude-status",
        "/usr/bin/python3 /Users/u/Projects/tmux-workspace/integrations/agent-hook.py claude",
        r#"[ -n "$TMUX_PANE" ] && /usr/bin/env python3 /h/.local/lib/tmux-workspace/agent-hook.py codex; true"#,
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
        // A user's own hook that happens to be called agent-hook.py.
        "python3 /home/u/bin/my-agent-hook.py stop",
        "python3 /home/u/bin/agent-hook.py stop",
        "/usr/bin/python3 /home/u/bin/agent-hook.py claude",
        "agent-hook.py",
        "",
    ] {
        assert!(!ours(command), "{command}");
    }
}

#[test]
fn a_users_own_agent_hook_py_survives_install_and_uninstall() {
    let mine = json!({"type": "command", "command": "python3 /home/u/bin/my-agent-hook.py stop"});
    let theirs =
        json!({"type": "command", "command": "/usr/bin/python3 /home/u/bin/agent-hook.py claude"});
    let (world, data, _) = claude_install(Some(json!({"hooks": {"Stop": [
        {"hooks": [mine, theirs]}]}})));
    assert_eq!(
        commands(&data, "Stop"),
        [
            "python3 /home/u/bin/my-agent-hook.py stop",
            "/usr/bin/python3 /home/u/bin/agent-hook.py claude"
        ]
    );
    world.uninstall();
    let after = world.read_json(".claude/settings.json");
    assert_eq!(
        commands(&after, "Stop"),
        [
            "python3 /home/u/bin/my-agent-hook.py stop",
            "/usr/bin/python3 /home/u/bin/agent-hook.py claude"
        ]
    );
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

/// A link of the Python version, whose clone is gone.
fn old_link(world: &World) -> PathBuf {
    world.mkdir(".local/lib/tmux-workspace");
    let link = world.path(".local/lib/tmux-workspace/agent-hook.py");
    symlink("/old/clone/integrations/agent-hook.py", &link).unwrap();
    link
}

/// Settings of the Python version: a hook and a status line that run the helper through that link.
fn old_settings(world: &World) -> Value {
    let helper = world.path(".local/lib/tmux-workspace/agent-hook.py");
    json!({"statusLine": {"type": "command", "refreshInterval": 1,
            "command": format!("/usr/bin/python3 {} claude-status", helper.display())},
        "model": "opus",
        "hooks": {"SessionStart": [{"hooks": [{"type": "command", "timeout": 3,
            "command": format!("/usr/bin/python3 {} claude", helper.display())}]}]}})
}

#[test]
fn a_failing_codex_step_keeps_the_old_hook_link() {
    let world = World::new();
    let link = old_link(&world);
    world.mkdir(".codex");
    fs::write(world.path(".codex/hooks.json"), "not json").unwrap();
    world.mkdir(".claude");
    let old = old_settings(&world);
    world.write_json(".claude/settings.json", &old);

    let error = install_on(&world.target("Linux")).unwrap_err();

    assert!(error.to_string().contains("hooks.json"), "{error}");
    assert!(
        fs::symlink_metadata(&link).is_ok(),
        "the Claude hooks still run the helper through this link"
    );
    assert_eq!(world.read_json(".claude/settings.json"), old);
}

#[test]
fn a_failing_claude_step_keeps_the_old_hook_link() {
    let world = World::new();
    let link = old_link(&world);
    world.mkdir(".claude");
    let mut broken = old_settings(&world);
    broken["hooks"]["Stop"] = json!("not a list");
    world.write_json(".claude/settings.json", &broken);

    assert!(install_on(&world.target("Linux")).is_err());

    assert!(fs::symlink_metadata(&link).is_ok());
}

#[test]
fn a_clean_run_replaces_the_old_entries_and_then_removes_the_old_hook_link() {
    let world = World::new();
    let link = old_link(&world);
    world.mkdir(".claude");
    let old = old_settings(&world);
    world.write_json(".claude/settings.json", &old);

    let actions = world.install("Linux");

    assert!(fs::symlink_metadata(&link).is_err(), "{actions:?}");
    let settings = world.read_json(".claude/settings.json");
    assert!(!settings.to_string().contains("agent-hook.py"));
    // The link goes once nothing runs through it, so it is told after the hooks.
    let hooks = actions
        .iter()
        .position(|a| a.starts_with("Claude hooks installed"))
        .unwrap();
    let removed = actions
        .iter()
        .position(|a| a == &format!("link {} removed", link.display()))
        .unwrap();
    assert!(hooks < removed, "{actions:?}");
}

#[test]
fn old_entries_that_install_did_not_reach_keep_the_old_hook_link() {
    // Codex is looked for in CODEX_HOME, but the old entries are in ~/.codex.
    let world = World::new();
    let link = old_link(&world);
    world.mkdir("custom-codex");
    world.mkdir(".codex");
    let helper = world.path(".local/lib/tmux-workspace/agent-hook.py");
    world.write_json(
        ".codex/hooks.json",
        &json!({"hooks": {"Stop": [{"hooks": [{"type": "command",
            "command": format!("/usr/bin/python3 {} codex", helper.display())}]}]}}),
    );
    let mut target = world.target("Linux");
    target.codex_home = Some(world.path("custom-codex"));

    let actions = install_on(&target).unwrap();

    assert!(fs::symlink_metadata(&link).is_ok());
    let kept = actions
        .iter()
        .find(|a| a.starts_with(&format!("link {} kept", link.display())))
        .unwrap();
    assert!(
        kept.contains(&world.path(".codex/hooks.json").display().to_string()),
        "{kept}"
    );
    // The next run, once the entries are gone, removes it.
    fs::remove_file(world.path(".codex/hooks.json")).unwrap();
    let actions = install_on(&target).unwrap();
    assert!(
        actions.contains(&format!("link {} removed", link.display())),
        "{actions:?}"
    );
}

#[test]
fn uninstall_keeps_the_key_order_of_the_settings() {
    let world = World::new();
    world.mkdir(".claude");
    // The status line comes first, so removing it must not move the last key into its place.
    world.write_json(
        ".claude/settings.json",
        &json!({"statusLine": {"type": "command",
                "command": format!("{} agent-hook claude-status", world.binary())},
            "model": "opus", "env": {"A": "1"}, "theme": "dark", "hooks": {}, "effort": "high"}),
    );

    let actions = world.uninstall();

    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_eq!(
        fs::read_to_string(world.path(".claude/settings.json")).unwrap(),
        "{\n  \"model\": \"opus\",\n  \"env\": {\n    \"A\": \"1\"\n  },\n  \"theme\": \"dark\",\n  \"hooks\": {},\n  \"effort\": \"high\"\n}\n"
    );
}

#[test]
fn install_keeps_the_key_order_of_the_settings() {
    let (world, _, _) = claude_install(Some(json!({
        "statusLine": {"type": "command", "command": "/h/.local/lib/tmux-workspace/agent-hook.py claude-status"},
        "model": "opus", "hooks": {}, "effort": "high"
    })));
    let settings = world.read_json(".claude/settings.json");
    let keys: Vec<&String> = settings.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["statusLine", "model", "hooks", "effort"]);
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
fn the_command_needs_install_uninstall_or_status() {
    let world = World::new();
    for args in [vec!["integrations"], vec!["integrations", "sideways"]] {
        let out = run(&world, &args, None);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            text(&out.stderr),
            "Workspace error: Usage: tmux-workspace integrations install|uninstall|status\n"
        );
        // Outside tmux the error contract's message goes to stdout.
        assert_eq!(
            text(&out.stdout),
            "Workspace: Usage: tmux-workspace integrations install|uninstall|status\n"
        );
    }
}

/// `install.sh --update` registers the integrations again only where they were: this is how it asks.
#[test]
fn installed_on_sees_this_projects_hook_in_claude_or_codex() {
    let world = World::new();
    let target = world.target("Linux");
    assert!(!installed_on(&target), "nothing is installed");
    world.mkdir(".claude");
    let own = json!({"type": "command", "command": "my-agent-hook run"});
    world.write_json(
        ".claude/settings.json",
        &json!({"hooks": {"Stop": [{"hooks": [own]}]}}),
    );
    assert!(!installed_on(&target), "a user's own hook is not ours");
    world.install("Linux");
    assert!(installed_on(&target));
    world.uninstall();
    assert!(!installed_on(&target));
    // Codex alone counts, and so does the form before the Rust rewrite.
    world.mkdir(".codex");
    let old = "/usr/bin/python3 /h/.local/lib/tmux-workspace/agent-hook.py codex";
    world.write_json(
        ".codex/hooks.json",
        &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": old}]}]}}),
    );
    assert!(installed_on(&target));
}

#[test]
fn status_prints_whether_the_integrations_are_installed() {
    let scratch = ScratchTmux::new();
    let status = || {
        let out = scratch
            .bin(EXE)
            .args(["integrations", "status"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(out.stderr, b"");
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(status(), "not installed\n");
    let claude = scratch.home().join(".claude");
    fs::create_dir_all(&claude).unwrap();
    let ours = json!({"statusLine": {"type": "command",
        "command": "/h/.local/bin/tmux-workspace agent-hook claude-status"}});
    fs::write(claude.join("settings.json"), ours.to_string()).unwrap();
    assert_eq!(status(), "installed\n");
}

/// A release install in `world`, as install.sh lays it out: a folder per version under
/// `.local/share/tmux-workspace`, each with this checkout's integration files, and `current` leading
/// to `current`. Gives that folder.
fn release_home(world: &World, versions: &[&str], current: &str) -> PathBuf {
    let home = world.path(".local/share/tmux-workspace");
    for version in versions {
        let dir = home.join(version).join("integrations");
        fs::create_dir_all(&dir).unwrap();
        for entry in fs::read_dir(root().join("integrations")).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
            }
        }
    }
    move_current(&home, current);
    home
}

/// What an update does to a release install: `current` leads to `version` now.
fn move_current(home: &Path, version: &str) {
    let _ = fs::remove_file(home.join("current"));
    symlink(version, home.join("current")).unwrap();
}

/// `integrations ARGS` with `TMUX_WORKSPACE_SHARE` at `share`.
fn run_in(world: &World, share: &Path, args: &[&str]) -> std::process::Output {
    Command::new(EXE)
        .env_clear()
        .env("HOME", &world.home)
        .env("PATH", "/usr/bin:/bin")
        .env("TMUX_WORKSPACE_SHARE", share)
        .args(args)
        .output()
        .unwrap()
}

const LINKS: [(&str, &str); 3] = [
    (
        ".config/nvim/lua/custom/tmux_workspace.lua",
        "tmux_workspace.lua",
    ),
    (
        ".omp/agent/extensions/tmux-workspace.ts",
        "tmux-workspace.ts",
    ),
    (
        "Library/Application Support/iTerm2/DynamicProfiles/tmux-workspace.json",
        "iterm2-profile.json",
    ),
];

/// install.sh runs it with the share at `<home>/current`, and run without it the binary finds its own
/// version folder: either way the links go through `current`, so they outlive the updates that prune
/// the version they were made in.
#[test]
fn a_release_install_links_through_current_and_the_links_outlive_updates() {
    for given in ["current", "2.0.0"] {
        let world = World::new();
        world.mkdir(".config/nvim");
        world.mkdir(".omp");
        let home = release_home(&world, &["2.0.0"], "2.0.0");

        let out = run_in(&world, &home.join(given), &["integrations", "install"]);

        assert!(out.status.success(), "{}", text(&out.stderr));
        let linked: Vec<_> = if cfg!(target_os = "macos") {
            &LINKS[..]
        } else {
            &LINKS[..2]
        }
        .to_vec();
        for (link, name) in &linked {
            assert_eq!(
                fs::read_link(world.path(link)).unwrap(),
                home.join("current/integrations").join(name),
                "{given}: {link}"
            );
        }
        release_home(&world, &["2.0.1", "2.0.2"], "2.0.2");
        fs::remove_dir_all(home.join("2.0.0")).unwrap();
        fs::remove_dir_all(home.join("2.0.1")).unwrap();
        for (link, _) in &linked {
            assert!(
                world.path(link).is_file(),
                "{given}: {link} after two updates"
            );
        }
        // The binary of the new version finds its own folder.
        let now = if given == "current" { given } else { "2.0.2" };
        let again = run_in(&world, &home.join(now), &["integrations", "install"]);
        assert_eq!(text(&again.stdout), "Integrations already installed.\n");
    }
}

/// An update registers the integrations again when any of them is installed: the links count, not
/// only the hooks of Claude and Codex.
#[test]
fn installed_on_counts_the_links_of_omp_neovim_and_iterm() {
    for (dir, system) in [
        (".config/nvim", "Linux"),
        (".omp", "Linux"),
        ("Library", "Darwin"),
    ] {
        let world = World::new();
        let target = world.target(system);
        world.mkdir(dir);
        assert!(!installed_on(&target), "{dir}: nothing is installed");
        world.install(system);
        assert!(installed_on(&target), "{dir}");
        uninstall_on(&target).unwrap();
        assert!(!installed_on(&target), "{dir}: uninstalled");
    }
    // A link of the user's own at one of those paths is not ours.
    let world = World::new();
    world.mkdir(".config/nvim/lua/custom");
    fs::write(world.path("mine.lua"), "mine").unwrap();
    symlink(world.path("mine.lua"), world.path(LINKS[0].0)).unwrap();
    assert!(!installed_on(&world.target("Linux")));
}

/// A link made before the version it names was pruned, or through `current`, is ours to report and to
/// remove; a link into another install's folder, or into a folder of the install home that is not a
/// version, is not.
#[test]
fn links_into_any_version_of_the_release_home_are_ours() {
    let world = World::new();
    let home = release_home(&world, &["2.0.2"], "2.0.2");
    let other = world.path("elsewhere");
    for dir in [
        ".config/nvim/lua/custom",
        ".omp/agent/extensions",
        "Library/Application Support/iTerm2/DynamicProfiles",
    ] {
        world.mkdir(dir);
    }
    let target = Target {
        home: world.home.clone(),
        share: home.join("current"),
        codex_home: None,
        system: "Darwin".into(),
    };
    // Pruned: 2.0.0 is gone.
    symlink(
        home.join("2.0.0/integrations/tmux_workspace.lua"),
        world.path(LINKS[0].0),
    )
    .unwrap();
    assert!(installed_on(&target), "a link into a pruned version");
    symlink(
        other.join("2.0.0/integrations/tmux-workspace.ts"),
        world.path(LINKS[1].0),
    )
    .unwrap();
    world.mkdir(".local/share/tmux-workspace/notes");
    symlink(
        home.join("notes/iterm2-profile.json"),
        world.path(LINKS[2].0),
    )
    .unwrap();

    let actions = uninstall_on(&target).unwrap();

    assert_eq!(
        actions,
        [format!("link {}", world.path(LINKS[0].0).display())]
    );
    assert!(fs::symlink_metadata(world.path(LINKS[1].0)).is_ok());
    assert!(fs::symlink_metadata(world.path(LINKS[2].0)).is_ok());
    assert!(!installed_on(&target));

    // Through `current`, as install now makes them.
    fs::remove_file(world.path(LINKS[1].0)).unwrap();
    symlink(
        home.join("current/integrations/tmux-workspace.ts"),
        world.path(LINKS[1].0),
    )
    .unwrap();
    assert!(installed_on(&target));
    assert_eq!(
        uninstall_on(&target).unwrap(),
        [format!("link {}", world.path(LINKS[1].0).display())]
    );
}
