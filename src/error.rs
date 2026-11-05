//! The error type every command returns, with Python-compatible message text.

use std::fmt;
use std::io;

/// What can go wrong. `Display` gives the text printed after `Workspace error: `.
#[derive(Debug)]
pub enum Error {
    /// A plain message: Python's `ValueError`.
    Msg(String),
    /// A checked child process that exited non-zero: Python's `CalledProcessError`.
    Command {
        argv: Vec<String>,
        status: i32,
    },
    Io(io::Error),
    Json(String),
    /// End of input at a prompt: Python's `EOFError`. Not a failure; the command exits 0.
    Eof,
}

pub type Result<T> = std::result::Result<T, Error>;

/// `Err(Error::Msg(msg))`.
pub fn fail<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Msg(msg.into()))
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Msg(text) | Error::Json(text) => f.write_str(text),
            Error::Command { argv, status } => {
                let items: Vec<String> = argv.iter().map(|a| py_repr_str(a)).collect();
                write!(
                    f,
                    "Command '[{}]' returned non-zero exit status {status}.",
                    items.join(", ")
                )
            }
            Error::Io(e) => e.fmt(f),
            Error::Eof => f.write_str("EOF when reading a line"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Python's `repr(str)`: single quotes, unless the text holds a `'` and no `"`.
///
/// Control characters (and the two Unicode line separators) are escaped as Python does, as
/// `\xNN` or `\uNNNN`. Other non-printable code points (format characters, unassigned ones)
/// are left as they are.
pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else {
                    out.push_str(&format!("\\u{n:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_error_reads_like_python() {
        let e = Error::Command {
            argv: vec!["tmux".into(), "it's".into()],
            status: 1,
        };
        assert_eq!(
            e.to_string(),
            "Command '['tmux', \"it's\"]' returned non-zero exit status 1."
        );
    }

    #[test]
    fn command_error_quotes_each_argument_like_a_python_list_repr() {
        let argv = ["tmux", "kill-session", "-t", "=x"]
            .map(String::from)
            .to_vec();
        let e = Error::Command { argv, status: 1 };
        assert_eq!(
            e.to_string(),
            "Command '['tmux', 'kill-session', '-t', '=x']' returned non-zero exit status 1."
        );
        let argv = vec![
            "a\\b".to_string(),
            "x'y\"z".into(),
            "tab\t".into(),
            "nl\n".into(),
            "é".into(),
            "\u{1f}".into(),
            "\u{85}".into(),
        ];
        let e = Error::Command { argv, status: 2 };
        assert_eq!(
            e.to_string(),
            r#"Command '['a\\b', 'x\'y"z', 'tab\t', 'nl\n', 'é', '\x1f', '\x85']' returned non-zero exit status 2."#
        );
    }

    #[test]
    fn repr_escapes_the_unicode_line_separators_with_four_digits() {
        assert_eq!(py_repr_str("\u{2028}\u{2029}"), "'\\u2028\\u2029'");
        assert_eq!(py_repr_str(""), "''");
    }

    #[test]
    fn other_errors_display_their_text() {
        assert_eq!(Error::Msg("boom".into()).to_string(), "boom");
        assert_eq!(Error::Json("bad json".into()).to_string(), "bad json");
        assert_eq!(Error::Eof.to_string(), "EOF when reading a line");
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        assert_eq!(Error::from(io).to_string(), "gone");
    }

    #[test]
    fn fail_builds_a_msg_error() {
        let r: Result<()> = fail("nope");
        assert!(matches!(r, Err(Error::Msg(m)) if m == "nope"));
    }
}
