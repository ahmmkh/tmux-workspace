//! The tmux client: one process per call, Python-compatible results and errors.

use crate::error::{Error, Result, fail};
use crate::process::{Output, Spawn, spawn, which};
use std::collections::HashMap;
use std::time::Duration;

/// Separates the fields of a `-F` format. tmux prints it as the text `\037` when it writes to a pipe.
pub const SEP: char = '\x1f';

const FALLBACK_PROGRAM: &str = "/opt/homebrew/bin/tmux";
const CONTROL_CHARACTERS: &str =
    "A workspace name or directory contains unsupported control characters";

/// One row of `-F` output: values by format name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fields(HashMap<String, String>);

impl Fields {
    /// The value, or an empty string when the row has no such name.
    pub fn get(&self, name: &str) -> &str {
        self.0.get(name).map_or("", String::as_str)
    }

    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        self.0.insert(name.to_string(), value.into());
    }
}

/// A tmux command line: the program, which server to talk to, and how long a call may take.
#[derive(Clone, Debug)]
pub struct Tmux {
    program: String,
    socket: Option<String>,
    timeout: Option<Duration>,
}

impl Default for Tmux {
    fn default() -> Self {
        Self::new()
    }
}

impl Tmux {
    /// `tmux` from `PATH`, or Homebrew's. The server is the one `TMUX` names, as for any tmux command.
    pub fn new() -> Self {
        let program = which("tmux").map_or_else(
            || FALLBACK_PROGRAM.to_string(),
            |path| path.to_string_lossy().into_owned(),
        );
        Tmux {
            program,
            socket: None,
            timeout: None,
        }
    }

    /// Talk to the server on this socket, and never start one (`-N -S <path>`).
    pub fn socket(mut self, path: &str) -> Self {
        self.socket = Some(path.to_string());
        self
    }

    /// Kill a call that takes longer.
    pub fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }

    /// The command line for `args`.
    pub fn argv(&self, args: &[&str]) -> Vec<String> {
        let mut argv = vec![self.program.clone()];
        if let Some(socket) = &self.socket {
            argv.extend(["-N".to_string(), "-S".to_string(), socket.clone()]);
        }
        argv.extend(args.iter().map(|a| a.to_string()));
        argv
    }

    /// Run a tmux command and return its output without the trailing newlines. A non-zero exit is an error.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let out = self.call(args)?;
        if out.status != 0 {
            return Err(Error::Command {
                argv: self.argv(args),
                status: out.status,
            });
        }
        Ok(finish(&out.stdout, args))
    }

    /// Like `run`, but a failure gives whatever the command printed, which is usually nothing.
    pub fn quiet(&self, args: &[&str]) -> String {
        self.call(args)
            .map(|out| finish(&out.stdout, args))
            .unwrap_or_default()
    }

    /// The value of a tmux option: the pane's own when `target` names one, else the global one.
    pub fn option(&self, name: &str, target: Option<&str>) -> String {
        let mut args = vec!["show-options", "-qv"];
        push_scope(&mut args, target);
        args.push(name);
        self.quiet(&args)
    }

    /// Set a tmux option, on the pane `target` or globally.
    pub fn put(&self, name: &str, value: &str, target: Option<&str>) -> Result<()> {
        let mut args = vec!["set-option"];
        push_scope(&mut args, target);
        args.extend([name, value]);
        self.run(&args).map(|_| ())
    }

    /// Run a listing command (`list-panes`, `list-windows`, ...) with a `-F` format of `names`, one row per line.
    pub fn fields(&self, command: &str, names: &[&str], args: &[&str]) -> Result<Vec<Fields>> {
        let format = format_of(names);
        let mut all = vec![command];
        all.extend(args);
        all.extend(["-F", &format]);
        parse_fields(&self.run(&all)?, names)
    }

    /// Read the format values `names` of one target (the current one when `None`) in a single call.
    pub fn read(&self, target: Option<&str>, names: &[&str]) -> Result<Fields> {
        let format = format_of(names);
        let mut args = vec!["display-message", "-p"];
        if let Some(target) = target.filter(|t| !t.is_empty()) {
            args.extend(["-t", target]);
        }
        args.push(&format);
        row(&self.run(&args)?, names)
    }

    /// Run several commands in one tmux call, joined by `;` arguments. Returns the output of them all.
    pub fn chain(&self, cmds: &[Vec<String>]) -> Result<String> {
        let mut args: Vec<&str> = Vec::new();
        for (i, cmd) in cmds.iter().enumerate() {
            if i > 0 {
                args.push(";");
            }
            args.extend(cmd.iter().map(String::as_str));
        }
        if args.is_empty() {
            // A bare `tmux` would start a session.
            return Ok(String::new());
        }
        self.run(&args)
    }

    fn call(&self, args: &[&str]) -> Result<Output> {
        spawn(&Spawn {
            argv: self.argv(args),
            timeout: self.timeout,
            ..Spawn::default()
        })
    }
}

