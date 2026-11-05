//! What the hook knows about processes: the table, what an executable is, and which one owns a pane.
//! Each lookup is one `ps` call, and only for the processes that matter.

use crate::error::{Error, Result, py_repr_str};
use crate::host::Host;
use crate::process::which;
use crate::text::{py_split_ws, py_splitlines, shlex_split};
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where Claude Code keeps the executables it runs.
const CLAUDE_VERSIONS: &str = "/.local/share/claude/versions/";
/// More links than this in one path is a loop (the limit of the kernel on Linux).
const MAX_LINKS: usize = 40;

/// Process id -> (parent id, tty as `ps` prints it, command).
pub type Table = BTreeMap<i32, (i32, String, String)>;

/// `os.path.realpath`: symlinks resolved as far as the path exists, the rest taken as written.
pub fn realpath(path: &str) -> String {
    let mut resolved = if path.starts_with('/') {
        PathBuf::from("/")
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    };
    let mut pending: Vec<String> = path.split('/').rev().map(str::to_string).collect();
    let mut links = 0;
    while let Some(name) = pending.pop() {
        match name.as_str() {
            "" | "." => continue,
            ".." => {
                resolved.pop();
                continue;
            }
            _ => {}
        }
        let next = resolved.join(&name);
        let target = std::fs::symlink_metadata(&next)
            .ok()
            .filter(|meta| meta.file_type().is_symlink() && links < MAX_LINKS)
            .and_then(|_| std::fs::read_link(&next).ok());
        let Some(target) = target else {
            resolved = next;
            continue;
        };
        links += 1;
        let target = target.to_string_lossy().into_owned();
        if target.starts_with('/') {
            resolved = PathBuf::from("/");
        }
        pending.extend(target.split('/').rev().map(str::to_string));
    }
    resolved.to_string_lossy().into_owned()
}

/// `ps`, as a command line: `/bin/ps` if it exists, else the one on `PATH`.
pub fn ps(args: &[&str]) -> Vec<String> {
    static PROGRAM: OnceLock<String> = OnceLock::new();
    let program = PROGRAM.get_or_init(|| {
        if Path::new("/bin/ps").exists() {
            "/bin/ps".to_string()
        } else {
            which("ps").map_or_else(
                || "ps".to_string(),
                |path| path.to_string_lossy().into_owned(),
            )
        }
    });
    let mut argv = vec![program.clone()];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    argv
}

/// Python's `int(text)` for a pid: a decimal number, or the error Python raises.
pub fn py_int(text: &str) -> Result<i32> {
    text.parse().map_err(|_| {
        Error::Msg(format!(
            "invalid literal for int() with base 10: {}",
            py_repr_str(text)
        ))
    })
}

/// The process with this pid, or every process on this tty, or all of them.
pub fn process_table(host: &dyn Host, pid: Option<i32>, tty: Option<&str>) -> Result<Table> {
    let pid_text = pid.map(|pid| pid.to_string());
    let tty_name = tty.map(|tty| tty.strip_prefix("/dev/").unwrap_or(tty));
    let selection: Vec<&str> = match (tty_name, &pid_text) {
        (Some(tty), _) => vec!["-t", tty, "-o"],
        (None, Some(pid)) => vec!["-p", pid, "-o"],
        (None, None) => vec!["-axo"],
    };
    let mut args = selection;
    args.push("pid=,ppid=,tty=,comm=");
    let mut processes = Table::new();
    for line in py_splitlines(&host.run(&ps(&args))?) {
        if let [pid, ppid, tty, command] = py_split_ws(line, 3)[..] {
            processes.insert(
                py_int(pid)?,
                (py_int(ppid)?, tty.to_string(), command.to_string()),
            );
        }
    }
    Ok(processes)
}

