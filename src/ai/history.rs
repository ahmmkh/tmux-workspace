//! Opt-in, bounded search of retained terminal snapshots; never current status.

use crate::ai::core::{
    AiError, AiHost, AiResult, Transport, clean, collect_panes, rank, take_chars, word_tokens,
};
use crate::paths;
use crate::sha256;
use crate::text;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

pub const SNAPSHOTS: usize = 3;
pub const METADATA_BYTES: usize = 1024 * 1024;
pub const SOURCE_BYTES: usize = 256 * 1024;
pub const TOTAL_BYTES: usize = 4 * 1024 * 1024;
pub const CHUNK_CHARS: usize = 2500;
pub const CANDIDATES: usize = 32;

fn is_hex32(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_numeric_snapshot(name: &str) -> bool {
    (1..=30).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_digit())
}

enum FsErr {
    NotFound,
    Other,
}

fn fstat_fd(fd: RawFd) -> std::result::Result<libc::stat, FsErr> {
    // SAFETY: `fstat` writes to the stack `libc::stat`.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut st) != 0 {
            return Err(FsErr::Other);
        }
        Ok(st)
    }
}

fn check_owned(fd: RawFd, directory: bool) -> std::result::Result<libc::stat, FsErr> {
    let info = fstat_fd(fd)?;
    let kind = info.st_mode & libc::S_IFMT;
    let expected = if directory {
        libc::S_IFDIR
    } else {
        libc::S_IFREG
    };
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if kind != expected || info.st_uid != uid {
        return Err(FsErr::Other);
    }
    if (info.st_mode & 0o022) != 0 {
        return Err(FsErr::Other);
    }
    if !directory && info.st_nlink != 1 {
        return Err(FsErr::Other);
    }
    Ok(info)
}

fn open_dir(name: &str, parent: Option<RawFd>, owned: bool) -> std::result::Result<OwnedFd, FsErr> {
    let c_name = CString::new(name.as_bytes()).map_err(|_| FsErr::Other)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
    // SAFETY: `c_name` is NUL-terminated; returned fd is wrapped immediately in `OwnedFd`.
    let raw = unsafe {
        match parent {
            Some(dir_fd) => libc::openat(dir_fd, c_name.as_ptr(), flags),
            None => libc::open(c_name.as_ptr(), flags),
        }
    };
    if raw < 0 {
        let errno = std::io::Error::last_os_error().raw_os_error();
        return if errno == Some(libc::ENOENT) {
            Err(FsErr::NotFound)
        } else {
            Err(FsErr::Other)
        };
    }
    // SAFETY: `raw` is a newly opened, valid file descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    if owned {
        check_owned(fd.as_raw_fd(), true)?;
    }
    Ok(fd)
}

fn open_root(path: &Path) -> std::result::Result<(OwnedFd, PathBuf), FsErr> {
    // Walk descriptors rather than resolve(): no symlink, including ancestors,
    // may redirect an authorized read after a path check.
    let expanded = paths::expand_user(&path.to_string_lossy());
    let path_str = expanded.to_string_lossy();
    if expanded
        .components()
        .any(|c| matches!(c, Component::ParentDir))
        || path_str.chars().any(|c| !text::is_printable(c))
    {
        return Err(FsErr::Other);
    }
    let abs = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map_err(|_| FsErr::Other)?
            .join(expanded)
    };
    let mut fd = open_dir("/", None, false)?;
    for comp in abs.components() {
        match comp {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => {
                let name = std::str::from_utf8(part.as_bytes()).map_err(|_| FsErr::Other)?;
                fd = open_dir(name, Some(fd.as_raw_fd()), false)?;
            }
            _ => return Err(FsErr::Other),
        }
    }
    check_owned(fd.as_raw_fd(), true)?;
    Ok((fd, abs))
}

