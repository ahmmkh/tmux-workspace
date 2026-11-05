//! Saved workspaces: what `save` writes and `restore` and `pane-start` read.

use crate::error::{Error, Result, fail};
use crate::paths::{self, cannot_read};
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

/// The latest snapshot of the state folder: its path and its content. `None` when there is none.
pub fn read_snapshot() -> Result<Option<(PathBuf, Value)>> {
    read_snapshot_in(&paths::state_dir())
}

/// `read_snapshot` for the state folder `root`.
pub fn read_snapshot_in(root: &Path) -> Result<Option<(PathBuf, Value)>> {
    let latest_path = root.join("latest.json");
    let Some(latest) = read_json(&latest_path)? else {
        return Ok(None);
    };
    let Some(path) = latest.get("path").and_then(Value::as_str) else {
        return fail(format!(
            "Cannot read {}: no snapshot path",
            latest_path.display()
        ));
    };
    let path = PathBuf::from(path);
    let Some(data) = read_json(&path)? else {
        return Ok(None);
    };
    // Python compares `data['version'] != 1`: 1.0 passes, a missing version is an error.
    if data.get("version").and_then(Value::as_f64) != Some(1.0) {
        return fail("Unsupported workspace snapshot version");
    }
    Ok(Some((path, data)))
}

/// The JSON in a file. `None` when the file does not exist.
fn read_json(path: &Path) -> Result<Option<Value>> {
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(cannot_read(path, e)),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| Error::Json(e.to_string()))
}

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
}
