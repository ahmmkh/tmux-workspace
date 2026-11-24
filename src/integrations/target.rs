//! Integration install target paths, symlink ownership checks, and release-folder helpers.

use super::json::{hook_commands, hooked_agy, ours};
use crate::paths::{self, resolve, share_root};
use std::fs;
use std::path::{Path, PathBuf};

/// The part of a share root that holds the shipped integration files.
const INTEGRATIONS: &str = "integrations";

/// The folder of Codex's hooks: `$CODEX_HOME` when it is set and not empty, else `~/.codex`.
pub fn codex_dir(home: &Path) -> PathBuf {
    paths::codex_home(home)
}

pub(crate) fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Where a link leads, resolved: a link whose target is missing still leads somewhere.
fn leads_to(link: &Path) -> PathBuf {
    match link.canonicalize() {
        Ok(real) => real,
        Err(_) => match fs::read_link(link) {
            Ok(target) => resolve(&link.parent().unwrap_or(Path::new(".")).join(target)),
            Err(_) => resolve(link),
        },
    }
}

/// The folder of a release install's versions, when `share` is the version that its `current` link
/// leads to (or that link itself): `<home>/<version>` beside `<home>/current`. A clone has none.
fn release_home(share: &Path) -> Option<PathBuf> {
    let real = resolve(share);
    let home = real.parent()?;
    let current = home.join("current");
    (is_symlink(&current) && resolve(&current) == real).then(|| home.to_path_buf())
}

/// The share folder as the links name it: `<home>/current` for a release install, so that they outlive
/// updates (the version folder they would name otherwise goes with the update after next), else the
/// folder itself.
pub(crate) fn link_share(share: &Path) -> PathBuf {
    release_home(share).map_or_else(|| resolve(share), |home| home.join("current"))
}

/// A version folder's name, as install.sh's `VERSION_RE` has it: `1.2.3`, maybe with `-pre` or `+build`.
pub(crate) fn is_version(name: &str) -> bool {
    let (core, suffix) = match name.find(['-', '+']) {
        Some(at) => (&name[..at], Some(&name[at + 1..])),
        None => (name, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && suffix.is_none_or(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        })
}

/// The link the Python version made for its hook: it leads to `integrations/agent-hook.py` of some clone.
pub(crate) fn old_hook_link(link: &Path) -> bool {
    is_symlink(link)
        && fs::read_link(link).is_ok_and(|target| target.ends_with("integrations/agent-hook.py"))
}

/// What an installation acts on. Apart from the real machine (`Target::current`) a test supplies it.
#[derive(Clone, Debug)]
pub struct Target {
    pub home: PathBuf,
    /// The folder with `integrations/`: what the links lead to and the templates come from. For a
    /// release install it is `<home>/current` (see [`link_share`]).
    pub share: PathBuf,
    /// Where Codex keeps `hooks.json`, when not at `~/.codex`.
    pub codex_home: Option<PathBuf>,
    /// `Darwin` or `Linux`, as `uname -s` says. Only Darwin gets the iTerm2 profile.
    pub system: String,
}

impl Target {
    /// This machine, for this `home` and `system`.
    pub fn current(home: &Path, system: &str) -> Target {
        Target {
            home: home.to_path_buf(),
            share: link_share(&share_root()),
            codex_home: Some(codex_dir(home)),
            system: system.to_string(),
        }
    }

    /// What `integrations status` and reports list, one by one.
    pub fn each(&self) -> [(&'static str, bool); 5] {
        let hooked = |path: PathBuf| hook_commands(&path).iter().any(|command| ours(command));
        [
            ("claude", hooked(self.claude_settings())),
            ("codex", hooked(self.codex_hooks())),
            ("omp", self.made(&self.omp_extension())),
            ("agy", hooked_agy(&self.agy_settings())),
            ("nvim", self.made(&self.neovim_helper())),
        ]
    }

    pub(crate) fn codex(&self) -> PathBuf {
        self.codex_home
            .clone()
            .unwrap_or_else(|| self.home.join(".codex"))
    }

    pub(crate) fn agy_dir(&self) -> PathBuf {
        paths::agy_dir(&self.home)
    }

    pub(crate) fn agy_settings(&self) -> PathBuf {
        self.agy_dir().join("settings.json")
    }

    pub(crate) fn source(&self, name: &str) -> PathBuf {
        self.share.join(INTEGRATIONS).join(name)
    }

    pub(crate) fn claude_settings(&self) -> PathBuf {
        self.home.join(".claude/settings.json")
    }

    pub(crate) fn codex_hooks(&self) -> PathBuf {
        self.codex().join("hooks.json")
    }

    pub(crate) fn omp_extension(&self) -> PathBuf {
        self.home.join(".omp/agent/extensions/tmux-workspace.ts")
    }

    pub(crate) fn neovim_helper(&self) -> PathBuf {
        self.home.join(".config/nvim/lua/custom/tmux_workspace.lua")
    }

    pub(crate) fn iterm_profile(&self) -> PathBuf {
        self.home
            .join("Library/Application Support/iTerm2/DynamicProfiles/tmux-workspace.json")
    }

    /// The link of the Python version's hook, which nothing uses any more.
    pub(crate) fn old_hook_link(&self) -> PathBuf {
        self.home.join(".local/lib/tmux-workspace/agent-hook.py")
    }

    /// Where install puts its links.
    pub(crate) fn links(&self) -> [PathBuf; 3] {
        [
            self.omp_extension(),
            self.neovim_helper(),
            self.iterm_profile(),
        ]
    }

    /// Whether `link` is a link install made: it leads into the share folder, or, for a release
    /// install, into `current` or any version folder beside it. An update moves `current` on and
    /// prunes the older versions, so a link may name one that is gone.
    pub(crate) fn made(&self, link: &Path) -> bool {
        if !is_symlink(link) {
            return false;
        }
        let leads = leads_to(link);
        let share = resolve(&self.share);
        if leads != share && leads.starts_with(&share) {
            return true;
        }
        let Some(home) = release_home(&self.share) else {
            return false;
        };
        let mut parts = leads
            .strip_prefix(&home)
            .into_iter()
            .flat_map(Path::components);
        let first = parts.next().map(|part| part.as_os_str().to_string_lossy());
        parts.next().is_some() && first.is_some_and(|name| name == "current" || is_version(&name))
    }
}
