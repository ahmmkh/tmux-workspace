//! `errors.log` in the state folder: one line for every command that ended with an error, so that a
//! bug report can say what went wrong lately. A line holds the time (UTC), the name of the command and
//! the error message, and nothing else: never the arguments, never anything of a screen.

use crate::error::Error;
use crate::paths::{expand_user, make_private_dir, state_dir};
use crate::text::now;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// How many lines are kept.
pub const KEEP: usize = 200;

/// The log is cut back to [`KEEP`] lines when it has more than this many.
pub const TRIM_AT: usize = 250;

/// A longer message is cut here, in characters.
const MAX_MESSAGE: usize = 300;

/// The file in a state folder.
pub fn path(state: &Path) -> PathBuf {
    state.join("errors.log")
}

/// `1970-01-01 00:00`: the time of an epoch second in UTC, to the minute.
pub fn utc_stamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let minutes = secs.rem_euclid(86_400) / 60;
    // The civil date of a day count, as Howard Hinnant's `civil_from_days` has it.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        minutes / 60,
        minutes % 60
    )
}

/// What the log says about an error: its message, but of a failed program only its name and, for
/// tmux, the command, because the arguments of a call can hold a label or a folder.
pub fn describe(error: &Error) -> String {
    match error {
        Error::Command { argv, status } => {
            format!("{} exited with status {status}", program_and_command(argv))
        }
        other => other.to_string(),
    }
}

/// `tmux set-option` for a tmux call, else the name of the program.
fn program_and_command(argv: &[String]) -> String {
    let program = argv
        .first()
        .map_or("", |program| program.rsplit('/').next().unwrap_or(program));
    if program != "tmux" {
        return program.to_string();
    }
    let mut rest = argv.iter().skip(1);
    while let Some(arg) = rest.next() {
        if matches!(arg.as_str(), "-S" | "-L" | "-f" | "-T" | "-c") {
            rest.next();
        } else if !arg.starts_with('-') {
            return format!("tmux {arg}");
        }
    }
    "tmux".to_string()
}

/// The message on one line, with no control character in it, and not too long.
fn one_line(message: &str) -> String {
    message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_MESSAGE)
        .collect()
}

fn append(state: &Path, line: &str) -> io::Result<()> {
    make_private_dir(state)?;
    let log = path(state);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&log)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(format!("{line}\n").as_bytes())?;
    drop(file);
    trim(state)
}

/// Cut the log back to its last [`KEEP`] lines when it has more than [`TRIM_AT`].
fn trim(state: &Path) -> io::Result<()> {
    let log = path(state);
    let text = fs::read_to_string(&log)?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= TRIM_AT {
        return Ok(());
    }
    let kept = format!("{}\n", lines[lines.len() - KEEP..].join("\n"));
    let temporary = state.join(format!(".errors.log-{}", std::process::id()));
    let written = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut file| file.write_all(kept.as_bytes()))
        .and_then(|()| fs::rename(&temporary, &log));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Add a line to the log in `state` and cut the log back when it has grown. A failure is ignored.
pub fn record_in(state: &Path, secs: i64, command: &str, message: &str) {
    let line = format!("{} {command}: {}", utc_stamp(secs), one_line(message));
    let _ = append(state, &line);
}

/// `message` without the arguments of the command that failed, and without the names that it made of
/// them: each is `<arg>`. A flag, and an argument of one character, say nothing and stay.
pub fn redact(message: &str, arguments: &[String]) -> String {
    let mut needles: Vec<String> = Vec::new();
    for argument in arguments
        .iter()
        .filter(|argument| !argument.starts_with('-'))
    {
        for name in names_of(argument) {
            if name.chars().count() >= 2 && !needles.contains(&name) {
                needles.push(name);
            }
        }
    }
    // The longest first, so that `acme-secret` is not cut short by `acme`.
    needles.sort_by_key(|needle| std::cmp::Reverse(needle.len()));
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(message.len());
    let mut at = 0;
    while at < message.len() {
        let found = needles.iter().find(|needle| {
            let end = at + needle.len();
            message[at..].starts_with(needle.as_str())
                && !(needle.starts_with(word) && message[..at].ends_with(word))
                && !(needle.ends_with(word) && message[end..].starts_with(word))
        });
        if let Some(needle) = found {
            out.push_str("<arg>");
            at += needle.len();
            continue;
        }
        let c = message[at..].chars().next().unwrap_or(' ');
        out.push(c);
        at += c.len_utf8();
    }
    out
}

