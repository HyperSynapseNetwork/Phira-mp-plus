//! Interactive administrative console.

use crate::command_registry::runtime_v2_registry;
use crate::terminal::{sanitize_paste, strip_ansi, EraseKeyGuard, TuiCapabilities};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    style::{Attribute, ResetColor, SetAttribute},
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear as TermClear, ClearType, DisableLineWrap,
        EnableLineWrap, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame, Terminal,
};
use std::collections::HashMap;
use std::io::{self, Write};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use unicode_width::UnicodeWidthChar;

const MAX_LOGICAL_LINES: usize = 2_000;
const RETAIN_LOGICAL_LINES: usize = 1_500;

struct TerminalSession {
    capabilities: TuiCapabilities,
}

impl TerminalSession {
    fn enter(capabilities: TuiCapabilities) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        let enter_result = if capabilities.plain_ui {
            // Plain mode targets old screen/linux/ansi-like environments. Avoid
            // DEC private mode toggles such as cursor hide and line-wrap disable;
            // some consoles render the final bytes of those sequences literally.
            execute!(
                stdout,
                ResetColor,
                SetAttribute(Attribute::Reset),
                TermClear(ClearType::All),
                MoveTo(0, 0)
            )
        } else if capabilities.alternate_screen {
            execute!(
                stdout,
                ResetColor,
                SetAttribute(Attribute::Reset),
                EnterAlternateScreen,
                TermClear(ClearType::All),
                MoveTo(0, 0),
                DisableLineWrap,
                Hide
            )
        } else {
            execute!(
                stdout,
                ResetColor,
                SetAttribute(Attribute::Reset),
                TermClear(ClearType::All),
                MoveTo(0, 0),
                DisableLineWrap,
                Hide
            )
        };
        if let Err(err) = enter_result {
            let _ = disable_raw_mode();
            return Err(err);
        }
        let session = Self { capabilities };
        if capabilities.bracketed_paste {
            execute!(stdout, EnableBracketedPaste)?;
        }
        if capabilities.mouse_capture {
            execute!(stdout, EnableMouseCapture)?;
        }
        Ok(session)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        if self.capabilities.mouse_capture {
            let _ = execute!(stdout, DisableMouseCapture);
        }
        if self.capabilities.bracketed_paste {
            let _ = execute!(stdout, DisableBracketedPaste);
        }
        let _ = execute!(stdout, ResetColor, SetAttribute(Attribute::Reset));
        if self.capabilities.plain_ui {
            let _ = execute!(stdout, TermClear(ClearType::All), MoveTo(0, 0));
        } else if self.capabilities.alternate_screen {
            let _ = execute!(
                stdout,
                Show,
                EnableLineWrap,
                LeaveAlternateScreen,
                TermClear(ClearType::All),
                MoveTo(0, 0)
            );
        } else {
            let _ = execute!(
                stdout,
                Show,
                EnableLineWrap,
                TermClear(ClearType::All),
                MoveTo(0, 0)
            );
        }
        let _ = stdout.flush();
        let _ = disable_raw_mode();
    }
}

pub fn run_tui(
    cmd_tx: mpsc::UnboundedSender<String>,
    mut out_rx: mpsc::UnboundedReceiver<String>,
    mut log_rx: mpsc::UnboundedReceiver<String>,
    capabilities: TuiCapabilities,
) -> io::Result<()> {
    let _session = match TerminalSession::enter(capabilities) {
        Ok(session) => session,
        Err(err) => {
            eprintln!(
                "TUI unavailable ({err}); falling back to the line-oriented compatibility console."
            );
            run_stdin_cli_with_logs(cmd_tx, out_rx, log_rx, capabilities.ctrl_h_backspace);
            return Ok(());
        }
    };
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    // Always clear once before the first draw. This is especially important when
    // running without alternate-screen in GNU screen/linux console, otherwise
    // old shell/compiler output can remain under ratatui's diff renderer.
    terminal.clear()?;

    let mut app = TuiApp::new(
        cmd_tx,
        capabilities.colors && !capabilities.plain_ui,
        capabilities.plain_ui,
    );
    let result = app.run_loop(&mut terminal, &mut out_rx, &mut log_rx);
    let _ = terminal.clear();
    if !capabilities.plain_ui {
        let _ = terminal.show_cursor();
    }
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
    colors: bool,
    plain_ui: bool,
    dirty: bool,
    cached_stats: String,
}

