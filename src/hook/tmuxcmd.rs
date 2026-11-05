//! The hook's tmux vocabulary: the commands it writes, and the compare-and-set that guards them.
//! Text and order follow agent-hook.py exactly; the tests pin the strings.

use super::{CODEX, FAILED, OPTION, PANE, STATUS, TURN_OVER, WAIT};
use crate::error::Result;
use crate::host::Host;
use crate::model::option_json;
use serde_json::{Map, Value};

/// What one read of the pane gives: its process, its status options, the turn-over value and its identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneState {
    pub pid: String,
    pub tty: String,
    pub status: String,
    pub wait: String,
    pub failed: String,
    pub turn_over: String,
    pub app: String,
}

/// Run `tmux <args>` with the hook's prefix (`-N -S <socket>`).
pub fn run(host: &dyn Host, tmux: &[String], args: &[&str]) -> Result<String> {
    let mut argv = tmux.to_vec();
    argv.extend(args.iter().map(|arg| arg.to_string()));
    host.run(&argv)
}

/// An option value as stored. tmux 3.4 prints `$` as `\$`; JSON never contains `\$`, so undoing it is safe.
pub fn option_value(raw: &str) -> String {
    raw.replace("\\$", "$")
}

/// The identity on a pane. A value that is not a JSON object counts as none.
pub fn published(raw: &str) -> Map<String, Value> {
    match option_json(raw) {
        Ok(Value::Object(identity)) => identity,
        _ => Map::new(),
    }
}

/// One `display-message` read of `PANE`, with the identity unescaped.
pub fn pane_state(host: &dyn Host, tmux: &[String], pane: &str) -> Result<PaneState> {
    let read = run(host, tmux, &["display-message", "-p", "-t", pane, PANE])?;
    // run() strips the output, which takes the trailing empty fields with it.
    let read = option_value(&read);
    let mut fields = read.splitn(7, '\t').map(str::to_string);
    let mut next = || fields.next().unwrap_or_default();
    Ok(PaneState {
        pid: next(),
        tty: next(),
        status: next(),
        wait: next(),
        failed: next(),
        turn_over: next(),
        app: next(),
    })
}

/// `set-option -p -t <pane> <option> '<value>'`, the value quoted for tmux's command parser.
pub fn assign(pane: &str, option: &str, value: &str) -> String {
    format!(
        "set-option -p -t {pane} {option} '{}'",
        value.replace('\'', "'\\''")
    )
}

/// One `set-option -pu` per option, joined by ` ; `.
pub fn unset(pane: &str, options: &[&str]) -> String {
    let commands: Vec<String> = options
        .iter()
        .map(|option| format!("set-option -pu -t {pane} {option}"))
        .collect();
    commands.join(" ; ")
}

/// Let tmux decide the turn is over: the status becomes `done` or `idle`.
pub fn turn_over(pane: &str) -> String {
    format!("set-option -pF -t {pane} {STATUS} '{TURN_OVER}'")
}

/// A tmux format that is true while the option still holds `old`.
pub fn same(option: &str, old: &str) -> String {
    let escaped = old.replace('#', "##").replace(',', "#,").replace('}', "#}");
    format!("#{{==:#{{{option}}},{escaped}}}")
}

/// Run `command` only while the option still holds the value that was read (and `also` still holds).
pub fn set_if(
    host: &dyn Host,
    tmux: &[String],
    pane: &str,
    option: &str,
    old: &str,
    command: &str,
    also: Option<(&str, &str)>,
) -> Result<()> {
    let mut guard = same(option, old);
    if let Some((also_option, also_old)) = also {
        guard = format!("#{{&&:{guard},{}}}", same(also_option, also_old));
    }
    run(host, tmux, &["if-shell", "-F", "-t", pane, &guard, command]).map(|_| ())
}

