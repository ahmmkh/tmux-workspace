//! Neovim editor specification.

use super::{AgentKind, AgentSpec};

pub const SPEC: AgentSpec = AgentSpec {
    kind: AgentKind::Nvim,
    id: "nvim",
    display_name: "Neovim",
    is_ai_agent: false,
    resume_argv: &["nvim", "-S"],
    checks_session_file: true,
    one_shot_flags: &[],
    non_interactive_subcommands: &[],
    value_flags: &[],
    hook_modes: &[],
};