fn completion_prefix(before_cursor: &str) -> &str {
    if before_cursor
        .chars()
        .last()
        .is_some_and(char::is_whitespace)
    {
        ""
    } else {
        before_cursor.split_whitespace().last().unwrap_or("")
    }
}

impl TuiApp {
    fn new(cmd_tx: mpsc::UnboundedSender<String>, colors: bool, plain_ui: bool) -> Self {
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
            colors,
            plain_ui,
            dirty: true,
            cached_stats: String::new(),
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
                    Ok(msg) => self.add_output(sanitize_output(&msg)),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.running = false;
                        break;
                    }
                }
            }
            while let Ok(msg) = log_rx.try_recv() {
                self.add_output(sanitize_output(&msg));
            }
            if !self.running {
                break;
            }

            if event::poll(Duration::from_millis(40))? {
                match event::read()? {
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        self.handle_key(key);
                        self.dirty = true;
                    }
                    Event::Paste(text) => {
                        self.insert_text(&sanitize_paste(&text));
                        self.dirty = true;
                    }
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            self.scroll_up(3);
                            self.dirty = true;
                        }
                        MouseEventKind::ScrollDown => {
                            self.scroll_down(3);
                            self.dirty = true;
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => {
                        self.dirty = true;
                    }
                    _ => {}
                }
            }

            if self.running && self.dirty {
                if self.plain_ui {
                    // Conservative terminals and old GNU screen sessions are the
                    // places where wide CJK glyphs and diff-based redraws most
                    // often disagree. Clear before every frame in this mode so
                    // stale SGR/cursor fragments cannot remain between Chinese
                    // characters or leak into the input line.
                    terminal.clear()?;
                }
                terminal.draw(|frame| self.render(frame))?;
                self.dirty = false;
            }
        }
        Ok(())
    }

    fn add_output(&mut self, msg: String) {
        // Parse 📊 stats lines: cache for panels, don't show in output
        if msg.starts_with('📊') {
            self.cached_stats = msg;
            self.dirty = true;
            return;
        }

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

        self.dirty = true;
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
            KeyCode::Char('k') if ctrl => self.delete_to_end(),
            KeyCode::Char('a') if ctrl => self.cursor_pos = 0,
            KeyCode::Char('e') if ctrl => self.cursor_pos = self.input.chars().count(),
            KeyCode::Char('b') if ctrl => self.cursor_pos = self.cursor_pos.saturating_sub(1),
            KeyCode::Char('f') if ctrl => {
                self.cursor_pos = (self.cursor_pos + 1).min(self.input.chars().count())
            }
            KeyCode::Char('p') if ctrl => self.history_previous(),
            KeyCode::Char('n') if ctrl => self.history_next(),
            // GNU screen commonly translates Backspace into Ctrl+H.
            KeyCode::Char('h') if ctrl => self.backspace(),
            KeyCode::Backspace | KeyCode::Char('\x08') | KeyCode::Char('\x7f') => self.backspace(),
            KeyCode::Delete if key.modifiers.contains(KeyModifiers::ALT) => self.delete_next_word(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Tab => self.complete_input(),
            KeyCode::Enter => self.submit(),
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.scroll_up(1),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => self.scroll_down(1),
            KeyCode::PageUp => self.scroll_up(self.last_page_height.max(1)),
            KeyCode::PageDown => self.scroll_down(self.last_page_height.max(1)),
            KeyCode::Up => self.history_previous(),
            KeyCode::Down => self.history_next(),
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => self.move_previous_word(),
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => self.move_next_word(),
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

    fn delete_to_end(&mut self) {
        let start = char_to_byte_index(&self.input, self.cursor_pos);
        self.input.truncate(start);
        self.history_idx = None;
    }

    fn move_previous_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut pos = self.cursor_pos.min(chars.len());
        while pos > 0 && chars[pos - 1].is_whitespace() {
            pos -= 1;
        }
        while pos > 0 && !chars[pos - 1].is_whitespace() {
            pos -= 1;
        }
        self.cursor_pos = pos;
    }

    fn move_next_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut pos = self.cursor_pos.min(chars.len());
        while pos < chars.len() && !chars[pos].is_whitespace() {
            pos += 1;
        }
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }
        self.cursor_pos = pos;
    }

    fn delete_next_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let start = self.cursor_pos.min(chars.len());
        let mut end = start;
        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }
        while end < chars.len() && !chars[end].is_whitespace() {
            end += 1;
        }
        if end > start {
            let byte_start = char_to_byte_index(&self.input, start);
            let byte_end = char_to_byte_index(&self.input, end);
            self.input.replace_range(byte_start..byte_end, "");
            self.history_idx = None;
        }
    }

    fn complete_input(&mut self) {
        let before_cursor = self.input.chars().take(self.cursor_pos).collect::<String>();
        let trimmed_start = before_cursor.trim_start();
        let prefix = completion_prefix(trimmed_start);
        let registry = runtime_v2_registry();
        let matches = registry.complete_line(trimmed_start);
        match matches.as_slice() {
            [] => self.add_output(format!("  无补全候选: {prefix}")),
            [only] => self.apply_completion(prefix, only),
            many => self.add_output(format!("  补全: {}", many.join("  "))),
        }
    }

    fn apply_completion(&mut self, prefix: &str, completion: &str) {
        if completion.len() < prefix.len() {
            return;
        }
        let suffix = &completion[prefix.len()..];
        self.insert_text(suffix);
        if !self.input.ends_with(' ') {
            self.insert_char(' ');
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
        if area.width < 12 || area.height < 6 {
            return;
        }
        if self.plain_ui {
            self.render_plain(frame, area);
            return;
        }

        // Overall vertical layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Header
                Constraint::Length(6), // Top panels (Rooms | Users)
                Constraint::Min(1),    // Output / Log area
                Constraint::Length(3), // Command input area
                Constraint::Length(1), // Status bar
            ])
            .split(area);

        // ── Header ──────────────────────────────────────────────────
        let header_text = if self.cached_stats.is_empty() {
            format!(
                "◆ Phira-mp+ v{}  —  等待统计数据...",
                env!("CARGO_PKG_VERSION")
            )
        } else {
            let stats_display = self.cached_stats.trim_start_matches('📊').trim();
            format!(
                "◆ Phira-mp+ v{}  {}",
                env!("CARGO_PKG_VERSION"),
                stats_display
            )
        };
        frame.render_widget(Clear, chunks[0]);
        frame.render_widget(
            Paragraph::new(Span::styled(header_text, self.accent_style())),
            chunks[0],
        );

        // ── Top panels (Rooms | Users) ──────────────────────────────
        let panel_area = chunks[1];
        let panel_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(panel_area);

        let stats = parse_stats(&self.cached_stats);

        // Left panel: Rooms
        let room_count = stats.get("rooms").map(|s| s.as_str()).unwrap_or("?");
        let session_count = stats.get("sessions").map(|s| s.as_str()).unwrap_or("?");
        let sim_state = stats.get("sim").map(|s| s.as_str()).unwrap_or("?");
        let left_lines = vec![
            format!("rooms:     {}", room_count),
            format!("sessions:  {}", session_count),
            format!("sim:       {}", sim_state),
        ];
        let left_content = left_lines.join("\n");

        frame.render_widget(Clear, panel_chunks[0]);
        frame.render_widget(
            Block::default()
                .title(" Rooms ")
                .borders(Borders::ALL)
                .border_style(self.border_style()),
            panel_chunks[0],
        );
        let left_inner = inner_rect(panel_chunks[0]);
        frame.render_widget(Paragraph::new(left_content), left_inner);

        // Right panel: Users
        let user_count = stats.get("users").map(|s| s.as_str()).unwrap_or("?");
        let plugin_count = stats.get("plugins").map(|s| s.as_str()).unwrap_or("?");
        let right_lines = vec![
            format!("users:    {}", user_count),
            format!("plugins:  {}", plugin_count),
        ];
        let right_content = right_lines.join("\n");

        frame.render_widget(Clear, panel_chunks[1]);
        frame.render_widget(
            Block::default()
                .title(" Users ")
                .borders(Borders::ALL)
                .border_style(self.border_style()),
            panel_chunks[1],
        );
        let right_inner = inner_rect(panel_chunks[1]);
        frame.render_widget(Paragraph::new(right_content), right_inner);

        // ── Output / Log area ───────────────────────────────────────
        let output_area = chunks[2];
        let output_inner = inner_rect(output_area);
        let output_height = output_inner.height as usize;
        let output_width = output_inner.width.saturating_sub(1).max(1) as usize;
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

        frame.render_widget(Clear, output_area);
        frame.render_widget(
            Block::default()
                .title(" 输出 / 日志 ")
                .borders(Borders::ALL)
                .border_style(self.border_style()),
            output_area,
        );
        frame.render_widget(
            Paragraph::new(visible).wrap(Wrap { trim: false }),
            output_inner,
        );

        if self.scroll_from_bottom > 0 && output_height > 0 && output_inner.width > 0 {
            let max_offset = max_start.max(1);
            let from_top = max_start.saturating_sub(self.scroll_from_bottom);
            let y =
                output_inner.y + ((from_top * output_height.saturating_sub(1)) / max_offset) as u16;
            frame.render_widget(
                Paragraph::new(Span::styled("┃", self.accent_style())),
                Rect::new(
                    output_inner.x + output_inner.width.saturating_sub(1),
                    y,
                    1,
                    1,
                ),
            );
        }

        // ── Input area ──────────────────────────────────────────────
        let input_area = chunks[3];
        let input_inner = inner_rect(input_area);
        let input_width = input_inner.width.saturating_sub(2) as usize;
        let (input_visible, cursor_col) = input_window(&self.input, self.cursor_pos, input_width);
        let prompt = if self.input.is_empty() {
            Span::styled("› ", self.muted_style())
        } else {
            Span::styled("› ", self.accent_style())
        };
        frame.render_widget(Clear, input_area);
        frame.render_widget(
            Block::default()
                .title(" 命令输入 ")
                .borders(Borders::ALL)
                .border_style(self.input_border_style()),
            input_area,
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![prompt, Span::raw(input_visible)])),
            input_inner,
        );
        frame.set_cursor_position(Position::new(
            input_inner.x + 2 + cursor_col.min(input_width) as u16,
            input_inner.y,
        ));

        // ── Status bar ──────────────────────────────────────────────
        let scroll_info = if self.scroll_from_bottom == 0 {
            format!("{}行", visual_rows.len())
        } else {
            format!("↑{}", self.scroll_from_bottom)
        };
        let status = format!(" {}  |  Tab补全  ↑↓历史  PgUp/PgDn  ^C退出", scroll_info,);
        frame.render_widget(Clear, chunks[4]);
        frame.render_widget(
            Paragraph::new(Span::styled(status, self.muted_style())),
            chunks[4],
        );
    }

    fn render_plain(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        let width = area.width.max(1) as usize;

        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(pad_cells("Phira-mp+  help / status / benchmark", width)),
            chunks[0],
        );
        frame.render_widget(Paragraph::new("-".repeat(width)), chunks[1]);

        let output_area = chunks[2];
        let output_height = output_area.height as usize;
        let output_width = output_area.width.max(1) as usize;
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
            .map(|line| pad_cells(line, output_width))
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(Paragraph::new(visible), output_area);

        frame.render_widget(Paragraph::new("-".repeat(width)), chunks[3]);
        let prompt = "> ";
        let input_width = width.saturating_sub(prompt.len()).max(1);
        let (input_visible, cursor_col) = input_window(&self.input, self.cursor_pos, input_width);
        let input_line = pad_cells(&format!("{prompt}{input_visible}"), width);
        frame.render_widget(Paragraph::new(input_line), chunks[4]);
        frame.set_cursor_position(Position::new(
            chunks[4].x + prompt.len() as u16 + cursor_col.min(input_width) as u16,
            chunks[4].y,
        ));

        let status = if self.scroll_from_bottom == 0 {
            format!(
                "{} lines | Tab complete | Up/Down history | PgUp/PgDn scroll | Ctrl+C exit",
                visual_rows.len()
            )
        } else {
            format!(
                "{} lines from bottom | PgDn returns | Ctrl+L clear | Esc clears input",
                self.scroll_from_bottom
            )
        };
        frame.render_widget(Paragraph::new(pad_cells(&status, width)), chunks[5]);
    }

    fn accent_style(&self) -> Style {
        if self.colors {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        }
    }

    fn muted_style(&self) -> Style {
        if self.colors {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        }
    }

    fn border_style(&self) -> Style {
        if self.colors {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        }
    }

    fn input_border_style(&self) -> Style {
        if self.colors {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        }
    }
}