/// What the executable is. Use the executable, not occurrences of agent names in shell/tool arguments.
pub fn process_kind(host: &dyn Host, pid: i32, command: &str) -> Result<Option<&'static str>> {
    let name = command.rsplit('/').next().unwrap_or(command);
    if let Some(agent) = ["claude", "codex", "omp", "nvim"]
        .into_iter()
        .find(|agent| *agent == name)
    {
        return Ok(Some(agent));
    }
    if realpath(command).contains(CLAUDE_VERSIONS) {
        return Ok(Some("claude"));
    }
    if name != "bun" && name != "node" {
        return Ok(None);
    }
    let args = shlex_split(&host.run(&ps(&["-ww", "-p", &pid.to_string(), "-o", "args="]))?)?;
    let Some(first) = args.first() else {
        return Ok(None);
    };
    let found =
        which(first).map_or_else(|| first.clone(), |path| path.to_string_lossy().into_owned());
    if realpath(&found).contains(CLAUDE_VERSIONS) {
        return Ok(Some("claude"));
    }
    if let Some(script) = args.get(1) {
        let script = realpath(script);
        if script.ends_with("/@oh-my-pi/pi-coding-agent/dist/cli.js") {
            return Ok(Some("omp"));
        }
        if script.ends_with("/@anthropic-ai/claude-code/cli.js") {
            return Ok(Some("claude"));
        }
        if script.ends_with("/@openai/codex/bin/codex.js") {
            return Ok(Some("codex-wrapper"));
        }
    }
    Ok(None)
}

