//! `omp` coding agent specification.

use super::{AgentKind, AgentSpec};

pub const SPEC: AgentSpec = AgentSpec {
    kind: AgentKind::Omp,
    id: "omp",
    display_name: "omp",
    is_ai_agent: true,
    resume_argv: &["omp", "--resume"],
    checks_session_file: true,
    one_shot_flags: &["-p", "--print", "--mode", "--no-session"],
    non_interactive_subcommands: &[],
    value_flags: &[],
    hook_modes: &["omp"],
};
