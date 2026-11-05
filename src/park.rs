//! Parking: move a pane out of its window into the hidden `__parked` session, and back.
//!
//! A parked pane keeps running. It carries the time it was parked (`@workspace_parked`, in
//! nanoseconds) and the workspace it came from (`@workspace_home`); both go when it is restored.

use crate::cli::message_via;
use crate::error::{Error, Result, fail, py_repr_str};
use crate::model::PARKING;
use crate::pick::{agent_status, status_word};
use crate::text::{now_ns, py_strip};
use crate::tmux::{Fields, Tmux};
use std::io::{self, BufRead, Write};

/// What `park` and `unpark` read of every pane, in one call.
const PANE_FIELDS: [&str; 6] = [
    "session_name",
    "window_id",
    "pane_id",
    "@workspace_parked",
    "@workspace_label",
    "@workspace_home",
];

fn panes(tmux: &Tmux) -> Result<Vec<Fields>> {
    tmux.fields("list-panes", &PANE_FIELDS, &["-a"])
}

/// The row of this pane. tmux names panes by `%id`; anything else is not found.
fn pane_info<'a>(rows: &'a [Fields], pane: &str) -> Result<&'a Fields> {
    rows.iter()
        .find(|row| row.get("pane_id") == pane)
        .ok_or_else(|| Error::Msg(format!("No such pane: {pane}")))
}