fn parse_stats(line: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(stats) = line.strip_prefix('📊') {
        for part in stats.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                map.insert(key.to_string(), value.to_string());
            }
        }
    }
    map
}

fn inner_rect(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
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
        cursor_width =
            cursor_width.saturating_sub(UnicodeWidthChar::width(chars[start]).unwrap_or(0));
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

fn sanitize_output(input: &str) -> String {
    strip_ansi(input)
        .replace('\r', "\n")
        .chars()
        .filter(|ch| matches!(*ch, '\n' | '\t') || !ch.is_control())
        .collect()
}

fn pad_cells(input: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in input.chars() {
        let cell_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cell_width > width {
            break;
        }
        out.push(ch);
        used += cell_width;
    }
    if used < width {
        out.push_str(&" ".repeat(width - used));
    }
    out
}

/// Line-oriented fallback used when stdin/stdout are redirected or not TTYs.
pub fn run_stdin_cli(
    cmd_tx: mpsc::UnboundedSender<String>,
    out_rx: mpsc::UnboundedReceiver<String>,
) {
    let (_log_tx, log_rx) = mpsc::unbounded_channel();
    run_stdin_cli_with_logs(cmd_tx, out_rx, log_rx, false);
}

pub fn run_stdin_cli_with_logs(
    cmd_tx: mpsc::UnboundedSender<String>,
    mut out_rx: mpsc::UnboundedReceiver<String>,
    mut log_rx: mpsc::UnboundedReceiver<String>,
    ctrl_h_backspace: bool,
) {
    let _erase_key_guard = if ctrl_h_backspace {
        EraseKeyGuard::ctrl_h().ok()
    } else {
        None
    };
    std::thread::spawn(move || {
        while let Some(line) = out_rx.blocking_recv() {
            println!("{}", strip_ansi(&line));
        }
    });
    std::thread::spawn(move || {
        while let Some(line) = log_rx.blocking_recv() {
            print!("{}", strip_ansi(&line));
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
                let command = normalize_line_input(&strip_ansi(&line_buf))
                    .trim()
                    .to_string();
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

fn normalize_line_input(input: &str) -> String {
    let mut normalized = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\x08' | '\x7f' => {
                normalized.pop();
            }
            _ => normalized.push(ch),
        }
    }
    normalized
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
    fn screen_ctrl_h_is_backspace_but_plain_h_is_text() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = TuiApp::new(tx, false, false);
        app.insert_text("ah");
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "a");
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert_eq!(app.input, "ah");
    }

    #[test]
    fn line_console_applies_ctrl_h_and_delete_as_backspace() {
        assert_eq!(normalize_line_input("roomx\x08 list\n"), "room list\n");
        assert_eq!(normalize_line_input("helpx\x7f\n"), "help\n");
    }

    #[test]
    fn output_sanitizer_removes_cursor_control_noise() {
        assert_eq!(sanitize_output("a\r\x1b[31mb\x1b[0m\x08c"), "a\nbc");
    }

    #[test]
    fn plain_padding_respects_cjk_display_width() {
        assert_eq!(pad_cells("ab中", 5), "ab中 ");
        assert_eq!(pad_cells("ab中文", 5), "ab中 ");
    }
}