/// An argument, and what the program makes of it: where it leads as a path, and the name of the folder
/// (`work .` is a workspace named after the folder it is run in).
fn names_of(argument: &str) -> Vec<String> {
    let mut names = vec![argument.to_string()];
    let expanded = expand_user(argument);
    let mut paths = vec![expanded.clone()];
    if let Ok(real) = expanded.canonicalize() {
        paths.push(real);
    }
    for path in paths {
        names.push(path.to_string_lossy().into_owned());
        if let Some(name) = path.file_name() {
            names.push(name.to_string_lossy().into_owned());
        }
    }
    names
}

/// Add a line for `error` to the log of the state folder, now. The arguments of the command, which the
/// message may have in it, are taken out. A failure is ignored.
pub fn record(command: &str, arguments: &[String], error: &Error) {
    let message = redact(&describe(error), arguments);
    record_in(&state_dir(), now() as i64, command, &message);
}

/// The last `count` lines of the log in `state`, the oldest first.
pub fn last(state: &Path, count: usize) -> Vec<String> {
    let text = fs::read_to_string(path(state)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    lines[lines.len().saturating_sub(count)..]
        .iter()
        .map(|line| line.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn log_lines(state: &Path) -> Vec<String> {
        fs::read_to_string(path(state))
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_stamp_is_utc_to_the_minute() {
        assert_eq!(utc_stamp(0), "1970-01-01 00:00");
        assert_eq!(utc_stamp(1_793_919_088), "2026-11-05 22:51");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29 00:00");
        assert_eq!(utc_stamp(1_709_164_799), "2024-02-28 23:59");
        assert_eq!(utc_stamp(4_102_444_800), "2100-01-01 00:00");
    }

    #[test]
    fn a_line_is_the_time_the_command_and_the_message() {
        let dir = TempDir::new();
        let state = dir.path().join("state");
        record_in(&state, 1_793_919_088, "open", "cannot write ~/Projects/x");
        assert_eq!(
            log_lines(&state),
            ["2026-11-05 22:51 open: cannot write ~/Projects/x"]
        );
        record_in(&state, 1_793_919_100, "save", "second");
        assert_eq!(log_lines(&state)[1], "2026-11-05 22:51 save: second");
    }

    #[test]
    fn the_folder_is_private_and_so_is_the_file_even_when_it_was_not() {
        let dir = TempDir::new();
        let state = dir.path().join("state");
        record_in(&state, 0, "open", "x");
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&path(&state)), 0o600);
        fs::set_permissions(path(&state), fs::Permissions::from_mode(0o644)).unwrap();
        record_in(&state, 0, "open", "y");
        assert_eq!(mode(&path(&state)), 0o600);
    }

    #[test]
    fn a_message_is_one_line_and_not_too_long() {
        let dir = TempDir::new();
        record_in(dir.path(), 0, "open", "one\ntwo\r\nthree\tfour\u{1b}[31m");
        assert_eq!(
            log_lines(dir.path()),
            ["1970-01-01 00:00 open: one two  three four [31m"]
        );
        record_in(dir.path(), 0, "open", &"é".repeat(1000));
        let line = &log_lines(dir.path())[1];
        assert_eq!(
            line.chars().count(),
            "1970-01-01 00:00 open: ".len() + MAX_MESSAGE
        );
    }

    #[test]
    fn a_failed_program_is_named_without_its_arguments() {
        let command = |argv: &[&str]| Error::Command {
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            status: 1,
        };
        assert_eq!(
            describe(&command(&[
                "/opt/homebrew/bin/tmux",
                "-N",
                "-S",
                "/tmp/socket",
                "set-option",
                "-p",
                "-t",
                "%1",
                "@workspace_label",
                "secret-label"
            ])),
            "tmux set-option exited with status 1"
        );
        assert_eq!(
            describe(&command(&["tmux", "display-message", "-p", "#{pane_id}"])),
            "tmux display-message exited with status 1"
        );
        assert_eq!(
            describe(&command(&["ps", "-p", "123", "-o", "args="])),
            "ps exited with status 1"
        );
        assert_eq!(
            describe(&Error::Msg("Unknown workspace command: x".into())),
            "Unknown workspace command: x"
        );
    }

    #[test]
    fn the_log_is_cut_back_to_two_hundred_lines_when_it_passes_two_hundred_and_fifty() {
        let dir = TempDir::new();
        for n in 1..=TRIM_AT {
            record_in(dir.path(), 0, "open", &format!("error {n}"));
        }
        assert_eq!(log_lines(dir.path()).len(), TRIM_AT);
        record_in(dir.path(), 0, "open", &format!("error {}", TRIM_AT + 1));
        let lines = log_lines(dir.path());
        assert_eq!(lines.len(), KEEP);
        assert!(lines[0].ends_with("error 52"), "{}", lines[0]);
        assert!(lines[KEEP - 1].ends_with(&format!("error {}", TRIM_AT + 1)));
        assert_eq!(mode(&path(dir.path())), 0o600);
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "no temporary file is left");
    }

    fn args(arguments: &[&str]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect()
    }

    #[test]
    fn an_argument_is_not_in_what_the_log_says() {
        assert_eq!(
            redact("No such pane: initech-layoffs", &args(&["initech-layoffs"])),
            "No such pane: <arg>"
        );
        assert_eq!(
            redact(
                "Cannot write /home/x/Projects/Globex Corp: Permission denied",
                &args(&["Globex Corp"])
            ),
            "Cannot write /home/x/Projects/<arg>: Permission denied"
        );
        // Every place, and the longest argument first.
        assert_eq!(
            redact(
                "acme-secret and acme and acme-secret",
                &args(&["acme", "acme-secret"])
            ),
            "<arg> and <arg> and <arg>"
        );
        // Whole words only, so that the message stays a message.
        assert_eq!(
            redact("nopeness is not nope", &args(&["nope"])),
            "nopeness is not <arg>"
        );
    }

    #[test]
    fn flags_and_one_letter_arguments_and_empty_ones_stay() {
        let message = "Usage: tmux-workspace update [--check] x";
        assert_eq!(redact(message, &args(&["--check", "-x", "x", ""])), message);
        assert_eq!(redact(message, &[]), message);
    }

    #[test]
    fn the_name_an_argument_resolves_to_is_not_in_it_either() {
        let dir = TempDir::new();
        let folder = dir.path().join("Globex Corp");
        fs::create_dir(&folder).unwrap();
        let argument = folder.to_string_lossy().into_owned();
        let real = folder.canonicalize().unwrap();
        // The folder's own name, as the program makes a workspace of it, and its full path as it resolves.
        assert_eq!(
            redact(
                "Cannot write /x/Projects/Globex Corp: no",
                &args(&[&argument])
            ),
            "Cannot write /x/Projects/<arg>: no"
        );
        assert_eq!(
            redact(
                &format!("Cannot read {}: no", real.display()),
                &args(&[&argument])
            ),
            "Cannot read <arg>: no"
        );
    }

    #[test]
    fn the_log_line_is_made_with_the_arguments_taken_out() {
        let dir = TempDir::new();
        let message = redact("No such pane: initech-layoffs", &args(&["initech-layoffs"]));
        record_in(dir.path(), 0, "park", &message);
        assert_eq!(
            log_lines(dir.path()),
            ["1970-01-01 00:00 park: No such pane: <arg>"]
        );
    }

    #[test]
    fn the_last_lines_come_oldest_first_and_a_missing_log_has_none() {
        let dir = TempDir::new();
        assert_eq!(last(dir.path(), 10), Vec::<String>::new());
        for n in 1..=12 {
            record_in(dir.path(), 0, "open", &format!("error {n}"));
        }
        let last_ten = last(dir.path(), 10);
        assert_eq!(last_ten.len(), 10);
        assert!(last_ten[0].ends_with("error 3"));
        assert!(last_ten[9].ends_with("error 12"));
        assert_eq!(last(dir.path(), 100).len(), 12);
    }

    #[test]
    fn a_log_that_cannot_be_written_is_no_failure() {
        let dir = TempDir::new();
        let file = dir.path().join("a-file");
        fs::write(&file, "").unwrap();
        record_in(&file.join("state"), 0, "open", "x");
        assert_eq!(last(&file.join("state"), 10), Vec::<String>::new());
    }
}