/// A tmux command as the owned arguments `Tmux::chain` takes.
fn command(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

/// Park a pane: out of its window, into a window of its own in the `__parked` session. It stays
/// running there, and `unpark` brings it back next to another pane.
pub fn park(tmux: &Tmux, pane: &str) -> Result<()> {
    let rows = panes(tmux)?;
    let current = pane_info(&rows, pane)?;
    if current.get("session_name") == PARKING {
        return fail("This pane is already parked");
    }
    let window = current.get("window_id");
    if rows
        .iter()
        .filter(|row| row.get("window_id") == window)
        .count()
        == 1
    {
        return fail("This is the only pane. Create a split first, or switch windows.");
    }
    // A session needs a pane while it exists. The first pane to be parked finds none, so a
    // placeholder holds the session until that pane arrives.
    let placeholder = if rows.iter().any(|row| row.get("session_name") == PARKING) {
        None
    } else {
        Some(tmux.run(&[
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-s",
            PARKING,
            "-n",
            "holding",
            "/bin/sleep 2147483647",
        ])?)
    };
    let label = match current.get("@workspace_label") {
        "" => "shell",
        label => label,
    };
    let moved = move_out(tmux, pane, current.get("session_name"), label);
    if let Some(placeholder) = placeholder {
        tmux.quiet(&["kill-pane", "-t", &placeholder]);
    }
    moved?;
    message_via(
        tmux,
        &format!("Parked {label} — prefix u restores; prefix U chooses"),
    );
    Ok(())
}

/// Mark the pane as parked and break it out into the `__parked` session, in a window named `label`.
/// A pane that cannot be moved is not left marked.
fn move_out(tmux: &Tmux, pane: &str, home: &str, label: &str) -> Result<()> {
    tmux.chain(&[
        command(&[
            "set-option",
            "-p",
            "-t",
            pane,
            "@workspace_parked",
            &now_ns().to_string(),
        ]),
        command(&["set-option", "-p", "-t", pane, "@workspace_home", home]),
    ])?;
    let target = format!("={PARKING}:");
    tmux.run(&["break-pane", "-d", "-s", pane, "-t", &target, "-n", label])
        .inspect_err(|_| {
            tmux.quiet(&["set-option", "-pu", "-t", pane, "@workspace_parked"]);
        })
        .map(|_| ())
}

/// The panes of the rows that are parked, newest first. Each gets `label` (`shell` when it has none),
/// `home` and `stamp` besides its fields.
fn parked_in(rows: &[Fields]) -> Result<Vec<Fields>> {
    let mut found = Vec::new();
    for row in rows {
        let stamp = row.get("@workspace_parked");
        if row.get("session_name") != PARKING || stamp.is_empty() {
            continue;
        }
        let time: i128 = py_strip(stamp).parse().or_else(|_| {
            fail(format!(
                "invalid literal for int() with base 10: {}",
                py_repr_str(stamp)
            ))
        })?;
        let mut pane = row.clone();
        pane.set(
            "label",
            match row.get("@workspace_label") {
                "" => "shell",
                label => label,
            },
        );
        pane.set("home", row.get("@workspace_home"));
        pane.set("stamp", time.to_string());
        found.push((time, pane));
    }
    // A stable sort, so that panes parked in the same nanosecond keep their order.
    found.sort_by_key(|(time, _)| std::cmp::Reverse(*time));
    Ok(found.into_iter().map(|(_, pane)| pane).collect())
}

/// The parked panes, newest first.
pub fn parked(tmux: &Tmux) -> Result<Vec<Fields>> {
    parked_in(&panes(tmux)?)
}

/// One line of the list `choose-park` prints: the number, the label, the workspace it came from,
/// and the status of its agent.
pub fn parked_row(number: usize, pane: &Fields, status: &str) -> String {
    let mut row = format!(" {number}. {}  [{}]", pane.get("label"), pane.get("home"));
    if !status.is_empty() {
        row.push_str("  ");
        row.push_str(&status_word(status));
    }
    row
}

/// The target and direction that the arguments of `unpark` and `choose-park` give. A first argument
/// that starts with `%` or is all digits is the pane to restore next to, and the next one is the
/// direction (`right`, else below). Without a target it is the current pane, and without a direction
/// `right`.
pub fn unpark_args(args: &[String]) -> (Option<String>, String) {
    let mut rest = args;
    let mut target = None;
    if let Some(first) = rest.first()
        && (first.starts_with('%')
            || (!first.is_empty() && first.chars().all(|c| c.is_ascii_digit())))
    {
        target = Some(first.clone());
        rest = &rest[1..];
    }
    let direction = rest.first().map_or("right", String::as_str);
    (target, direction.to_string())
}

/// Bring a parked pane back next to `target`: the newest one, or the one asked for with `choose`.
pub fn unpark(tmux: &Tmux, target: &str, direction: &str, choose: bool) -> Result<()> {
    unpark_with(
        tmux,
        target,
        direction,
        choose,
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
    )
}

/// `unpark` with its input and output passed in.
fn unpark_with(
    tmux: &Tmux,
    target: &str,
    direction: &str,
    choose: bool,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<()> {
    let rows = panes(tmux)?;
    let choices = parked_in(&rows)?;
    if choices.is_empty() {
        return fail("No parked panes");
    }
    check_target(&rows, target)?;
    let mut selected = &choices[0];
    if choose {
        writeln!(
            output,
            "Parked panes — restore {direction} of the selected pane\n"
        )?;
        let status = agent_status(tmux).by_pane;
        for (number, pane) in choices.iter().enumerate() {
            let status = status.get(pane.get("pane_id")).copied().unwrap_or("");
            writeln!(output, "{}", parked_row(number + 1, pane, status))?;
        }
        write!(output, "\nNumber (Enter cancels): ")?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Err(Error::Eof);
        }
        let answer = py_strip(&line);
        if answer.is_empty() {
            return Ok(());
        }
        let number = answer
            .chars()
            .all(|c| c.is_ascii_digit())
            .then(|| answer.parse::<usize>().ok())
            .flatten()
            .filter(|number| (1..=choices.len()).contains(number));
        let Some(number) = number else {
            return fail("Choose a listed pane number");
        };
        selected = &choices[number - 1];
    }
    join_back(tmux, selected.get("pane_id"), target, direction)
}

/// Bring the parked pane `source` back next to `target`.
pub fn rejoin(tmux: &Tmux, source: &str, target: &str, direction: &str) -> Result<()> {
    check_target(&panes(tmux)?, target)?;
    join_back(tmux, source, target, direction)
}

fn check_target(rows: &[Fields], target: &str) -> Result<()> {
    if pane_info(rows, target)?.get("session_name") == PARKING {
        return fail("Switch to a project window before restoring a parked pane");
    }
    Ok(())
}

/// Join the pane to the target, 40% wide or high, and forget that it was parked. One tmux call, which
/// stops at the first command that fails.
fn join_back(tmux: &Tmux, source: &str, target: &str, direction: &str) -> Result<()> {
    let split = if direction == "right" { "-h" } else { "-v" };
    tmux.chain(&[
        command(&["join-pane", split, "-s", source, "-t", target, "-l", "40%"]),
        command(&["set-option", "-pu", "-t", source, "@workspace_parked"]),
        command(&["set-option", "-pu", "-t", source, "@workspace_home"]),
    ])
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(label: &str, home: &str) -> Fields {
        let mut pane = Fields::default();
        pane.set("label", label);
        pane.set("home", home);
        pane
    }

    fn row(session: &str, id: &str, stamp: &str, label: &str, home: &str) -> Fields {
        let mut row = Fields::default();
        row.set("session_name", session);
        row.set("pane_id", id);
        row.set("@workspace_parked", stamp);
        row.set("@workspace_label", label);
        row.set("@workspace_home", home);
        row
    }

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    // tests/test_agent_status.py::Rows::test_parked_row_shows_the_status_word_and_keeps_its_number
    #[test]
    fn parked_row_shows_the_status_word_and_keeps_its_number() {
        let parked = pane("logs", "alpha");
        let plain = parked_row(2, &parked, "");
        assert_eq!(plain, " 2. logs  [alpha]");
        let marked = parked_row(2, &parked, "done");
        assert!(marked.starts_with(&plain));
        assert!(marked.contains("done"));
        assert_eq!(marked, " 2. logs  [alpha]  \x1b[36mdone   \x1b[0m");
    }

    #[test]
    fn a_status_that_is_not_one_leaves_two_spaces() {
        assert_eq!(parked_row(1, &pane("a", "b"), "busy"), " 1. a  [b]  ");
    }

    #[test]
    fn parked_panes_are_listed_newest_first() {
        let rows = [
            row(PARKING, "%1", "100", "old", "alpha"),
            row("alpha", "%2", "", "shown", ""),
            row(PARKING, "%3", "300", "", "beta"),
            row("alpha", "%4", "999", "not in the parking session", "alpha"),
            row(PARKING, "%5", "200", "mid", "alpha"),
            row(PARKING, "%6", "", "no stamp", "alpha"),
        ];
        let found = parked_in(&rows).unwrap();
        let ids: Vec<_> = found.iter().map(|p| p.get("pane_id")).collect();
        assert_eq!(ids, ["%3", "%5", "%1"]);
        assert_eq!(found[0].get("label"), "shell", "no label means shell");
        assert_eq!(found[0].get("home"), "beta");
        assert_eq!(found[0].get("stamp"), "300");
        assert_eq!(found[1].get("label"), "mid");
    }

    #[test]
    fn panes_parked_at_the_same_moment_keep_their_order() {
        let rows = [
            row(PARKING, "%1", "5", "a", ""),
            row(PARKING, "%2", "5", "b", ""),
        ];
        let found = parked_in(&rows).unwrap();
        assert_eq!(found[0].get("pane_id"), "%1");
    }

    #[test]
    fn a_stamp_that_is_not_a_number_is_an_error() {
        let rows = [row(PARKING, "%1", "soon", "a", "")];
        assert_eq!(
            parked_in(&rows).unwrap_err().to_string(),
            "invalid literal for int() with base 10: 'soon'"
        );
    }

    // bin/tmux-workspace:1513-1520
    #[test]
    fn unpark_arguments_are_a_target_and_a_direction() {
        let parsed = |parts: &[&str]| unpark_args(&args(parts));
        let some =
            |target: &str, direction: &str| (Some(target.to_string()), direction.to_string());
        let none = |direction: &str| (None, direction.to_string());
        assert_eq!(parsed(&[]), none("right"));
        assert_eq!(parsed(&["%5"]), some("%5", "right"));
        assert_eq!(parsed(&["%5", "below"]), some("%5", "below"));
        assert_eq!(parsed(&["12", "below"]), some("12", "below"));
        assert_eq!(parsed(&["below"]), none("below"));
        assert_eq!(parsed(&["right"]), none("right"));
        assert_eq!(parsed(&["%5", "below", "extra"]), some("%5", "below"));
        // Anything that is not "right" means below, as the direction is only compared with right.
        assert_eq!(parsed(&["up"]), none("up"));
        assert_eq!(parsed(&[""]), none(""));
        assert_eq!(parsed(&["1a"]), none("1a"));
        assert_eq!(parsed(&["%"]), some("%", "right"));
    }

    #[test]
    fn a_pane_that_is_not_listed_is_named() {
        let rows = [row("alpha", "%1", "", "", "")];
        assert!(pane_info(&rows, "%1").is_ok());
        assert_eq!(
            pane_info(&rows, "%2").unwrap_err().to_string(),
            "No such pane: %2"
        );
    }
}
