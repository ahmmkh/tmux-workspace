//! Interactive raw-termios terminal loop for the dashboard. No curses dependency.

use crate::dash::render::{Line, SELECTION_RGB, State, render, selected_index, style_rgb};
use crate::dash::{
    DashHost, Key, handle_key, keep_selection, read_preview, read_rows, say, visible_rows,
};
use crate::error::{Error, Result};
use crate::text;
use std::io::Write;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

static RESIZED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigwinch(_sig: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorMode {
    TrueColor,
    Color256,
    Color16,
}

fn detect_color_mode() -> ColorMode {
    let colorterm = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return ColorMode::TrueColor;
    }
    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if term.contains("256color")
        || term.contains("direct")
        || term.contains("kitty")
        || term.contains("alacritty")
        || term.contains("wezterm")
        || term.contains("ghostty")
        || term.contains("foot")
        || term.starts_with("tmux")
        || term.starts_with("screen")
        || term.starts_with("xterm")
    {
        ColorMode::Color256
    } else {
        ColorMode::Color16
    }
}

fn rgb_to_256((r, g, b): (u8, u8, u8)) -> u8 {
    let cube = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    let level = |i: u8| -> i32 { [0, 95, 135, 175, 215, 255][i as usize] };
    let (cr, cg, cb) = (cube(r), cube(g), cube(b));
    let index = 16 + 36 * cr + 6 * cg + cb;
    let avg = (r as i32 + g as i32 + b as i32) / 3;
    let grey_idx = ((avg - 8).max(0) / 10).min(23);
    let grey = (232 + grey_idx) as u8;
    let grey_v = 8 + 10 * grey_idx;
    let dist = |a: (i32, i32, i32)| -> i32 {
        (a.0 - r as i32).pow(2) + (a.1 - g as i32).pow(2) + (a.2 - b as i32).pow(2)
    };
    if dist((level(cr), level(cg), level(cb))) <= dist((grey_v, grey_v, grey_v)) {
        index
    } else {
        grey
    }
}

fn rgb_to_16(rgb: (u8, u8, u8)) -> u8 {
    match rgb {
        (0xf7, 0x76, 0x8e) => 31, // red
        (0x9e, 0xce, 0x6a) => 32, // green
        (0xe0, 0xaf, 0x68) => 33, // yellow
        (0x7a, 0xa2, 0xf7) => 34, // blue
        (0x7d, 0xcf, 0xff) => 36, // cyan
        _ => 37,                  // white
    }
}

fn sgr_for(style: &str, mode: ColorMode) -> String {
    let names: Vec<&str> = style.split_whitespace().collect();
    let base = match names.first().copied() {
        Some(first) if first != "sel" => first,
        _ => "",
    };
    let (rgb, bold) = style_rgb(base);
    let selected = names.contains(&"sel");
    let mut codes: Vec<String> = Vec::with_capacity(4);
    if bold {
        codes.push("1".to_string());
    }
    match mode {
        ColorMode::TrueColor => {
            codes.push(format!("38;2;{};{};{}", rgb.0, rgb.1, rgb.2));
            if selected {
                codes.push(format!(
                    "48;2;{};{};{}",
                    SELECTION_RGB.0, SELECTION_RGB.1, SELECTION_RGB.2
                ));
            }
        }
        ColorMode::Color256 => {
            codes.push(format!("38;5;{}", rgb_to_256(rgb)));
            if selected {
                codes.push(format!("48;5;{}", rgb_to_256(SELECTION_RGB)));
            }
        }
        ColorMode::Color16 => {
            if matches!(base, "dim" | "frame" | "header") {
                codes.push("2".to_string());
            }
            if selected {
                codes.push("7".to_string());
            }
            codes.push(rgb_to_16(rgb).to_string());
        }
    }
    format!("\x1b[0;{}m", codes.join(";"))
}

struct RawTerminal {
    fd: RawFd,
    orig: libc::termios,
    old_sigwinch: libc::sigaction,
    had_sigwinch: bool,
}