/// Clear the identity and the status from the pane when the identity on it is still `metadata`'s.
pub fn clear_owned(
    host: &dyn Host,
    tmux: &[String],
    pane: &str,
    metadata: &Map<String, Value>,
) -> Result<()> {
    let read = run(
        host,
        tmux,
        &["show-option", "-p", "-v", "-q", "-t", pane, OPTION],
    )?;
    let raw = option_value(&read);
    if raw.is_empty() {
        return Ok(());
    }
    let current = published(&raw);
    if ["kind", "id", "pid", "birth"]
        .iter()
        .any(|key| current.get(*key) != metadata.get(*key))
    {
        return Ok(());
    }
    // Compare-and-clear inside the server, not a racy read followed by an unset.
    // The status goes with the identity, because both belong to the same agent.
    let clear = unset(pane, &[OPTION, STATUS, WAIT, FAILED, CODEX]);
    set_if(host, tmux, pane, OPTION, &raw, &clear, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHost;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn prefix() -> Vec<String> {
        ["/opt/homebrew/bin/tmux", "-N", "-S", "/tmp/sock"]
            .map(String::from)
            .to_vec()
    }

    /// A host whose tmux answers every call with `answer` and records the command lines.
    fn answering(answer: &str) -> (FakeHost, Rc<RefCell<Vec<String>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&seen);
        let answer = answer.to_string();
        let host = FakeHost::scripted(move |argv| {
            log.borrow_mut().push(argv.join(" "));
            Ok(answer.clone())
        });
        (host, seen)
    }

    // test_agent_status_tmux.py::HookJson

    #[test]
    fn published_identity_survives_tmux_3_4_dollar_escaping() {
        let identity = published(r#"{"kind": "claude", "id": "\$HOME"}"#);
        assert_eq!(
            Value::Object(identity),
            json!({"kind": "claude", "id": "$HOME"})
        );
    }

    #[test]
    fn values_read_from_tmux_3_4_are_unescaped() {
        // The guard compares with the stored value, so the read copy must be the stored text.
        assert_eq!(option_value(r#"{"id": "\$HOME"}"#), r#"{"id": "$HOME"}"#);
        assert_eq!(option_value("working"), "working");
    }

    #[test]
    fn pane_state_unescapes_the_identity() {
        let line = [
            "4242",
            "/dev/ttys001",
            "working",
            "",
            "",
            "idle",
            r#"{"id": "\$x"}"#,
        ]
        .join("\t");
        let (host, _) = answering(&line);
        let state = pane_state(&host, &["tmux".to_string()], "%1").unwrap();
        assert_eq!(state.app, r#"{"id": "$x"}"#);
    }

    #[test]
    fn published_garbage_is_no_identity() {
        assert!(published("{broken").is_empty());
    }

    // The rest pins agent-hook.py:239-305 text for text.

    #[test]
    fn published_accepts_only_objects() {
        for raw in ["", "[1]", "\"text\"", "7", "null", "not json", "{\"a\":"] {
            assert!(published(raw).is_empty(), "{raw}");
        }
        assert_eq!(published(r#"{"pid":4242}"#)["pid"], 4242);
    }

    #[test]
    fn pane_state_reads_the_pane_in_one_call_and_pads_missing_fields() {
        // run() strips the output, which takes the trailing empty fields with it.
        let (host, seen) = answering("99\t/dev/ttys001\tworking");
        let state = pane_state(&host, &prefix(), "%7").unwrap();
        assert_eq!(
            state,
            PaneState {
                pid: "99".into(),
                tty: "/dev/ttys001".into(),
                status: "working".into(),
                ..PaneState::default()
            }
        );
        assert_eq!(
            *seen.borrow(),
            [format!(
                "/opt/homebrew/bin/tmux -N -S /tmp/sock display-message -p -t %7 {PANE}"
            )]
        );
    }

    #[test]
    fn pane_state_keeps_tabs_inside_the_identity() {
        let (host, _) = answering("1\t2\t3\t4\t5\t6\t{\"a\":\"x\ty\"}");
        let state = pane_state(&host, &prefix(), "%1").unwrap();
        assert_eq!(state.app, "{\"a\":\"x\ty\"}");
    }

    #[test]
    fn the_pane_format_names_the_options_in_field_order() {
        let fields: Vec<String> = [
            "pane_pid",
            "pane_tty",
            STATUS,
            WAIT,
            FAILED,
            "E:@agent_turn_over",
            OPTION,
        ]
        .iter()
        .map(|name| format!("#{{{name}}}"))
        .collect();
        assert_eq!(PANE, fields.join("\t"));
    }

    #[test]
    fn assign_quotes_the_value_for_tmux() {
        assert_eq!(
            assign("%7", "@workspace_agent", "working"),
            "set-option -p -t %7 @workspace_agent 'working'"
        );
        assert_eq!(
            assign("%7", OPTION, r#"{"id":"it's"}"#),
            r#"set-option -p -t %7 @workspace_app '{"id":"it'\''s"}'"#
        );
    }

    #[test]
    fn unset_joins_one_command_per_option() {
        assert_eq!(
            unset("%7", &[FAILED]),
            "set-option -pu -t %7 @workspace_agent_failed"
        );
        assert_eq!(
            unset("%7", &[OPTION, STATUS, WAIT]),
            "set-option -pu -t %7 @workspace_app ; set-option -pu -t %7 @workspace_agent ; \
             set-option -pu -t %7 @workspace_agent_wait"
        );
    }

    #[test]
    fn turn_over_lets_tmux_expand_the_status() {
        assert_eq!(
            turn_over("%7"),
            "set-option -pF -t %7 @workspace_agent '#{?#{@agent_turn_over},#{E:@agent_turn_over},idle}'"
        );
    }

    #[test]
    fn same_escapes_what_a_format_would_misread() {
        assert_eq!(
            same("@workspace_agent", "idle"),
            "#{==:#{@workspace_agent},idle}"
        );
        assert_eq!(
            same("@workspace_agent_wait", ""),
            "#{==:#{@workspace_agent_wait},}"
        );
        assert_eq!(
            same("@workspace_app", "{a},#{b}"),
            "#{==:#{@workspace_app},{a#}#,##{b#}}"
        );
    }

    #[test]
    fn set_if_guards_the_command_with_the_value_that_was_read() {
        let (host, seen) = answering("");
        set_if(&host, &prefix(), "%7", STATUS, "idle", "do-it", None).unwrap();
        set_if(
            &host,
            &prefix(),
            "%7",
            STATUS,
            "blocked",
            "do-it",
            Some((WAIT, "5")),
        )
        .unwrap();
        let tmux = "/opt/homebrew/bin/tmux -N -S /tmp/sock";
        assert_eq!(
            *seen.borrow(),
            [
                format!("{tmux} if-shell -F -t %7 #{{==:#{{@workspace_agent}},idle}} do-it"),
                format!(
                    "{tmux} if-shell -F -t %7 #{{&&:#{{==:#{{@workspace_agent}},blocked}},\
                     #{{==:#{{@workspace_agent_wait}},5}}}} do-it"
                ),
            ]
        );
    }

    #[test]
    fn set_if_passes_the_command_as_one_argument() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&calls);
        let host = FakeHost::scripted(move |argv| {
            log.borrow_mut().push(argv.to_vec());
            Ok(String::new())
        });
        set_if(&host, &prefix(), "%7", OPTION, "", "a ; b", None).unwrap();
        let argv = calls.borrow()[0].clone();
        assert_eq!(argv.len(), 4 + 6);
        assert_eq!(argv.last().unwrap(), "a ; b");
    }

    fn identity(session: &str) -> Map<String, Value> {
        let Value::Object(map) =
            json!({"kind": "claude", "id": session, "pid": 4242, "cwd": "/work", "birth": "b"})
        else {
            unreachable!()
        };
        map
    }

    /// A host whose `show-option` gives `raw` and which records the other commands.
    fn pane_holding(raw: &str) -> (FakeHost, Rc<RefCell<Vec<String>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&seen);
        let raw = raw.to_string();
        let host = FakeHost::scripted(move |argv| {
            if argv.iter().any(|a| a == "show-option") {
                log.borrow_mut()
                    .push(format!("read {}", argv[4..].join(" ")));
                return Ok(raw.clone());
            }
            log.borrow_mut().push(argv[4..].join(" "));
            Ok(String::new())
        });
        (host, seen)
    }

    #[test]
    fn clear_owned_clears_with_a_compare_and_set_on_the_identity() {
        let raw = r#"{"kind":"claude","id":"s","pid":4242,"cwd":"/work","birth":"b"}"#;
        let (host, seen) = pane_holding(raw);
        clear_owned(&host, &prefix(), "%7", &identity("s")).unwrap();
        let seen = seen.borrow();
        assert_eq!(seen[0], "read show-option -p -v -q -t %7 @workspace_app");
        assert_eq!(
            seen[1],
            format!(
                "if-shell -F -t %7 #{{==:#{{@workspace_app}},{}}} {}",
                raw.replace(',', "#,").replace('}', "#}"),
                "set-option -pu -t %7 @workspace_app ; set-option -pu -t %7 @workspace_agent ; \
                 set-option -pu -t %7 @workspace_agent_wait ; set-option -pu -t %7 @workspace_agent_failed ; \
                 set-option -pu -t %7 @workspace_codex"
            )
        );
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn clear_owned_leaves_an_identity_that_is_not_the_callers() {
        let (host, seen) =
            pane_holding(r#"{"kind":"claude","id":"other","pid":4242,"cwd":"/work","birth":"b"}"#);
        clear_owned(&host, &prefix(), "%7", &identity("s")).unwrap();
        assert_eq!(seen.borrow().len(), 1, "only the read");
        for raw in ["", "not json", "[1]"] {
            let (host, seen) = pane_holding(raw);
            clear_owned(&host, &prefix(), "%7", &identity("s")).unwrap();
            assert_eq!(seen.borrow().len(), 1, "{raw}");
        }
    }

    #[test]
    fn clear_owned_reads_a_tmux_3_4_escaped_identity_as_stored() {
        let (host, seen) =
            pane_holding(r#"{"kind":"claude","id":"\$HOME","pid":4242,"cwd":"/work","birth":"b"}"#);
        clear_owned(&host, &prefix(), "%7", &identity("$HOME")).unwrap();
        let seen = seen.borrow();
        assert!(seen[1].contains(r#""id":"$HOME""#), "{}", seen[1]);
        assert!(!seen[1].contains("\\$"), "{}", seen[1]);
    }
}
