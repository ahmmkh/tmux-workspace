//! The anonymizer: the one function that stands between what this machine knows and what a bug report
//! says. Everything a report or a link holds has passed through [`Anonymizer::apply`], and the type of
//! its result, [`Clean`], cannot be made any other way.

use crate::paths::{account_name, home};
use crate::text::is_printable;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

/// Text that has been through the anonymizer, or a literal of this program that no user data can be in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clean(String);

impl Clean {
    /// Text that is part of this program, such as a heading.
    pub fn literal(text: &'static str) -> Clean {
        Clean(text.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Clean {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who and where this machine is: what a report must not say.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    /// The home folder as `HOME` gives it and as the file system resolves it.
    pub homes: Vec<String>,
    /// The login name, as `USER`, `LOGNAME` and the account database give it.
    pub users: Vec<String>,
    /// The host name, short and full.
    pub hosts: Vec<String>,
}

impl Identity {
    /// This machine: `HOME` and where it leads, the login name as `USER`, `LOGNAME` and the account
    /// database have it, and the host name.
    pub fn current() -> Identity {
        let variable = |name: &str| std::env::var(name).ok();
        Identity::from_parts(
            &home(),
            [variable("USER"), variable("LOGNAME"), Some(account_name())],
            &host_name(),
        )
    }

    /// The identity of a home folder, login names (the empty ones do not count) and a host name, whose
    /// short form is what comes before its first dot.
    pub fn from_parts(
        home: &Path,
        users: impl IntoIterator<Item = Option<String>>,
        host: &str,
    ) -> Identity {
        let mut identity = Identity::default();
        let push = |list: &mut Vec<String>, name: String| {
            if !name.is_empty() && !list.contains(&name) {
                list.push(name);
            }
        };
        push(&mut identity.homes, home.to_string_lossy().into_owned());
        if let Ok(real) = home.canonicalize() {
            push(&mut identity.homes, real.to_string_lossy().into_owned());
        }
        for user in users.into_iter().flatten() {
            push(&mut identity.users, user);
        }
        push(&mut identity.hosts, host.to_string());
        push(
            &mut identity.hosts,
            host.split('.').next().unwrap_or_default().to_string(),
        );
        identity
    }
}

/// The name of this machine, as `hostname` says it; empty when it cannot be read.
fn host_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: writes at most `buffer.len()` bytes into `buffer`.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return String::new();
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// The names of the tmux server, in the order it lists them.
#[derive(Clone, Debug, Default)]
pub struct Names {
    pub sessions: Vec<String>,
    pub windows: Vec<String>,
    pub labels: Vec<String>,
    /// Names of folders: every component of the paths that are known, and the entries of the projects
    /// folder.
    pub folders: Vec<String>,
}

impl Names {
    /// Every component of `path` is the name of a folder.
    pub fn add_folder_path(&mut self, path: &str) {
        for component in path.split('/') {
            if !matches!(component, "" | "." | "..") && !self.folders.iter().any(|f| f == component)
            {
                self.folders.push(component.to_string());
            }
        }
    }
}

/// Folder names that say nothing about a person or a project. A name of the server that is one of
/// these is left as it is as well.
const GENERIC: [&str; 28] = [
    "Users",
    "home",
    "Projects",
    "projects",
    "src",
    "code",
    "dev",
    "Documents",
    "Desktop",
    "Library",
    "Applications",
    ".local",
    "bin",
    "share",
    "state",
    "lib",
    ".config",
    ".claude",
    ".codex",
    "nvim",
    "tmux-workspace",
    "opt",
    "homebrew",
    "usr",
    "local",
    "tmp",
    "private",
    "var",
];

/// Names of this program's own files and programs that the doctor puts in its lines: they say what
/// is wrong, not who has it.
const OWN: [&str; 14] = [
    "tmux",
    "hooks",
    "hooks.json",
    "settings.json",
    "config.json",
    "latest.json",
    "agent-status.conf",
    ".tmux.conf",
    "errors.log",
    "install.log",
    "tw-clip",
    "tw-sound",
    "current",
    "WORKSPACE_GUIDE.md",
];

/// Words that a report says of its own, and the names of programs that a tab is called after. None of
/// them is a secret, and to map one would change the report: a tab called `zsh` would make the shell of
/// the environment `tab-1`, a session called `errors` the count of them.
const PUBLIC_WORDS: [&str; 52] = [
    "zsh", "bash", "fish", "sh", "dash", "ksh", "tcsh", "csh", "nu", "xonsh", "pwsh", "python",
    "python3", "node", "ruby", "claude", "codex", "omp", "nvim", "vim", "vi", "nano", "emacs",
    "less", "htop", "top", "git", "ssh", "docker", "make", "cargo", "npm", "lazygit", "fzf",
    "zoxide", "sesh", "yazi", "tmux", "macOS", "Linux", "release", "clone", "unknown", "none",
    "pane", "panes", "warning", "warnings", "error", "errors", "doctor", "install",
];

/// What the numbered replacements are called.
const NUMBERED: [&str; 4] = ["workspace", "tab", "label", "folder"];

/// The other replacements.
const ANGLE: [&str; 6] = ["<user>", "<host>", "<email>", "<ip>", "<token>", "<arg>"];

/// What may stand before a path.
const BEFORE_PATH: &str = "\"'`([{<>=,;|:";

/// Prefixes of the keys that services hand out.
const SECRET_PREFIXES: [&str; 9] = [
    "sk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "xoxa-",
    "xoxb-",
    "xoxp-",
    "xoxr-",
    "AKIA",
];

/// A run of this many letters, digits, `_` and `-` that mixes letters and digits is a token.
const LONG_RUN: usize = 24;

/// The names of the Rust targets that a report says (see `bug::target`). They mix letters and digits
/// and the longer ones are as long as a token, but they are no secret.
const TARGETS: [&str; 7] = [
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "armv7-unknown-linux-gnueabihf",
    "riscv64-unknown-linux-gnu",
    "x86_64-apple-darwin",
];

/// One word to replace: a name of the server, the login name or the host name.
#[derive(Clone)]
struct Word {
    needle: String,
    replacement: String,
    /// Whether upper and lower case letters are the same (user and host names).
    fold: bool,
}

/// Replaces what identifies a person, a machine, a project or a secret. It numbers names and folders
/// in the order it meets them, and keeps the numbers for as long as it lives, so that the same name
/// is the same number everywhere in one report.
pub struct Anonymizer {
    homes: Vec<String>,
    /// The names of the server, the login name and the host name, the longest first.
    words: Vec<Word>,
    /// Only the login name and the host name.
    people: Vec<Word>,
    /// The names of folders that are known, the longest first.
    known_folders: Vec<String>,
    folders: HashMap<String, usize>,
}

impl Anonymizer {
    pub fn new(identity: &Identity, names: &Names) -> Anonymizer {
        let mut words: Vec<Word> = Vec::new();
        let mut taken: HashSet<&str> = HashSet::new();
        for (list, prefix) in [
            (&names.sessions, "workspace"),
            (&names.windows, "tab"),
            (&names.labels, "label"),
        ] {
            let mut number = 0;
            for name in list {
                if name.is_empty() || is_public(name) || !taken.insert(name.as_str()) {
                    continue;
                }
                number += 1;
                words.push(Word {
                    needle: name.clone(),
                    replacement: format!("{prefix}-{number}"),
                    fold: false,
                });
            }
        }
        let mut people: Vec<Word> = Vec::new();
        for (list, replacement) in [(&identity.users, "<user>"), (&identity.hosts, "<host>")] {
            for name in list.iter().filter(|name| !name.is_empty()) {
                people.push(Word {
                    needle: name.clone(),
                    replacement: replacement.to_string(),
                    fold: true,
                });
            }
        }
        words.extend(people.iter().cloned());
        // The longest first, so that `acme-secret` is never cut short by `acme`.
        words.sort_by_key(|word| std::cmp::Reverse(word.needle.len()));
        people.sort_by_key(|word| std::cmp::Reverse(word.needle.len()));
        let mut known_folders: Vec<String> = Vec::new();
        for folder in &names.folders {
            if !keeps(folder) && !known_folders.contains(folder) {
                known_folders.push(folder.clone());
            }
        }
        known_folders.sort_by_key(|folder| std::cmp::Reverse(folder.len()));
        let mut homes: Vec<String> = identity
            .homes
            .iter()
            .filter(|home| home.len() > 1)
            .cloned()
            .collect();
        homes.sort_by_key(|home| std::cmp::Reverse(home.len()));
        homes.dedup();
        Anonymizer {
            homes,
            words,
            people,
            known_folders,
            folders: HashMap::new(),
        }
    }

    /// Text that this program wrote itself and that holds no name of the server (the environment, the
    /// counts): what identifies a person or a machine is taken out, and no name of the server is looked for,
    /// so that a tab called `zsh` cannot change the version of a shell.
    pub fn apply_own(&mut self, text: &str) -> Clean {
        self.run(text, false)
    }

    /// `text` without anything that identifies. Applying it again changes nothing.
    pub fn apply(&mut self, text: &str) -> Clean {
        self.run(text, true)
    }

    fn run(&mut self, text: &str, with_names: bool) -> Clean {
        // Nothing but printable characters and line ends: what a terminal or a form could be made to do
        // with the rest is not for a report to say.
        let text: String = text
            .chars()
            .filter(|c| *c == '\n' || is_printable(*c))
            .collect();
        let text = strip_url_credentials(&text);
        let text = hide_emails(&text);
        let text = replace_homes(&text, &self.homes);
        let text = if with_names {
            let text = replace_words(&text, &self.words);
            // The folders that are known, by their names, before any guess at where a path ends.
            self.replace_known_folders(&text)
        } else {
            replace_words(&text, &self.people)
        };
        let text = self.hide_paths(&text);
        let text = hide_ipv6(&text);
        let text = hide_ipv4(&text);
        Clean(hide_tokens(&text))
    }

    /// A folder that is known, as a whole component of a path (after a `/`), is `folder-N`, whatever
    /// its name holds and whatever follows it.
    fn replace_known_folders(&mut self, text: &str) -> String {
        if self.known_folders.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        while at < text.len() {
            if let Some(len) = placeholder_len(text, at) {
                out.push_str(&text[at..at + len]);
                at += len;
                continue;
            }
            if prev_char(text, at) == Some('/') {
                let found = self
                    .known_folders
                    .iter()
                    .find(|folder| {
                        text[at..].starts_with(folder.as_str())
                            && ends_component(text, at + folder.len())
                    })
                    .cloned();
                if let Some(folder) = found {
                    out.push_str(&self.folder_name(&folder));
                    at += folder.len();
                    continue;
                }
            }
            let c = next_char(text, at).unwrap_or(' ');
            out.push(c);
            at += c.len_utf8();
        }
        out
    }

    /// `folder-N` for the folder `component`: its number is the order in which folders come up.
    fn folder_name(&mut self, component: &str) -> String {
        let next = self.folders.len() + 1;
        let number = *self.folders.entry(component.to_string()).or_insert(next);
        format!("folder-{number}")
    }

    /// Every path in `text`: the folders that are not generic are called `folder-N`.
    fn hide_paths(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        while at < text.len() {
            let c = next_char(text, at).unwrap_or(' ');
            if (c == '/' || c == '~') && before_path(prev_char(text, at)) {
                let end = path_end(text, at);
                let run = &text[at..end];
                let is_path = if c == '/' {
                    run.len() > 1
                        && !run.starts_with("//")
                        && !next_char(text, at + 1).is_none_or(char::is_whitespace)
                } else {
                    // `~/x`, `~bob/x`; not `~` alone and not `~5 minutes`.
                    run.split(' ').next().is_some_and(|word| word.contains('/'))
                };
                if is_path {
                    out.push_str(&self.map_path(run));
                    at = end;
                    continue;
                }
            }
            out.push(c);
            at += c.len_utf8();
        }
        out
    }

    fn map_path(&mut self, path: &str) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (index, component) in path.split('/').enumerate() {
            if keeps(component) || (index == 0 && component == "~") {
                parts.push(component.to_string());
            } else {
                parts.push(self.folder_name(component));
            }
        }
        parts.join("/")
    }
}

fn is_generic(name: &str) -> bool {
    GENERIC.contains(&name) || OWN.contains(&name)
}

/// Whether a name of the server is left as it is: a generic one, a number, a version, a program or a
/// word of the report.
fn is_public(name: &str) -> bool {
    is_generic(name)
        || PUBLIC_WORDS.contains(&name)
        || name.bytes().all(|b| b.is_ascii_digit())
        || is_version(name)
}

/// Whether a folder name stays: a generic one, a version, or a replacement made earlier.
fn keeps(component: &str) -> bool {
    component.is_empty()
        || component == "."
        || component == ".."
        || is_generic(component)
        || is_version(component)
        || placeholder_len(component, 0) == Some(component.len())
}

/// `2.0.1`, `3.8`: digits and dots, with a dot.
fn is_version(component: &str) -> bool {
    component.contains('.')
        && component
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn prev_char(text: &str, at: usize) -> Option<char> {
    text[..at].chars().next_back()
}

fn next_char(text: &str, at: usize) -> Option<char> {
    text[at..].chars().next()
}

/// The length of the replacement of an earlier pass that starts at `at`: `<user>`, `workspace-3`.
fn placeholder_len(text: &str, at: usize) -> Option<usize> {
    let rest = &text[at..];
    if rest.starts_with('<') {
        return ANGLE
            .iter()
            .find(|placeholder| rest.starts_with(**placeholder))
            .map(|placeholder| placeholder.len());
    }
    if prev_char(text, at).is_some_and(is_word) {
        return None;
    }
    NUMBERED.iter().find_map(|word| {
        let after = rest.strip_prefix(word)?.strip_prefix('-')?;
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let ends = !after[digits..].chars().next().is_some_and(is_word);
        (digits > 0 && ends).then_some(word.len() + 1 + digits)
    })
}

/// `https://user:password@host/` is `https://host/`.
fn strip_url_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        out.push_str(
            authority
                .rfind('@')
                .map_or(authority, |at| &authority[at + 1..]),
        );
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Whether `domain` reads as a host name: two labels or more.
fn is_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    labels.len() >= 2
        && labels
            .iter()
            .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'))
}

fn hide_emails(text: &str) -> String {
    let local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let domain = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(found) = text[search..].find('@') {
        let at = search + found;
        search = at + 1;
        let start = text[copied..at]
            .char_indices()
            .rev()
            .take_while(|(_, c)| local(*c))
            .last()
            .map_or(at, |(index, _)| copied + index);
        if start == at {
            continue;
        }
        let mut end = at
            + 1
            + text[at + 1..]
                .chars()
                .take_while(|c| domain(*c))
                .map(char::len_utf8)
                .sum::<usize>();
        while end > at + 1 && !text[..end].ends_with(|c: char| c.is_ascii_alphanumeric()) {
            end -= 1;
        }
        if !is_domain(&text[at + 1..end]) {
            continue;
        }
        out.push_str(&text[copied..start]);
        out.push_str("<email>");
        copied = end;
        search = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// `/Users/kh/x` is `~/x`: the home folder, where it is a whole path component.
fn replace_homes(text: &str, homes: &[String]) -> String {
    if homes.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let home = homes.iter().find(|home| {
            let end = at + home.len();
            rest.starts_with(home.as_str())
                && !prev_char(text, at).is_some_and(|c| is_word(c) || matches!(c, '/' | '.' | '-'))
                && match next_char(text, end) {
                    None | Some('/') => true,
                    Some('.') => !text[end + 1..].chars().next().is_some_and(is_word),
                    Some(c) => !(is_word(c) || c == '-'),
                }
        });
        if let Some(home) = home {
            out.push('~');
            at += home.len();
            continue;
        }
        let c = next_char(text, at).unwrap_or(' ');
        out.push(c);
        at += c.len_utf8();
    }
    out
}

/// Whether `word` stands at `at` as a whole word.
fn word_at(text: &str, at: usize, word: &Word) -> bool {
    let end = at + word.needle.len();
    if end > text.len() || !text.is_char_boundary(end) {
        return false;
    }
    let found = &text[at..end];
    let same = if word.fold {
        found.eq_ignore_ascii_case(&word.needle)
    } else {
        found == word.needle
    };
    let first = word.needle.chars().next().unwrap_or(' ');
    let last = word.needle.chars().next_back().unwrap_or(' ');
    same && !(is_word(first) && prev_char(text, at).is_some_and(is_word))
        && !(is_word(last) && next_char(text, end).is_some_and(is_word))
        && !in_version(text, at, end)
}

/// Whether a path component ends at `at`: the end of the text or of the line, a `/`, a colon, a quote, a
/// closing bracket or a pipe. A space, a comma or a full stop does not end one: what follows may be the
/// rest of a longer name, and the path goes on to the end of its segment.
fn ends_component(text: &str, at: usize) -> bool {
    next_char(text, at).is_none_or(|c| {
        matches!(
            c,
            '/' | ':' | '"' | '\'' | '`' | ')' | ']' | '}' | '|' | '\n'
        )
    })
}

/// Whether the text at `at..end` stands between the digits of a version: `1.beta.2`, `v2.0`, `beta.1`.
fn in_version(text: &str, at: usize, end: usize) -> bool {
    let after_digits = text[..at]
        .strip_suffix('.')
        .is_some_and(|before| before.ends_with(|c: char| c.is_ascii_digit()));
    let before_digits = text[end..]
        .strip_prefix('.')
        .is_some_and(|after| after.starts_with(|c: char| c.is_ascii_digit()));
    after_digits || before_digits
}

/// The names of the server, the login and the host, wherever they stand as a whole word.
fn replace_words(text: &str, words: &[Word]) -> String {
    if words.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        if let Some(len) = placeholder_len(text, at) {
            out.push_str(&text[at..at + len]);
            at += len;
            continue;
        }
        if let Some(word) = words.iter().find(|word| word_at(text, at, word)) {
            out.push_str(&word.replacement);
            at += word.needle.len();
            continue;
        }
        let c = next_char(text, at).unwrap_or(' ');
        out.push(c);
        at += c.len_utf8();
    }
    out
}

fn before_path(prev: Option<char>) -> bool {
    prev.is_none_or(|c| c.is_whitespace() || BEFORE_PATH.contains(c))
}

/// Where the path that starts at `start` ends: at the end of its segment, which is the end of the
/// line, a colon before a space (a message puts its path before `: why`), a space before the next path, a
/// bracket that nothing opened in the path, and the quote that closes the quote it came after. Spaces,
/// quotes, brackets and commas inside a folder name are the folder's. To hide a few words too many is
/// safe; to stop at a space is not.
fn path_end(text: &str, start: usize) -> usize {
    let quote = prev_char(text, start).filter(|c| matches!(c, '\'' | '"' | '`'));
    let mut depth = [0i32; 3];
    let mut delimited = false;
    let mut end = start;
    for (offset, c) in text[start..].char_indices() {
        let at = start + offset;
        let rest = &text[at + c.len_utf8()..];
        let after = rest.chars().next();
        let stop = match c {
            '\n' => true,
            ':' => after.is_none_or(|next| next.is_whitespace() || next == '/' || next == '~'),
            ' ' => rest.starts_with('/') || rest.starts_with("~/"),
            '(' | '[' | '{' => {
                depth[bracket(c)] += 1;
                false
            }
            ')' | ']' | '}' => {
                depth[bracket(c)] -= 1;
                depth[bracket(c)] < 0
            }
            c if Some(c) == quote => {
                !after.is_some_and(|next| next.is_alphanumeric() || next == '/' || next == '_')
            }
            _ => false,
        };
        if stop {
            delimited = matches!(c, ':' | ')' | ']' | '}') || Some(c) == quote;
            break;
        }
        end = at + c.len_utf8();
    }
    // The full stop of a sentence and the like are not the path's, unless a colon or a quote closes it.
    while !delimited
        && end > start + 1
        && text[start..end].ends_with(['.', ',', ';', '!', '?', ' '])
    {
        end -= 1;
    }
    end
}

/// Which of the three kinds of brackets `c` is.
fn bracket(c: char) -> usize {
    match c {
        '(' | ')' => 0,
        '[' | ']' => 1,
        _ => 2,
    }
}

/// Whether `part` is an IPv4 address.
fn is_ipv4(part: &str) -> bool {
    let groups: Vec<&str> = part.split('.').collect();
    groups.len() == 4
        && groups.iter().all(|group| {
            (1..=3).contains(&group.len())
                && group.bytes().all(|b| b.is_ascii_digit())
                && group.parse::<u32>().is_ok_and(|n| n <= 255)
        })
}

/// Whether `text` is an IPv6 address: eight groups, or fewer around one `::`.
fn is_ipv6(text: &str) -> bool {
    let doubled = text.matches("::").count();
    if doubled > 1 || text.contains(":::") {
        return false;
    }
    let groups: Vec<&str> = text.split(':').filter(|group| !group.is_empty()).collect();
    let mut count = 0;
    for (index, group) in groups.iter().enumerate() {
        if group.contains('.') {
            if index + 1 != groups.len() || !is_ipv4(group) {
                return false;
            }
            count += 2;
        } else if group.len() <= 4 && group.bytes().all(|b| b.is_ascii_hexdigit()) {
            count += 1;
        } else {
            return false;
        }
    }
    if groups.is_empty() {
        return false;
    }
    if doubled == 1 {
        count <= 7
    } else {
        count == 8 && !text.starts_with(':') && !text.ends_with(':')
    }
}

fn hide_ipv6(text: &str) -> String {
    let candidate = |c: char| c.is_ascii_hexdigit() || c == ':' || c == '.';
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let c = next_char(text, at).unwrap_or(' ');
        let starts = candidate(c)
            && c != '.'
            && !prev_char(text, at).is_some_and(|p| is_word(p) || p == ':' || p == '.');
        if starts {
            let mut end = at
                + text[at..]
                    .chars()
                    .take_while(|c| candidate(*c))
                    .map(char::len_utf8)
                    .sum::<usize>();
            while end > at && text[at..end].ends_with('.') {
                end -= 1;
            }
            if text[at..end].ends_with(':') && !text[at..end].ends_with("::") {
                end -= 1;
            }
            if is_ipv6(&text[at..end]) && !next_char(text, end).is_some_and(is_word) {
                // A zone, as in fe80::1%en0.
                if text[end..].starts_with('%') {
                    end += 1 + text[end + 1..]
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                        .map(char::len_utf8)
                        .sum::<usize>();
                }
                out.push_str("<ip>");
                at = end;
                continue;
            }
        }
        out.push(c);
        at += c.len_utf8();
    }
    out
}

fn hide_ipv4(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let c = next_char(text, at).unwrap_or(' ');
        if c.is_ascii_digit() && !prev_char(text, at).is_some_and(|p| is_word(p) || p == '.') {
            let len = text[at..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .count();
            let mut end = at + len;
            while text[at..end].ends_with('.') {
                end -= 1;
            }
            let after = &text[end..];
            let goes_on = after.chars().next().is_some_and(is_word);
            if is_ipv4(&text[at..end]) && !goes_on {
                out.push_str("<ip>");
                at = end;
                continue;
            }
        }
        out.push(c);
        at += c.len_utf8();
    }
    out
}

/// The part of a run of word characters that starts with the prefix of a key, if any.
fn secret_start(run: &str) -> Option<usize> {
    run.char_indices()
        .filter(|(index, _)| *index == 0 || matches!(run.as_bytes()[index - 1], b'-' | b'_'))
        .find(|(index, _)| {
            SECRET_PREFIXES.iter().any(|prefix| {
                run[*index..].len() > prefix.len() && run[*index..].starts_with(prefix)
            })
        })
        .map(|(index, _)| index)
}

fn hide_tokens(text: &str) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let c = next_char(text, at).unwrap_or(' ');
        if !word(c) {
            out.push(c);
            at += c.len_utf8();
            continue;
        }
        let len: usize = text[at..].chars().take_while(|c| word(*c)).count();
        let run = &text[at..at + len];
        if let Some(start) = secret_start(run) {
            out.push_str(&run[..start]);
            out.push_str("<token>");
        } else if len >= LONG_RUN
            && !TARGETS.contains(&run)
            && run.bytes().any(|b| b.is_ascii_alphabetic())
            && run.bytes().any(|b| b.is_ascii_digit())
        {
            out.push_str("<token>");
        } else {
            out.push_str(run);
        }
        at += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity {
            homes: vec!["/Users/kh".into(), "/private/Users/kh".into()],
            users: vec!["kh".into()],
            hosts: vec!["kh-mbp.local".into(), "kh-mbp".into()],
        }
    }

    fn names(sessions: &[&str], windows: &[&str], labels: &[&str]) -> Names {
        let own = |list: &[&str]| list.iter().map(|name| name.to_string()).collect();
        Names {
            sessions: own(sessions),
            windows: own(windows),
            labels: own(labels),
            ..Names::default()
        }
    }

    fn plain() -> Anonymizer {
        Anonymizer::new(&identity(), &Names::default())
    }

    fn clean(anonymizer: &mut Anonymizer, text: &str) -> String {
        anonymizer.apply(text).as_str().to_string()
    }

    #[test]
    fn the_home_folder_becomes_a_tilde() {
        let mut anon = plain();
        assert_eq!(clean(&mut anon, "/Users/kh"), "~");
        assert_eq!(
            clean(&mut anon, "/Users/kh/Projects/x"),
            "~/Projects/folder-1"
        );
        assert_eq!(
            clean(
                &mut anon,
                "Cannot read /private/Users/kh/.codex/hooks.json: gone"
            ),
            "Cannot read ~/.codex/hooks.json: gone"
        );
    }

    #[test]
    fn a_folder_that_only_starts_like_the_home_is_not_the_home() {
        let mut anon = plain();
        assert_eq!(
            clean(&mut anon, "/Users/kh2/Projects"),
            "/Users/folder-1/Projects"
        );
        assert_eq!(
            clean(&mut anon, "/mnt/Users/kh/x"),
            "/folder-2/Users/<user>/folder-3"
        );
    }

    #[test]
    fn user_and_host_names_are_removed_as_whole_words() {
        let mut anon = plain();
        assert_eq!(
            clean(&mut anon, "login kh on kh-mbp.local"),
            "login <user> on <host>"
        );
        assert_eq!(clean(&mut anon, "kh-mbp is up"), "<host> is up");
        assert_eq!(clean(&mut anon, "Khatab khan okhra"), "Khatab khan okhra");
        assert_eq!(clean(&mut anon, "KH-MBP.LOCAL"), "<host>");
    }

    #[test]
    fn e_mail_addresses_become_email() {
        let mut anon = plain();
        assert_eq!(
            clean(&mut anon, "mail me at first.last+tag@example.co.uk, thanks"),
            "mail me at <email>, thanks"
        );
        assert_eq!(
            clean(&mut anon, "git@github.com:a/b.git"),
            "<email>:a/b.git"
        );
        assert_eq!(clean(&mut anon, "a @ b and @handle"), "a @ b and @handle");
    }

    #[test]
    fn ipv4_addresses_become_ip() {
        let mut anon = plain();
        assert_eq!(clean(&mut anon, "to 192.168.1.20 failed"), "to <ip> failed");
        assert_eq!(clean(&mut anon, "10.0.0.1."), "<ip>.");
        assert_eq!(clean(&mut anon, "127.0.0.1:8080"), "<ip>:8080");
        // Versions are not addresses.
        assert_eq!(
            clean(&mut anon, "tmux 3.8 python 3.15.2"),
            "tmux 3.8 python 3.15.2"
        );
        assert_eq!(clean(&mut anon, "1.2.3.4.5"), "1.2.3.4.5");
        assert_eq!(clean(&mut anon, "v1.2.3.4"), "v1.2.3.4");
        assert_eq!(clean(&mut anon, "1.2.3.256"), "1.2.3.256");
    }

    #[test]
    fn ipv6_addresses_become_ip() {
        let mut anon = plain();
        assert_eq!(clean(&mut anon, "bind ::1 ok"), "bind <ip> ok");
        assert_eq!(clean(&mut anon, "on fe80::1%en0."), "on <ip>.");
        assert_eq!(
            clean(
                &mut anon,
                "2001:db8:0:0:0:ff00:42:8329 and 2001:db8::ff00:42:8329"
            ),
            "<ip> and <ip>"
        );
        assert_eq!(clean(&mut anon, "::ffff:10.0.0.1"), "<ip>");
        // Times, pane locations and MAC addresses are not addresses.
        assert_eq!(
            clean(&mut anon, "at 12:34:56 in a:1.1 aa:bb:cc:dd:ee:ff"),
            "at 12:34:56 in a:1.1 aa:bb:cc:dd:ee:ff"
        );
    }

    #[test]
    fn tokens_with_a_known_prefix_become_token() {
        let mut anon = plain();
        for secret in [
            "sk-live-4eC39HqLyjWDarjtT1zdp7dc",
            "sk-x",
            "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
            "gho_16C7e42F292c6912E7710c838347Ae178B4a",
            "github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuvwxyz",
            "xoxb-123456789012-abcdefghij",
            "xoxa-1",
            "xoxp-1",
            "xoxr-1",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            assert_eq!(
                clean(&mut anon, &format!("key {secret} used")),
                "key <token> used",
                "{secret}"
            );
            assert_eq!(
                clean(&mut anon, &format!("Authorization: Bearer {secret}")),
                "Authorization: Bearer <token>",
                "{secret}"
            );
        }
        // A prefix inside a word is no token.
        assert_eq!(
            clean(&mut anon, "risk-free desk-lamp task-1"),
            "risk-free desk-lamp task-1"
        );
        assert_eq!(
            clean(&mut anon, "xoxz-1 sk- ghp_ AKIA"),
            "xoxz-1 sk- ghp_ AKIA"
        );
    }

    #[test]
    fn a_long_run_that_mixes_letters_and_digits_becomes_token() {
        let mut anon = plain();
        let forty = "a1b2c3d4e5f6a7b8c9d0a1b2c3d4e5f6a7b8c9d0";
        assert_eq!(clean(&mut anon, &format!("sha {forty}.")), "sha <token>.");
        assert_eq!(
            clean(&mut anon, "id 123e4567-e89b-12d3-a456-426614174000 end"),
            "id <token> end"
        );
        let exactly_24 = "abcdefghijklmnopqrstuvw1";
        assert_eq!(exactly_24.len(), 24);
        assert_eq!(clean(&mut anon, exactly_24), "<token>");
        // 23 is short, and neither letters alone nor digits alone mix.
        assert_eq!(
            clean(&mut anon, "abcdefghijklmnopqrstuv1"),
            "abcdefghijklmnopqrstuv1"
        );
        assert_eq!(
            clean(
                &mut anon,
                "abcdefghijklmnopqrstuvwxyzabcdef 123456789012345678901234567890"
            ),
            "abcdefghijklmnopqrstuvwxyzabcdef 123456789012345678901234567890"
        );
    }

    #[test]
    fn a_target_triple_is_no_token_although_it_mixes_letters_and_digits() {
        let mut anon = plain();
        for target in [
            "x86_64-unknown-linux-musl",
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-musl",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "armv7-unknown-linux-gnueabihf",
            "riscv64-unknown-linux-gnu",
        ] {
            let text = format!("tmux-workspace 2.1.0 (release, {target})");
            assert_eq!(clean(&mut anon, &text), text);
        }
        // What only looks like one is a token all the same.
        assert_eq!(
            clean(&mut anon, "x86_64-unknown-linux-musl-9f8e7d6c5b4a"),
            "<token>"
        );
        assert_eq!(
            clean(&mut anon, "x86_64-secret1234567890abcdef-x"),
            "<token>"
        );
    }

    #[test]
    fn credentials_in_a_url_are_removed() {
        let mut anon = plain();
        assert_eq!(
            clean(
                &mut anon,
                "clone https://user:p%40ss@example.com/a/b.git now"
            ),
            "clone https://example.com/a/b.git now"
        );
        assert_eq!(
            clean(&mut anon, "https://bob@example.com:8443/x?y=a@b#z"),
            "https://example.com:8443/x?y=a@b#z"
        );
        assert_eq!(
            clean(&mut anon, "ssh://deploy:hunter2@10.1.2.3/srv"),
            "ssh://<ip>/srv"
        );
        // Without credentials a URL is left alone, its path included.
        assert_eq!(
            clean(
                &mut anon,
                "see https://github.com/ahmmkh/tmux-workspace/issues"
            ),
            "see https://github.com/ahmmkh/tmux-workspace/issues"
        );
    }

    #[test]
    fn tmux_names_get_numbers_in_the_order_the_server_lists_them() {
        let mut anon = Anonymizer::new(
            &identity(),
            &names(
                &["acme-secret", "beta"],
                &["client-x", "logs"],
                &["secret-label"],
            ),
        );
        assert_eq!(
            clean(
                &mut anon,
                "beta:2.1 acme-secret:1.1 in client-x and logs, label secret-label"
            ),
            "workspace-2:2.1 workspace-1:1.1 in tab-1 and tab-2, label label-1"
        );
    }

    #[test]
    fn the_same_name_is_always_the_same_number() {
        let mut anon = Anonymizer::new(&identity(), &names(&["a-b", "c-d"], &[], &[]));
        let first = clean(&mut anon, "c-d then a-b");
        let second = clean(&mut anon, "a-b, c-d, a-b");
        assert_eq!(first, "workspace-2 then workspace-1");
        assert_eq!(second, "workspace-1, workspace-2, workspace-1");
    }

    #[test]
    fn a_name_is_replaced_as_a_whole_word_and_the_longer_name_wins() {
        let mut anon =
            Anonymizer::new(&identity(), &names(&["acme", "acme-secret"], &["one"], &[]));
        assert_eq!(
            clean(&mut anon, "acme-secret acme phone one-off one"),
            "workspace-2 workspace-1 phone tab-1-off tab-1"
        );
        assert_eq!(
            clean(&mut anon, "acmeX Xacme acme_ _acme"),
            "acmeX Xacme acme_ _acme"
        );
    }

    #[test]
    fn a_name_that_is_also_a_window_or_label_keeps_its_first_number() {
        let mut anon = Anonymizer::new(&identity(), &names(&["api"], &["api", "web"], &["web"]));
        assert_eq!(clean(&mut anon, "api web"), "workspace-1 tab-1");
    }

    #[test]
    fn generic_names_are_never_mapped() {
        let mut anon = Anonymizer::new(
            &identity(),
            &names(&["tmux-workspace", "dev", ""], &["src"], &["bin"]),
        );
        assert_eq!(
            clean(&mut anon, "tmux-workspace 2.1.0 in dev, src, bin"),
            "tmux-workspace 2.1.0 in dev, src, bin"
        );
    }

    #[test]
    fn a_name_is_not_mapped_inside_a_number_it_was_given() {
        let mut anon =
            Anonymizer::new(&identity(), &names(&["workspace", "tab", "user"], &[], &[]));
        let once = clean(&mut anon, "workspace tab user <user> workspace-1");
        assert_eq!(
            once,
            "workspace-1 workspace-2 workspace-3 <user> workspace-1"
        );
        assert_eq!(clean(&mut anon, &once), once);
    }

    #[test]
    fn a_path_keeps_the_generic_folders_and_hides_the_others() {
        let mut anon = plain();
        assert_eq!(
            clean(&mut anon, "/Users/kh/Projects/acme/src/lib.rs"),
            "~/Projects/folder-1/src/folder-2"
        );
        assert_eq!(
            clean(
                &mut anon,
                "/opt/homebrew/bin/tmux /usr/local/share/x /var/tmp/y /private/tmp"
            ),
            "/opt/homebrew/bin/tmux /usr/local/share/folder-3 /var/tmp/folder-4 /private/tmp"
        );
        assert_eq!(
            clean(
                &mut anon,
                "~/.local/state/tmux-workspace/snapshots ~/.config/nvim ~/.claude ~/.codex"
            ),
            "~/.local/state/tmux-workspace/folder-5 ~/.config/nvim ~/.claude ~/.codex"
        );
        assert_eq!(
            clean(
                &mut anon,
                "/home/bob/code/dev/lib/Documents/Desktop/Library/Applications"
            ),
            "/home/folder-6/code/dev/lib/Documents/Desktop/Library/Applications"
        );
    }

    #[test]
    fn the_same_folder_is_always_the_same_number_and_folders_count_up_as_they_appear() {
        let mut anon = plain();
        assert_eq!(
            clean(&mut anon, "~/x/y ~/y/x ~/x"),
            "~/folder-1/folder-2 ~/folder-2/folder-1 ~/folder-1"
        );
        assert_eq!(clean(&mut anon, "/z/y"), "/folder-3/folder-2");
    }

    #[test]
    fn a_path_is_found_after_punctuation_and_ends_at_it() {
        let mut anon = plain();
        assert_eq!(
            clean(&mut anon, "(~/a), '~/b' \"~/c\" PATH=~/d:~/e; ~/f."),
            "(~/folder-1), '~/folder-2' \"~/folder-3\" PATH=~/folder-4:~/folder-5; ~/folder-6."
        );
        assert_eq!(
            clean(
                &mut anon,
                "Cannot write ~/Projects/folder-2: Permission denied"
            ),
            "Cannot write ~/Projects/folder-2: Permission denied"
        );
    }

    #[test]
    fn a_path_goes_on_through_spaces_to_the_end_of_its_segment() {
        let mut anon = plain();
        assert_eq!(
            clean(
                &mut anon,
                "write /Users/kh/My Projects/acme secret/x failed"
            ),
            "write ~/folder-1/folder-2/folder-3"
        );
    }

    fn with_folders(folders: &[&str]) -> Anonymizer {
        let known = Names {
            folders: folders.iter().map(|folder| folder.to_string()).collect(),
            ..Names::default()
        };
        Anonymizer::new(&identity(), &known)
    }

    #[test]
    fn a_folder_that_holds_a_space_a_quote_a_bracket_or_a_comma_is_one_folder() {
        for path in [
            "Globex Corp",
            "Jane's Bakery",
            "Initech (Jane Doe)",
            "[Cyberdyne]",
            "hooli,inc",
            "Soylent Green",
            "Acme & Sons, Ltd.",
            "Smith, John",
        ] {
            // Nothing is known of the folder but the line that says it cannot be written: the end of its
            // segment is the end of the path.
            let mut anon = plain();
            let line = format!("Cannot write /Users/kh/Projects/{path}: Permission denied");
            assert_eq!(
                clean(&mut anon, &line),
                "Cannot write ~/Projects/folder-1: Permission denied",
                "{path}"
            );
        }
    }

    #[test]
    fn a_path_in_the_middle_of_a_line_ends_at_the_colon_or_the_next_path() {
        let mut anon = plain();
        assert_eq!(
            clean(
                &mut anon,
                "Cannot read /Users/kh/Ahmed's Clients/Globex Corp/notes.json: gone"
            ),
            "Cannot read ~/folder-1/folder-2/folder-3: gone"
        );
        assert_eq!(
            clean(
                &mut anon,
                "~/Projects/Initech (Jane Doe)/api/x.json: no ~/Projects/[Cyberdyne]/x"
            ),
            "~/Projects/folder-4/folder-5/folder-6: no ~/Projects/folder-7/folder-8"
        );
        assert_eq!(
            clean(
                &mut anon,
                "~/Projects/hooli,inc/plan/latest.json: no ~/b; ~/c."
            ),
            "~/Projects/folder-9/folder-10/latest.json: no ~/folder-11; ~/folder-12."
        );
        // What follows the last component up to the next path is taken for the rest of its name.
        assert_eq!(
            clean(&mut anon, "~/Projects/Smith, John and then ~/d"),
            "~/Projects/folder-13 ~/folder-14"
        );
    }

    #[test]
    fn a_quoted_path_ends_at_the_quote_that_closes_it_and_not_at_a_quote_in_a_name() {
        let mut anon = plain();
        assert_eq!(
            clean(&mut anon, "write \"/Users/kh/Projects/Soylent Green\" now"),
            "write \"~/Projects/folder-1\" now"
        );
        assert_eq!(
            clean(&mut anon, "write '/Users/kh/Projects/Jane's Bakery/x' now"),
            "write '~/Projects/folder-2/folder-3' now"
        );
        assert_eq!(
            clean(
                &mut anon,
                "(see ~/Projects/Initech (Jane Doe)/x) and [~/y z]"
            ),
            "(see ~/Projects/folder-4/folder-3) and [~/folder-5]"
        );
    }

    #[test]
    fn a_folder_that_is_known_is_hidden_whatever_its_name_holds() {
        let mut anon = with_folders(&[
            "Globex Corp",
            "Jane's Bakery",
            "Initech (Jane Doe)",
            "hooli,inc",
            "Odd ) Name",
            "Notes /archive",
        ]);
        assert_eq!(
            clean(
                &mut anon,
                "a /Users/kh/Projects/Globex Corp: b /Users/kh/Projects/Jane's Bakery: c"
            ),
            "a ~/Projects/folder-1: b ~/Projects/folder-2: c"
        );
        assert_eq!(
            clean(
                &mut anon,
                "~/Projects/Initech (Jane Doe)/api: ~/Projects/hooli,inc/x: Globex Corp"
            ),
            "~/Projects/folder-3/folder-5: ~/Projects/folder-4/folder-6: Globex Corp"
        );
        // Names that the end of a segment would cut in two, which only a known name gets whole.
        assert_eq!(
            clean(
                &mut anon,
                "x /Users/kh/Projects/Odd ) Name: and /Users/kh/Projects/Notes /archive/z: no"
            ),
            "x ~/Projects/folder-7: and ~/Projects/folder-8/folder-9: no"
        );
        let mut unknown = plain();
        assert_eq!(
            clean(&mut unknown, "x /Users/kh/Projects/Odd ) Name: no"),
            "x ~/Projects/folder-1) Name: no",
            "the cut an unknown name gets"
        );
        // A known name counts as a whole folder name only: a longer one is left to the segment rule.
        assert_eq!(
            clean(&mut anon, "x /Users/kh/Projects/Globex Corporation/y: no"),
            "x ~/Projects/folder-10/folder-11: no"
        );
        // The same folder, the same number.
        assert_eq!(
            clean(&mut anon, "/Users/kh/Projects/Globex Corp/"),
            "~/Projects/folder-1/"
        );
    }

    #[test]
    fn the_longest_known_folder_is_tried_first_and_generic_ones_are_never_hidden() {
        let mut anon = with_folders(&[
            "Globex Corp",
            "Globex Corp International",
            "Projects",
            "src",
            "",
        ]);
        assert_eq!(
            clean(
                &mut anon,
                "/Users/kh/Projects/Globex Corp International/src/Globex Corp"
            ),
            "~/Projects/folder-1/src/folder-2"
        );
    }

    #[test]
    fn a_tab_called_like_a_program_or_a_number_does_not_change_the_report() {
        let mut anon = Anonymizer::new(
            &identity(),
            &names(
                &["0", "1", "2", "acme"],
                &["zsh", "claude", "codex", "python3", "errors", "release"],
                &["fzf", "42", "3.8"],
            ),
        );
        let text = "tmux-workspace 2.1.0 (release, aarch64-apple-darwin)\nmacOS 27.2 arm64 · tmux 3.8 · zsh 5.9 · Ghostty\n\
                    python3 3.15 · fzf ✓ · zoxide ✓ · sesh ✗ · lazygit ✓\nintegrations: claude ✗ codex ✗ omp ✗ nvim ✗\n\
                    1 pane · 2 warnings · 0 errors\nWARN  0:1.0 codex: No Codex status. Review /hooks\nstatus 1 of 2, acme";
        assert_eq!(
            clean(&mut anon, text),
            text.replace("acme", "workspace-1"),
            "only the name that is a name goes"
        );
    }

    #[test]
    fn a_name_is_not_replaced_inside_a_version_number() {
        let mut anon = Anonymizer::new(&identity(), &names(&["beta", "v2"], &[], &[]));
        assert_eq!(
            clean(&mut anon, "1.beta.2 3.beta v2.0 beta.1 beta"),
            "1.beta.2 3.beta v2.0 beta.1 workspace-1"
        );
    }

    #[test]
    fn text_of_this_program_is_checked_for_who_and_where_but_not_for_names() {
        let mut anon = Anonymizer::new(&identity(), &names(&["Ghostty", "tab"], &[], &[]));
        assert_eq!(
            clean(
                &mut anon,
                "x Ghostty tab kh@box.org kh /Users/kh/Projects/Ghostty"
            ),
            "x workspace-1 workspace-2 <email> <user> ~/Projects/workspace-1"
        );
        assert_eq!(
            anon.apply_own("x Ghostty tab kh@box.org kh-mbp ~/Projects/p")
                .as_str(),
            "x Ghostty tab <email> <host> ~/Projects/folder-1"
        );
    }

    #[test]
    fn an_argument_stand_in_is_a_placeholder_like_the_others() {
        let mut anon = Anonymizer::new(&identity(), &names(&["arg"], &[], &[]));
        let once = clean(&mut anon, "arg park <arg> in /Users/kh/Projects/<arg>/x");
        assert_eq!(once, "workspace-1 park <arg> in ~/Projects/<arg>/folder-1");
        assert_eq!(clean(&mut anon, &once), once);
    }

    #[test]
    fn names_are_collected_from_paths_component_by_component() {
        let mut found = Names::default();
        found.add_folder_path("/Users/kh/Projects/Globex Corp/api");
        found.add_folder_path("/Users/kh/Projects/Globex Corp/web/");
        found.add_folder_path("./x/../y");
        found.add_folder_path("");
        assert_eq!(
            found.folders,
            [
                "Users",
                "kh",
                "Projects",
                "Globex Corp",
                "api",
                "web",
                "x",
                "y"
            ]
        );
    }

    #[test]
    fn a_url_path_is_not_a_folder() {
        let mut anon = plain();
        assert_eq!(
            clean(&mut anon, "GET https://example.com/a/b and //host/x a/b"),
            "GET https://example.com/a/b and //host/x a/b"
        );
    }

    #[test]
    fn placeholders_in_a_path_stay() {
        let mut anon = plain();
        assert_eq!(
            clean(
                &mut anon,
                "/Users/<user>/<email>/workspace-3/tab-1/label-2/folder-9/x"
            ),
            "/Users/<user>/<email>/workspace-3/tab-1/label-2/folder-9/folder-1"
        );
    }

    #[test]
    fn applying_it_twice_changes_nothing() {
        let mut anon = Anonymizer::new(
            &identity(),
            &names(&["acme-secret"], &["client-x"], &["secret-label"]),
        );
        let text = "acme-secret:1.1 client-x secret-label /Users/kh/clients/acme failed for kh@kh-mbp.local \
                    at 10.1.2.3 and ::1 with sk-live-abcdefghijklmnop and a1b2c3d4e5f6a7b8c9d0a1b2c3d4e5f6 \
                    via https://u:p@example.com/x, see ~/Projects/tmux-workspace/bin/tmux-workspace";
        let once = clean(&mut anon, text);
        assert_eq!(clean(&mut anon, &once), once);
        let mut fresh = Anonymizer::new(&identity(), &Names::default());
        assert_eq!(clean(&mut fresh, &once), once);
        for secret in [
            "acme", "client", "secret", "clients", "kh", "10.1.2.3", "sk-live", "a1b2c3", "u:p",
        ] {
            assert!(!once.contains(secret), "{secret} survived in {once}");
        }
    }

    #[test]
    fn text_without_anything_to_hide_is_left_as_it_is() {
        let mut anon = plain();
        let text = "tmux-workspace 2.1.0 (release, aarch64-apple-darwin)\nmacOS 27.2 arm64 · tmux 3.8 · zsh 5.9 · Ghostty\n\
                    python3 3.15 · fzf ✓ · zoxide ✓ · sesh ✗ · lazygit ✓\n12 panes · 5 warnings · 0 errors\n\
                    WARN  workspace-3:1.1 codex: No Codex status. Review /hooks";
        assert_eq!(clean(&mut anon, text), text);
    }

    #[test]
    fn odd_text_goes_through_without_a_panic_and_every_cut_of_it_is_stable() {
        let mut anon = Anonymizer::new(&identity(), &names(&["café"], &["日本"], &["📁"]));
        assert_eq!(clean(&mut anon, ""), "");
        assert_eq!(
            clean(&mut anon, "/Users/kh/Café/📁/日本: é"),
            "~/folder-1/label-1/tab-1: é"
        );
        let text = "é /Users/kh/Café/x kh@kh-mbp.local ::1 1.2.3.4 sk-é-1 https://u:p@h.io/é ~/ a/b @ : :: / ~ <";
        for cut in (0..=text.len()).filter(|cut| text.is_char_boundary(*cut)) {
            let once = clean(&mut anon, &text[..cut]);
            assert_eq!(clean(&mut anon, &once), once, "{:?}", &text[..cut]);
        }
    }

    #[test]
    fn control_characters_are_dropped_and_lines_are_kept() {
        let mut anon = plain();
        assert_eq!(
            clean(&mut anon, "a\u{1b}[31mred\u{7}\u{0}\ttab\r\nnext\nline"),
            "a[31mredtab\nnext\nline"
        );
    }

    #[test]
    fn an_identity_has_the_home_where_it_leads_the_names_once_and_the_short_host() {
        use crate::testing::TempDir;
        let dir = TempDir::new();
        let real = dir.path().join("real-home");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("home-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let identity = Identity::from_parts(
            &link,
            [
                Some("kh".into()),
                Some(String::new()),
                None,
                Some("kh".into()),
                Some("root".into()),
            ],
            "kh-mbp.local",
        );
        assert_eq!(identity.homes[0], link.to_string_lossy());
        assert_eq!(
            identity.homes[1],
            real.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(identity.homes.len(), 2);
        assert_eq!(identity.users, ["kh", "root"]);
        assert_eq!(identity.hosts, ["kh-mbp.local", "kh-mbp"]);
        let plain = Identity::from_parts(Path::new("/nonexistent/home"), [], "box");
        assert_eq!((plain.homes.len(), plain.hosts.len()), (1, 1));
        assert!(
            Identity::from_parts(Path::new("/x"), [], "")
                .hosts
                .is_empty()
        );
    }

    #[test]
    fn literals_and_cleaned_text_display_as_their_text() {
        let mut anon = plain();
        assert_eq!(Clean::literal("### Doctor").to_string(), "### Doctor");
        assert_eq!(anon.apply("a/b").to_string(), "a/b");
    }
}
