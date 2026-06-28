use std::env;
use std::io::{self, IsTerminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TuiCapabilities {
    pub colors: bool,
    /// Use the terminal alternate screen buffer. Disabled for GNU screen and other conservative profiles.
    pub alternate_screen: bool,
    /// Enable mouse reporting. Disabled for multiplexers/legacy consoles that often leak escape sequences.
    pub mouse_capture: bool,
    /// Enable bracketed paste. Disabled for terminals that are known to mis-handle it.
    pub bracketed_paste: bool,
    /// Line-mode fallback should rewrite VERASE to Ctrl+H for terminals that emit BS.
    pub ctrl_h_backspace: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleMode {
    Line,
    Tui(TuiCapabilities),
}

#[cfg(unix)]
pub struct EraseKeyGuard {
    fd: std::os::fd::RawFd,
    original: libc::termios,
}

#[cfg(unix)]
impl EraseKeyGuard {
    pub fn ctrl_h() -> io::Result<Self> {
        use std::mem::MaybeUninit;
        use std::os::fd::AsRawFd;

        let fd = io::stdin().as_raw_fd();
        let mut attributes = MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }

        let original = unsafe { attributes.assume_init() };
        let mut updated = original;
        updated.c_cc[libc::VERASE] = b'\x08' as libc::cc_t;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &updated) } != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { fd, original })
    }
}

#[cfg(unix)]
impl Drop for EraseKeyGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

#[cfg(not(unix))]
pub struct EraseKeyGuard;

