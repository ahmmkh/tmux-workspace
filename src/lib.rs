//! tmux-workspace: personal tmux controls and private, allowlisted workspace recovery.

pub mod agent;
pub mod ai;
pub mod anonymize;
pub mod bug;
pub mod cleanup;
pub mod cli;
pub mod dash;
pub mod doctor;
pub mod error;
pub mod errorlog;
pub mod help;
pub(crate) mod help_topics;
pub mod hook;
pub mod host;
pub mod integrations;
pub mod model;
pub mod onboarding;
pub mod pane;
pub mod park;
pub mod paths;
pub mod pick;
pub mod process;
pub mod pyjson;
pub mod report;
pub mod sha256;
pub mod snapshot;
pub mod text;
pub mod tmux;
pub mod update;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
