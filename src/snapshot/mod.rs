//! Saved workspaces: `save` and the autosave `tick` write them, `restore` and `init` bring them back,
//! `pane-start` replays them into each new pane, and `history` shows a pane's saved terminal text.

mod apps;
mod restore;
mod save;
mod store;

pub use apps::{ProcessTable, live_app, process_table};
pub use restore::{history, history_in, initialize, initialize_in, restore, restore_in, tick};
pub use save::{save, save_in};
pub use store::{atomic, create_temporary, lock, lock_in, read_snapshot, read_snapshot_in};

#[cfg(test)]
#[allow(unused_imports)]
use {
    crate::error::{Error, Result, fail},
    crate::hook::process::{self as procs, Table, py_int},
    crate::host::Host,
    crate::model::{BOOTSTRAP, PARKING},
    crate::paths,
    crate::pyjson::{Style, dumps},
    crate::tmux::{Fields, SEP, Tmux},
    apps::*,
    restore::*,
    save::*,
    serde_json::{Value, json},
    std::fs::{self, File},
    std::os::unix::fs::PermissionsExt,
    std::path::{Path, PathBuf},
    std::time::Duration,
    store::*,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use serde_json::json;

    fn write(path: &Path, value: &Value) {
        std::fs::write(path, value.to_string()).unwrap();
    }

    /// A state folder whose latest snapshot is `data`.
    fn state_with(data: Value) -> (TempDir, PathBuf) {
        let dir = TempDir::new();
        let snapshot = dir.path().join("snapshot.json");
        write(&snapshot, &data);
        write(&dir.path().join("latest.json"), &json!({"path": snapshot}));
        (dir, snapshot)
    }

    #[test]
    fn no_latest_file_means_no_snapshot() {
        let dir = TempDir::new();
        assert!(read_snapshot_in(dir.path()).unwrap().is_none());
    }

    #[test]
    fn a_latest_file_whose_snapshot_is_gone_means_no_snapshot() {
        let dir = TempDir::new();
        write(
            &dir.path().join("latest.json"),
            &json!({"path": dir.path().join("gone.json")}),
        );
        assert!(read_snapshot_in(dir.path()).unwrap().is_none());
    }

    #[test]
    fn the_latest_snapshot_is_read_with_its_path() {
        let (dir, snapshot) = state_with(json!({"version": 1, "panes": []}));
        let (path, data) = read_snapshot_in(dir.path()).unwrap().unwrap();
        assert_eq!(path, snapshot);
        assert_eq!(data, json!({"version": 1, "panes": []}));
    }

    #[test]
    fn version_one_may_be_written_as_a_float() {
        let (dir, _) = state_with(json!({"version": 1.0}));
        assert!(read_snapshot_in(dir.path()).unwrap().is_some());
    }

    #[test]
    fn any_other_version_is_refused() {
        for version in [json!({"version": 2}), json!({"version": "1"}), json!({})] {
            let (dir, _) = state_with(version);
            let e = read_snapshot_in(dir.path()).unwrap_err();
            assert_eq!(e.to_string(), "Unsupported workspace snapshot version");
        }
    }

    #[test]
    fn a_damaged_file_is_an_error() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("latest.json"), "{").unwrap();
        assert!(matches!(read_snapshot_in(dir.path()), Err(Error::Json(_))));
        write(&dir.path().join("latest.json"), &json!({"nope": 1}));
        let e = read_snapshot_in(dir.path()).unwrap_err();
        assert!(
            e.to_string().ends_with("latest.json: no snapshot path"),
            "{e}"
        );
    }

    // ---- tmux-free pieces

    use crate::host::RealHost;
    use crate::testing::{FakeHost, Proc, ScratchTmux};

    /// A host that answers every command with the same text.
    struct Says(&'static str);

    impl Host for Says {
        fn run(&self, _: &[String]) -> Result<String> {
            Ok(self.0.to_string())
        }

        fn parent_pid(&self) -> i32 {
            0
        }
    }

    #[test]
    fn the_process_table_keeps_what_it_can_read_and_skips_the_rest() {
        let table = process_table(&Says(
            "  12     1    12    12 Mon Sep 28 07:00:00 2026\n\
             garbage\n\
             13 x 13 13 Mon Sep 28 07:00:01 2026\n\
             14 1 14\n\
             15 12 15 0 Tue  Sep  29 08:00:00 2026   \n\
             \n\
             -3 1 0 0 a b c d e",
        ))
        .unwrap();
        let mut seen: Vec<_> = table.into_iter().collect();
        seen.sort();
        assert_eq!(
            seen,
            [
                (-3, (1, 0, 0, "a b c d e".to_string())),
                (12, (1, 12, 12, "Mon Sep 28 07:00:00 2026".to_string())),
                (15, (12, 15, 0, "Tue  Sep  29 08:00:00 2026".to_string())),
            ]
        );
    }

    #[test]
    fn the_process_table_comes_from_one_ps_call() {
        let mut host = FakeHost::new();
        host.procs.insert(
            7,
            Proc {
                pgid: 7,
                tpgid: 7,
                lstart: "Mon Sep 28 07:00:00 2026".into(),
                ..Proc::new(1, "ttys001", "sh")
            },
        );
        let table = process_table(&host).unwrap();
        assert_eq!(table[&7], (1, 7, 7, "Mon Sep 28 07:00:00 2026".to_string()));
        assert_eq!(host.calls_to("ps").len(), 1);
        // A failing ps is the error of the save.
        assert!(process_table(&FakeHost::new()).is_err());
    }

    fn pane_on(tty: &str) -> Fields {
        let mut pane = Fields::default();
        pane.set("pane_tty", tty);
        pane
    }

    #[test]
    fn the_ttys_of_the_panes_are_named_as_ps_names_them_and_listed_once() {
        let panes = [
            pane_on("/dev/ttys001"),
            pane_on("/dev/ttys002"),
            pane_on("/dev/ttys001"),
            pane_on(""),
            pane_on("/dev/pts/3"),
            pane_on("ttys009"),
        ];
        assert_eq!(
            pane_ttys(&panes),
            ["ttys001", "ttys002", "pts/3", "ttys009"]
        );
        assert!(pane_ttys(&[]).is_empty());
    }

    /// Three ttys: 100 and 200 are on the first, 300 on the second, 400 on a tty that has no pane.
    fn host_with_four_processes() -> FakeHost {
        let mut host = FakeHost::new();
        for (pid, tty) in [
            (100, "ttys001"),
            (200, "ttys001"),
            (300, "ttys002"),
            (400, "ttys003"),
        ] {
            host.procs.insert(
                pid,
                Proc {
                    pgid: pid,
                    tpgid: pid,
                    lstart: format!("birth {pid}"),
                    ..Proc::new(1, tty, "sh")
                },
            );
        }
        host
    }

    #[test]
    fn the_table_of_a_save_is_the_processes_on_the_ttys_of_the_panes_from_one_ps_call() {
        let host = host_with_four_processes();
        let table = process_table_on(&host, &["ttys001".into(), "ttys002".into()], &[]).unwrap();
        let mut pids: Vec<_> = table.keys().copied().collect();
        pids.sort();
        assert_eq!(pids, [100, 200, 300]);
        assert_eq!(table[&300], (1, 300, 300, "birth 300".to_string()));
        let calls = host.calls_to("ps");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0][1..],
            ["-t", "ttys001,ttys002", "-o", PROCESS_COLUMNS]
        );
    }

    #[test]
    fn the_table_of_a_save_also_has_the_processes_the_panes_record() {
        let host = host_with_four_processes();
        // 400 is on a tty that has no pane (it stands for a process that has left its tty), and 777 is gone.
        let table = process_table_on(&host, &["ttys001".into()], &[400, 777]).unwrap();
        let mut pids: Vec<_> = table.keys().copied().collect();
        pids.sort();
        assert_eq!(
            pids,
            [100, 200, 400],
            "the recorded process that is gone is not in it"
        );
        let calls = host.calls_to("ps");
        assert_eq!(calls.len(), 1, "one ps call for both lists");
        assert_eq!(
            calls[0][1..],
            ["-t", "ttys001", "-p", "400,777", "-o", PROCESS_COLUMNS]
        );
    }

    fn pane_recording(app: &str) -> Fields {
        let mut pane = Fields::default();
        pane.set("@workspace_app", app);
        pane
    }

    #[test]
    fn the_pids_a_save_asks_for_are_the_positive_ones_the_panes_record_once() {
        let panes = [
            pane_recording(r#"{"kind": "nvim", "pid": 9001}"#),
            pane_recording(r#"{"kind": "claude", "pid": 42, "id": "x"}"#),
            pane_recording(r#"{"kind": "nvim", "pid": 9001}"#),
            pane_recording(r#"{"kind": "codex", "pid": "77"}"#),
            pane_recording(""),
            pane_recording("{not json"),
            pane_recording(r#"{"kind": "nvim"}"#),
            pane_recording(r#"{"pid": 0}"#),
            pane_recording(r#"{"pid": -5}"#),
            pane_recording(r#"{"pid": 99999999999}"#),
            pane_recording("[1, 2]"),
            Fields::default(),
        ];
        assert_eq!(recorded_pids(&panes), [9001, 42, 77]);
    }

    #[test]
    fn the_table_of_a_save_is_every_process_when_ps_refuses_the_list_of_ttys() {
        // A pane whose tty is gone makes `ps -t` fail for all of them.
        let host = Failing {
            host: host_with_four_processes(),
            program: "ps",
            word: "-t",
            error: "ps: no such tty",
        };
        let table = process_table_on(&host, &["ttys001".into(), "gone".into()], &[400]).unwrap();
        assert_eq!(table.len(), 4, "nothing is missed for it");
        let calls = host.host.calls_to("ps");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][1], "-t");
        assert_eq!(calls[1][1..], ["-axo", PROCESS_COLUMNS]);
    }

    #[test]
    fn the_table_of_a_save_is_every_process_when_no_pane_has_a_tty() {
        let host = host_with_four_processes();
        assert_eq!(process_table_on(&host, &[], &[100]).unwrap().len(), 4);
        assert_eq!(host.calls_to("ps").len(), 1);
        assert_eq!(host.calls_to("ps")[0][1], "-axo");
    }

    /// Pane shell 100 > app 200 (foreground) > helper 300 (background job); 600 is below the shell with no
    /// foreground group on its tty. 400, 401 and 500 are not below the pane.
    fn processes() -> ProcessTable {
        [
            (100, (1, 100, 200, "b100")),
            (200, (100, 200, 200, "b200")),
            (300, (200, 300, 200, "b300")),
            (400, (401, 400, 0, "b400")),
            (401, (400, 401, 0, "b401")),
            (500, (1, 500, -1, "b500")),
            (600, (100, 600, 0, "b600")),
        ]
        .into_iter()
        .map(|(pid, (ppid, group, foreground, birth))| {
            (pid, (ppid, group, foreground, birth.to_string()))
        })
        .collect()
    }

    fn app_pane(app: &str) -> Fields {
        let mut pane = Fields::default();
        pane.set("pane_id", "%1");
        pane.set("pane_pid", "100");
        pane.set("@workspace_app", app);
        pane
    }

    fn lives(app: Value) -> Option<Value> {
        live_app(&Tmux::new(), &app_pane(&app.to_string()), &processes())
    }

    #[test]
    fn a_recorded_app_is_live_when_its_process_is_the_one_in_the_foreground_below_the_pane() {
        let app = json!({"kind": "claude", "id": "abc", "pid": 200, "birth": "b200", "cwd": "/w"});
        assert_eq!(lives(app.clone()), Some(app));
        for kind in ["claude", "codex", "omp", "nvim"] {
            let app = json!({"kind": kind, "id": "x", "pid": 200, "birth": "b200"});
            assert_eq!(lives(app.clone()), Some(app), "{kind}");
        }
        // The pid may be written as text or with a fraction; the pane's own process counts.
        for pid in [json!("200"), json!(200.0), json!(" 200 ")] {
            let app = json!({"kind": "claude", "id": "x", "pid": pid, "birth": "b200"});
            assert_eq!(lives(app.clone()), Some(app), "{pid}");
        }
        // No foreground group on the tty (tpgid <= 0) is not a background job.
        let detached = json!({"kind": "omp", "id": "x", "pid": 600, "birth": "b600"});
        assert_eq!(lives(detached.clone()), Some(detached));
        // The pane's own process counts, when it is in the foreground.
        let own = json!({"kind": "omp", "id": "x", "pid": 200, "birth": "b200"});
        let mut pane = app_pane(&own.to_string());
        pane.set("pane_pid", "200");
        assert_eq!(live_app(&Tmux::new(), &pane, &processes()), Some(own));
    }

    #[test]
    fn a_recorded_app_that_is_not_what_runs_now_is_not_live() {
        let base = json!({"kind": "claude", "id": "abc", "pid": 200, "birth": "b200"});
        let with = |key: &str, value: Value| {
            let mut app = base.clone();
            app[key] = value;
            app
        };
        let without = |key: &str| {
            let mut app = base.clone();
            app.as_object_mut().unwrap().remove(key);
            app
        };
        assert_eq!(lives(with("birth", json!("old"))), None, "a reused pid");
        assert_eq!(lives(with("birth", json!(7))), None);
        assert_eq!(lives(with("pid", json!(999))), None, "no such process");
        assert_eq!(lives(with("pid", json!(300))), None, "a background job");
        assert_eq!(lives(with("pid", json!(400))), None, "not below the pane");
        assert_eq!(
            lives(with("pid", json!(401))),
            None,
            "a loop that never reaches it"
        );
        assert_eq!(lives(with("pid", json!(0))), None);
        assert_eq!(
            lives(with("pid", json!(100))),
            None,
            "the shell, behind the app that is in the foreground"
        );
        assert_eq!(lives(with("pid", json!(500))), None, "not below the pane");
        assert_eq!(lives(with("pid", json!("x"))), None);
        assert_eq!(lives(with("pid", json!(null))), None);
        assert_eq!(lives(with("kind", json!("bash"))), None);
        assert_eq!(lives(with("kind", json!(["claude"]))), None);
        for key in ["pid", "birth", "kind"] {
            assert_eq!(lives(without(key)), None, "no {key}");
        }
        assert_eq!(lives(json!([1, 2])), None);
        assert_eq!(lives(json!("text")), None);
        assert_eq!(live_app(&Tmux::new(), &app_pane(""), &processes()), None);
        assert_eq!(
            live_app(&Tmux::new(), &app_pane("{not json"), &processes()),
            None
        );
        let mut no_pid = app_pane(&base.to_string());
        no_pid.set("pane_pid", "x");
        assert_eq!(live_app(&Tmux::new(), &no_pid, &processes()), None);
    }

    #[test]
    fn a_live_app_without_an_exact_identity_is_recorded_as_an_error() {
        let base = json!({"kind": "codex", "pid": 200, "birth": "b200", "cwd": "/w"});
        let error =
            json!({"kind": "codex", "error": "No exact conversation identity was recorded"});
        assert_eq!(lives(base.clone()), Some(error.clone()));
        for id in [json!(""), json!(null), json!(5), json!(["x"])] {
            let mut app = base.clone();
            app["id"] = id;
            assert_eq!(lives(app), Some(error.clone()));
        }
    }

    #[test]
    fn a_dollar_in_the_recorded_app_is_read_as_tmux_3_4_prints_it() {
        let raw = r#"{"kind": "claude", "id": "a\$b", "pid": 200, "birth": "b200"}"#;
        let found = live_app(&Tmux::new(), &app_pane(raw), &processes()).unwrap();
        assert_eq!(found["id"], json!("a$b"));
    }

    #[test]
    fn live_app_reads_the_option_when_the_row_has_none() {
        let scratch = ScratchTmux::new();
        let pane = scratch.expand("alpha:one", "#{pane_id}");
        let pid: i32 = scratch.expand("alpha:one", "#{pane_pid}").parse().unwrap();
        let app = json!({"kind": "omp", "id": "x", "pid": pid, "birth": "now"});
        scratch.tmux(&[
            "set-option",
            "-p",
            "-t",
            &pane,
            "@workspace_app",
            &app.to_string(),
        ]);
        let table: ProcessTable = [(pid, (1, pid, pid, "now".to_string()))].into();
        let mut row = Fields::default();
        row.set("pane_id", &pane);
        row.set("pane_pid", pid.to_string());
        let tmux = Tmux::new().socket(scratch.socket());
        assert_eq!(live_app(&tmux, &row, &table), Some(app));
        // A row that holds the option, even empty, is not asked again.
        row.set("@workspace_app", "");
        assert_eq!(live_app(&tmux, &row, &table), None);
    }

    #[test]
    fn a_pane_shows_the_conversation_when_its_title_names_it() {
        let id = "01a128f8-8621-7561-8972-8d02a1234567";
        let app = json!({"kind": "codex", "id": id});
        assert!(shows_current_conversation(&app, &format!("codex | {id}")));
        assert!(shows_current_conversation(
            &app,
            &format!("codex | {}...", &id[..29])
        ));
        assert!(!shows_current_conversation(
            &app,
            &format!("codex | {}", &id[..29])
        ));
        assert!(!shows_current_conversation(
            &app,
            "codex | 01a128f8-8621-7561-8972-8d02a1234568"
        ));
        assert!(!shows_current_conversation(&app, id));
        assert!(!shows_current_conversation(
            &json!({"kind": "claude", "id": id}),
            &format!("codex | {id}")
        ));
        assert!(!shows_current_conversation(
            &json!({"kind": "codex", "id": id, "error": "e"}),
            &format!("codex | {id}")
        ));
        assert!(shows_current_conversation(
            &json!({"kind": "codex", "id": id, "error": ""}),
            &format!("codex | {id}")
        ));
        assert!(!shows_current_conversation(
            &json!({"kind": "codex"}),
            "codex | "
        ));
    }

    // ---- the Codex lookup

    const THREAD: &str = "01a128f8-8621-7561-8972-8d02a1234567";

    fn codex_pane(command: &str, title: &str) -> Fields {
        let mut pane = Fields::default();
        for (name, value) in [
            ("pane_pid", "100"),
            ("pane_tty", "/dev/ttys001"),
            ("pane_current_command", command),
            ("pane_title", title),
            ("pane_current_path", "/work"),
        ] {
            pane.set(name, value);
        }
        pane
    }

    fn lookup(host: &dyn Host) -> Codex<'_> {
        Codex {
            host,
            home: PathBuf::from("/nonexistent"),
            ttys: Vec::new(),
            scan: None,
        }
    }

    /// A shell (100) that runs Codex (200) in the foreground of the tty.
    fn codex_machine() -> FakeHost {
        let mut host = FakeHost::new();
        host.procs.insert(100, Proc::new(1, "ttys001", "-zsh"));
        host.procs.insert(
            200,
            Proc {
                args: "codex".into(),
                pgid: 200,
                tpgid: 200,
                lstart: "birth".into(),
                ..Proc::new(100, "ttys001", "codex")
            },
        );
        host
    }

    /// The scan error of a failed process table, as a pane that runs Codex gets it.
    #[test]
    fn a_failed_codex_scan_marks_only_the_panes_that_run_codex() {
        // The scan fails (a FakeHost without processes answers like a ps that found nothing).
        let host = FakeHost::new();
        let mut codex = lookup(&host);
        let found = codex.identify(&codex_pane("codex", "codex | unknown"));
        assert_eq!(
            found,
            Some(json!({"kind": "codex", "error": "FakeHost: no such process: nothing matched"}))
        );
        // The same failure says nothing about a pane that does not run Codex.
        assert_eq!(codex.identify(&codex_pane("zsh", "")), None);
        assert_eq!(host.calls_to("ps").len(), 1, "the scan is made once");
    }

    #[test]
    fn the_codex_scan_reads_the_processes_on_the_ttys_of_the_panes_only() {
        let mut host = codex_machine();
        // A process on another tty, which no pane has, is not asked for.
        host.procs.insert(900, Proc::new(1, "ttys099", "codex"));
        let mut codex = Codex {
            ttys: vec!["ttys001".into(), "ttys002".into()],
            ..lookup(&host)
        };
        let pane = codex_pane("codex", "codex | unknown");
        // Found, as with every process: the shell below the pane's process owns it.
        let found = codex.identify(&pane).expect("a Codex in the pane");
        assert_eq!(found["pid"], 200);
        assert_eq!(found["birth"], "birth");
        assert_eq!(
            found["error"],
            "Codex has not published a current thread-id title"
        );
        let calls = host.calls_to("ps");
        let scans: Vec<_> = calls.iter().filter(|call| call[1] == "-t").collect();
        assert_eq!(scans.len(), 1, "{calls:?}");
        assert_eq!(
            scans[0][1..],
            ["-t", "ttys001,ttys002", "-o", "pid=,ppid=,tty=,comm="]
        );
        assert!(
            calls.iter().all(|call| call[1] != "-axo"),
            "no scan of every process: {calls:?}"
        );
    }

    #[test]
    fn the_codex_scan_reads_every_process_when_ps_refuses_the_list_of_ttys() {
        let host = Failing {
            host: codex_machine(),
            program: "ps",
            word: "-t",
            error: "ps: no such tty",
        };
        let mut codex = Codex {
            ttys: vec!["ttys001".into(), "gone".into()],
            ..lookup(&host)
        };
        let found = codex
            .identify(&codex_pane("codex", "codex | unknown"))
            .expect("a Codex in the pane");
        assert_eq!(found["pid"], 200);
        let calls = host.host.calls_to("ps");
        let scans: Vec<_> = calls
            .iter()
            .filter(|call| call[1] == "-t" || call[1] == "-axo")
            .collect();
        assert_eq!(scans.len(), 2, "{calls:?}");
        assert_eq!(scans[1][1..], ["-axo", "pid=,ppid=,tty=,comm="]);
    }

    /// A world with one extra pane, `editor` (identity `bbb...`), that runs `sleep`. `app` is what it records
    /// in `@workspace_app`.
    const EDITOR_PANE_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn editor_pane(scratch: &ScratchTmux, app: &Value) -> (String, i32, String) {
        let pane = scratch.pane(&["new-window", "-t", "alpha:", "-n", "editor"]);
        scratch.tmux(&[
            "set-option",
            "-p",
            "-t",
            &pane,
            "@workspace_id",
            EDITOR_PANE_ID,
        ]);
        scratch.tmux(&[
            "set-option",
            "-p",
            "-t",
            &pane,
            "@workspace_app",
            &app.to_string(),
        ]);
        let pid = scratch.expand(&pane, "#{pane_pid}").parse().unwrap();
        let tty = scratch.expand(&pane, "#{pane_tty}");
        (pane, pid, tty.trim_start_matches("/dev/").to_string())
    }

    fn saved_editor_app(root: &Path) -> Value {
        let (_path, data) = read_snapshot_in(root).unwrap().unwrap();
        data["panes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pane| pane["id"] == json!(EDITOR_PANE_ID))
            .unwrap_or_else(|| panic!("the editor pane is not in the snapshot: {data}"))["app"]
            .clone()
    }

    /// Neovim 0.10 and later record the pid of `nvim --embed`, the server below the Neovim on the tty. It
    /// is detached: it has a session of its own and no tty (`??`). The save must still find it, as it did
    /// when it read every process.
    #[test]
    fn an_app_that_has_left_the_tty_is_found_below_the_pane_and_saved() {
        let scratch = ScratchTmux::new();
        let session = TempDir::new();
        let session_file = session.path().join("session.vim");
        fs::write(&session_file, "").unwrap();
        let app = json!({
            "kind": "nvim",
            "id": session_file.to_string_lossy(),
            "pid": 9001,
            "birth": "Mon Sep 28 07:00:00 2026",
            "cwd": "/work",
        });
        let (_pane, pane_pid, tty) = editor_pane(&scratch, &app);
        let mut host = FakeHost::new();
        host.procs.insert(
            pane_pid,
            Proc {
                pgid: pane_pid,
                tpgid: pane_pid,
                lstart: "Mon Sep 28 06:00:00 2026".into(),
                ..Proc::new(1, &tty, "nvim")
            },
        );
        // Detached: its own process group, no tty, and so no foreground group of one (tpgid 0).
        host.procs.insert(
            9001,
            Proc {
                pgid: 9001,
                tpgid: 0,
                lstart: "Mon Sep 28 07:00:00 2026".into(),
                ..Proc::new(pane_pid, "", "nvim")
            },
        );
        let tmux = Tmux::new().socket(scratch.socket());
        let root = TempDir::new();
        save_in(root.path(), &tmux, &host, true).unwrap();
        assert_eq!(saved_editor_app(root.path()), app);
        // Still two ps calls: the table names the recorded pid in the call that lists the ttys.
        let calls = host.calls_to("ps");
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0][1], "-t");
        assert!(
            calls[0][2].split(',').any(|listed| listed == tty),
            "{calls:?}"
        );
        assert_eq!(calls[0][3..5], ["-p", "9001"], "{calls:?}");
    }

    /// The same with a real process, which `ps -t` really does not list: a child of the pane's process that
    /// called `setsid`. (`perl` does it: macOS has no `setsid` command.)
    #[test]
    fn a_really_detached_app_below_the_pane_is_saved() {
        struct Kill(i32);
        impl Drop for Kill {
            fn drop(&mut self) {
                // SAFETY: a pid of a process this test started.
                unsafe { libc::kill(self.0, libc::SIGKILL) };
            }
        }
        let scratch = ScratchTmux::new();
        let dir = TempDir::new();
        let pid_file = dir.path().join("pid");
        let script = "use POSIX; $p = fork(); if (!$p) { POSIX::setsid(); exec \"sleep\", \"300\" } \
                      open F, \">\", $ARGV[0]; print F $p; close F; sleep 300";
        let command = format!("exec perl -e '{script}' '{}'", pid_file.display());
        let pane = scratch.tmux(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "alpha:",
            "-n",
            "editor",
            &command,
        ]);
        scratch.tmux(&[
            "set-option",
            "-p",
            "-t",
            &pane,
            "@workspace_id",
            EDITOR_PANE_ID,
        ]);
        let read_pid = || fs::read_to_string(&pid_file).unwrap_or_default();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while read_pid().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let pid: i32 = read_pid().trim().parse().expect("perl wrote the pid");
        let _kill = Kill(pid);
        let pid_text = pid.to_string();
        let listing = |args: &[&str]| RealHost.run(&procs::ps(args)).unwrap_or_default();
        let birth = listing(&["-p", &pid_text, "-o", "lstart="]);
        assert!(!birth.is_empty(), "the child runs");
        let tty = scratch.expand(&pane, "#{pane_tty}");
        let tty = tty.trim_start_matches("/dev/");
        // The premise: listing the tty does not show it.
        assert!(
            !listing(&["-t", tty, "-o", "pid="])
                .split_whitespace()
                .any(|listed| listed == pid_text),
            "the child is on the tty"
        );
        let app = json!({"kind": "nvim", "id": "/x/session.vim", "pid": pid, "birth": birth});
        scratch.tmux(&[
            "set-option",
            "-p",
            "-t",
            &pane,
            "@workspace_app",
            &app.to_string(),
        ]);
        let tmux = Tmux::new().socket(scratch.socket());
        let root = TempDir::new();
        save_in(root.path(), &tmux, &RealHost, true).unwrap();
        assert_eq!(saved_editor_app(root.path()), app);
    }

    /// A machine whose commands fail on request. Every command is recorded, the failing ones too.
    struct Failing {
        host: FakeHost,
        /// The program (`lsof`, `ps`) and a word of its arguments that make the command fail.
        program: &'static str,
        word: &'static str,
        error: &'static str,
    }

    impl Host for Failing {
        fn run(&self, argv: &[String]) -> Result<String> {
            let program = argv
                .first()
                .map_or("", |p| p.rsplit('/').next().unwrap_or(p));
            if program == self.program && argv.iter().any(|arg| arg == self.word) {
                self.host.calls.borrow_mut().push(argv.to_vec());
                return fail(self.error);
            }
            self.host.run(argv)
        }

        fn parent_pid(&self) -> i32 {
            0
        }
    }

    /// An executable that tmux calls `codex` and that sleeps for its first argument (300 seconds without
    /// one). tmux names a pane's program differently on the two systems:
    ///
    /// - macOS: by the executable. A symlink to `sleep` would show up as `sleep`, so the program is a
    ///   copy of it, which macOS kills until the copy has an ad-hoc signature.
    /// - Linux: by the first word of `/proc/<pgrp>/cmdline`, so a script that sets its own name (`$0` of
    ///   Perl) will do. A copy, a link or `exec -a` of `sleep` would not: the coreutils of Ubuntu 26.04
    ///   are one program (uutils) that picks the command from `argv[0]` and fails on `codex`.
    fn codex_program(dir: &Path) -> PathBuf {
        let program = dir.join("codex");
        if cfg!(target_os = "macos") {
            fs::copy(
                crate::process::which("sleep").expect("sleep on PATH"),
                &program,
            )
            .unwrap();
            let signed = std::process::Command::new("codesign")
                .args(["-s", "-", "-f"])
                .arg(&program)
                .output()
                .expect("codesign");
            assert!(signed.status.success(), "{signed:?}");
        } else {
            crate::testing::write_script(
                &program,
                "#!/usr/bin/env perl\n$0 = 'codex';\nsleep(defined $ARGV[0] ? $ARGV[0] : 300);\n",
            );
        }
        let runs = std::process::Command::new(&program)
            .arg("0")
            .status()
            .unwrap();
        assert!(
            runs.success(),
            "the program named codex does not run: {runs}"
        );
        program
    }

    /// Alpha's pane `one` runs `sleep`. A second pane, with identity `aaa...` and label `agent`, shows
    /// `saved text` and then runs a program that tmux knows as `codex`, under a title that names a thread.
    /// The machine lists that process on the pane's tty, in the foreground, as Codex.
    struct CodexPane {
        scratch: ScratchTmux,
        tmux: Tmux,
        root: TempDir,
        _program: TempDir,
    }

    const CODEX_PANE_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn codex_pane_world() -> (CodexPane, FakeHost) {
        let scratch = ScratchTmux::new();
        let program_dir = TempDir::new();
        let program = codex_program(program_dir.path());
        let command = format!("printf 'saved text\\n'; exec '{}' 300", program.display());
        let pane = scratch.tmux(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "alpha:",
            "-n",
            "agent",
            &command,
        ]);
        crate::testing::wait_for(|| scratch.expand(&pane, "#{pane_current_command}"), "codex");
        crate::testing::wait_for(
            || {
                scratch
                    .tmux(&["capture-pane", "-p", "-t", &pane])
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string()
            },
            "saved text",
        );
        scratch.tmux(&[
            "select-pane",
            "-t",
            &pane,
            "-T",
            &format!("codex | {THREAD}"),
        ]);
        scratch.tmux(&[
            "set-option",
            "-p",
            "-t",
            &pane,
            "@workspace_id",
            CODEX_PANE_ID,
        ]);
        scratch.tmux(&["set-option", "-p", "-t", &pane, "@workspace_label", "agent"]);
        let pid: i32 = scratch.expand(&pane, "#{pane_pid}").parse().unwrap();
        let tty = scratch.expand(&pane, "#{pane_tty}");
        let mut host = FakeHost::new();
        host.procs.insert(
            pid,
            Proc {
                args: "codex 300".into(),
                pgid: pid,
                tpgid: pid,
                lstart: "Mon Sep 28 07:00:00 2026".into(),
                ..Proc::new(1, tty.trim_start_matches("/dev/"), "codex")
            },
        );
        let tmux = Tmux::new().socket(scratch.socket());
        let world = CodexPane {
            scratch,
            tmux,
            root: TempDir::new(),
            _program: program_dir,
        };
        (world, host)
    }

    /// The record of the Codex pane in the latest snapshot, and its saved terminal text.
    fn saved_codex_pane(world: &CodexPane) -> (Value, String) {
        let (path, data) = read_snapshot_in(world.root.path()).unwrap().unwrap();
        let record = data["panes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pane| pane["id"] == json!(CODEX_PANE_ID))
            .unwrap_or_else(|| panic!("the Codex pane is not in the snapshot: {data}"))
            .clone();
        let text = fs::read_to_string(path.parent().unwrap().join(format!("{CODEX_PANE_ID}.txt")));
        (record, text.unwrap())
    }

    /// test_performance.py::Saves, first half. The process table is read, the lookup for the one pane that
    /// runs Codex then fails (here `lsof` times out, where Python's resolver raised `TimeoutExpired`). The
    /// save still completes, the record carries the reason, and the pane's terminal text is saved.
    #[test]
    fn identity_timeout_still_saves_history_with_explicit_recovery_error() {
        let (world, machine) = codex_pane_world();
        let host = Failing {
            host: machine,
            program: "lsof",
            word: "-Ffan",
            error: "timed out: lsof",
        };
        save_in(world.root.path(), &world.tmux, &host, true).unwrap();
        assert_eq!(
            host.host.calls_to("lsof").len(),
            1,
            "the lookup ran and failed"
        );
        let (record, text) = saved_codex_pane(&world);
        assert_eq!(
            record["app"],
            json!({"kind": "codex", "error": "Identity lookup failed: timed out: lsof"})
        );
        assert_eq!(text, "saved text\n");
        // The pane that runs no Codex is saved too, without an application.
        let (_, data) = read_snapshot_in(world.root.path()).unwrap().unwrap();
        assert_eq!(data["panes"].as_array().unwrap().len(), 2);
        assert!(data["panes"][0]["app"].is_null(), "{}", data["panes"][0]);
        assert_eq!(
            world
                .scratch
                .tmux(&["show-options", "-gqv", "@workspace_save_error"]),
            ""
        );
    }

    /// The process table itself cannot be read: the Codex pane gets that error as its reason, and again
    /// the pane's text is saved.
    #[test]
    fn a_failed_process_scan_still_saves_a_codex_pane_with_the_scan_error() {
        let (world, machine) = codex_pane_world();
        let host = Failing {
            host: machine,
            program: "ps",
            word: "pid=,ppid=,tty=,comm=",
            error: "timed out: ps",
        };
        save_in(world.root.path(), &world.tmux, &host, true).unwrap();
        assert!(
            host.host.calls_to("lsof").is_empty(),
            "no lookup after a failed scan"
        );
        let (record, text) = saved_codex_pane(&world);
        assert_eq!(
            record["app"],
            json!({"kind": "codex", "error": "timed out: ps"})
        );
        assert_eq!(text, "saved text\n");
        let (_, data) = read_snapshot_in(world.root.path()).unwrap().unwrap();
        assert!(
            data["panes"][0]["app"].is_null(),
            "a sleep is no Codex: {}",
            data["panes"][0]
        );
    }

    #[test]
    fn the_codex_scan_is_made_once_for_all_panes() {
        let host = codex_machine();
        let mut codex = lookup(&host);
        for _ in 0..3 {
            codex.identify(&codex_pane("zsh", ""));
        }
        let scans: Vec<_> = host
            .calls_to("ps")
            .into_iter()
            .filter(|call| call.contains(&"-axo".to_string()))
            .collect();
        assert_eq!(scans.len(), 1, "{scans:?}");
    }

    #[test]
    fn every_failure_of_the_lookup_is_recorded_and_the_save_goes_on() {
        // The process table reads; Codex is the owner; the open files cannot be listed.
        let host = codex_machine();
        let found = lookup(&host).identify(&codex_pane("codex", &format!("codex | {THREAD}")));
        assert_eq!(
            found,
            Some(
                json!({"kind": "codex", "error": "Identity lookup failed: FakeHost: no such process: 200"})
            )
        );
        // A pid that is no number.
        let mut pane = codex_pane("codex", "");
        pane.set("pane_pid", "x");
        let found = lookup(&host).identify(&pane).unwrap();
        assert_eq!(
            found["error"],
            json!("Identity lookup failed: invalid literal for int() with base 10: 'x'")
        );
        // Without a thread id in the title there is nothing to look up, and it says so.
        let found = lookup(&host)
            .identify(&codex_pane("codex", "codex | working"))
            .unwrap();
        assert_eq!(
            found["error"],
            json!("Codex has not published a current thread-id title")
        );
        assert_eq!(found["pid"], json!(200));
        // Nothing runs in this pane.
        let mut elsewhere = codex_pane("zsh", "");
        elsewhere.set("pane_tty", "/dev/ttys009");
        assert_eq!(lookup(&host).identify(&elsewhere), None);
    }

    #[test]
    fn an_application_without_a_verified_identity_is_an_error_record() {
        let host = FakeHost::new();
        let tmux = Tmux::new();
        let mut applications = Applications {
            tmux: &tmux,
            table: ProcessTable::new(),
            codex: lookup(&host),
        };
        for command in ["claude", "omp", "nvim"] {
            let mut pane = codex_pane(command, "");
            pane.set("@workspace_app", "");
            assert_eq!(
                applications.of(&pane),
                Some(json!({
                    "kind": command,
                    "error": "Active application has no verified conversation/session identity"
                })),
                "{command}"
            );
        }
        let mut shell = codex_pane("zsh", "");
        shell.set("@workspace_app", "");
        assert_eq!(applications.of(&shell), None);
    }

    #[test]
    fn a_pane_whose_title_shows_its_conversation_is_not_looked_up_again() {
        let host = FakeHost::new();
        let tmux = Tmux::new();
        let recorded = json!({"kind": "codex", "id": THREAD, "pid": 200, "birth": "b200"});
        let mut applications = Applications {
            tmux: &tmux,
            table: processes(),
            codex: lookup(&host),
        };
        let mut pane = codex_pane("codex", &format!("codex | {THREAD}"));
        pane.set("pane_id", "%1");
        pane.set("@workspace_app", recorded.to_string());
        assert_eq!(applications.of(&pane), Some(recorded.clone()));
        assert!(host.calls.borrow().is_empty(), "no ps, no lsof");
        // Another title: the record is only a hint, and Codex is looked up.
        pane.set("pane_title", "codex | other");
        assert_ne!(applications.of(&pane), Some(recorded));
        assert!(!host.calls.borrow().is_empty());
    }

    #[test]
    fn a_missing_nvim_session_is_the_reason_not_a_failed_save() {
        let app = json!({"kind": "nvim", "id": "/nonexistent/session.vim", "socket": "/nonexistent/socket"});
        let failed = json!({"kind": "nvim", "error": "Could not save the active Neovim session"});
        assert_eq!(save_editor_session(app), failed);
        // Applications of other kinds, and Neovim without a socket, are left as they are.
        for app in [
            json!({"kind": "claude", "id": "x", "socket": "/s"}),
            json!({"kind": "nvim", "id": "x"}),
            json!({"kind": "nvim", "id": "x", "socket": ""}),
            json!({"kind": "nvim", "error": "e"}),
        ] {
            assert_eq!(save_editor_session(app.clone()), app);
        }
    }

    // ---- files

    #[test]
    fn a_file_is_flushed_with_fsync() {
        let dir = TempDir::new();
        let file = File::create(dir.path().join("f")).unwrap();
        assert!(fsync(&file).is_ok());
        // The descriptor is the file's own: a file opened for reading only is flushed as well.
        let read = File::open(dir.path().join("f")).unwrap();
        assert!(fsync(&read).is_ok());
    }

    #[test]
    fn atomic_replaces_the_file_and_leaves_nothing_behind() {
        let dir = TempDir::new();
        let path = dir.path().join("latest.json");
        atomic(&path, "one").unwrap();
        atomic(&path, "two \u{e9}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two \u{e9}");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["latest.json"]);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn atomic_makes_a_private_folder_and_cleans_up_when_it_cannot_replace() {
        let dir = TempDir::new();
        let path = dir.path().join("deep/er/file.txt");
        atomic(&path, "x").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "x");
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        // A folder cannot be replaced by a file: the error names the file, and no temporary file stays.
        let target = dir.path().join("taken");
        fs::create_dir(&target).unwrap();
        let e = atomic(&target, "x").unwrap_err();
        assert!(
            e.to_string()
                .starts_with(&format!("Cannot write {}: ", target.display())),
            "{e}"
        );
        let stray: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(stray.is_empty(), "{stray:?}");
    }

    #[test]
    fn temporary_files_are_named_after_the_target() {
        let dir = TempDir::new();
        let (_, first) = create_temporary(dir.path(), "workspace.json").unwrap();
        let (_, second) = create_temporary(dir.path(), "workspace.json").unwrap();
        for path in [&first, &second] {
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(
                name.starts_with(".workspace.json") && name.len() == ".workspace.json".len() + 8,
                "{name}"
            );
        }
        assert_ne!(first, second);
    }

    #[test]
    fn the_lock_makes_a_private_folder_and_is_exclusive() {
        let dir = TempDir::new();
        let root = dir.path().join("state");
        let held = lock_in(&root).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join("lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        // Another descriptor, as another process would have, must wait.
        let other = fs::OpenOptions::new()
            .append(true)
            .open(root.join("lock"))
            .unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(held);
        // A child that another test thread has forked, and not yet executed, still holds the lock for an instant.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while other.try_lock().is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "the lock was not released"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The folder of a wider mode is made private again.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        drop(other);
        drop(lock_in(&root).unwrap());
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn only_the_three_newest_all_digit_folders_stay() {
        let dir = TempDir::new();
        for name in ["100", "200", "300", "400", "500", "abc", "9x", ""] {
            if !name.is_empty() {
                fs::create_dir(dir.path().join(name)).unwrap();
            }
        }
        fs::write(dir.path().join("0"), "a file").unwrap();
        prune(dir.path()).unwrap();
        let mut left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        // From the newest: abc, 9x and 500 stay. 400 to 100 are folders of digits and go. The file 0 stays.
        assert_eq!(left, ["0", "500", "9x", "abc"]);
    }

    #[test]
    fn a_recent_save_is_one_less_than_five_minutes_old() {
        let dir = TempDir::new();
        let latest = dir.path().join("latest.json");
        assert!(!saved_recently(dir.path(), 1000.0).unwrap(), "no file");
        for (text, now, expected) in [
            (r#"{"saved_at": 800.0, "path": "p"}"#, 1000.0, true),
            (r#"{"saved_at": 800, "path": "p"}"#, 1099.0, true),
            (r#"{"saved_at": 800, "path": "p"}"#, 1100.0, false),
            (r#"{"saved_at": 700.0}"#, 1000.0, false),
            (r#"{"saved_at": 701.0}"#, 1000.0, true),
            (r#"{"path": "p"}"#, 1000.0, false),
            (r#"{"saved_at": "soon"}"#, 1000.0, false),
            ("{", 1000.0, false),
            ("", 1000.0, false),
        ] {
            fs::write(&latest, text).unwrap();
            assert_eq!(
                saved_recently(dir.path(), now).unwrap(),
                expected,
                "{text} at {now}"
            );
        }
        // Not being able to read it is an error, as for Python's OSError.
        fs::remove_file(&latest).unwrap();
        fs::create_dir(&latest).unwrap();
        assert!(saved_recently(dir.path(), 1000.0).is_err());
    }

    // ---- tmux

    fn world() -> (ScratchTmux, Tmux, TempDir) {
        let scratch = ScratchTmux::new();
        let tmux = Tmux::new().socket(scratch.socket());
        (scratch, tmux, TempDir::new())
    }

    #[test]
    fn terminal_text_is_read_for_every_pane_in_the_order_asked() {
        let (scratch, tmux, _) = world();
        let mut panes = Vec::new();
        for n in 0..11 {
            let command = format!("printf 'pane {n}\\n'; exec sleep 300");
            let pane = scratch.tmux(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                "alpha:",
                &command,
            ]);
            crate::testing::wait_for(
                || {
                    scratch
                        .tmux(&["capture-pane", "-p", "-t", &pane])
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_string()
                },
                &format!("pane {n}"),
            );
            panes.push(pane);
        }
        // Newest first, to show the answer does not follow the order in which the reads finish.
        panes.reverse();
        let texts = capture_all(&tmux, panes.iter().map(String::as_str)).unwrap();
        let expected: Vec<String> = (0..11).rev().map(|n| format!("pane {n}")).collect();
        assert_eq!(texts, expected);
        assert!(capture_all(&tmux, std::iter::empty()).unwrap().is_empty());
    }

    #[test]
    fn terminal_text_that_cannot_be_read_is_the_error_of_the_first_such_pane() {
        let (_scratch, tmux, _) = world();
        let panes = ["%0", "%998", "%0", "%999"];
        let e = capture_all(&tmux, panes.into_iter()).unwrap_err();
        assert!(
            matches!(&e, Error::Command { argv, .. } if argv.contains(&"%998".to_string())),
            "{e}"
        );
    }

    #[test]
    fn restore_has_nothing_to_do_without_a_snapshot_or_a_missing_session() {
        let (scratch, tmux, root) = world();
        assert!(!restore_in(root.path(), &tmux, false).unwrap());
        let snapshot = root.path().join("s.json");
        let data = json!({
            "version": 1, "saved_at": 1.0, "directory": root.path(), "active_session": "alpha",
            "windows": [{"session_name": "alpha", "window_index": "0", "window_name": "one",
                "window_layout": "x", "window_active": "1", "window_width": "120",
                "window_height": "40", "window_zoomed_flag": "0"}],
            "panes": [{"id": "a".repeat(32), "session": "alpha", "window": 0, "index": 0,
                "label": "shell", "cwd": "/", "active": true, "parked": "", "home": "", "app": null}],
        });
        fs::write(&snapshot, data.to_string()).unwrap();
        fs::write(
            root.path().join("latest.json"),
            json!({"saved_at": 1.0, "path": snapshot}).to_string(),
        )
        .unwrap();
        assert!(
            !restore_in(root.path(), &tmux, false).unwrap(),
            "alpha is there"
        );
        assert_eq!(
            scratch.tmux(&["show-options", "-gqv", "@workspace_restoring"]),
            ""
        );
        assert!(root.path().join("lock").exists());
    }

    #[test]
    fn a_locked_restore_does_not_take_the_lock_again() {
        let (_scratch, tmux, root) = world();
        let root = root.path().to_path_buf();
        let _held = lock_in(&root).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let path = root.clone();
        std::thread::spawn(move || {
            let _ = sender.send(restore_in(&path, &tmux, true).map(|_| ()));
        });
        let done = receiver.recv_timeout(Duration::from_secs(5));
        assert!(
            done.is_ok(),
            "the restore waited for the lock its caller holds"
        );
    }

    #[test]
    fn restore_refuses_a_snapshot_it_cannot_read_before_it_touches_tmux() {
        let (scratch, tmux, root) = world();
        let snapshot = root.path().join("s.json");
        for (data, reason) in [
            (json!({"version": 1, "windows": []}), "no panes"),
            (json!({"version": 1, "panes": []}), "no windows"),
            (
                json!({"version": 1, "windows": [], "panes": [{"session": "x"}]}),
                "no id",
            ),
        ] {
            fs::write(&snapshot, data.to_string()).unwrap();
            fs::write(
                root.path().join("latest.json"),
                json!({"path": snapshot}).to_string(),
            )
            .unwrap();
            let e = restore_in(root.path(), &tmux, false).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("The saved workspace is damaged: {reason}"),
                "{reason}"
            );
        }
        assert_eq!(
            scratch.tmux(&["show-options", "-gqv", "@workspace_restoring"]),
            ""
        );
    }

    #[test]
    fn init_does_nothing_on_a_server_that_was_initialized() {
        let (scratch, tmux, root) = world();
        scratch.tmux(&["set-option", "-g", "@workspace_initialized", "1"]);
        initialize_in(root.path(), &tmux).unwrap();
        assert!(!root.path().join("lock").exists(), "not even the lock");
    }

    #[test]
    fn init_marks_the_server_even_when_there_is_nothing_to_restore() {
        let (scratch, tmux, root) = world();
        initialize_in(root.path(), &tmux).unwrap();
        assert_eq!(
            scratch.tmux(&["show-options", "-gqv", "@workspace_initialized"]),
            "1"
        );
    }

    #[test]
    fn history_says_why_there_is_none() {
        let (scratch, tmux, root) = world();
        let pane = scratch.expand("alpha:one", "#{pane_id}");
        let e = history_in(root.path(), &tmux, &pane).unwrap_err();
        assert_eq!(
            e.to_string(),
            "No saved history yet; use prefix Ctrl+s first"
        );
        let mut host = FakeHost::new();
        host.procs.insert(1, Proc::new(0, "??", "launchd"));
        save_in(root.path(), &tmux, &host, true).unwrap();
        let id = scratch.tmux(&["show-options", "-pqv", "-t", &pane, "@workspace_id"]);
        assert_eq!(id.len(), 32, "the save gave the pane its identity");
        // Taking the snapshot's text away leaves the pane out of it.
        let (path, _) = read_snapshot_in(root.path()).unwrap().unwrap();
        fs::remove_file(path.parent().unwrap().join(format!("{id}.txt"))).unwrap();
        let e = history_in(root.path(), &tmux, &pane).unwrap_err();
        assert_eq!(e.to_string(), "This pane is not in the latest snapshot");
    }

    #[test]
    fn a_save_lists_every_pane_once_and_gives_a_snapshot_of_all_of_them() {
        let (scratch, tmux, root) = world();
        scratch.pane(&["split-window", "-t", "alpha:one"]);
        scratch.tmux(&[
            "new-session",
            "-d",
            "-s",
            "__workspace_bootstrap",
            "exec sleep 300",
        ]);
        let mut host = FakeHost::new();
        host.procs.insert(1, Proc::new(0, "??", "launchd"));
        save_in(root.path(), &tmux, &host, true).unwrap();
        let (_, data) = read_snapshot_in(root.path()).unwrap().unwrap();
        assert_eq!(data["panes"].as_array().unwrap().len(), 2);
        assert_eq!(data["windows"].as_array().unwrap().len(), 1);
        assert_eq!(data["panes"][0]["index"], json!(0));
        assert_eq!(data["panes"][1]["index"], json!(1));
        assert_eq!(data["active_session"], json!(""));
    }

    #[test]
    fn a_panic_is_reported_by_its_text() {
        let text = |panic: Box<dyn std::any::Any + Send>| panic_text(panic.as_ref());
        assert_eq!(text(Box::new("boom")), "internal error: boom");
        assert_eq!(
            text(Box::new(String::from("kaboom"))),
            "internal error: kaboom"
        );
        assert_eq!(text(Box::new(5)), "internal error: unknown cause");
    }

    #[test]
    fn a_pane_record_has_python_s_keys_and_a_shell_label_by_default() {
        let mut pane = Fields::default();
        for (name, value) in [
            ("session_name", "s"),
            ("window_index", "2"),
            ("pane_index", "3"),
            ("pane_current_path", "/w"),
            ("pane_active", "1"),
        ] {
            pane.set(name, value);
        }
        let record = pane_record(&pane, "id", None).unwrap();
        assert_eq!(
            dumps(&record, Style::Default, false),
            r#"{"id": "id", "session": "s", "window": 2, "index": 3, "label": "shell", "cwd": "/w", "active": true, "parked": "", "home": "", "app": null}"#
        );
        pane.set("pane_active", "0");
        pane.set("@workspace_label", "api");
        pane.set("window_index", "x");
        assert!(
            pane_record(&pane, "id", None).is_err(),
            "a window index that is no number"
        );
    }
}
