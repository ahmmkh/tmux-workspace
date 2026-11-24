//! Central registry of supported terminal agents and editors.
//!
//! Every module that enumerates agents, maps resume commands, checks process ownership,
//! or dispatches hook modes reads from [`ALL`] instead of maintaining its own hardcoded list.

pub mod agy;
pub mod claude;
pub mod codex;
pub mod nvim;
pub mod omp;

/// Canonical identifier for every supported terminal application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Claude,
    Codex,
    Omp,
    Agy,
    Nvim,
}

/// Static capabilities and CLI rules of a supported application.
#[derive(Clone, Copy, Debug)]
pub struct AgentSpec {
    pub kind: AgentKind,
    /// Canonical lowercase id used in `@workspace_app.kind`, snapshot JSON, and reports.
    pub id: &'static str,
    /// Human-readable display name used in `doctor` and `integrations`.
    pub display_name: &'static str,
    /// True for AI coding agents (`claude`, `codex`, `omp`, `agy`), false for editors (`nvim`).
    pub is_ai_agent: bool,
    /// Command prefix to reopen a saved conversation/session: `resume_argv + [token]`.
    pub resume_argv: &'static [&'static str],
    /// Whether an absolute-path token names a session file that must still exist on disk.
    pub checks_session_file: bool,
    /// Flags (exact or `--flag=...`) indicating a non-interactive / one-shot invocation.
    pub one_shot_flags: &'static [&'static str],
    /// Non-interactive subcommands rejected during foreground owner detection.
    pub non_interactive_subcommands: &'static [&'static str],
    /// CLI flags that take a following argument (so their value is not misread as a subcommand).
    pub value_flags: &'static [&'static str],
    /// Accepted `agent-hook <mode>` values handled by this agent.
    pub hook_modes: &'static [&'static str],
}

/// All supported terminal applications, in display order.
pub const ALL: &[AgentSpec] = &[claude::SPEC, codex::SPEC, omp::SPEC, agy::SPEC, nvim::SPEC];

/// Canonical IDs of supported AI agents (excludes editors like `nvim`).
pub const AI_AGENT_IDS: [&str; 4] = ["claude", "codex", "omp", "agy"];

/// Look up an application spec by its canonical lowercase id.
pub fn by_id(id: &str) -> Option<&'static AgentSpec> {
    ALL.iter().find(|spec| spec.id == id)
}

/// Whether `id` names any supported application (agent or editor).
pub fn is_known(id: &str) -> bool {
    by_id(id).is_some()
}

/// Whether `id` names an AI coding agent.
pub fn is_ai_agent(id: &str) -> bool {
    by_id(id).is_some_and(|spec| spec.is_ai_agent)
}

/// Command prefix used to reopen a saved conversation/session for `id`.
pub fn resume_argv(id: &str) -> Option<&'static [&'static str]> {
    by_id(id).map(|spec| spec.resume_argv)
}

/// Whether `mode` is a recognized `agent-hook <mode>` argument.
pub fn valid_hook_mode(mode: &str) -> bool {
    ALL.iter().any(|spec| spec.hook_modes.contains(&mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_every_agent_and_editor() {
        let ids: Vec<&str> = ALL.iter().map(|spec| spec.id).collect();
        assert_eq!(ids, ["claude", "codex", "omp", "agy", "nvim"]);
        let ai: Vec<&str> = ALL
            .iter()
            .filter(|spec| spec.is_ai_agent)
            .map(|spec| spec.id)
            .collect();
        assert_eq!(ai, AI_AGENT_IDS);
    }

    #[test]
    fn resume_commands_and_hook_modes_are_consistent() {
        assert_eq!(resume_argv("claude"), Some(&["claude", "--resume"][..]));
        assert_eq!(resume_argv("codex"), Some(&["codex", "resume"][..]));
        assert_eq!(resume_argv("omp"), Some(&["omp", "--resume"][..]));
        assert_eq!(resume_argv("agy"), Some(&["agy", "--conversation"][..]));
        assert_eq!(resume_argv("nvim"), Some(&["nvim", "-S"][..]));
        assert_eq!(resume_argv("unknown"), None);

        for mode in ["claude", "claude-status", "codex", "omp", "agy"] {
            assert!(valid_hook_mode(mode), "{mode}");
        }
        assert!(!valid_hook_mode("nvim"));
        assert!(!valid_hook_mode("bogus"));
    }
}