/// Undo tmux 3.4's escaping of the separator in a format call's output. Text that did not come
/// from a format, such as `capture-pane`, is left as it is.
pub fn unescape_sep(output: &str, args: &[&str]) -> String {
    if args.iter().any(|a| a.contains(SEP)) {
        output.replace("\\037", &SEP.to_string())
    } else {
        output.to_string()
    }
}

fn finish(stdout: &str, args: &[&str]) -> String {
    unescape_sep(stdout.trim_end_matches('\n'), args)
}

fn push_scope<'a>(args: &mut Vec<&'a str>, target: Option<&'a str>) {
    match target.filter(|t| !t.is_empty()) {
        Some(target) => args.extend(["-p", "-t", target]),
        None => args.push("-g"),
    }
}

/// `#{a}<SEP>#{b}`...
fn format_of(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("#{{{name}}}"))
        .collect::<Vec<_>>()
        .join(&SEP.to_string())
}

/// Split `-F` output into rows. A value that holds a line break or the separator throws the count off.
fn parse_fields(output: &str, names: &[&str]) -> Result<Vec<Fields>> {
    if output.is_empty() {
        return Ok(Vec::new());
    }
    py_splitlines(output)
        .into_iter()
        .map(|line| row(line, names))
        .collect()
}

fn row(line: &str, names: &[&str]) -> Result<Fields> {
    let values: Vec<&str> = line.split(SEP).collect();
    if values.len() != names.len() {
        return fail(CONTROL_CHARACTERS);
    }
    let mut fields = Fields::default();
    for (name, value) in names.iter().zip(values) {
        fields.set(name, value);
    }
    Ok(fields)
}