fn read_private_file(
    parent: RawFd,
    name: &str,
    limit: usize,
) -> std::result::Result<(Vec<u8>, bool), FsErr> {
    let c_name = CString::new(name.as_bytes()).map_err(|_| FsErr::Other)?;
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    // SAFETY: `c_name` is NUL-terminated; returned fd is immediately owned.
    let raw = unsafe { libc::openat(parent, c_name.as_ptr(), flags) };
    if raw < 0 {
        let errno = std::io::Error::last_os_error().raw_os_error();
        return if errno == Some(libc::ENOENT) {
            Err(FsErr::NotFound)
        } else {
            Err(FsErr::Other)
        };
    }
    // SAFETY: `raw` is a newly opened file descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let info = check_owned(fd.as_raw_fd(), false)?;
    let file = std::fs::File::from(fd);
    let mut content = Vec::new();
    file.take(limit as u64)
        .read_to_end(&mut content)
        .map_err(|_| FsErr::Other)?;
    let limited = info.st_size as u64 > content.len() as u64;
    Ok((content, limited))
}

fn list_dir_names(
    dir_fd: RawFd,
    max_entries: usize,
) -> std::result::Result<(Vec<String>, bool), FsErr> {
    // SAFETY: `dup` creates an independent fd for `fdopendir` which takes ownership of the dup'd fd.
    let duped = unsafe { libc::dup(dir_fd) };
    if duped < 0 {
        return Err(FsErr::Other);
    }
    // SAFETY: `duped` is an open directory fd.
    let dirp = unsafe { libc::fdopendir(duped) };
    if dirp.is_null() {
        unsafe { libc::close(duped) };
        return Err(FsErr::Other);
    }
    let mut names = Vec::new();
    let mut count = 0usize;
    let mut hit_cap = false;
    loop {
        // SAFETY: `dirp` is a live `DIR*`.
        let entry = unsafe { libc::readdir(dirp) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `d_name` is NUL-terminated inside `dirent`.
        let c_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let Ok(name) = c_name.to_str() else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        if count >= max_entries {
            hit_cap = true;
            break;
        }
        count += 1;
        if is_numeric_snapshot(name) {
            names.push(name.to_string());
        }
    }
    // SAFETY: closes `dirp` and its dup'd fd.
    unsafe { libc::closedir(dirp) };
    Ok((names, hit_cap))
}

fn format_utc_iso(saved: f64) -> Option<String> {
    // Match Python datetime valid range (years 0001..=9999) and reject NaN/inf/overflow.
    if !saved.is_finite() || !(-62_135_596_800.0..=253_402_300_799.0).contains(&saved) {
        return None;
    }
    let secs = saved.floor() as libc::time_t;
    // SAFETY: `tm` is zeroed and populated by `gmtime_r`.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::gmtime_r(&secs, &mut tm).is_null() {
            return None;
        }
        Some(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ))
    }
}

fn decode_utf8_source(raw: &[u8], limited: bool) -> Option<String> {
    match std::str::from_utf8(raw) {
        Ok(s) => Some(s.to_string()),
        Err(err) if limited && err.error_len().is_none() => {
            std::str::from_utf8(&raw[..err.valid_up_to()])
                .ok()
                .map(str::to_string)
        }
        Err(_) => None,
    }
}

fn get_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn authorized_panes(host: &dyn AiHost) -> AiResult<HashMap<String, Value>> {
    let panes = collect_panes(host, false, None).map_err(|e| AiError::Other(e.to_string()))?;
    let mut grouped: HashMap<String, Vec<Value>> = HashMap::new();
    for pane in panes {
        let uid = get_str(&pane, "workspace_id");
        if is_hex32(uid) {
            grouped.entry(uid.to_string()).or_default().push(pane);
        }
    }
    // A copied stable ID is ambiguous, even if one of its panes is opted in.
    Ok(grouped
        .into_iter()
        .filter_map(|(uid, mut vals)| {
            if vals.len() == 1 && vals[0].get("shared").and_then(Value::as_bool) == Some(true) {
                Some((uid, vals.remove(0)))
            } else {
                None
            }
        })
        .collect())
}

