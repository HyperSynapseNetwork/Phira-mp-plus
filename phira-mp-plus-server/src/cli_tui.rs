//! Phira-mp+ interactive terminal console.
//!
//! The renderer keeps logical output lines separate from wrapped visual rows.
//! This avoids the mixed scroll-index model that caused corrupted layouts after
//! scrolling, especially inside GNU screen/tmux.

use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
        EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, DisableLineWrap, EnableLineWrap,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame, Terminal,
};
use std::io::{self, Write};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use unicode_width::UnicodeWidthChar;

const MAX_LOGICAL_LINES: usize = 10_000;
const RETAIN_LOGICAL_LINES: usize = 8_000;

/// Restores the terminal even when the TUI exits through an error path.
struct TerminalSession;

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(err) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            DisableLineWrap,
            Hide,
        ) {
            let _ = disable_raw_mode();
            return Err(err);
        }
        Ok(Self)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            Show,
            EnableLineWrap,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
        );
        let _ = disable_raw_mode();
    }
}

/// Run the interactive TUI on the current thread.
pub fn run_tui(
    cmd_tx: mpsc::UnboundedSender<String>,
    mut out_rx: mpsc::UnboundedReceiver<String>,
    mut log_rx: mpsc::UnboundedReceiver<String>,
) -> io::Result<()> {
    let _session = TerminalSession::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = TuiApp::new(cmd_tx);
    let result = app.run_loop(&mut terminal, &mut out_rx, &mut log_rx);
    let _ = terminal.show_cursor();
    result
}

struct TuiApp {
    output_lines: Vec<String>,
    /// Number of visual rows above the bottom. Zero means follow output.
    scroll_from_bottom: usize,
    input: String,
    /// Cursor position measured in Unicode scalar values, not bytes/cells.
    cursor_pos: usize,
    history: Vec<String>,
    history_idx: Option<usize>,
    cmd_tx: mpsc::UnboundedSender<String>,
    running: bool,
    last_wrap_width: usize,
    last_page_height: usize,
}

impl TuiApp {
    fn new(cmd_tx: mpsc::UnboundedSender<String>) -> Self {
        Self {
            output_lines: Vec::with_capacity(1024),
            scroll_from_bottom: 0,
            input: String::new(),
            cursor_pos: 0,
            history: Vec::new(),
            history_idx: None,
            cmd_tx,
            running: true,
            last_wrap_width: 80,
            last_page_height: 20,
        }
    }

    fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        out_rx: &mut mpsc::UnboundedReceiver<String>,
        log_rx: &mut mpsc::UnboundedReceiver<String>,
    ) -> io::Result<()> {
        self.add_output(format!(
            "Phira-mp+ v{} 管理控制台 — 输入 help 查看命令",
            env!("CARGO_PKG_VERSION")
        ));
        self.add_output(String::new());

        while self.running {
            loop {
                match out_rx.try_recv() {
                    Ok(msg) => self.add_output(strip_ansi(&msg)),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.running = false;
                        break;
                    }
                }
            }
            while let Ok(msg) = log_rx.try_recv() {
                self.add_output(strip_ansi(&msg));
            }
            if !self.running {
                break;
            }

            if event::poll(Duration::from_millis(40))? {
                match event::read()? {
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        self.handle_key(key)
                    }
                    Event::Paste(text) => self.insert_text(&sanitize_paste(&text)),
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp => self.scroll_up(3),
                        MouseEventKind::ScrollDown => self.scroll_down(3),
                        _ => {}
                    },
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }

            if self.running {
                terminal.draw(|frame| self.render(frame))?;
            }
        }
        Ok(())
    }

    fn add_output(&mut self, msg: String) {
        let lines: Vec<String> = if msg.is_empty() {
            vec![String::new()]
        } else {
            msg.split_terminator('\n')
                .map(|line| line.trim_end_matches('\r').to_string())
                .collect()
        };

        if self.scroll_from_bottom > 0 {
            let added_rows = lines
                .iter()
                .map(|line| wrap_display_line(line, self.last_wrap_width).len())
                .sum::<usize>();
            self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(added_rows);
        }
        self.output_lines.extend(lines);

        if self.output_lines.len() > MAX_LOGICAL_LINES {
            let remove = self.output_lines.len() - RETAIN_LOGICAL_LINES;
            let removed_rows = self.output_lines[..remove]
                .iter()
                .map(|line| wrap_display_line(line, self.last_wrap_width).len())
                .sum::<usize>();
            self.output_lines.drain(..remove);
            self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(removed_rows);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                let _ = self.cmd_tx.send("exit".to_string());
                self.add_output("> exit".to_string());
                self.add_output("  ⟳ 正在关闭服务器...".to_string());
                self.running = false;
            }
            KeyCode::Char('d') if ctrl => self.running = false,
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                self.cursor_pos = 0;
            }
            KeyCode::Char('l') if ctrl => {
                self.output_lines.clear();
                self.scroll_from_bottom = 0;
            }
            KeyCode::Char('w') if ctrl => self.delete_previous_word(),
            // GNU screen commonly translates Backspace into Ctrl+H.
            KeyCode::Char('h') if ctrl => self.backspace(),
            KeyCode::Backspace | KeyCode::Char('\x08') | KeyCode::Char('\x7f') => {
                self.backspace()
            }
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Enter => self.submit(),
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.scroll_up(1),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => self.scroll_down(1),
            KeyCode::PageUp => self.scroll_up(self.last_page_height.max(1)),
            KeyCode::PageDown => self.scroll_down(self.last_page_height.max(1)),
            KeyCode::Up => self.history_previous(),
            KeyCode::Down => self.history_next(),
            KeyCode::Left => self.cursor_pos = self.cursor_pos.saturating_sub(1),
            KeyCode::Right => {
                self.cursor_pos = (self.cursor_pos + 1).min(self.input.chars().count())
            }
            KeyCode::Home => self.cursor_pos = 0,
            KeyCode::End => self.cursor_pos = self.input.chars().count(),
            KeyCode::Esc => {
                self.input.clear();
                self.cursor_pos = 0;
                self.history_idx = None;
            }
            KeyCode::Char(ch) if !ctrl => self.insert_char(ch),
            _ => {}
        }
    }

    fn insert_char(&mut self, ch: char) {
        let byte = char_to_byte_index(&self.input, self.cursor_pos);
        self.input.insert(byte, ch);
        self.cursor_pos += 1;
        self.history_idx = None;
    }

    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let byte = char_to_byte_index(&self.input, self.cursor_pos);
        self.input.insert_str(byte, text);
        self.cursor_pos += text.chars().count();
        self.history_idx = None;
    }

    fn backspace(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let end = char_to_byte_index(&self.input, self.cursor_pos);
        let start = char_to_byte_index(&self.input, self.cursor_pos - 1);
        self.input.replace_range(start..end, "");
        self.cursor_pos -= 1;
        self.history_idx = None;
    }

    fn delete_forward(&mut self) {
        let count = self.input.chars().count();
        if self.cursor_pos >= count {
            return;
        }
        let start = char_to_byte_index(&self.input, self.cursor_pos);
        let end = char_to_byte_index(&self.input, self.cursor_pos + 1);
        self.input.replace_range(start..end, "");
        self.history_idx = None;
    }

    fn delete_previous_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut start = self.cursor_pos.min(chars.len());
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        if start < self.cursor_pos {
            let byte_start = char_to_byte_index(&self.input, start);
            let byte_end = char_to_byte_index(&self.input, self.cursor_pos);
            self.input.replace_range(byte_start..byte_end, "");
            self.cursor_pos = start;
            self.history_idx = None;
        }
    }

    fn submit(&mut self) {
        let cmd = self.input.trim().to_string();
        if !cmd.is_empty() {
            if self.history.last() != Some(&cmd) {
                self.history.push(cmd.clone());
            }
            self.history_idx = None;
            self.scroll_from_bottom = 0;
            self.add_output(format!("> {cmd}"));
            let _ = self.cmd_tx.send(cmd);
        }
        self.input.clear();
        self.cursor_pos = 0;
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = self.history_idx.unwrap_or(self.history.len());
        let next = idx.saturating_sub(1);
        self.history_idx = Some(next);
        self.input = self.history[next].clone();
        self.cursor_pos = self.input.chars().count();
    }

    fn history_next(&mut self) {
        let Some(idx) = self.history_idx else { return };
        if idx + 1 < self.history.len() {
            let next = idx + 1;
            self.history_idx = Some(next);
            self.input = self.history[next].clone();
        } else {
            self.history_idx = None;
            self.input.clear();
        }
        self.cursor_pos = self.input.chars().count();
    }

    fn scroll_up(&mut self, rows: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(rows.max(1));
    }

    fn scroll_down(&mut self, rows: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(rows.max(1));
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 12 || area.height < 4 {
            return;
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        let output_height = chunks[0].height as usize;
        let output_width = chunks[0].width.saturating_sub(1).max(1) as usize;
        self.last_wrap_width = output_width;
        self.last_page_height = output_height;

        let visual_rows: Vec<String> = self
            .output_lines
            .iter()
            .flat_map(|line| wrap_display_line(line, output_width))
            .collect();
        let max_start = visual_rows.len().saturating_sub(output_height);
        self.scroll_from_bottom = self.scroll_from_bottom.min(max_start);
        let start = max_start.saturating_sub(self.scroll_from_bottom);
        let visible = visual_rows
            .iter()
            .skip(start)
            .take(output_height)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");

        frame.render_widget(Clear, chunks[0]);
        frame.render_widget(Paragraph::new(visible), chunks[0]);

        if self.scroll_from_bottom > 0 && output_height > 0 {
            let max_offset = max_start.max(1);
            let from_top = max_start.saturating_sub(self.scroll_from_bottom);
            let y = chunks[0].y
                + ((from_top * output_height.saturating_sub(1)) / max_offset) as u16;
            frame.render_widget(
                Paragraph::new(Span::styled("┃", Style::default().fg(Color::Cyan))),
                ratatui::layout::Rect::new(
                    chunks[0].x + chunks[0].width.saturating_sub(1),
                    y,
                    1,
                    1,
                ),
            );
        }

        let input_width = chunks[1].width.saturating_sub(2) as usize;
        let (input_visible, cursor_col) = input_window(&self.input, self.cursor_pos, input_width);
        let prompt = if self.input.is_empty() {
            Span::styled("→ ", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled("→ ", Style::default().fg(Color::Cyan))
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![prompt, Span::raw(input_visible)])),
            chunks[1],
        );
        frame.set_cursor_position(Position::new(
            chunks[1].x + 2 + cursor_col.min(input_width) as u16,
            chunks[1].y,
        ));

        let status = if self.scroll_from_bottom == 0 {
            format!("  {} 行  Shift+↑↓/PgUp/PgDn滚动  Ctrl+C退出", visual_rows.len())
        } else {
            format!(
                "  距底部 {} 行  Shift+↓/PgDn返回  Ctrl+C退出",
                self.scroll_from_bottom
            )
        };
        frame.render_widget(
            Paragraph::new(Span::styled(status, Style::default().fg(Color::DarkGray))),
            chunks[2],
        );
    }
}

fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

fn wrap_display_line(line: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(1);
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut width = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = 4 - (width % 4);
            for _ in 0..spaces {
                if width == max_width {
                    rows.push(std::mem::take(&mut row));
                    width = 0;
                }
                row.push(' ');
                width += 1;
            }
            continue;
        }
        let cell_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width > 0 && width + cell_width > max_width {
            rows.push(std::mem::take(&mut row));
            width = 0;
        }
        row.push(ch);
        width += cell_width;
        if width >= max_width {
            rows.push(std::mem::take(&mut row));
            width = 0;
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

fn input_window(input: &str, cursor_pos: usize, max_width: usize) -> (String, usize) {
    if max_width == 0 {
        return (String::new(), 0);
    }
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor_pos.min(chars.len());
    let mut start = 0usize;
    let mut cursor_width = chars[..cursor]
        .iter()
        .map(|ch| UnicodeWidthChar::width(*ch).unwrap_or(0))
        .sum::<usize>();
    while cursor_width >= max_width && start < cursor {
        cursor_width = cursor_width.saturating_sub(
            UnicodeWidthChar::width(chars[start]).unwrap_or(0),
        );
        start += 1;
    }

    let mut visible = String::new();
    let mut width = 0usize;
    for ch in chars.into_iter().skip(start) {
        let cell_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cell_width > max_width {
            break;
        }
        visible.push(ch);
        width += cell_width;
    }
    (visible, cursor_width)
}

fn sanitize_paste(text: &str) -> String {
    strip_ansi(text)
        .replace('\r', " ").replace('\n', " ")
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\t')
        .collect()
}

/// Remove CSI/OSC and common two-byte ANSI escape sequences.
fn strip_ansi(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\x1b' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1;
        match chars.get(i).copied() {
            Some('[') => {
                i += 1;
                while let Some(ch) = chars.get(i).copied() {
                    i += 1;
                    if ('\x40'..='\x7e').contains(&ch) {
                        break;
                    }
                }
            }
            Some(']') => {
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\x07' {
                        i += 1;
                        break;
                    }
                    if chars[i] == '\x1b' && chars.get(i + 1) == Some(&'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            Some(ch) if ('\x40'..='\x5f').contains(&ch) => i += 1,
            _ => {}
        }
    }
    out
}

/// Line-oriented fallback used when stdin/stdout are redirected or not TTYs.
pub fn run_stdin_cli(
    cmd_tx: mpsc::UnboundedSender<String>,
    out_rx: mpsc::UnboundedReceiver<String>,
) {
    let (_log_tx, log_rx) = mpsc::unbounded_channel();
    run_stdin_cli_with_logs(cmd_tx, out_rx, log_rx);
}

pub fn run_stdin_cli_with_logs(
    cmd_tx: mpsc::UnboundedSender<String>,
    mut out_rx: mpsc::UnboundedReceiver<String>,
    mut log_rx: mpsc::UnboundedReceiver<String>,
) {
    std::thread::spawn(move || {
        while let Some(line) = out_rx.blocking_recv() {
            println!("{line}");
        }
    });
    std::thread::spawn(move || {
        while let Some(line) = log_rx.blocking_recv() {
            print!("{line}");
            let _ = io::stdout().flush();
        }
    });

    let stdin = io::stdin();
    let mut line_buf = String::new();
    loop {
        print!("> ");
        let _ = io::stdout().flush();
        line_buf.clear();
        match stdin.read_line(&mut line_buf) {
            Ok(0) => break,
            Err(err) => {
                eprintln!("\n[input error: {err}]");
                continue;
            }
            Ok(_) => {
                let command = strip_ansi(&line_buf).trim().to_string();
                if command.is_empty() {
                    continue;
                }
                let exiting = matches!(command.as_str(), "exit" | "quit" | "q");
                let _ = cmd_tx.send(command);
                if exiting {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_cjk_by_display_cells() {
        assert_eq!(wrap_display_line("ab中文", 4), vec!["ab中", "文"]);
    }

    #[test]
    fn input_window_keeps_cjk_cursor_visible() {
        let (visible, cursor) = input_window("ab中文cd", 6, 5);
        assert!(cursor < 5);
        assert!(!visible.is_empty());
    }

    #[test]
    fn strips_ansi_sequences() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn screen_ctrl_h_is_backspace_but_plain_h_is_text() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = TuiApp::new(tx);
        app.insert_text("ah");
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "a");
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert_eq!(app.input, "ah");
    }
}
