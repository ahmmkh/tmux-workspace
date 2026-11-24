//! Self-update CLI (`work update [--check | --auto [on|off|status]]`) and opt-in daily
//! background auto-update hook run at the end of `snapshot::tick`.

use crate::cli::message_via;
use crate::error::{Error, Result, fail};
use crate::errorlog;
use crate::paths::{self, Locations, share_root};
use crate::process::{self, Output, Spawn};
use crate::text;
use crate::tmux::Tmux;
use serde_json::{Map, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

/// Default interval between background auto-update checks (24 hours).
pub const DEFAULT_INTERVAL_SECS: u64 = 86_400;
const AUTO_UPDATE_TIMEOUT: Duration = Duration::from_secs(120);
const USAGE: &str = "Usage: tmux-workspace update [--check | --auto [on|off|status]]";

/// Whether `"auto_update": true` is enabled in the user's `config.json`.
pub fn is_enabled() -> bool {
    is_enabled_in(&Locations::current())
}

pub fn is_enabled_in(locations: &Locations) -> bool {
    locations
        .load_config()
        .ok()
        .and_then(|cfg| cfg.get("auto_update").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// Set `"auto_update"` in the user's `config.json` (preserving all other keys).
pub fn set_enabled_in(locations: &Locations, enabled: bool) -> Result<()> {
    let mut updates = Map::new();
    updates.insert("auto_update".into(), Value::Bool(enabled));
    locations.save_config(updates)?;
    Ok(())
}

/// `update [--check | --auto [on|off|status]]`.
pub fn main(args: &[String]) -> Result<i32> {
    main_with(args, &Locations::current(), &mut std::io::stdout())
}

pub fn main_with(args: &[String], locations: &Locations, out: &mut dyn Write) -> Result<i32> {
    match args {
        [] => Err(exec_installer(locations, false)),
        [flag] if flag == "--check" => Err(exec_installer(locations, true)),
        [flag] if flag == "--auto" => {
            print_status(locations, out)?;
            Ok(0)
        }
        [flag, sub] if flag == "--auto" => match sub.as_str() {
            "status" => {
                print_status(locations, out)?;
                Ok(0)
            }
            "on" => {
                set_enabled_in(locations, true)?;
                writeln!(out, "Auto-update enabled (daily background check).")?;
                Ok(0)
            }
            "off" => {
                set_enabled_in(locations, false)?;
                writeln!(out, "Auto-update disabled.")?;
                Ok(0)
            }
            _ => fail(USAGE),
        },
        _ => fail(USAGE),
    }
}

fn print_status(locations: &Locations, out: &mut dyn Write) -> Result<()> {
    if is_enabled_in(locations) {
        writeln!(out, "auto-update: on (daily)")?;
    } else {
        writeln!(out, "auto-update: off (run: work update --auto on)")?;
    }
    Ok(())
}

/// Exec `bash <share>/install.sh --update [--check]`. Returns only on error.
fn exec_installer(locations: &Locations, check: bool) -> Error {
    let script = locations.share_root().join("install.sh");
    if !script.is_file() {
        return Error::Msg(format!("Missing {}: update needs it", script.display()));
    }
    let mut argv = vec![
        "bash".to_string(),
        script.to_string_lossy().into_owned(),
        "--update".to_string(),
    ];
    if check {
        argv.push("--check".to_string());
    }
    process::exec(&argv)
}

/// Non-blocking exclusive `flock` guard.
struct NonBlockingLock(File);

impl NonBlockingLock {
    fn try_acquire(path: &Path) -> Option<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .ok()?;
        // SAFETY: `file` owns an open fd; `LOCK_EX | LOCK_NB` never blocks.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 { Some(Self(file)) } else { None }
    }
}

impl Drop for NonBlockingLock {
    fn drop(&mut self) {
        // SAFETY: releasing our held flock on drop.
        unsafe {
            let _ = libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn interval_secs() -> u64 {
    std::env::var("TMUX_WORKSPACE_AUTO_UPDATE_INTERVAL")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

fn read_timestamp(path: &Path) -> Option<f64> {
    fs::read_to_string(path).ok()?.trim().parse::<f64>().ok()
}

fn write_timestamp(path: &Path, now: f64) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{}", now.floor() as i64)
}

/// Extract a short user-facing notice when `install.sh --update` actually installed a newer version.
/// Returns `None` when already up to date.
pub fn updated_notice(stdout: &str) -> Option<String> {
    for line in text::py_splitlines(stdout) {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("already up to date") || lower.contains("up to date") {
            return None;
        }
        if lower.contains("updated") && !lower.contains("not updated") {
            let body = trimmed
                .strip_prefix("done")
                .map(str::trim_start)
                .unwrap_or(trimmed);
            return Some(format!("tmux-workspace: {body}"));
        }
    }
    None
}

/// Called at the end of `snapshot::tick`. Returns immediately with zero child processes when
/// disabled or checked within the last 24h; never fails `tick`.
pub fn maybe_auto_update(tmux: &Tmux) {
    let locations = Locations::current();
    let script = share_root().join("install.sh");
    let state = locations.state_dir();
    let enabled = is_enabled_in(&locations);
    let interval = interval_secs();
    let runner = |script_path: &Path| -> Result<Output> {
        process::spawn(&Spawn {
            argv: vec![
                "sh".into(),
                "-c".into(),
                r#"umask 022; exec bash "$1" --update"#.into(),
                "_".into(),
                script_path.to_string_lossy().into_owned(),
            ],
            timeout: Some(AUTO_UPDATE_TIMEOUT),
            ..Spawn::default()
        })
    };
    if let Some(notice) =
        maybe_auto_update_with(enabled, &state, &script, interval, text::now(), &runner)
    {
        message_via(tmux, &notice);
    }
}

/// Testable core of `maybe_auto_update`: returns `Some(notice)` only when an update actually ran
/// and installed a newer release.
pub fn maybe_auto_update_with(
    enabled: bool,
    state_dir: &Path,
    install_script: &Path,
    interval: u64,
    now: f64,
    runner: &dyn Fn(&Path) -> Result<Output>,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let stamp = state_dir.join("last-update-check");
    if let Some(last) = read_timestamp(&stamp)
        && now - last < interval as f64
    {
        return None;
    }
    if paths::make_private_dir(state_dir).is_err() {
        return None;
    }
    let _lock = NonBlockingLock::try_acquire(&state_dir.join("update.lock"))?;
    // Re-check timestamp inside lock in case another tick just finished.
    if let Some(last) = read_timestamp(&stamp)
        && now - last < interval as f64
    {
        return None;
    }
    if !install_script.is_file() {
        errorlog::record(
            "auto-update",
            &[],
            &Error::Msg(format!(
                "Missing {}: auto-update needs it",
                install_script.display()
            )),
        );
        return None;
    }
    // Write timestamp before running so persistent failures or offline hosts don't retry every 5m.
    let _ = write_timestamp(&stamp, now);
    match runner(install_script) {
        Ok(out) if out.status == 0 => updated_notice(&out.stdout),
        Ok(out) => {
            let detail = out.stderr.trim();
            let msg = if detail.is_empty() {
                format!("install.sh --update exited with status {}", out.status)
            } else {
                detail.to_string()
            };
            errorlog::record("auto-update", &[], &Error::Msg(msg));
            None
        }
        Err(err) => {
            errorlog::record("auto-update", &[], &err);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use std::cell::Cell;
    use std::path::PathBuf;

    fn locations_in(tmp: &TempDir) -> Locations {
        let share = tmp.path().join("share");
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join("config.example.json"), "{}\n").unwrap();
        fs::write(share.join("install.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        Locations {
            home: tmp.path().join("home"),
            xdg_config_home: Some(tmp.path().join("config")),
            projects: None,
            state_dir: Some(tmp.path().join("state").to_string_lossy().into_owned()),
            share: Some(share),
        }
    }

    #[test]
    fn auto_toggle_persists_to_config_and_reports_status() {
        let tmp = TempDir::new();
        let loc = locations_in(&tmp);
        assert!(!is_enabled_in(&loc));

        let mut out = Vec::new();
        assert_eq!(main_with(&["--auto".into()], &loc, &mut out).unwrap(), 0);
        assert!(String::from_utf8(out).unwrap().contains("auto-update: off"));

        let mut out = Vec::new();
        assert_eq!(
            main_with(&["--auto".into(), "on".into()], &loc, &mut out).unwrap(),
            0
        );
        assert!(is_enabled_in(&loc));
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Auto-update enabled")
        );

        let mut out = Vec::new();
        assert_eq!(
            main_with(&["--auto".into(), "status".into()], &loc, &mut out).unwrap(),
            0
        );
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("auto-update: on (daily)")
        );

        let mut out = Vec::new();
        assert_eq!(
            main_with(&["--auto".into(), "off".into()], &loc, &mut out).unwrap(),
            0
        );
        assert!(!is_enabled_in(&loc));
    }

    #[test]
    fn disabled_or_within_interval_spawns_nothing() {
        let tmp = TempDir::new();
        let loc = locations_in(&tmp);
        let state = loc.state_dir();
        let script = loc.share_root().join("install.sh");
        let calls = Cell::new(0usize);
        let runner = |_: &Path| {
            calls.set(calls.get() + 1);
            Ok(Output {
                status: 0,
                stdout: "Updated 2.1.0 -> 2.2.0\n".into(),
                stderr: String::new(),
            })
        };

        // Disabled: never runs.
        assert_eq!(
            maybe_auto_update_with(false, &state, &script, 86_400, 1_000_000.0, &runner),
            None
        );
        assert_eq!(calls.get(), 0);

        // First enabled run executes and returns notice.
        let notice = maybe_auto_update_with(true, &state, &script, 86_400, 1_000_000.0, &runner);
        assert!(notice.as_deref().is_some_and(|n| n.contains("2.2.0")));
        assert_eq!(calls.get(), 1);

        // Second run within 24h is rate-limited (0 calls).
        assert_eq!(
            maybe_auto_update_with(true, &state, &script, 86_400, 1_000_100.0, &runner),
            None
        );
        assert_eq!(calls.get(), 1);

        // Run after 24h executes again.
        assert!(
            maybe_auto_update_with(true, &state, &script, 86_400, 1_086_401.0, &runner).is_some()
        );
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn already_up_to_date_and_errors_produce_no_notice_and_held_lock_skips() {
        let tmp = TempDir::new();
        let loc = locations_in(&tmp);
        let state = loc.state_dir();
        let script = loc.share_root().join("install.sh");

        let up_to_date = |_: &Path| {
            Ok(Output {
                status: 0,
                stdout: "tmux-workspace 2.2.0 is already up to date.\n".into(),
                stderr: String::new(),
            })
        };
        assert_eq!(
            maybe_auto_update_with(true, &state, &script, 10, 100.0, &up_to_date),
            None
        );

        let failing = |_: &Path| {
            Ok(Output {
                status: 1,
                stdout: String::new(),
                stderr: "network unreachable".into(),
            })
        };
        assert_eq!(
            maybe_auto_update_with(true, &state, &script, 10, 200.0, &failing),
            None
        );
        // Timestamp was recorded even on failure so it won't spam retries.
        assert_eq!(
            read_timestamp(&state.join("last-update-check")),
            Some(200.0)
        );

        // Held lock causes immediate skip without running.
        let _held = NonBlockingLock::try_acquire(&state.join("update.lock")).unwrap();
        let calls = Cell::new(0usize);
        let counting = |_: &Path| {
            calls.set(calls.get() + 1);
            Ok(Output {
                status: 0,
                stdout: "Updated\n".into(),
                stderr: String::new(),
            })
        };
        assert_eq!(
            maybe_auto_update_with(true, &state, &script, 10, 500.0, &counting),
            None
        );
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn missing_script_error_survives_the_anonymizer() {
        use crate::anonymize::{Anonymizer, Identity, Names};
        let loc = Locations {
            home: PathBuf::from("/tmp/none"),
            xdg_config_home: None,
            projects: None,
            state_dir: None,
            share: Some(PathBuf::from("/opt/clients/Globex Corp")),
        };
        let message = exec_installer(&loc, false).to_string();
        assert_eq!(
            message,
            "Missing /opt/clients/Globex Corp/install.sh: update needs it"
        );
        let shown = Anonymizer::new(&Identity::default(), &Names::default()).apply(&message);
        assert_eq!(
            shown.as_str(),
            "Missing /opt/folder-1/folder-2/folder-3: update needs it"
        );
    }
}