#[cfg(not(unix))]
impl EraseKeyGuard {
    pub fn ctrl_h() -> io::Result<Self> {
        Ok(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalProfile {
    tui: bool,
    screen: bool,
    color: bool,
    alternate_screen: bool,
    mouse_capture: bool,
    bracketed_paste: bool,
    ctrl_h_backspace: bool,
}

impl TerminalProfile {
    pub fn detect() -> Self {
        let term = env::var("TERM").unwrap_or_default();
        Self::from_context(
            &term,
            env::var_os("STY").is_some(),
            env::var_os("TMUX").is_some(),
            env::var_os("NO_COLOR").is_some(),
            io::stdin().is_terminal() && io::stdout().is_terminal(),
        )
    }

    fn from_context(
        term: &str,
        has_sty: bool,
        has_tmux: bool,
        no_color: bool,
        interactive: bool,
    ) -> Self {
        let term_lc = term.to_ascii_lowercase();
        let screen = has_sty || (term_lc.starts_with("screen") && !has_tmux);
        let dumb = term_lc.is_empty() || term_lc == "dumb";
        let linux_console = term_lc == "linux" || term_lc.starts_with("vt");
        let ansi_console = term_lc == "ansi" || term_lc == "cons25";
        let emacs_shell = term_lc.contains("emacs");
        let conservative = screen || linux_console || ansi_console;
        let tui = interactive && !dumb && !emacs_shell;
        let color = interactive && !no_color && !dumb;
        Self {
            tui,
            screen,
            color,
            alternate_screen: tui && !conservative,
            mouse_capture: tui && !conservative,
            bracketed_paste: tui && !conservative,
            ctrl_h_backspace: screen && !has_tmux,
        }
    }

    pub fn console_mode(self) -> ConsoleMode {
        if !self.tui {
            return ConsoleMode::Line;
        }

        ConsoleMode::Tui(TuiCapabilities {
            colors: self.color,
            alternate_screen: self.alternate_screen,
            mouse_capture: self.mouse_capture,
            bracketed_paste: self.bracketed_paste,
            ctrl_h_backspace: self.ctrl_h_backspace,
        })
    }

    pub fn is_screen(self) -> bool {
        self.screen
    }

    pub fn apply_environment(self) {
        if !self.color {
            env::set_var("NO_COLOR", "1");
            env::set_var("CLICOLOR", "0");
            env::set_var("CLICOLOR_FORCE", "0");
        }
    }
}

pub fn strip_ansi(input: &str) -> String {
    let chars = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;

    while index < chars.len() {
        match chars[index] {
            '\u{001b}' => skip_escape_sequence(&chars, &mut index),
            '\u{009b}' => skip_csi(&chars, &mut index),
            '\u{009d}' | '\u{0090}' | '\u{009e}' | '\u{009f}' => {
                skip_control_string(&chars, &mut index)
            }
            ch if ('\u{0080}'..='\u{009f}').contains(&ch) => index += 1,
            ch => {
                output.push(ch);
                index += 1;
            }
        }
    }

    output
}

pub fn sanitize_paste(input: &str) -> String {
    strip_ansi(input)
        .replace('\r', " ")
        .replace('\n', " ")
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\t')
        .collect()
}

fn skip_escape_sequence(chars: &[char], index: &mut usize) {
    *index += 1;
    let Some(next) = chars.get(*index).copied() else {
        return;
    };

    match next {
        '[' => {
            *index += 1;
            skip_csi(chars, index);
        }
        ']' | 'P' | '^' | '_' => {
            *index += 1;
            skip_control_string(chars, index);
        }
        _ => {
            while chars
                .get(*index)
                .is_some_and(|ch| ('\u{0020}'..='\u{002f}').contains(ch))
            {
                *index += 1;
            }
            if chars
                .get(*index)
                .is_some_and(|ch| ('\u{0030}'..='\u{007e}').contains(ch))
            {
                *index += 1;
            }
        }
    }
}

fn skip_csi(chars: &[char], index: &mut usize) {
    while let Some(ch) = chars.get(*index).copied() {
        *index += 1;
        if ('\u{0040}'..='\u{007e}').contains(&ch) {
            break;
        }
    }
}

fn skip_control_string(chars: &[char], index: &mut usize) {
    while let Some(ch) = chars.get(*index).copied() {
        match ch {
            '\u{0007}' | '\u{009c}' => {
                *index += 1;
                break;
            }
            '\u{001b}' if chars.get(*index + 1) == Some(&'\\') => {
                *index += 2;
                break;
            }
            _ => *index += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_uses_conservative_tui() {
        let profile = TerminalProfile::from_context("screen-256color", false, false, false, true);
        assert_eq!(
            profile.console_mode(),
            ConsoleMode::Tui(TuiCapabilities {
                colors: true,
                alternate_screen: false,
                mouse_capture: false,
                bracketed_paste: false,
                ctrl_h_backspace: true,
            })
        );

        let profile = TerminalProfile::from_context("xterm-256color", true, false, false, true);
        assert_eq!(
            profile.console_mode(),
            ConsoleMode::Tui(TuiCapabilities {
                colors: true,
                alternate_screen: false,
                mouse_capture: false,
                bracketed_paste: false,
                ctrl_h_backspace: true,
            })
        );
    }

    #[test]
    fn normal_tty_keeps_tui() {
        let profile = TerminalProfile::from_context("xterm-256color", false, false, false, true);
        assert!(matches!(profile.console_mode(), ConsoleMode::Tui(_)));
    }

    #[test]
    fn no_color_keeps_tui_without_styles() {
        let profile = TerminalProfile::from_context("xterm-256color", false, false, true, true);
        assert_eq!(
            profile.console_mode(),
            ConsoleMode::Tui(TuiCapabilities {
                colors: false,
                alternate_screen: true,
                mouse_capture: true,
                bracketed_paste: true,
                ctrl_h_backspace: false,
            })
        );
    }

    #[test]
    fn tmux_does_not_trigger_screen_fallback() {
        let profile = TerminalProfile::from_context("screen-256color", false, true, false, true);
        assert!(matches!(profile.console_mode(), ConsoleMode::Tui(_)));
    }

    #[test]
    fn strips_terminal_control_sequences() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("a\x1b]0;title\x07b"), "ab");
        assert_eq!(strip_ansi("a\x1bPpayload\x1b\\b"), "ab");
        assert_eq!(strip_ansi("a\x1b(Bb"), "ab");
        assert_eq!(strip_ansi("a\u{009b}32mb"), "ab");
    }

    #[test]
    fn paste_is_single_line_plain_text() {
        assert_eq!(sanitize_paste("a\x1b[32mb\x1b[0m\r\nc"), "ab  c");
    }
}
