//! Where tmux-workspace finds its files and its config.
//!
//! The free functions read the real environment. Each one is a thin wrapper over a method of
//! [`Locations`] or over a pure helper; those take their inputs as arguments, so tests never touch
//! the environment.

use crate::error::{Error, Result};
use crate::pyjson::{Style, dumps};
use serde_json::{Map, Value};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fmt::Display;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

/// tmux 3.2 lacks display-popup -B/-e/-T, popup-border-* and the client-focus-in hook.
/// 3.3 and 3.4 work; tmux.conf sets their missing cosmetic options with -q.
pub const TMUX_MIN: (u32, u32) = (3, 3);

/// How far above the binary's directory `share_root` looks for `agent-status.conf`.
const SHARE_LEVELS: usize = 3;

/// Everything the paths depend on, so a test can supply it instead of the real environment.
#[derive(Clone, Debug)]
pub struct Locations {
    /// `$HOME`.
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME`, when it is set and not empty.
    pub xdg_config_home: Option<PathBuf>,
    /// `$TMUX_WORKSPACE_PROJECTS`, when it is set and not empty.
    pub projects: Option<String>,
    /// `$TMUX_WORKSPACE_STATE_DIR`, when it is set.
    pub state_dir: Option<String>,
    /// The share root, or `None` to work it out from the binary when it is needed.
    pub share: Option<PathBuf>,
}

impl Locations {
    /// The real environment.
    pub fn current() -> Locations {
        let text = |name: &str| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned());
        Locations {
            home: home(),
            xdg_config_home: non_empty(std::env::var_os("XDG_CONFIG_HOME")).map(PathBuf::from),
            projects: text("TMUX_WORKSPACE_PROJECTS").filter(|v| !v.is_empty()),
            state_dir: text("TMUX_WORKSPACE_STATE_DIR"),
            share: None,
        }
    }

    /// `Path(text).expanduser()`: a leading `~` or `~user` becomes that home folder, and
    /// repeated or trailing slashes go.
    pub fn expand_user(&self, text: &str) -> PathBuf {
        let path = normalize(Path::new(text));
        let mut parts = path.components();
        let Some(Component::Normal(first)) = parts.next() else {
            return path;
        };
        let first = first.as_bytes();
        if first.first() != Some(&b'~') {
            return path;
        }
        let base = if first.len() == 1 {
            self.home.clone()
        } else {
            match user_home(OsStr::from_bytes(&first[1..])) {
                Some(home) => home,
                None => return path,
            }
        };
        normalize(&base.join(parts.as_path()))
    }

    /// Where state lives: `$TMUX_WORKSPACE_STATE_DIR` or `~/.local/state/tmux-workspace`.
    pub fn state_dir(&self) -> PathBuf {
        match &self.state_dir {
            Some(dir) => self.expand_user(dir),
            None => self.home.join(".local/state/tmux-workspace"),
        }
    }

    /// The folder that holds `config.json` and `config.example.json` next to the shipped files.
    pub fn share_root(&self) -> PathBuf {
        self.share.clone().unwrap_or_else(share_root)
    }

    pub fn config_dir(&self) -> PathBuf {
        let base = self
            .xdg_config_home
            .clone()
            .unwrap_or_else(|| self.home.join(".config"));
        base.join("tmux-workspace")
    }

    pub fn user_config_path(&self) -> PathBuf {
        self.config_dir().join("config.json")
    }

    /// The user's config, then the legacy config in the share root, then the shipped example.
    pub fn config_path(&self) -> PathBuf {
        let user = self.user_config_path();
        if user.is_file() {
            return user;
        }
        let share = self.share_root();
        let legacy = share.join("config.json");
        if legacy.is_file() {
            legacy
        } else {
            share.join("config.example.json")
        }
    }

    pub fn load_config(&self) -> Result<Map<String, Value>> {
        let path = self.config_path();
        let text = fs::read_to_string(&path).map_err(|e| cannot_read(&path, e))?;
        match serde_json::from_str(&text) {
            Ok(Value::Object(config)) => Ok(config),
            Ok(_) => Err(cannot_read(&path, "the top level must be an object")),
            Err(e) => Err(cannot_read(&path, e)),
        }
    }

    /// True until the user has chosen a projects folder. A broken config is reported, never replaced.
    pub fn needs_onboarding(&self) -> bool {
        match fs::read_to_string(self.user_config_path()) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(config)) => !config.contains_key("projects_dir"),
                _ => false,
            },
            Err(e) => e.kind() == io::ErrorKind::NotFound,
        }
    }

    /// Merge updates into the user's config, starting from the legacy or example config the first time.
    pub fn save_config(&self, updates: Map<String, Value>) -> Result<PathBuf> {
        let mut data = self.load_config()?;
        for (key, value) in updates {
            data.insert(key, value);
        }
        let path = self.user_config_path();
        let dir = path.parent().unwrap_or(Path::new("."));
        make_private_dir(dir).map_err(|e| cannot_write(dir, e))?;
        let temporary = dir.join(format!(".config.json.{}", std::process::id()));
        let text = dumps(&Value::Object(data), Style::Indent2, false) + "\n";
        write_private(&temporary, &text)
            .and_then(|()| fs::rename(&temporary, &path))
            .map_err(|e| {
                let _ = fs::remove_file(&temporary);
                cannot_write(&path, e)
            })?;
        Ok(path)
    }

    /// The folder that holds one directory per project. A broken config never blocks a workspace.
    pub fn projects_dir(&self) -> PathBuf {
        if let Some(over) = &self.projects {
            return self.expand_user(over);
        }
        if let Ok(config) = self.load_config()
            && let Some(Value::String(configured)) = config.get("projects_dir")
            && !configured.trim().is_empty()
        {
            return self.expand_user(configured.trim());
        }
        self.home.join("Projects")
    }
}

/// `Path::components` joined again: no repeated, trailing or inner `.` parts.
pub fn normalize(path: &Path) -> PathBuf {
    path.components().collect()
}

/// `$HOME`, else the password database. `/` when neither answers.
pub fn home() -> PathBuf {
    home_from(std::env::var_os("HOME"))
}

/// Python's `expanduser("~")`: a set `HOME` wins even when empty, trailing slashes go.
fn home_from(env: Option<OsString>) -> PathBuf {
    let Some(value) = env else {
        // SAFETY: `getuid` has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        return passwd_home(Lookup::Uid(uid)).unwrap_or_else(|| "/".into());
    };
    let mut bytes = value.into_vec();
    while bytes.last() == Some(&b'/') {
        bytes.pop();
    }
    if bytes.is_empty() {
        return PathBuf::from("/");
    }
    PathBuf::from(OsString::from_vec(bytes))
}

pub fn expand_user(text: &str) -> PathBuf {
    Locations::current().expand_user(text)
}

pub fn state_dir() -> PathBuf {
    Locations::current().state_dir()
}

/// Where the shipped files are. `TMUX_WORKSPACE_SHARE`, else the nearest folder above the running
/// binary that holds `agent-status.conf` (a checkout's root from `target/debug`, a release's root
/// from `bin`), else the binary's own folder.
pub fn share_root() -> PathBuf {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    share_root_from(std::env::var_os("TMUX_WORKSPACE_SHARE"), exe)
}

fn share_root_from(env: Option<OsString>, exe: Option<PathBuf>) -> PathBuf {
    if let Some(share) = non_empty(env) {
        return PathBuf::from(share);
    }
    let dir = exe
        .as_deref()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    dir.ancestors()
        .take(SHARE_LEVELS + 1)
        .find(|candidate| candidate.join("agent-status.conf").is_file())
        .map_or_else(|| dir.clone(), Path::to_path_buf)
}

pub fn config_dir() -> PathBuf {
    Locations::current().config_dir()
}

pub fn user_config_path() -> PathBuf {
    Locations::current().user_config_path()
}

pub fn config_path() -> PathBuf {
    Locations::current().config_path()
}

pub fn load_config() -> Result<Map<String, Value>> {
    Locations::current().load_config()
}

pub fn needs_onboarding() -> bool {
    Locations::current().needs_onboarding()
}

pub fn save_config(updates: Map<String, Value>) -> Result<PathBuf> {
    Locations::current().save_config(updates)
}

pub fn projects_dir() -> PathBuf {
    Locations::current().projects_dir()
}

/// How to run this program again from `run-shell` and fzf previews: the stable link in
/// `~/.local/bin` when it leads here, else the binary itself.
pub fn self_path() -> String {
    match std::env::current_exe().and_then(|exe| exe.canonicalize()) {
        Ok(exe) => self_path_for(&home(), &exe),
        Err(_) => "tmux-workspace".to_string(),
    }
}

fn self_path_for(home: &Path, exe: &Path) -> String {
    let link = home.join(".local/bin/tmux-workspace");
    let chosen = if link.canonicalize().is_ok_and(|real| real == exe) {
        link
    } else {
        exe.to_path_buf()
    };
    chosen.to_string_lossy().into_owned()
}

/// A value of an environment variable, unless it is unset or empty.
fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|v| !v.is_empty())
}

/// The error for a failed read: the path leads, because the bare OS text does not say where.
pub fn cannot_read(path: &Path, why: impl Display) -> Error {
    Error::Msg(format!("Cannot read {}: {why}", path.display()))
}

/// The error for a failed write: the path leads, as in "Cannot read", because the bare OS text
/// ("Permission denied (os error 13)") does not say where.
pub fn cannot_write(path: &Path, why: impl Display) -> Error {
    Error::Msg(format!("Cannot write {}: {why}", path.display()))
}

/// `mkdir(parents=True, exist_ok=True, mode=0o700)`: the folder is private, its parents are not
/// made so.
pub fn make_private_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        other => other,
    }
}

/// Write a file only its owner can read, even when an older file of that name was wider.
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

enum Lookup<'a> {
    Uid(libc::uid_t),
    Name(&'a CStr),
}

/// One text field of a user's password database entry (`pw_dir`, `pw_shell`). `None` when there is
/// no such user, or the field is missing or empty.
fn passwd_field(
    lookup: Lookup,
    field: fn(&libc::passwd) -> *const libc::c_char,
) -> Option<OsString> {
    let mut buffer = vec![0u8; 1024];
    loop {
        // SAFETY: an all-zero `passwd` is valid plain data, and `getpw*_r` fills it in. It writes
        // at most `buffer.len()` bytes into `buffer`, and `entry` points into it until the next call.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let status = unsafe {
            match lookup {
                Lookup::Uid(uid) => libc::getpwuid_r(
                    uid,
                    &mut entry,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut found,
                ),
                Lookup::Name(name) => libc::getpwnam_r(
                    name.as_ptr(),
                    &mut entry,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut found,
                ),
            }
        };
        if status == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        let text = field(&entry);
        if status != 0 || found.is_null() || text.is_null() {
            return None;
        }
        // SAFETY: the field is a NUL-terminated string inside `buffer`.
        let text = unsafe { CStr::from_ptr(text) };
        return (!text.to_bytes().is_empty())
            .then(|| OsStr::from_bytes(text.to_bytes()).to_owned());
    }
}

/// The home folder of a user from the password database.
fn passwd_home(lookup: Lookup) -> Option<PathBuf> {
    passwd_field(lookup, |entry| entry.pw_dir).map(PathBuf::from)
}

/// The login shell the password database gives this account: empty when there is none.
pub fn account_shell() -> String {
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    passwd_field(Lookup::Uid(uid), |entry| entry.pw_shell)
        .map(|shell| shell.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn user_home(name: &OsStr) -> Option<PathBuf> {
    let name = CString::new(name.as_bytes()).ok()?;
    passwd_home(Lookup::Name(&name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::{Style, dumps};
    use crate::testing::TempDir;
    use serde_json::json;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// A temporary home with a legacy clone (`repo`) that holds the shipped example config.
    struct World {
        tmp: TempDir,
        repo: PathBuf,
    }

    impl World {
        fn new() -> World {
            let tmp = TempDir::new();
            let repo = tmp.path().join("repo");
            fs::create_dir(&repo).unwrap();
            fs::write(
                repo.join("config.example.json"),
                r#"{"projects_dir": "~/Projects"}"#,
            )
            .unwrap();
            World { tmp, repo }
        }

        /// HOME and XDG_CONFIG_HOME inside the temporary home, REPO patched to `repo`.
        fn locations(&self) -> Locations {
            Locations {
                home: self.tmp.path().to_path_buf(),
                xdg_config_home: Some(self.tmp.path().join("xdg")),
                projects: None,
                state_dir: None,
                share: Some(self.repo.clone()),
            }
        }

        fn user_config(&self) -> PathBuf {
            self.tmp.path().join("xdg/tmux-workspace/config.json")
        }

        fn write(&self, path: PathBuf, value: &Value) -> PathBuf {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, dumps(value, Style::Default, false)).unwrap();
            path
        }
    }

    // tests/test_portability.py::Config

    #[test]
    fn xdg_config_wins_over_legacy_and_example() {
        let w = World::new();
        w.write(w.repo.join("config.json"), &json!({}));
        let xdg = w.write(w.user_config(), &json!({}));
        assert_eq!(w.locations().config_path(), xdg);
    }

    #[test]
    fn legacy_repo_config_beats_the_example() {
        let w = World::new();
        let legacy = w.write(w.repo.join("config.json"), &json!({}));
        assert_eq!(w.locations().config_path(), legacy);
    }

    #[test]
    fn example_is_the_last_resort() {
        let w = World::new();
        assert_eq!(
            w.locations().config_path().file_name().unwrap(),
            "config.example.json"
        );
    }

    #[test]
    fn config_dir_defaults_to_dot_config() {
        let w = World::new();
        let loc = Locations {
            xdg_config_home: None,
            ..w.locations()
        };
        assert_eq!(
            loc.config_dir(),
            w.tmp.path().join(".config").join("tmux-workspace")
        );
    }

    #[test]
    fn unreadable_config_names_the_file() {
        let w = World::new();
        let bad = w.user_config();
        fs::create_dir_all(bad.parent().unwrap()).unwrap();
        fs::write(&bad, "{not json").unwrap();
        let message = w.locations().load_config().unwrap_err().to_string();
        assert!(
            message.starts_with(&format!("Cannot read {}: ", bad.display())),
            "{message}"
        );
    }

    #[test]
    fn projects_dir_env_beats_config() {
        let w = World::new();
        w.write(w.user_config(), &json!({"projects_dir": "~/src"}));
        let loc = Locations {
            projects: Some(w.tmp.path().join("code").to_string_lossy().into_owned()),
            ..w.locations()
        };
        assert_eq!(loc.projects_dir(), w.tmp.path().join("code"));
    }

    #[test]
    fn projects_dir_expands_tilde_from_config() {
        let w = World::new();
        w.write(w.user_config(), &json!({"projects_dir": "~/src"}));
        assert_eq!(w.locations().projects_dir(), w.tmp.path().join("src"));
    }

    #[test]
    fn projects_dir_defaults_to_home_projects() {
        let w = World::new();
        assert_eq!(w.locations().projects_dir(), w.tmp.path().join("Projects"));
    }

    #[test]
    fn projects_dir_survives_a_broken_config() {
        let w = World::new();
        let bad = w.user_config();
        fs::create_dir_all(bad.parent().unwrap()).unwrap();
        fs::write(&bad, "{not json").unwrap();
        assert_eq!(w.locations().projects_dir(), w.tmp.path().join("Projects"));
    }

    // Beyond the Python cases.

    #[test]
    fn projects_dir_defaults_to_home_projects_when_no_config_names_one() {
        let w = World::new();
        fs::write(w.repo.join("config.example.json"), "{}").unwrap();
        assert_eq!(w.locations().projects_dir(), w.tmp.path().join("Projects"));
    }

    #[test]
    fn a_blank_or_non_text_projects_dir_means_the_default() {
        let w = World::new();
        for value in [json!("  "), json!(5), json!(null), json!(["x"])] {
            w.write(w.user_config(), &json!({"projects_dir": value}));
            assert_eq!(
                w.locations().projects_dir(),
                w.tmp.path().join("Projects"),
                "{value}"
            );
        }
    }

    #[test]
    fn a_projects_dir_setting_is_trimmed_before_it_is_expanded() {
        let w = World::new();
        w.write(w.user_config(), &json!({"projects_dir": "  ~/src \n"}));
        assert_eq!(w.locations().projects_dir(), w.tmp.path().join("src"));
    }

    #[test]
    fn the_top_level_of_a_config_must_be_an_object() {
        let w = World::new();
        w.write(w.user_config(), &json!(["x"]));
        let message = w.locations().load_config().unwrap_err().to_string();
        assert_eq!(
            message,
            format!(
                "Cannot read {}: the top level must be an object",
                w.user_config().display()
            )
        );
    }

    #[test]
    fn a_missing_config_names_the_example_it_could_not_read() {
        let w = World::new();
        fs::remove_file(w.repo.join("config.example.json")).unwrap();
        let message = w.locations().load_config().unwrap_err().to_string();
        assert!(
            message.starts_with(&format!(
                "Cannot read {}: ",
                w.repo.join("config.example.json").display()
            )),
            "{message}"
        );
    }

    #[test]
    fn load_config_keeps_the_key_order_of_the_file() {
        let w = World::new();
        w.write(
            w.user_config(),
            &serde_json::from_str(r#"{"z": 1, "a": 2, "m": 3}"#).unwrap(),
        );
        let keys: Vec<String> = w
            .locations()
            .load_config()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, ["z", "a", "m"]);
    }

    #[test]
    fn save_config_writes_python_json_and_a_newline() {
        let w = World::new();
        fs::write(w.repo.join("config.example.json"), r#"{"model": "é"}"#).unwrap();
        let mut updates = Map::new();
        updates.insert("projects_dir".into(), json!("~/code"));
        let path = w.locations().save_config(updates).unwrap();
        assert_eq!(path, w.user_config());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "{\n  \"model\": \"\\u00e9\",\n  \"projects_dir\": \"~/code\"\n}\n"
        );
    }

    #[test]
    fn save_config_replaces_a_key_in_place() {
        let w = World::new();
        w.write(
            w.user_config(),
            &serde_json::from_str(r#"{"a": 1, "projects_dir": "~/old", "b": 2}"#).unwrap(),
        );
        let mut updates = Map::new();
        updates.insert("projects_dir".into(), json!("~/new"));
        w.locations().save_config(updates).unwrap();
        assert_eq!(
            fs::read_to_string(w.user_config()).unwrap(),
            "{\n  \"a\": 1,\n  \"projects_dir\": \"~/new\",\n  \"b\": 2\n}\n"
        );
    }

    #[test]
    fn save_config_makes_a_private_folder_and_leaves_no_temporary_file() {
        let w = World::new();
        let mut updates = Map::new();
        updates.insert("projects_dir".into(), json!("~/code"));
        let path = w.locations().save_config(updates).unwrap();
        let dir = path.parent().unwrap();
        assert_eq!(
            fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from("config.json")]);
    }

    #[test]
    fn save_config_does_not_replace_a_broken_config() {
        let w = World::new();
        let bad = w.user_config();
        fs::create_dir_all(bad.parent().unwrap()).unwrap();
        fs::write(&bad, "{broken").unwrap();
        let mut updates = Map::new();
        updates.insert("projects_dir".into(), json!("~/code"));
        assert!(w.locations().save_config(updates).is_err());
        assert_eq!(fs::read_to_string(&bad).unwrap(), "{broken");
    }

    // Home and `~`.

    #[test]
    fn home_is_the_environment_value_without_trailing_slashes() {
        assert_eq!(
            home_from(Some("/home/me".into())),
            PathBuf::from("/home/me")
        );
        assert_eq!(
            home_from(Some("/home/me//".into())),
            PathBuf::from("/home/me")
        );
        assert_eq!(home_from(Some("/".into())), PathBuf::from("/"));
    }

    #[test]
    fn without_a_home_variable_the_password_database_answers() {
        let home = home_from(None);
        assert!(home.is_absolute(), "{}", home.display());
    }

    #[test]
    fn expand_user_replaces_a_leading_tilde() {
        let w = World::new();
        let loc = w.locations();
        let home = w.tmp.path();
        assert_eq!(loc.expand_user("~"), home);
        assert_eq!(loc.expand_user("~/code"), home.join("code"));
        assert_eq!(loc.expand_user("~/code/"), home.join("code"));
        assert_eq!(loc.expand_user("~//a///b"), home.join("a/b"));
        assert_eq!(loc.expand_user("/abs/~x"), PathBuf::from("/abs/~x"));
        assert_eq!(loc.expand_user("rel/dir"), PathBuf::from("rel/dir"));
    }

    #[test]
    fn expand_user_looks_other_users_up() {
        let w = World::new();
        let root = w.locations().expand_user("~root/x");
        assert!(
            root.is_absolute() && root.ends_with("x"),
            "{}",
            root.display()
        );
        assert_ne!(root, w.tmp.path().join("x"));
        assert_eq!(
            w.locations().expand_user("~no-such-user-here/x"),
            PathBuf::from("~no-such-user-here/x")
        );
    }

    #[test]
    fn state_dir_defaults_under_home_and_expands_the_override() {
        let w = World::new();
        assert_eq!(
            w.locations().state_dir(),
            w.tmp.path().join(".local/state/tmux-workspace")
        );
        let loc = Locations {
            state_dir: Some("~/state".into()),
            ..w.locations()
        };
        assert_eq!(loc.state_dir(), w.tmp.path().join("state"));
        let loc = Locations {
            state_dir: Some("/var/tw".into()),
            ..w.locations()
        };
        assert_eq!(loc.state_dir(), PathBuf::from("/var/tw"));
    }

    // The share root.

    /// `root/agent-status.conf` plus an empty executable at each given path under `root`.
    fn install(root: &Path, exes: &[&str]) {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("agent-status.conf"), "").unwrap();
        for exe in exes {
            let path = root.join(exe);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
    }

    #[test]
    fn share_root_prefers_the_environment() {
        let dir = TempDir::new();
        install(dir.path(), &["bin/tmux-workspace"]);
        let exe = dir.path().join("bin/tmux-workspace");
        assert_eq!(
            share_root_from(Some("/elsewhere".into()), Some(exe)),
            PathBuf::from("/elsewhere")
        );
    }

    #[test]
    fn share_root_ignores_an_empty_environment_value() {
        let dir = TempDir::new();
        install(dir.path(), &["bin/tmux-workspace"]);
        let exe = dir.path().join("bin/tmux-workspace");
        assert_eq!(share_root_from(Some("".into()), Some(exe)), dir.path());
    }

    #[test]
    fn share_root_walks_up_from_the_binary_to_agent_status_conf() {
        let dir = TempDir::new();
        install(
            dir.path(),
            &[
                "bin/tmux-workspace",
                "target/debug/tmux-workspace",
                "target/debug/deps/tmux_workspace-1a2b",
            ],
        );
        for exe in [
            "bin/tmux-workspace",
            "target/debug/tmux-workspace",
            "target/debug/deps/tmux_workspace-1a2b",
        ] {
            let found = share_root_from(None, Some(dir.path().join(exe)));
            assert_eq!(found, dir.path(), "from {exe}");
        }
    }

    #[test]
    fn share_root_takes_the_nearest_directory_that_has_the_file() {
        let dir = TempDir::new();
        install(dir.path(), &[]);
        install(&dir.path().join("inner"), &["bin/tmux-workspace"]);
        let exe = dir.path().join("inner/bin/tmux-workspace");
        assert_eq!(share_root_from(None, Some(exe)), dir.path().join("inner"));
    }

    #[test]
    fn share_root_gives_up_after_three_levels() {
        let dir = TempDir::new();
        install(dir.path(), &["a/b/c/d/tmux-workspace"]);
        let exe = dir.path().join("a/b/c/d/tmux-workspace");
        assert_eq!(share_root_from(None, Some(exe)), dir.path().join("a/b/c/d"));
    }

    #[test]
    fn share_root_falls_back_to_the_directory_of_the_binary() {
        let dir = TempDir::new();
        let exe = dir.path().join("bin/tmux-workspace");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        assert_eq!(share_root_from(None, Some(exe)), dir.path().join("bin"));
    }

    #[test]
    fn share_root_of_this_checkout_is_the_repository() {
        // The unit-test binary lives in target/debug/deps, three levels below the checkout.
        let root = share_root();
        assert!(
            root.join("agent-status.conf").is_file(),
            "{}",
            root.display()
        );
        assert!(root.join("config.example.json").is_file());
    }

    #[test]
    fn locations_use_the_share_root_for_the_legacy_and_example_configs() {
        let w = World::new();
        let loc = Locations {
            share: None,
            ..w.locations()
        };
        assert_eq!(loc.share_root(), share_root());
        assert_eq!(
            Locations {
                share: Some("/s".into()),
                ..loc
            }
            .share_root(),
            PathBuf::from("/s")
        );
    }

    // self_path

    #[test]
    fn self_path_prefers_the_link_in_local_bin() {
        let dir = TempDir::new();
        let real = dir.path().join("root/bin/tmux-workspace");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "").unwrap();
        let home = dir.path().join("home");
        let link = home.join(".local/bin/tmux-workspace");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&real, &link).unwrap();
        let exe = real.canonicalize().unwrap();
        assert_eq!(self_path_for(&home, &exe), link.to_string_lossy());
    }

    #[test]
    fn self_path_is_the_binary_itself_without_a_link() {
        let dir = TempDir::new();
        let real = dir.path().join("root/bin/tmux-workspace");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "").unwrap();
        let exe = real.canonicalize().unwrap();
        assert_eq!(
            self_path_for(&dir.path().join("home"), &exe),
            exe.to_string_lossy()
        );
    }

    #[test]
    fn self_path_ignores_a_link_to_another_program() {
        let dir = TempDir::new();
        let real = dir.path().join("root/bin/tmux-workspace");
        let other = dir.path().join("other/tmux-workspace");
        for path in [&real, &other] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let home = dir.path().join("home");
        let link = home.join(".local/bin/tmux-workspace");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&other, &link).unwrap();
        let exe = real.canonicalize().unwrap();
        assert_eq!(self_path_for(&home, &exe), exe.to_string_lossy());
    }

    #[test]
    fn self_path_names_a_file_that_exists() {
        assert!(Path::new(&self_path()).is_file());
    }
}