fn same_pane(before: &Value, after: &Value) -> bool {
    ["workspace_id", "pane_id", "pane_pid"]
        .iter()
        .all(|k| before.get(*k) == after.get(*k))
}

/// Split `text` into bounded character chunks, returning `(char_offset, 1-based line, chunk)`.
fn chunks(text: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut line = 1usize;
    while start < chars.len() {
        let mut end = (start + CHUNK_CHARS).min(chars.len());
        if end < chars.len()
            && let Some(pos) = chars[start..end].iter().rposition(|&c| c == '\n')
            && pos > 0
        {
            end = start + pos + 1;
        }
        let chunk: String = chars[start..end].iter().collect();
        if !chunk.trim().is_empty() {
            out.push((start, line, chunk.clone()));
        }
        line += chars[start..end].iter().filter(|&&c| c == '\n').count();
        start = end;
    }
    out
}

fn one_line(s: &str) -> String {
    clean(s).split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Location {
    snapshot: String,
    saved: String,
    label: String,
    line: usize,
    offset: usize,
}

struct HistoryRecord {
    uid: String,
    quote: String,
    locations: Vec<Location>,
    pane: Value,
}

/// Return historical source chunks, restricted to current explicit opt-ins.
pub fn search(
    host: &dyn AiHost,
    transport: &dyn Transport,
    query: &str,
) -> AiResult<(Vec<Value>, String)> {
    let query = clean(query).trim().to_string();
    if query.is_empty() {
        return Ok((
            Vec::new(),
            "Enter a query to search retained terminal snapshots.".into(),
        ));
    }
    let authorized = authorized_panes(host)?;
    if authorized.is_empty() {
        return Ok((
            Vec::new(),
            "No unambiguous live pane with a stable workspace ID is opted in. \
             Opt in a pane to search its retained terminal snapshots; no archive text was read."
                .into(),
        ));
    }

    let mut notices: Vec<String> = Vec::new();
    let mut records: Vec<HistoryRecord> = Vec::new();
    let mut record_index: HashMap<(String, String), usize> = HashMap::new();
    let mut consumed = 0usize;
    let mut files = 0usize;
    let mut snapshots = 0usize;
    let mut truncated = 0usize;
    let mut skipped = 0usize;

    let state_root = host.state_root();
    let read_result: std::result::Result<(), FsErr> = (|| {
        let (root_fd, root_path) = open_root(&state_root)?;
        let snapshots_fd = open_dir("snapshots", Some(root_fd.as_raw_fd()), true)?;
        let (mut names, hit_cap) = list_dir_names(snapshots_fd.as_raw_fd(), 4096)?;
        if hit_cap {
            notices.push(
                "Snapshot directory listing stopped at 4096 entries; newest coverage is not guaranteed."
                    .into(),
            );
        }
        names.sort_by(|a, b| {
            let na: u128 = a.parse().unwrap_or(0);
            let nb: u128 = b.parse().unwrap_or(0);
            nb.cmp(&na)
        });
        if names.len() > SNAPSHOTS {
            notices.push(format!(
                "Only newest {SNAPSHOTS} of {} numeric snapshot entries considered.",
                names.len()
            ));
        }
        for name in names.iter().take(SNAPSHOTS) {
            let snap_res: std::result::Result<(), ()> = (|| {
                let dir_fd =
                    open_dir(name, Some(snapshots_fd.as_raw_fd()), true).map_err(|_| ())?;
                let (raw_meta, limited) =
                    read_private_file(dir_fd.as_raw_fd(), "workspace.json", METADATA_BYTES)
                        .map_err(|_| ())?;
                if limited {
                    return Err(());
                }
                let meta_str = std::str::from_utf8(&raw_meta).map_err(|_| ())?;
                let data: Value = serde_json::from_str(meta_str).map_err(|_| ())?;
                let obj = data.as_object().ok_or(())?;
                // Reject booleans masquerading as integer 1.
                match obj.get("version") {
                    Some(Value::Number(n)) if n.as_i64() == Some(1) => {}
                    _ => return Err(()),
                }
                let panes_arr = obj.get("panes").and_then(Value::as_array).ok_or(())?;
                let saved = match obj.get("saved_at") {
                    Some(Value::Number(n)) => n.as_f64().ok_or(())?,
                    _ => return Err(()),
                };
                let timestamp = format_utc_iso(saved).ok_or(())?;
                // Never follow data['directory']; it must name this exact directory.
                let directory = obj.get("directory").and_then(Value::as_str).ok_or(())?;
                if directory.is_empty()
                    || directory.chars().any(|c| !text::is_printable(c))
                    || Path::new(directory)
                        .components()
                        .any(|c| matches!(c, Component::ParentDir))
                {
                    return Err(());
                }
                let dir_abs = if Path::new(directory).is_absolute() {
                    paths::normalize(Path::new(directory))
                } else {
                    paths::normalize(&std::env::current_dir().map_err(|_| ())?.join(directory))
                };
                if dir_abs != paths::normalize(&root_path.join("snapshots").join(name)) {
                    return Err(());
                }
                snapshots += 1;
                let mut seen: HashSet<String> = HashSet::new();
                for pane in panes_arr {
                    let Some(pane_obj) = pane.as_object() else {
                        skipped += 1;
                        continue;
                    };
                    let Some(uid) = pane_obj.get("id").and_then(Value::as_str) else {
                        skipped += 1;
                        continue;
                    };
                    if !is_hex32(uid) {
                        skipped += 1;
                        continue;
                    }
                    if !authorized.contains_key(uid) || !seen.insert(uid.to_string()) {
                        continue;
                    }
                    let remaining = TOTAL_BYTES.saturating_sub(consumed);
                    if remaining == 0 {
                        skipped += 1;
                        continue;
                    }
                    // Recheck consent immediately before each archive text read.
                    let Ok(current) = authorized_panes(host) else {
                        skipped += 1;
                        continue;
                    };
                    let Some(cur_pane) = current.get(uid) else {
                        skipped += 1;
                        continue;
                    };
                    if !same_pane(&authorized[uid], cur_pane) {
                        skipped += 1;
                        continue;
                    }
                    let Ok((raw, limited)) = read_private_file(
                        dir_fd.as_raw_fd(),
                        &format!("{uid}.txt"),
                        SOURCE_BYTES.min(remaining),
                    ) else {
                        skipped += 1;
                        continue;
                    };
                    consumed += raw.len();
                    files += 1;
                    if limited {
                        truncated += 1;
                    }
                    let Some(decoded) = decode_utf8_source(&raw, limited) else {
                        skipped += 1;
                        continue;
                    };
                    let label_raw = pane_obj
                        .get("label")
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_else(|| "shell".to_string());
                    let label_clean = one_line(&label_raw);
                    let label_trimmed = take_chars(&label_clean, 160);
                    let label = if label_trimmed.is_empty() {
                        "shell".to_string()
                    } else {
                        label_trimmed.to_string()
                    };
                    for (offset, line, chunk) in chunks(&decoded) {
                        let key = (uid.to_string(), sha256::hex(chunk.as_bytes()));
                        let loc = Location {
                            snapshot: name.clone(),
                            saved: timestamp.clone(),
                            label: label.clone(),
                            line,
                            offset,
                        };
                        if let Some(&idx) = record_index.get(&key) {
                            records[idx].locations.push(loc);
                            continue;
                        }
                        let quote = clean(&chunk);
                        if quote.trim().is_empty() {
                            continue;
                        }
                        let idx = records.len();
                        record_index.insert(key, idx);
                        records.push(HistoryRecord {
                            uid: uid.to_string(),
                            quote,
                            locations: vec![loc],
                            pane: cur_pane.clone(),
                        });
                    }
                }
                Ok(())
            })();
            if snap_res.is_err() {
                notices.push(format!(
                    "Snapshot {name}: missing, unsafe, corrupt, or unsupported metadata; skipped."
                ));
            }
        }
        Ok(())
    })();

    if let Err(err) = read_result {
        match err {
            FsErr::NotFound => notices.push(
                "No retained snapshot directory found; save a workspace snapshot first.".into(),
            ),
            FsErr::Other => notices.push(
                "Snapshot directory unavailable or unsafe (ownership, permissions, or symlink); no further archive reads.".into(),
            ),
        }
    }

    // Sharing revoked or identity changed while reading: never transmit its text.
    let current = authorized_panes(host)?;
    let total_records = records.len();
    let mut eligible: Vec<HistoryRecord> = records
        .into_iter()
        .filter(|r| {
            current
                .get(&r.uid)
                .is_some_and(|cur| same_pane(&r.pane, cur))
        })
        .collect();
    let removed = total_records - eligible.len();
    if removed > 0 {
        notices.push(format!(
            "{removed} source chunks excluded after sharing/identity changed."
        ));
    }

    let terms = word_tokens(&query);
    eligible.sort_by(|a, b| {
        let ca = terms.intersection(&word_tokens(&a.quote)).count();
        let cb = terms.intersection(&word_tokens(&b.quote)).count();
        cb.cmp(&ca)
    });
    let total_eligible = eligible.len();
    eligible.truncate(CANDIDATES);

    let candidates: Vec<Value> = eligible
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let latest = &record.locations[0];
            let locations = record
                .locations
                .iter()
                .map(|loc| {
                    format!(
                        "Saved {} | pane {} | snapshots/{}/{}.txt | line {}, character {}",
                        loc.saved,
                        loc.label,
                        loc.snapshot,
                        record.uid,
                        loc.line,
                        loc.offset + 1
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            json!({
                "id": format!("history:{index}"),
                "title": format!("Historical | {} | {} | line {}", latest.saved, latest.label, latest.line),
                "detail": format!(
                    "HISTORICAL TERMINAL TEXT — not evidence of current pane status.\n{locations}\n\n\
                     Source quote (terminal controls removed; common secrets redacted):\n{}",
                    record.quote
                ),
                "search_text": take_chars(&record.quote, 4000),
                "pane": current[&record.uid],
            })
        })
        .collect();

    let mut scope = format!(
        "Historical search: {files} authorized text files from {snapshots} retained snapshots; \
         {consumed} source bytes read. {total_eligible} unique authorized chunks; \
         {} chunks selected for ranking. Snapshot retention normally keeps only three saves. \
         Quotes are historical, not current failures.",
        candidates.len()
    );
    if total_eligible > CANDIDATES {
        scope.push_str(
            " Local lexical-overlap shortlist (newest first on ties); \
             not an exhaustive semantic search of retained history.",
        );
    }
    if truncated > 0 {
        scope.push_str(&format!(
            " {truncated} text files limited to their first {SOURCE_BYTES} bytes or remaining byte budget."
        ));
    }
    if consumed >= TOTAL_BYTES {
        scope.push_str(&format!(
            " Total source-read budget reached ({TOTAL_BYTES} bytes); remaining files excluded."
        ));
    }
    if skipped > 0 {
        scope.push_str(&format!(
            " {skipped} missing, corrupt, unsafe, duplicate-invalid, or over-budget source records skipped."
        ));
    }
    notices.insert(0, scope);

    if candidates.is_empty() {
        notices.push("No usable authorized historical source chunks found.".into());
        return Ok((Vec::new(), notices.join(" ")));
    }

    let check_consent = || -> AiResult<()> {
        let latest = authorized_panes(host)?;
        for record in &eligible {
            if !latest
                .get(&record.uid)
                .is_some_and(|cur| same_pane(&record.pane, cur))
            {
                return Err(AiError::EvidenceChanged(
                    "History sharing or pane identity changed; run the search again.".into(),
                ));
            }
        }
        Ok(())
    };
    let (ranked, ranking_notice) = rank(transport, &query, &candidates, Some(&check_consent));
    if !ranking_notice.is_empty() {
        notices.push(ranking_notice);
    }
    Ok((ranked, notices.join(" ")))
}