/// Python's `str.splitlines()`, which breaks on more than `\n`.
fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            lines.push(&text[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && chars.next_if(|&(_, next)| next == '\n').is_some() {
                start += 1;
            }
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::ScratchTmux;

    fn client(scratch: &ScratchTmux) -> Tmux {
        Tmux::new().socket(scratch.socket())
    }

    #[test]
    fn separator_written_as_text_is_restored_for_any_format_call() {
        assert_eq!(
            unescape_sep("a\\037b", &["display-message", "-p", "#{a}\x1f#{b}"]),
            "a\x1fb"
        );
        assert_eq!(unescape_sep("a\\037b", &["capture-pane", "-p"]), "a\\037b"); // captured text untouched
    }

    #[test]
    fn fields_rejects_a_value_with_a_separator() {
        let e = parse_fields("a\x1fb\x1fc", &["x", "y"]).unwrap_err();
        assert_eq!(
            e.to_string(),
            "A workspace name or directory contains unsupported control characters"
        );
        let e = parse_fields("a\x1fb\nonly-one", &["x", "y"]).unwrap_err();
        assert!(matches!(e, Error::Msg(_)));
    }

    #[test]
    fn parse_fields_splits_rows_and_names_the_values() {
        assert!(parse_fields("", &["x", "y"]).unwrap().is_empty());
        let rows = parse_fields("a\x1fb\n\x1fd\n", &["x", "y"]).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].get("x"), rows[0].get("y")), ("a", "b"));
        assert_eq!((rows[1].get("x"), rows[1].get("y")), ("", "d"));
    }

    #[test]
    fn python_line_breaks_split_rows_like_splitlines() {
        let rows = parse_fields("a\x1fb\r\nc\x1fd\u{2028}e\x1ff\x0bg\x1fh", &["x", "y"]).unwrap();
        let xs: Vec<&str> = rows.iter().map(|r| r.get("x")).collect();
        assert_eq!(xs, ["a", "c", "e", "g"]);
        // \x1f is not a line break, but \x1e is: a lone separator-less row is a count error.
        assert!(parse_fields("a\x1e\x1fb", &["x", "y"]).is_err());
    }

    #[test]
    fn a_missing_field_reads_as_empty() {
        let mut f = Fields::default();
        assert_eq!(f.get("nope"), "");
        f.set("k", "v");
        assert_eq!(f.get("k"), "v");
        f.set("k", "w");
        assert_eq!(f.get("k"), "w");
    }

    #[test]
    fn argv_puts_the_socket_flags_after_the_program() {
        let t = Tmux::new().socket("/tmp/s");
        let argv = t.argv(&["list-sessions"]);
        assert_eq!(&argv[1..], ["-N", "-S", "/tmp/s", "list-sessions"]);
        let argv = Tmux::new().argv(&["list-sessions"]);
        assert_eq!(&argv[1..], ["list-sessions"]);
    }

    #[test]
    fn run_returns_stdout_without_trailing_newlines() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch);
        assert_eq!(
            t.run(&["display-message", "-p", "-t", "alpha", "#{session_name}"])
                .unwrap(),
            "alpha"
        );
        assert_eq!(
            t.run(&["display-message", "-p", "-t", "alpha", "x\n\n"])
                .unwrap(),
            "x"
        );
    }

    #[test]
    fn run_fails_like_python_on_a_non_zero_status() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch);
        let e = t.run(&["kill-session", "-t", "=missing"]).unwrap_err();
        let text = e.to_string();
        assert!(
            text.starts_with("Command '['") && text.contains("'kill-session', '-t', '=missing']"),
            "{text}"
        );
        assert!(
            text.ends_with("' returned non-zero exit status 1."),
            "{text}"
        );
    }

    #[test]
    fn run_restores_the_separator_tmux_prints_as_text() {
        let scratch = ScratchTmux::new();
        let out = client(&scratch)
            .run(&[
                "display-message",
                "-p",
                "-t",
                "alpha",
                "#{session_name}\x1f#{window_name}",
            ])
            .unwrap();
        assert_eq!(out, "alpha\x1fone");
    }

    #[test]
    fn quiet_gives_the_output_or_nothing() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch);
        assert_eq!(
            t.quiet(&["display-message", "-p", "-t", "alpha", "#{window_name}"]),
            "one"
        );
        assert_eq!(t.quiet(&["kill-session", "-t", "=missing"]), "");
    }

    #[test]
    fn a_dead_server_fails_run_and_is_quiet() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch);
        scratch.tmux(&["kill-server"]);
        assert!(t.run(&["list-sessions"]).is_err());
        assert_eq!(t.quiet(&["list-sessions"]), "");
    }

    #[test]
    fn options_round_trip_globally_and_per_pane() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch);
        assert_eq!(t.option("@workspace_x", None), "");
        t.put("@workspace_x", "global", None).unwrap();
        assert_eq!(t.option("@workspace_x", None), "global");
        let pane = scratch.expand("alpha", "#{pane_id}");
        assert_eq!(
            t.option("@workspace_x", Some(&pane)),
            "",
            "a pane option is separate from the global one"
        );
        t.put("@workspace_x", "mine", Some(&pane)).unwrap();
        assert_eq!(t.option("@workspace_x", Some(&pane)), "mine");
        assert_eq!(t.option("@workspace_x", None), "global");
        assert_eq!(
            scratch.tmux(&["show-options", "-pqv", "-t", &pane, "@workspace_x"]),
            "mine"
        );
    }

    #[test]
    fn put_fails_for_a_missing_target() {
        let scratch = ScratchTmux::new();
        assert!(client(&scratch).put("@x", "1", Some("%999")).is_err());
    }

    #[test]
    fn fields_lists_rows_by_name() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch);
        scratch.tmux(&[
            "new-window",
            "-d",
            "-t",
            "alpha:",
            "-n",
            "two",
            "exec sleep 300",
        ]);
        let rows = t
            .fields(
                "list-windows",
                &["session_name", "window_name", "window_index"],
                &["-t", "alpha"],
            )
            .unwrap();
        let seen: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r.get("session_name"),
                    r.get("window_name"),
                    r.get("window_index"),
                )
            })
            .collect();
        assert_eq!(seen, [("alpha", "one", "0"), ("alpha", "two", "1")]);
    }

    #[test]
    fn fields_is_empty_when_tmux_prints_nothing() {
        let scratch = ScratchTmux::new();
        let rows = client(&scratch)
            .fields(
                "list-windows",
                &["window_name"],
                &["-t", "alpha", "-f", "0"],
            )
            .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn fields_reports_a_name_with_a_control_character() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch);
        scratch.tmux(&[
            "set-option",
            "-p",
            "-t",
            "alpha:one",
            "@workspace_label",
            "a\x1fb",
        ]);
        let e = t
            .fields(
                "list-panes",
                &["@workspace_label", "pane_index"],
                &["-t", "alpha"],
            )
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "A workspace name or directory contains unsupported control characters"
        );
    }

    #[test]
    fn read_gets_several_values_in_one_call() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch);
        let pane = scratch.expand("alpha", "#{pane_id}");
        t.put("@workspace_label", "build", Some(&pane)).unwrap();
        let f = t
            .read(
                Some(&pane),
                &[
                    "session_name",
                    "@workspace_label",
                    "@workspace_missing",
                    "window_name",
                ],
            )
            .unwrap();
        assert_eq!(
            (
                f.get("session_name"),
                f.get("@workspace_label"),
                f.get("@workspace_missing"),
                f.get("window_name")
            ),
            ("alpha", "build", "", "one")
        );
        let f = t.read(None, &["@workspace_missing"]).unwrap();
        assert_eq!(f.get("@workspace_missing"), "");
    }

    #[test]
    fn chain_runs_several_commands_in_one_call() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch);
        let cmds = vec![
            vec![
                "set-option".to_string(),
                "-g".into(),
                "@a".into(),
                "1".into(),
            ],
            vec![
                "set-option".to_string(),
                "-g".into(),
                "@b".into(),
                "2".into(),
            ],
            vec![
                "display-message".to_string(),
                "-p".into(),
                "#{@a}-#{@b}".into(),
            ],
        ];
        assert_eq!(t.chain(&cmds).unwrap(), "1-2");
        assert_eq!(
            (t.option("@a", None), t.option("@b", None)),
            ("1".into(), "2".into())
        );
        assert_eq!(
            t.chain(&[]).unwrap(),
            "",
            "no commands is not a bare `tmux`"
        );
    }

    #[test]
    fn a_timeout_ends_a_stuck_call() {
        let scratch = ScratchTmux::new();
        let t = client(&scratch).timeout(std::time::Duration::from_millis(200));
        let e = t.run(&["run-shell", "-t", "alpha", "sleep 5"]).unwrap_err();
        assert!(e.to_string().starts_with("timed out: "), "{e}");
    }
}
