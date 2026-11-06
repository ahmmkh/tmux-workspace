//! The report of `bug` and `idea`: what it says, as Markdown, and the link that opens GitHub's issue
//! form with the same text filled in. Both are made of [`Clean`] text only.

use crate::anonymize::Clean;

/// Which issue form a report is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bug,
    Idea,
}

/// The longest link, in bytes, that GitHub and the browsers take.
pub const MAX_LINK: usize = 8000;

/// Where issues are made.
const NEW_ISSUE: &str = "https://github.com/ahmmkh/tmux-workspace/issues/new";

/// What `bug` found out beyond the environment.
#[derive(Clone, Debug)]
pub struct Diagnostics {
    /// `12 panes · 5 warnings · 0 errors`.
    pub summary: Clean,
    /// The doctor's warnings, the most general first.
    pub warnings: Vec<Clean>,
    /// The last lines of errors.log, the oldest first.
    pub errors: Vec<Clean>,
}

/// What a report is made of.
#[derive(Clone, Debug)]
pub struct Report {
    pub kind: Kind,
    pub environment: Vec<Clean>,
    /// Only a bug has them.
    pub diagnostics: Option<Diagnostics>,
}

/// The link to the issue form, and whether it had to leave something out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub shortened: bool,
}

/// `text` as a URL component: UTF-8 bytes, all but RFC 3986's unreserved characters as `%XX`.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

impl Kind {
    /// The file of the issue form in `.github/ISSUE_TEMPLATE/`.
    pub fn template(self) -> &'static str {
        match self {
            Kind::Bug => "bug.yml",
            Kind::Idea => "idea.yml",
        }
    }

    /// What the title of the issue starts with.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Bug => "[Bug]: ",
            Kind::Idea => "[Idea]: ",
        }
    }
}

