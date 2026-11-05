//! Text helpers shared by every command: Python-compatible printable filtering, shell quoting,
//! path shortening, clocks, ids and `strftime`.

use std::collections::hash_map::RandomState;
use std::ffi::CString;
use std::fs::File;
use std::hash::BuildHasher;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Result, fail, py_repr_str};

/// Code points `str.isprintable()` rejects, other than Cc (`char::is_control`): Zs except the
/// space, Zl, Zp, Co, and the Cf characters Python lists. Sorted and non-overlapping, inclusive.
/// Unassigned code points (Cn) are not listed: Rust has no category table, so they stay.
const NOT_PRINTABLE: &[(u32, u32)] = &[
    (0x00A0, 0x00A0), // no-break space (Zs)
    (0x00AD, 0x00AD), // soft hyphen (Cf)
    (0x0600, 0x0605),
    (0x061C, 0x061C),
    (0x06DD, 0x06DD),
    (0x070F, 0x070F),
    (0x0890, 0x0891),
    (0x08E2, 0x08E2),
    (0x1680, 0x1680), // Ogham space mark (Zs)
    (0x180E, 0x180E),
    (0x2000, 0x200A), // spaces (Zs)
    (0x200B, 0x200F), // zero-width and direction marks (Cf)
    (0x2028, 0x2029), // line and paragraph separators (Zl, Zp)
    (0x202A, 0x202E),
    (0x202F, 0x202F), // narrow no-break space (Zs)
    (0x205F, 0x205F), // medium mathematical space (Zs)
    (0x2060, 0x2064),
    (0x2066, 0x206F),
    (0x3000, 0x3000), // ideographic space (Zs)
    (0xE000, 0xF8FF), // private use (Co)
    (0xFEFF, 0xFEFF),
    (0xFFF9, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0001, 0xE0001),
    (0xE0020, 0xE007F),
    (0xF0000, 0xFFFFD),   // private use (Co)
    (0x100000, 0x10FFFD), // private use (Co)
];