/// The agent process that owns the pane, as `(pid, start time)`. It must be the only agent between
/// the starting process and the pane's process, of this kind, on the pane's tty and in the
/// foreground, and not a one-shot run.
#[allow(clippy::too_many_arguments)]
pub fn owner(
    host: &dyn Host,
    kind: &str,
    pane_pid: i32,
    pane_tty: &str,
    explicit_pid: Option<i32>,
    start_pid: Option<i32>,
    processes: Option<&Table>,
) -> Result<Option<(i32, String)>> {
    let mut gathered = Table::new();
    let mut pid = start_pid.unwrap_or_else(|| host.parent_pid());
    let mut candidates: Vec<(i32, &'static str, String, Vec<String>)> = Vec::new();
    let mut visited = HashSet::new();
    loop {
        if pid <= 1 || visited.contains(&pid) {
            return Ok(None);
        }
        if processes.is_none() {
            gathered.extend(process_table(host, Some(pid), None)?);
        }
        let Some((parent, tty, command)) = processes.unwrap_or(&gathered).get(&pid).cloned() else {
            return Ok(None);
        };
        visited.insert(pid);
        if let Some(app) = process_kind(host, pid, &command)? {
            let args =
                shlex_split(&host.run(&ps(&["-ww", "-p", &pid.to_string(), "-o", "args="]))?)?;
            // A local Codex app-server is a child of the actual interactive CLI.
            // Standalone app-server, exec, print, and inherited tool jobs are not panes.
            let app_server = app == "codex" && args.get(1).is_some_and(|arg| arg == "app-server");
            if !app_server && app != "codex-wrapper" {
                candidates.push((pid, app, tty, args));
            }
        }
        if pid == pane_pid {
            break;
        }
        pid = parent;
    }
    // Reject nested agents/editors, even when a tool inherited TMUX and TMUX_PANE.
    let [(pid, app, tty, args)] = &candidates[..] else {
        return Ok(None);
    };
    let pid = *pid;
    if *app != kind || explicit_pid.is_some_and(|explicit| pid != explicit) {
        return Ok(None);
    }
    if *tty != pane_tty.strip_prefix("/dev/").unwrap_or(pane_tty) {
        return Ok(None);
    }
    let groups = host.run(&ps(&["-p", &pid.to_string(), "-o", "pgid=,tpgid="]))?;
    let groups: Vec<&str> = py_split_ws(&groups, usize::MAX);
    let [group, foreground] = groups[..] else {
        return Ok(None);
    };
    if group != foreground || py_int(group)? <= 0 {
        return Ok(None);
    }
    let flags = args.iter().skip(1).map(String::as_str);
    let one_shot: &[&str] = match kind {
        "claude" => &["-p", "--print"],
        "omp" => &["-p", "--print", "--mode", "--no-session"],
        _ => &[],
    };
    if flags.clone().any(|arg| one_shot.contains(&arg)) {
        return Ok(None);
    }
    if kind == "codex"
        && args.get(1).is_some_and(|arg| {
            ["exec", "e", "review", "app-server", "mcp-server", "debug"].contains(&arg.as_str())
        })
    {
        return Ok(None);
    }
    let birth = host.run(&ps(&["-p", &pid.to_string(), "-o", "lstart="]))?;
    Ok((!birth.is_empty()).then_some((pid, birth)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeHost, Proc, TempDir};

    /// The `ps -p <pid> -o pid=,...` lookups a host has seen, as pids, in order.
    fn scanned(host: &FakeHost) -> Vec<i32> {
        host.calls_to("ps")
            .iter()
            .filter(|call| call[1] == "-p" && call[3] == "-o" && call[4] == "pid=,ppid=,tty=,comm=")
            .map(|call| call[2].parse().unwrap())
            .collect()
    }

    fn claude(ppid: i32) -> Proc {
        Proc {
            args: "claude".into(),
            pgid: 200,
            tpgid: 200,
            lstart: "birth".into(),
            ..Proc::new(ppid, "ttys001", "claude")
        }
    }

    // test_performance.py::ProcessLookups

    #[test]
    fn hook_checks_only_ancestors_and_still_rejects_nested_agents() {
        let mut host = FakeHost::new();
        host.procs
            .insert(300, Proc::new(200, "??", "/usr/bin/python3"));
        host.procs.insert(200, claude(100));
        host.procs.insert(100, Proc::new(1, "ttys001", "/bin/zsh"));
        assert_eq!(
            owner(&host, "claude", 100, "/dev/ttys001", None, Some(300), None).unwrap(),
            Some((200, "birth".to_string()))
        );
        assert_eq!(scanned(&host), [300, 200, 100]);
        assert!(
            host.calls_to("ps")
                .iter()
                .all(|call| call[2] != "-axo" && call[1] != "-axo"),
            "the status callback never scans every process"
        );

        host.procs.insert(
            300,
            Proc {
                args: "codex".into(),
                ..Proc::new(200, "ttys001", "codex")
            },
        );
        assert_eq!(
            owner(&host, "claude", 100, "/dev/ttys001", None, Some(300), None).unwrap(),
            None
        );
    }

    #[test]
    fn native_executables_do_not_spawn_ps() {
        let host = FakeHost::new();
        for command in ["/bin/zsh", "/usr/bin/python3", "/usr/bin/sleep"] {
            assert_eq!(process_kind(&host, 12, command).unwrap(), None);
        }
        assert_eq!(
            process_kind(&host, 12, "/Users/me/.local/share/claude/versions/2.1.296").unwrap(),
            Some("claude")
        );
        assert!(host.calls.borrow().is_empty(), "unnecessary ps");
    }

    // Beyond the Python tests.

    #[test]
    fn process_kind_names_an_agent_by_its_executable() {
        let host = FakeHost::new();
        for name in ["claude", "codex", "omp", "nvim"] {
            assert_eq!(process_kind(&host, 1, name).unwrap(), Some(name));
            assert_eq!(
                process_kind(&host, 1, &format!("/usr/local/bin/{name}")).unwrap(),
                Some(name)
            );
        }
        assert_eq!(process_kind(&host, 1, "claude-code-helper").unwrap(), None);
        assert!(host.calls.borrow().is_empty());
    }

    fn running(args: &str) -> FakeHost {
        let mut host = FakeHost::new();
        host.procs.insert(
            12,
            Proc {
                args: args.into(),
                ..Proc::new(1, "ttys001", "node")
            },
        );
        host
    }

    #[test]
    fn a_node_or_bun_script_is_told_apart_by_its_path() {
        for (args, expected) in [
            (
                "node /x/node_modules/@oh-my-pi/pi-coding-agent/dist/cli.js",
                Some("omp"),
            ),
            (
                "node /x/@anthropic-ai/claude-code/cli.js --resume",
                Some("claude"),
            ),
            ("node /x/@openai/codex/bin/codex.js", Some("codex-wrapper")),
            ("node /x/other/cli.js", None),
            ("node", None),
            ("", None),
            (
                "/Users/me/.local/share/claude/versions/2.1.296 --flag",
                Some("claude"),
            ),
        ] {
            let host = running(args);
            assert_eq!(process_kind(&host, 12, "node").unwrap(), expected, "{args}");
            assert_eq!(host.calls_to("ps").len(), 1, "{args}: exactly one ps");
        }
        assert!(process_kind(&running("node 'unterminated"), 12, "bun").is_err());
        assert!(
            process_kind(&FakeHost::new(), 12, "node").is_err(),
            "ps fails"
        );
    }

    #[test]
    fn process_table_parses_each_ps_form() {
        let mut host = FakeHost::new();
        host.procs.insert(5, Proc::new(1, "ttys001", "/bin/zsh -l"));
        host.procs.insert(6, Proc::new(5, "ttys002", "claude"));
        let one = process_table(&host, Some(5), None).unwrap();
        assert_eq!(
            one,
            Table::from([(5, (1, "ttys001".into(), "/bin/zsh -l".into()))])
        );
        let all = process_table(&host, None, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[&6], (5, "ttys002".to_string(), "claude".to_string()));
        let tty = process_table(&host, None, Some("/dev/ttys002")).unwrap();
        assert_eq!(tty.keys().copied().collect::<Vec<_>>(), [6]);
        let calls = host.calls_to("ps");
        assert!(calls[0][0].ends_with("ps"));
        assert_eq!(calls[0][1..], ["-p", "5", "-o", "pid=,ppid=,tty=,comm="]);
        assert_eq!(calls[1][1..], ["-axo", "pid=,ppid=,tty=,comm="]);
        assert_eq!(
            calls[2][1..],
            ["-t", "ttys002", "-o", "pid=,ppid=,tty=,comm="]
        );
        assert!(process_table(&host, Some(99), None).is_err());
    }

    #[test]
    fn owner_stops_at_a_process_it_does_not_know_or_one_it_has_seen() {
        let mut host = FakeHost::new();
        host.procs.insert(30, Proc::new(20, "ttys001", "claude"));
        let gone = owner(&host, "claude", 10, "ttys001", None, Some(30), None).unwrap_err();
        assert!(
            gone.to_string().contains("no such"),
            "ps fails for a pid that is gone: {gone}"
        );
        let table = Table::from([(30, (30, "ttys001".to_string(), "sleep".to_string()))]);
        assert_eq!(
            owner(&host, "claude", 10, "ttys001", None, Some(30), Some(&table)).unwrap(),
            None,
            "a loop"
        );
        let none = Table::new();
        assert_eq!(
            owner(&host, "claude", 10, "ttys001", None, Some(30), Some(&none)).unwrap(),
            None,
            "unknown pid"
        );
        assert_eq!(
            owner(&host, "claude", 10, "ttys001", None, Some(1), Some(&none)).unwrap(),
            None,
            "pid 1 is the end"
        );
    }

    #[test]
    fn owner_uses_the_supplied_table_and_starts_at_the_parent() {
        let mut host = FakeHost::new();
        host.parent = 200;
        host.procs.insert(200, claude(100));
        host.procs.insert(100, Proc::new(1, "ttys001", "zsh"));
        let table: Table = [
            (200, (100, "ttys001".to_string(), "claude".to_string())),
            (100, (1, "ttys001".to_string(), "zsh".to_string())),
        ]
        .into();
        assert_eq!(
            owner(&host, "claude", 100, "ttys001", None, None, Some(&table)).unwrap(),
            Some((200, "birth".to_string()))
        );
        assert!(
            scanned(&host).is_empty(),
            "the supplied table is not read again"
        );
    }

    fn pane_with(agent: Proc) -> FakeHost {
        let mut host = FakeHost::new();
        host.parent = 200;
        host.procs.insert(200, agent);
        host.procs.insert(100, Proc::new(1, "ttys001", "zsh"));
        host
    }

    fn found(host: &FakeHost, kind: &str, explicit: Option<i32>) -> Option<(i32, String)> {
        owner(host, kind, 100, "/dev/ttys001", explicit, None, None).unwrap()
    }

    #[test]
    fn owner_rejects_what_is_not_an_interactive_foreground_agent_of_the_kind() {
        let ok = |args: &str, kind: &str| {
            let agent = Proc {
                args: args.into(),
                pgid: 200,
                tpgid: 200,
                lstart: "b".into(),
                ..Proc::new(100, "ttys001", kind)
            };
            found(&pane_with(agent), kind, None)
        };
        assert!(ok("claude --resume x", "claude").is_some());
        assert_eq!(ok("claude -p hi", "claude"), None);
        assert_eq!(ok("claude --print hi", "claude"), None);
        assert!(ok("omp --model x", "omp").is_some());
        for flag in ["-p", "--print", "--mode", "--no-session"] {
            assert_eq!(ok(&format!("omp {flag}"), "omp"), None, "{flag}");
        }
        for sub in ["exec", "e", "review", "app-server", "mcp-server", "debug"] {
            assert_eq!(ok(&format!("codex {sub}"), "codex"), None, "{sub}");
        }
        assert!(ok("codex", "codex").is_some());
        // The kind has to be the asked one.
        let agent = Proc {
            args: "omp".into(),
            pgid: 200,
            tpgid: 200,
            lstart: "b".into(),
            ..Proc::new(100, "ttys001", "omp")
        };
        assert_eq!(found(&pane_with(agent), "claude", None), None);
    }

    #[test]
    fn owner_checks_the_pid_the_tty_the_foreground_group_and_the_start_time() {
        let agent = |tty: &str, pgid, tpgid, lstart: &str| Proc {
            args: "claude".into(),
            pgid,
            tpgid,
            lstart: lstart.into(),
            ..Proc::new(100, tty, "claude")
        };
        assert_eq!(
            found(
                &pane_with(agent("ttys001", 200, 200, "b")),
                "claude",
                Some(200)
            ),
            Some((200, "b".into()))
        );
        assert_eq!(
            found(
                &pane_with(agent("ttys001", 200, 200, "b")),
                "claude",
                Some(201)
            ),
            None,
            "not the asked pid"
        );
        assert_eq!(
            found(&pane_with(agent("ttys009", 200, 200, "b")), "claude", None),
            None,
            "another tty"
        );
        assert_eq!(
            found(&pane_with(agent("ttys001", 200, 300, "b")), "claude", None),
            None,
            "background"
        );
        assert_eq!(
            found(&pane_with(agent("ttys001", 0, 0, "b")), "claude", None),
            None,
            "no group"
        );
        assert_eq!(
            found(&pane_with(agent("ttys001", 200, 200, "")), "claude", None),
            None,
            "no start time"
        );
    }

    #[test]
    fn owner_needs_exactly_one_agent_on_the_way() {
        let mut host = pane_with(claude(100));
        host.procs.get_mut(&100).unwrap().comm = "claude".into();
        host.procs.get_mut(&100).unwrap().args = "claude".into();
        assert_eq!(
            found(&host, "claude", None),
            None,
            "the pane's own process is an agent too"
        );
        let host = pane_with(Proc::new(100, "ttys001", "zsh"));
        assert_eq!(found(&host, "claude", None), None, "no agent at all");
    }

    #[test]
    fn a_codex_wrapper_and_a_local_app_server_do_not_count() {
        let mut host = pane_with(Proc {
            args: "node /x/@openai/codex/bin/codex.js".into(),
            ..Proc::new(100, "ttys001", "node")
        });
        host.procs.insert(
            300,
            Proc {
                args: "codex".into(),
                pgid: 300,
                tpgid: 300,
                lstart: "b".into(),
                ..Proc::new(200, "ttys001", "codex")
            },
        );
        host.parent = 300;
        assert_eq!(found(&host, "codex", None), Some((300, "b".into())));
        host.procs.get_mut(&300).unwrap().args = "codex app-server".into();
        assert_eq!(
            found(&host, "codex", None),
            None,
            "an app-server is not a pane's agent"
        );
    }

    #[test]
    fn ps_is_the_bin_one_when_it_exists() {
        let argv = ps(&["-p", "1"]);
        assert_eq!(argv[1..], ["-p", "1"]);
        if std::path::Path::new("/bin/ps").exists() {
            assert_eq!(argv[0], "/bin/ps");
        }
    }

    #[test]
    fn py_int_reads_a_decimal_number_and_fails_like_python() {
        assert_eq!(py_int("4242").unwrap(), 4242);
        assert_eq!(py_int("+7").unwrap(), 7);
        assert_eq!(py_int("-3").unwrap(), -3);
        assert_eq!(
            py_int("12a").unwrap_err().to_string(),
            "invalid literal for int() with base 10: '12a'"
        );
        assert!(py_int("").is_err());
        assert!(py_int("99999999999").is_err());
    }

    #[test]
    fn realpath_resolves_symlinks_and_keeps_what_does_not_exist() {
        let dir = TempDir::new();
        let real = dir.path().canonicalize().unwrap();
        std::fs::create_dir(real.join("target")).unwrap();
        std::os::unix::fs::symlink(real.join("target"), real.join("link")).unwrap();
        std::os::unix::fs::symlink("target", real.join("relative")).unwrap();
        let base = real.to_str().unwrap();
        assert_eq!(realpath(&format!("{base}/link")), format!("{base}/target"));
        assert_eq!(
            realpath(&format!("{base}/relative/x/y")),
            format!("{base}/target/x/y")
        );
        assert_eq!(
            realpath(&format!("{base}/missing/./a/../b")),
            format!("{base}/missing/b")
        );
        assert_eq!(
            realpath(&format!("{base}//link/")),
            format!("{base}/target")
        );
        assert_eq!(realpath("/"), "/");
        assert_eq!(realpath("/.."), "/");
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(
            realpath("nothing-here"),
            format!("{}/nothing-here", cwd.display())
        );
        std::os::unix::fs::symlink("loop", real.join("loop")).unwrap();
        assert_eq!(
            realpath(&format!("{base}/loop/x")),
            format!("{base}/loop/x")
        );
    }
}
