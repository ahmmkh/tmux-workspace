//! What the pickers share: how an agent status is written, and where to read it. (The pickers
//! themselves come with the `open` command.)

use crate::model::{AGENT_ORDER, RollUp, roll_up};
use crate::text::ljust;
use crate::tmux::Tmux;

/// The width of the widest status word, so that the words line up.
pub const STATUS_WIDTH: usize = {
    let mut width = 0;
    let mut at = 0;
    while at < AGENT_ORDER.len() {
        if AGENT_ORDER[at].len() > width {
            width = AGENT_ORDER[at].len();
        }
        at += 1;
    }
    width
};

/// The ANSI colour of a status word: red, cyan, yellow, green.
fn colour(status: &str) -> Option<u8> {
    match status {
        "blocked" => Some(31),
        "done" => Some(36),
        "working" => Some(33),
        "idle" => Some(32),
        _ => None,
    }
}

/// A status as a coloured word of `STATUS_WIDTH` characters; nothing for a status that is not one.
pub fn status_word(status: &str) -> String {
    colour(status).map_or_else(String::new, |code| {
        format!("\x1b[{code}m{}\x1b[0m", ljust(status, STATUS_WIDTH))
    })
}

/// The status of every pane, read in one call. A failed read gives no status, so the pickers still
/// list their rows.
pub fn agent_status(tmux: &Tmux) -> RollUp {
    let names = [
        "session_name",
        "window_id",
        "pane_id",
        "pane_current_command",
        "@workspace_agent",
        "@workspace_home",
    ];
    tmux.fields("list-panes", &names, &["-a"])
        .map(|rows| roll_up(&rows))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_word_is_coloured_and_padded_to_the_widest() {
        assert_eq!(status_word("done"), "\x1b[36mdone   \x1b[0m");
        assert_eq!(status_word("blocked"), "\x1b[31mblocked\x1b[0m");
        assert_eq!(status_word("working"), "\x1b[33mworking\x1b[0m");
        assert_eq!(status_word("idle"), "\x1b[32midle   \x1b[0m");
    }

    #[test]
    fn something_that_is_not_a_status_has_no_word() {
        for status in ["", "busy", "Done", " done"] {
            assert_eq!(status_word(status), "", "{status:?}");
        }
    }

    #[test]
    fn every_status_has_a_colour_and_fits_the_width() {
        assert_eq!(STATUS_WIDTH, 7);
        for status in AGENT_ORDER {
            assert!(colour(status).is_some(), "{status}");
            assert!(status.len() <= STATUS_WIDTH, "{status}");
        }
    }

    #[test]
    fn a_failed_read_gives_no_status() {
        // A socket that does not exist: the read fails and the answer is empty, not an error.
        let missing = Tmux::new().socket("/nonexistent/tmux-workspace-test-socket");
        assert_eq!(agent_status(&missing), RollUp::default());
    }
}
