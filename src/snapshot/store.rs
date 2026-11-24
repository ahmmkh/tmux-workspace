//! Snapshot locking, atomic writes, and reading `latest.json`.

use crate::error::{Error, Result, fail};
use crate::paths::{self, cannot_read, cannot_write, make_private_dir};
use crate::text::uuid4_hex;
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// Files

/// Hold the state folder's lock until the file is dropped. Saves and restores of all processes take
/// turns. The folder is made private, with its lock file.
pub fn lock() -> Result<File> {
    lock_in(&paths::state_dir())
}

/// `lock` for the state folder `root`.
pub fn lock_in(root: &Path) -> Result<File> {
    make_private_dir(root).map_err(|e| cannot_write(root, e))?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))
        .map_err(|e| cannot_write(root, e))?;
    let path = root.join("lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| cannot_write(&path, e))?;
    file.lock().map_err(|e| cannot_write(&path, e))?;
    Ok(file)
}

/// Flush a file to the drive as Python's `os.fsync` does: `fsync(2)`. Rust's `File::sync_all` asks more
/// on macOS, where it also has the drive empty its cache (`F_FULLFSYNC`). That is 4 ms a file, and a
/// save writes a file for each pane, 28 ms of the save of five panes.
pub(super) fn fsync(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor belongs to `file`, which outlives the call.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Write a file so that a reader sees all of it or none: to a private temporary file in the same
/// folder, flushed to disk, then renamed over the target.
pub fn atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    make_private_dir(dir).map_err(|e| cannot_write(dir, e))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let (mut file, temporary) = create_temporary(dir, &name).map_err(|e| cannot_write(path, e))?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| fsync(&file))
        .and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written.map_err(|e| cannot_write(path, e))
}

/// A new private file `.<name><8 random characters>` in `dir`, as `mkstemp` makes it.
pub fn create_temporary(dir: &Path, name: &str) -> io::Result<(File, PathBuf)> {
    loop {
        let path = dir.join(format!(".{name}{}", &uuid4_hex()[..8]));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            other => return other.map(|file| (file, path)),
        }
    }
}

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
pub(super) fn read_json(path: &Path) -> Result<Option<Value>> {
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(cannot_read(path, e)),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| Error::Json(e.to_string()))
}