impl RawTerminal {
    fn enter() -> Result<Self> {
        let fd: RawFd = libc::STDIN_FILENO;
        // SAFETY: `orig` is initialized by `tcgetattr` when it returns 0.
        let orig = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return Err(Error::Io(std::io::Error::last_os_error()));
            }
            t
        };
        let mut raw = orig;
        // SAFETY: `cfmakeraw` only mutates the local `termios` struct.
        unsafe {
            libc::cfmakeraw(&mut raw);
            raw.c_oflag |= libc::OPOST;
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) != 0 {
                return Err(Error::Io(std::io::Error::last_os_error()));
            }
        }

        RESIZED.store(false, Ordering::SeqCst);
        let (old_sigwinch, had_sigwinch) = unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_sigwinch as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = 0; // no SA_RESTART so poll wakes immediately on resize
            let mut old: libc::sigaction = std::mem::zeroed();
            let ok = libc::sigaction(libc::SIGWINCH, &sa, &mut old) == 0;
            (old, ok)
        };

        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J");
        let _ = out.flush();

        Ok(Self {
            fd,
            orig,
            old_sigwinch,
            had_sigwinch,
        })
    }

    fn size(&self) -> (usize, usize) {
        // SAFETY: `TIOCGWINSZ` fills the local `winsize` struct.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0
                && ws.ws_col > 0
                && ws.ws_row > 0
            {
                return (ws.ws_col as usize, ws.ws_row as usize);
            }
            if libc::ioctl(self.fd, libc::TIOCGWINSZ, &mut ws) == 0
                && ws.ws_col > 0
                && ws.ws_row > 0
            {
                return (ws.ws_col as usize, ws.ws_row as usize);
            }
        }
        (80, 24)
    }

    fn poll_byte(&self, timeout_ms: i32) -> PollRead {
        if RESIZED.swap(false, Ordering::SeqCst) {
            return PollRead::Resized;
        }
        let mut pfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a valid stack `pollfd`.
        let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if rc == 0 {
            return PollRead::Timeout;
        }
        if rc < 0 {
            if RESIZED.swap(false, Ordering::SeqCst) {
                return PollRead::Resized;
            }
            return PollRead::Timeout;
        }
        if (pfd.revents & libc::POLLIN) == 0 {
            if (pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0 {
                return PollRead::Eof;
            }
            return PollRead::Timeout;
        }
        let mut b = 0u8;
        // SAFETY: reading 1 byte into a valid stack buffer.
        let n = unsafe { libc::read(self.fd, (&mut b as *mut u8).cast(), 1) };
        if n == 1 {
            PollRead::Byte(b)
        } else if n == 0 {
            PollRead::Eof
        } else if RESIZED.swap(false, Ordering::SeqCst) {
            PollRead::Resized
        } else {
            PollRead::Timeout
        }
    }

    fn read_key(&self, timeout_ms: i32) -> Option<Key> {
        let first = match self.poll_byte(timeout_ms) {
            PollRead::Timeout => return None,
            PollRead::Resized => return Some(Key::Resize),
            PollRead::Eof => return Some(Key::Quit),
            PollRead::Byte(b) => b,
        };
        Some(self.decode_byte(first))
    }

    fn decode_byte(&self, first: u8) -> Key {
        match first {
            0x03 => Key::Quit, // Ctrl-C
            b'\r' | b'\n' => Key::Enter,
            0x7f | 0x08 => Key::Backspace,
            0x1b => self.decode_escape(),
            b if b < 0x80 => Key::Char(b as char),
            b => self.decode_utf8(b),
        }
    }

    fn decode_escape(&self) -> Key {
        let next = match self.poll_byte(25) {
            PollRead::Byte(b) => b,
            _ => return Key::Escape,
        };
        match next {
            b'[' => {
                let mut seq = Vec::with_capacity(8);
                for _ in 0..16 {
                    match self.poll_byte(25) {
                        PollRead::Byte(b) => {
                            seq.push(b);
                            if (0x40..=0x7e).contains(&b) {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                decode_csi(&seq)
            }
            b'O' => match self.poll_byte(25) {
                PollRead::Byte(b) => decode_ss3(b),
                _ => Key::Escape,
            },
            _ => Key::Escape,
        }
    }

    fn decode_utf8(&self, first: u8) -> Key {
        let expected = if first & 0xe0 == 0xc0 {
            2
        } else if first & 0xf0 == 0xe0 {
            3
        } else if first & 0xf8 == 0xf0 {
            4
        } else {
            return Key::Other;
        };
        let mut buf = vec![first];
        for _ in 1..expected {
            match self.poll_byte(25) {
                PollRead::Byte(b) => buf.push(b),
                _ => return Key::Other,
            }
        }
        match std::str::from_utf8(&buf)
            .ok()
            .and_then(|s| s.chars().next())
        {
            Some(c) => Key::Char(c),
            None => Key::Other,
        }
    }

    fn paint(&self, lines: &[Line], mode: ColorMode) {
        let mut buf = String::with_capacity(lines.len() * 128);
        // Disable auto-wrap during paint so writing the bottom-right cell never scrolls.
        buf.push_str("\x1b[?7l\x1b[H");
        for (y, line) in lines.iter().enumerate() {
            buf.push_str(&format!("\x1b[{};1H", y + 1));
            for (seg_text, style) in line {
                buf.push_str(&sgr_for(style, mode));
                buf.push_str(seg_text);
            }
            buf.push_str("\x1b[0m\x1b[K");
        }
        buf.push_str("\x1b[J\x1b[?7h");
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(buf.as_bytes());
        let _ = out.flush();
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        // SAFETY: restoring previously saved termios and SIGWINCH handler.
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.orig);
            if self.had_sigwinch {
                let _ = libc::sigaction(libc::SIGWINCH, &self.old_sigwinch, std::ptr::null_mut());
            }
        }
    }
}

enum PollRead {
    Byte(u8),
    Timeout,
    Resized,
    Eof,
}

pub(crate) fn decode_csi(seq: &[u8]) -> Key {
    match seq {
        b"A" => Key::Up,
        b"B" => Key::Down,
        b"H" | b"1~" | b"7~" => Key::Home,
        b"F" | b"4~" | b"8~" => Key::End,
        _ => Key::Other,
    }
}

pub(crate) fn decode_ss3(final_byte: u8) -> Key {
    match final_byte {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'M' => Key::Enter,
        _ => Key::Other,
    }
}

pub fn run_loop(host: &dyn DashHost) -> Result<i32> {
    let term = RawTerminal::enter()?;
    let color_mode = detect_color_mode();
    let mut state = State {
        origin: std::env::var("TMUX_PANE").unwrap_or_default(),
        ..State::default()
    };
    let mut rows = Vec::new();
    let mut read_at = 0.0f64;
    let mut index = 0usize;

    while !state.quit {
        if text::now() - read_at >= 1.0 {
            match read_rows(host) {
                Ok(fresh) => rows = fresh,
                Err(err) => say(&mut state, &err.to_string()),
            }
            read_at = text::now();
        }
        let shown = visible_rows(&rows, &state);
        keep_selection(&shown, &mut state, index);
        index = if shown.is_empty() {
            0
        } else {
            selected_index(&shown, &state)
        };
        let (width, height) = term.size();
        let preview = if !shown.is_empty() && height >= 20 {
            read_preview(host, &shown[index].pane_id, 10)
        } else {
            Vec::new()
        };
        term.paint(&render(&shown, &state, width, height, &preview), color_mode);

        let Some(key) = term.read_key(1000) else {
            continue;
        };
        if key == Key::Resize {
            continue;
        }
        if key == Key::Quit {
            state.quit = true;
            break;
        }
        let before = (state.mode, state.message.clone());
        let trigger_refresh = matches!(key, Key::Enter | Key::Char('p' | 'u' | 'y' | 'Y'));
        handle_key(key, &mut state, &shown, host);
        if (state.mode, state.message.clone()) != before || trigger_refresh {
            read_at = 0.0;
        }
    }
    Ok(0)
}