fn join(lines: &[Clean]) -> String {
    lines
        .iter()
        .map(Clean::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

impl Diagnostics {
    /// The Doctor and Last errors sections. `shortened` adds the line that says lines were left out.
    fn render(&self, shortened: bool) -> String {
        let mut doctor = vec![self.summary.clone()];
        doctor.extend(self.warnings.iter().cloned());
        let mut text = format!("### Doctor\n{}", join(&doctor));
        if !self.errors.is_empty() || !shortened {
            let errors = if self.errors.is_empty() {
                "none".to_string()
            } else {
                join(&self.errors)
            };
            text.push_str(&format!(
                "\n\n### Last errors (tmux-workspace log)\n{errors}"
            ));
        }
        if shortened {
            text.push_str("\n(shortened to fit the link)");
        }
        text
    }

    /// Leave out one line: the oldest error, or when there is none the last warning.
    fn drop_one(&mut self) -> bool {
        if !self.errors.is_empty() {
            self.errors.remove(0);
            true
        } else {
            self.warnings.pop().is_some()
        }
    }
}

impl Report {
    /// The whole report, as it is shown and copied.
    pub fn text(&self) -> String {
        let mut text = format!("### Environment\n{}", join(&self.environment));
        if let Some(diagnostics) = &self.diagnostics {
            text.push_str("\n\n");
            text.push_str(&diagnostics.render(false));
        }
        text
    }

    /// The link to the issue form. It stays within [`MAX_LINK`] bytes by leaving out the oldest lines
    /// of the diagnostics, and never anything of the environment.
    pub fn link(&self) -> Link {
        let base = format!(
            "{NEW_ISSUE}?template={}&title={}&environment={}",
            self.kind.template(),
            percent_encode(self.kind.title()),
            percent_encode(&join(&self.environment))
        );
        let Some(diagnostics) = &self.diagnostics else {
            return Link {
                url: base,
                shortened: false,
            };
        };
        let mut diagnostics = diagnostics.clone();
        let mut shortened = false;
        loop {
            let url = format!(
                "{base}&diagnostics={}",
                percent_encode(&diagnostics.render(shortened))
            );
            if url.len() <= MAX_LINK {
                return Link { url, shortened };
            }
            shortened = true;
            if !diagnostics.drop_one() {
                return Link {
                    url: base,
                    shortened,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anonymize::{Anonymizer, Identity, Names};

    fn clean(text: &str) -> Clean {
        Anonymizer::new(&Identity::default(), &Names::default()).apply(text)
    }

    fn lines(texts: &[&str]) -> Vec<Clean> {
        texts.iter().map(|text| clean(text)).collect()
    }

    fn environment() -> Vec<Clean> {
        lines(&[
            "tmux-workspace 2.1.0 (release, aarch64-apple-darwin)",
            "macOS 27.2 arm64 · tmux 3.8 · zsh 5.9 · Ghostty",
            "python3 3.15 · fzf ✓ · zoxide ✓ · sesh ✗ · lazygit ✓",
            "integrations: claude ✓ codex ✓ omp ✗ nvim ✓",
        ])
    }

    fn bug(warnings: &[&str], errors: &[&str]) -> Report {
        Report {
            kind: Kind::Bug,
            environment: environment(),
            diagnostics: Some(Diagnostics {
                summary: clean("12 panes · 5 warnings · 0 errors"),
                warnings: lines(warnings),
                errors: lines(errors),
            }),
        }
    }

    fn idea() -> Report {
        Report {
            kind: Kind::Idea,
            environment: environment(),
            diagnostics: None,
        }
    }

    /// The value of a query parameter of the link, still encoded.
    fn param<'a>(url: &'a str, name: &str) -> Option<&'a str> {
        let query = url.split_once('?')?.1;
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
    }

    fn decode(encoded: &str) -> String {
        let bytes = encoded.as_bytes();
        let mut out = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'%' {
                out.push(u8::from_str_radix(&encoded[at + 1..at + 3], 16).unwrap());
                at += 3;
            } else {
                out.push(bytes[at]);
                at += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_value_is_percent_encoded_utf8_with_only_the_unreserved_characters_left() {
        assert_eq!(percent_encode("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(percent_encode("a b\nc"), "a%20b%0Ac");
        assert_eq!(percent_encode("· ✓"), "%C2%B7%20%E2%9C%93");
        assert_eq!(
            percent_encode("/?#&=+%:@!*'()[]"),
            "%2F%3F%23%26%3D%2B%25%3A%40%21%2A%27%28%29%5B%5D"
        );
        assert_eq!(percent_encode(""), "");
    }

    #[test]
    fn a_bug_report_reads_as_three_markdown_sections() {
        let text = bug(
            &["WARN  workspace-3:1.1 codex: No Codex status. Review /hooks"],
            &["2026-11-06 01:12 open: cannot write ~/Projects/folder-2"],
        )
        .text();
        assert_eq!(
            text,
            "### Environment\n\
             tmux-workspace 2.1.0 (release, aarch64-apple-darwin)\n\
             macOS 27.2 arm64 · tmux 3.8 · zsh 5.9 · Ghostty\n\
             python3 3.15 · fzf ✓ · zoxide ✓ · sesh ✗ · lazygit ✓\n\
             integrations: claude ✓ codex ✓ omp ✗ nvim ✓\n\
             \n\
             ### Doctor\n\
             12 panes · 5 warnings · 0 errors\n\
             WARN  workspace-3:1.1 codex: No Codex status. Review /hooks\n\
             \n\
             ### Last errors (tmux-workspace log)\n\
             2026-11-06 01:12 open: cannot write ~/Projects/folder-2"
        );
    }

    #[test]
    fn no_errors_in_the_log_is_said_so_and_an_idea_has_only_the_environment() {
        assert!(
            bug(&[], &[])
                .text()
                .ends_with("### Last errors (tmux-workspace log)\nnone")
        );
        let text = idea().text();
        assert!(
            text.starts_with("### Environment\ntmux-workspace 2.1.0 "),
            "{text}"
        );
        assert!(
            !text.contains("### Doctor") && !text.contains("Last errors"),
            "{text}"
        );
        assert!(
            text.ends_with("integrations: claude ✓ codex ✓ omp ✗ nvim ✓"),
            "{text}"
        );
    }

    #[test]
    fn the_bug_link_carries_a_title_the_environment_and_the_diagnostics() {
        let report = bug(&["WARN  a"], &["2026-11-06 01:12 open: x"]);
        let link = report.link();
        assert!(!link.shortened);
        assert!(
            link.url.starts_with(
                "https://github.com/ahmmkh/tmux-workspace/issues/new?template=bug.yml&title=%5BBug%5D%3A%20&environment="
            ),
            "{}",
            link.url
        );
        let names: Vec<&str> = link
            .url
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .map(|pair| pair.split_once('=').unwrap().0)
            .collect();
        assert_eq!(names, ["template", "title", "environment", "diagnostics"]);
        let environment = decode(param(&link.url, "environment").unwrap());
        assert_eq!(
            environment,
            "tmux-workspace 2.1.0 (release, aarch64-apple-darwin)\n\
             macOS 27.2 arm64 · tmux 3.8 · zsh 5.9 · Ghostty\n\
             python3 3.15 · fzf ✓ · zoxide ✓ · sesh ✗ · lazygit ✓\n\
             integrations: claude ✓ codex ✓ omp ✗ nvim ✓"
        );
        assert_eq!(
            decode(param(&link.url, "diagnostics").unwrap()),
            "### Doctor\n12 panes · 5 warnings · 0 errors\nWARN  a\n\n\
             ### Last errors (tmux-workspace log)\n2026-11-06 01:12 open: x"
        );
        assert!(link.url.is_ascii());
    }

    #[test]
    fn the_idea_link_has_no_diagnostics() {
        let link = idea().link();
        assert!(!link.shortened);
        assert!(
            link.url.starts_with(
                "https://github.com/ahmmkh/tmux-workspace/issues/new?template=idea.yml&title=%5BIdea%5D%3A%20&environment="
            ),
            "{}",
            link.url
        );
        assert_eq!(param(&link.url, "diagnostics"), None);
        assert_eq!(link.url.matches('&').count(), 2);
    }

    #[test]
    fn a_link_over_eight_thousand_bytes_loses_the_oldest_errors_first() {
        let errors: Vec<String> = (0..10)
            .map(|n| format!("2026-11-06 01:{n:02} open: {}", "x ".repeat(300)))
            .collect();
        let refs: Vec<&str> = errors.iter().map(String::as_str).collect();
        let report = bug(&["WARN  a", "WARN  b"], &refs);
        let link = report.link();
        assert!(link.url.len() <= MAX_LINK, "{}", link.url.len());
        assert!(link.shortened);
        let diagnostics = decode(param(&link.url, "diagnostics").unwrap());
        assert!(diagnostics.contains("01:09 open"), "the newest stays");
        assert!(!diagnostics.contains("01:00 open"), "the oldest goes");
        assert!(
            diagnostics
                .starts_with("### Doctor\n12 panes · 5 warnings · 0 errors\nWARN  a\nWARN  b\n")
        );
        assert!(
            diagnostics.ends_with("(shortened to fit the link)"),
            "{diagnostics}"
        );
        // Whatever stays of the errors is a run of the newest, with none missing in between.
        let kept: Vec<usize> = (0..10)
            .filter(|n| diagnostics.contains(&format!("01:{n:02} open")))
            .collect();
        assert_eq!(kept, (10 - kept.len()..10).collect::<Vec<_>>());
        // And the environment is whole.
        assert_eq!(
            decode(param(&link.url, "environment").unwrap()),
            report
                .environment
                .iter()
                .map(|line| line.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn when_the_errors_are_gone_the_last_warnings_go_and_the_summary_stays() {
        let warnings: Vec<String> = (0..200)
            .map(|n| format!("WARN  workspace-{n}:1.1 codex: No Codex status. Review /hooks"))
            .collect();
        let refs: Vec<&str> = warnings.iter().map(String::as_str).collect();
        let link = bug(&refs, &["2026-11-06 01:12 open: x"]).link();
        assert!(link.url.len() <= MAX_LINK, "{}", link.url.len());
        assert!(link.shortened);
        let diagnostics = decode(param(&link.url, "diagnostics").unwrap());
        assert!(diagnostics.starts_with(
            "### Doctor\n12 panes · 5 warnings · 0 errors\nWARN  workspace-0:1.1 codex"
        ));
        assert!(diagnostics.contains("WARN  workspace-1:1.1 codex"));
        assert!(
            !diagnostics.contains("WARN  workspace-199:1.1"),
            "the last ones go first"
        );
        assert!(
            !diagnostics.contains("open: x"),
            "the errors went before the warnings"
        );
        assert!(!diagnostics.contains("Last errors"));
    }

    #[test]
    fn the_diagnostics_go_whole_before_the_environment_loses_a_line() {
        let mut report = bug(&[], &[]);
        report.diagnostics.as_mut().unwrap().summary = clean(&"y ".repeat(5000));
        let link = report.link();
        assert!(link.url.len() <= MAX_LINK, "{}", link.url.len());
        assert!(link.shortened);
        assert_eq!(param(&link.url, "diagnostics"), None);
        assert!(decode(param(&link.url, "environment").unwrap()).ends_with("nvim ✓"));
    }

    #[test]
    fn a_link_that_fits_is_not_shortened() {
        let link = bug(&["WARN  a"], &["e1", "e2"]).link();
        assert!(!link.shortened);
        assert!(!decode(param(&link.url, "diagnostics").unwrap()).contains("shortened"));
    }
}