/// Python's `str.isprintable` for one character (surrogates cannot occur in a Rust `char`).
pub fn is_printable(c: char) -> bool {
    let code = c as u32;
    !c.is_control()
        && NOT_PRINTABLE
            .binary_search_by(|&(lo, hi)| {
                if hi < code {
                    std::cmp::Ordering::Less
                } else if lo > code {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_err()
}

/// `''.join(c for c in s if c.isprintable())`.
pub fn printable(s: &str) -> String {
    s.chars().filter(|&c| is_printable(c)).collect()
}

/// A pane label: printable characters only, trimmed, at most 80 characters, `shell` when empty.
/// After `printable` the only whitespace left is U+0020, so `trim` agrees with Python's `strip`.
pub fn clean_label(s: &str) -> String {
    let label: String = printable(s).trim().chars().take(80).collect();
    if label.is_empty() {
        "shell".to_string()
    } else {
        label
    }
}

/// `shlex.quote`.
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = |b: u8| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b);
    if s.bytes().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// `shlex.join`.
pub fn shlex_join(args: &[&str]) -> String {
    let quoted: Vec<String> = args.iter().map(|a| shlex_quote(a)).collect();
    quoted.join(" ")
}

/// `shlex.split` (POSIX mode, no comments): whitespace separates words; single quotes keep
/// everything; inside double quotes a backslash only escapes `"` and `\`; outside quotes it
/// escapes any character. The errors carry Python's messages.
pub fn shlex_split(s: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false; // a quoted empty string still makes a word
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return fail("No closing quotation"),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return fail("No escaped character"),
                        },
                        Some(c) => word.push(c),
                        None => return fail("No closing quotation"),
                    }
                }
            }
            '\\' => match chars.next() {
                Some(c) => {
                    started = true;
                    word.push(c);
                }
                None => return fail("No escaped character"),
            },
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// `~/code` for a path under the home folder, also when one of them is reached through a symlink:
/// `home` as given is tried first, then its canonical form.
pub fn short_path(path: &str, home: &Path) -> String {
    let mut prefixes = vec![home.to_string_lossy().into_owned()];
    if let Ok(real) = home.canonicalize() {
        let real = real.to_string_lossy().into_owned();
        if real != prefixes[0] {
            prefixes.push(real);
        }
    }
    for prefix in &prefixes {
        if path == prefix {
            return "~".to_string();
        }
        if let Some(rest) = path
            .strip_prefix(prefix.as_str())
            .filter(|r| r.starts_with('/'))
        {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

/// `time.strftime(fmt, time.localtime(epoch))`: the process's local time zone, C-locale names.
/// An empty result comes back for a format that contains a NUL or exceeds 8 KiB of output.
pub fn strftime(fmt: &str, epoch: f64) -> String {
    let Ok(format) = CString::new(fmt) else {
        return String::new();
    };
    if fmt.is_empty() {
        return String::new();
    }
    let secs = epoch.floor() as libc::time_t;
    // SAFETY: `tm` is plain data that `localtime_r` fills in; `strftime` writes at most `size`
    // bytes into `buf` and reads the NUL-terminated `format`.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&secs, &mut tm).is_null() {
            return String::new();
        }
        for size in [128, 512, 2048, 8192] {
            let mut buf = vec![0u8; size];
            let written = libc::strftime(buf.as_mut_ptr().cast(), size, format.as_ptr(), &tm);
            if written > 0 {
                buf.truncate(written);
                return String::from_utf8_lossy(&buf).into_owned();
            }
        }
    }
    String::new()
}

/// `uuid.uuid4().hex`: 16 random bytes with the version (4) and variant bits set. The bytes come
/// from `/dev/urandom`; if that cannot be read, from the randomly keyed hasher of the standard
/// library, so two ids never collapse into one.
pub fn uuid4_hex() -> String {
    let mut bytes = [0u8; 16];
    if File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_err()
    {
        for (i, half) in bytes.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            *half = RandomState::new().hash_one(i).to_le_bytes();
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// `time.time()`.
pub fn now() -> f64 {
    since_epoch().as_secs_f64()
}

/// `time.time_ns()`.
pub fn now_ns() -> u128 {
    since_epoch().as_nanos()
}

/// `int(time.time() * 1000)`.
pub fn now_ms() -> i64 {
    since_epoch().as_millis() as i64
}

/// Python's `str.isspace` for one character: Unicode `White_Space`, plus the separators U+001C to U+001F
/// that Rust leaves out.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\x1c'..='\x1f')
}

/// `s.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// `int(s)` for decimal text: surrounding whitespace and a sign are allowed. The error carries Python's message.
pub fn py_int(s: &str) -> Result<i128> {
    py_strip(s).parse().or_else(|_| {
        fail(format!(
            "invalid literal for int() with base 10: {}",
            py_repr_str(s)
        ))
    })
}

/// `s.splitlines()`: lines end at `\n`, `\r`, `\r\n`, `\v`, `\f`, U+001C to U+001E, U+0085, U+2028 and U+2029.
pub fn py_splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if !matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c'..='\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            continue;
        }
        lines.push(&s[start..at]);
        start = at + c.len_utf8();
        if c == '\r' && chars.next_if(|&(_, next)| next == '\n').is_some() {
            start += 1;
        }
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

/// `s.split(None, maxsplit)`: runs of whitespace separate fields, leading whitespace is skipped, and
/// the rest after `maxsplit` splits is kept as it is.
pub fn py_split_ws(s: &str, maxsplit: usize) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut rest = s.trim_start_matches(is_py_space);
    while !rest.is_empty() {
        let Some(end) = rest.find(is_py_space).filter(|_| fields.len() < maxsplit) else {
            fields.push(rest);
            break;
        };
        fields.push(&rest[..end]);
        rest = rest[end..].trim_start_matches(is_py_space);
    }
    fields
}

/// `str.ljust(width)`: pads with spaces, counting characters.
pub fn ljust(s: &str, width: usize) -> String {
    let len = s.chars().count();
    format!("{s}{}", " ".repeat(width.saturating_sub(len)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    #[test]
    fn shlex_quote_matches_python() {
        for (s, q) in [
            ("plain", "plain"),
            ("with space", "'with space'"),
            ("it's", r#"'it'"'"'s'"#),
            ("", "''"),
            ("a/b-c_d.e:f,g@h%i+j=k", "a/b-c_d.e:f,g@h%i+j=k"),
            ("é", "'é'"),
            ("$HOME", "'$HOME'"),
        ] {
            assert_eq!(shlex_quote(s), q);
        }
        assert_eq!(
            shlex_join(&["/x y/tw", "pane-start", "/s/w.json", "abc"]),
            "'/x y/tw' pane-start /s/w.json abc"
        );
    }

    #[test]
    fn shlex_split_handles_quotes() {
        assert_eq!(
            shlex_split(r#"node /n/codex.js 'a b' "c d""#).unwrap(),
            ["node", "/n/codex.js", "a b", "c d"]
        );
    }

    /// Each case from `shlex.split` in Python 3.15.
    #[test]
    fn shlex_split_matches_python() {
        for (input, expected) in [
            ("", vec![]),
            ("   ", vec![]),
            ("a b  c", vec!["a", "b", "c"]),
            ("'a b'c", vec!["a bc"]),
            (r#""a b"c"#, vec!["a bc"]),
            (r"a\ b", vec!["a b"]),
            (r#""a\"b""#, vec![r#"a"b"#]),
            (r#""a\\b""#, vec![r"a\b"]),
            (r#""a\nb""#, vec![r"a\nb"]),
            ("''", vec![""]),
            (r#""""#, vec![""]),
            ("a''b", vec!["ab"]),
            ("a ''", vec!["a", ""]),
            (r"a\\b", vec![r"a\b"]),
            (r"'a\b'", vec![r"a\b"]),
            ("\ta\nb\r", vec!["a", "b"]),
            ("x\\\ny", vec!["x\ny"]),
            ("#c d", vec!["#c", "d"]),
            ("a#b", vec!["a#b"]),
            ("é 'ü ö'", vec!["é", "ü ö"]),
            (r#""a\$b""#, vec![r"a\$b"]),
            (r#""a\`b""#, vec![r"a\`b"]),
            (r"\a", vec!["a"]),
        ] {
            assert_eq!(shlex_split(input).unwrap(), expected, "{input:?}");
        }
    }

    #[test]
    fn shlex_split_rejects_unterminated_input_like_python() {
        for (input, message) in [
            ("'abc", "No closing quotation"),
            ("\"abc", "No closing quotation"),
            ("abc\\", "No escaped character"),
            ("\"abc\\", "No escaped character"),
        ] {
            let err = shlex_split(input).unwrap_err();
            assert_eq!(err.to_string(), message, "{input:?}");
        }
    }

    #[test]
    fn shlex_split_inverts_shlex_join() {
        let args = ["/x y/tw", "it's", "", "$HOME", "é", "a\"b\\c", "plain"];
        assert_eq!(shlex_split(&shlex_join(&args)).unwrap(), args);
    }

    #[test]
    fn printable_drops_what_python_drops() {
        assert_eq!(
            printable("a\u{200b}b\u{00ad}c\u{2028}d\u{3000}e\u{1}f\u{F0000}g"),
            "abcdefg"
        );
    }

    #[test]
    fn printable_keeps_ordinary_text() {
        assert_eq!(printable("café 😀 ~ x/y"), "café 😀 ~ x/y");
        for c in [
            '\u{a0}',
            '\u{85}',
            '\u{7f}',
            '\u{e000}',
            '\u{feff}',
            '\u{e0001}',
            '\u{10fffd}',
        ] {
            assert!(!is_printable(c), "{c:?}");
        }
        for c in [
            ' ',
            '~',
            'é',
            '\u{2027}',
            '\u{2065}',
            '\u{1d172}',
            '\u{e0000}',
            '\u{e0080}',
        ] {
            assert!(is_printable(c), "{c:?}");
        }
    }

    #[test]
    fn clean_label_trims_and_caps() {
        assert_eq!(clean_label("  \t "), "shell");
        assert_eq!(clean_label(&"é".repeat(90)).chars().count(), 80);
        assert_eq!(clean_label("  my\tlabel \n"), "mylabel");
        assert_eq!(clean_label(""), "shell");
    }

    #[test]
    fn short_path_uses_tilde_for_home_and_its_real_path() {
        let home = Path::new("/h");
        assert_eq!(short_path("/h/x", home), "~/x");
        assert_eq!(short_path("/h", home), "~");
        assert_eq!(short_path("/hx", home), "/hx");
        assert_eq!(short_path("/elsewhere/h/x", home), "/elsewhere/h/x");
    }

    #[test]
    fn short_path_also_matches_the_resolved_home() {
        let tmp = TempDir::new();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let resolved = real.canonicalize().unwrap();
        // home reached through the symlink; the path through the real directory
        let path = format!("{}/code", resolved.display());
        assert_eq!(short_path(&path, &link), "~/code");
        // and the other way round, through the symlink itself
        let path = format!("{}/code", link.display());
        assert_eq!(short_path(&path, &link), "~/code");
    }

    #[test]
    fn uuid4_is_32_hex_with_version_4() {
        let u = uuid4_hex();
        assert_eq!(u.len(), 32);
        assert_eq!(&u[12..13], "4");
        assert!(
            u.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert!("89ab".contains(&u[16..17]));
        assert_ne!(u, uuid4_hex());
    }

    #[test]
    fn strftime_formats_local_time() {
        // 2001-09-09 01:46:40 UTC: the year and month hold in every time zone.
        assert_eq!(strftime("%Y-%m", 1_000_000_000.0), "2001-09");
        assert_eq!(strftime("100%%", 0.0), "100%");
        assert_eq!(strftime("", 0.0), "");
        let hm = strftime("%H:%M", 1_000_000_000.5);
        assert_eq!(hm.len(), 5);
        assert_eq!(&hm[2..3], ":");
        // fractions are floored like time.localtime
        assert_eq!(
            strftime("%Y%m%d%H%M%S", 1_000_000_000.9),
            strftime("%Y%m%d%H%M%S", 1_000_000_000.0)
        );
    }

    #[test]
    fn clocks_agree_with_each_other() {
        let (s, ms, ns) = (now(), now_ms(), now_ns());
        assert!(s > 1.7e9);
        assert!((ms as f64 / 1000.0 - s).abs() < 5.0);
        assert!((ns as f64 / 1e9 - s).abs() < 5.0);
    }

    #[test]
    fn ljust_pads_by_chars() {
        assert_eq!(ljust("ab", 5), "ab   ");
        assert_eq!(ljust("é😀", 4), "é😀  ");
        assert_eq!(ljust("abcdef", 3), "abcdef");
        assert_eq!(ljust("", 2), "  ");
    }

    #[test]
    fn py_int_reads_decimal_text_like_int() {
        assert_eq!(py_int("300").unwrap(), 300);
        assert_eq!(py_int(" -5\n").unwrap(), -5);
        assert_eq!(py_int("+7").unwrap(), 7);
        for bad in ["", "soon", "1.5", "0x10"] {
            assert_eq!(
                py_int(bad).unwrap_err().to_string(),
                format!("invalid literal for int() with base 10: '{bad}'"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn py_strip_also_strips_the_information_separators() {
        assert_eq!(
            py_strip(" \t\n\x1c\x1d\x1e\x1f a b \u{85}\u{a0}\u{2003}\x1f"),
            "a b"
        );
        assert_eq!(py_strip("\x1f\x1f"), "");
        assert_eq!(
            py_strip("é\u{200b}"),
            "é\u{200b}",
            "a zero-width space is not whitespace"
        );
        assert!(is_py_space('\x1c') && is_py_space('\x1f') && is_py_space(' '));
        assert!(!is_py_space('\x1b') && !is_py_space('a') && !is_py_space('\u{200b}'));
    }

    #[test]
    fn py_splitlines_matches_python() {
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_splitlines("a"), ["a"]);
        assert_eq!(py_splitlines("a\nb\n"), ["a", "b"]);
        assert_eq!(py_splitlines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(py_splitlines("a\r\nb\rc"), ["a", "b", "c"]);
        assert_eq!(
            py_splitlines("1\x0b2\x0c3\x1c4\x1d5\x1e6\u{85}7\u{2028}8\u{2029}9"),
            ["1", "2", "3", "4", "5", "6", "7", "8", "9"]
        );
        assert_eq!(
            py_splitlines("a\x1fb"),
            ["a\x1fb"],
            "U+001F does not end a line"
        );
        assert_eq!(py_splitlines("\n"), [""]);
    }

    #[test]
    fn py_split_ws_matches_python() {
        assert_eq!(py_split_ws("  a  b  c  ", 1), ["a", "b  c  "]);
        assert_eq!(
            py_split_ws("1 2 ttys001 /usr/bin/a b", 3),
            ["1", "2", "ttys001", "/usr/bin/a b"]
        );
        assert_eq!(py_split_ws("a b", 3), ["a", "b"]);
        assert_eq!(py_split_ws("   ", 3), Vec::<&str>::new());
        assert_eq!(py_split_ws("", 3), Vec::<&str>::new());
        assert_eq!(py_split_ws("a  ", 1), ["a"]);
        assert_eq!(py_split_ws("a\x1fb", 3), ["a", "b"]);
        assert_eq!(py_split_ws("a b c", 0), ["a b c"]);
    }
}
