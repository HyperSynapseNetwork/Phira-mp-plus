//! Phira-mp+ CLI TUI 界面
//!
//! 使用 ratatui + crossterm 实现的交互式终端界面，
//! 以独立的输出区域和输入行避免日志输出干扰命令输入。

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

/// 运行 TUI 主循环（阻塞当前线程）
pub fn run_tui(
    cmd_tx: mpsc::UnboundedSender<String>,
    mut out_rx: mpsc::UnboundedReceiver<String>,
    mut log_rx: mpsc::UnboundedReceiver<String>,
) -> io::Result<()> {
    // 设置终端
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
    )?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = TuiApp::new(cmd_tx);
    let result = app.run_loop(&mut terminal, &mut out_rx, &mut log_rx);

    // 恢复终端
    let _ = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
    );
    crossterm::terminal::disable_raw_mode()?;
    terminal.show_cursor()?;

    result
}

/// TUI 应用状态
struct TuiApp {
    /// 输出行缓冲区（按行存储）
    output_lines: Vec<String>,
    /// 输出滚动偏移
    scroll_offset: usize,
    /// 是否自动滚动到底部
    auto_scroll: bool,

    /// 当前输入缓冲区
    input: String,
    /// 光标在输入中的位置
    cursor_pos: usize,

    /// 命令历史
    history: Vec<String>,
    /// 历史浏览位置
    history_idx: Option<usize>,

    /// 向 CLI 处理器发送命令
    cmd_tx: mpsc::UnboundedSender<String>,

    /// 运行状态
    running: bool,
}

impl TuiApp {
    fn new(cmd_tx: mpsc::UnboundedSender<String>) -> Self {
        Self {
            output_lines: Vec::with_capacity(1024),
            scroll_offset: 0,
            auto_scroll: true,
            input: String::new(),
            cursor_pos: 0,
            history: Vec::new(),
            history_idx: None,
            cmd_tx,
            running: true,
        }
    }

    /// TUI 主循环
    fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        out_rx: &mut mpsc::UnboundedReceiver<String>,
        log_rx: &mut mpsc::UnboundedReceiver<String>,
    ) -> io::Result<()> {
        // 显示欢迎信息
        self.add_output(format!(
            "  {} Phira-mp+ v{} 管理控制台",
            "◆",
            env!("CARGO_PKG_VERSION"),
        ));
        self.add_output(format!(
            "  {} 输入 {} 查看命令帮助，{} 关闭服务器",
            "▸",
            "help",
            "exit",
        ));
        self.add_output(String::new());

        while self.running {
            // 清空输出通道（CLI 处理器发来的结果）
            while let Ok(msg) = out_rx.try_recv() {
                self.add_output(msg);
            }
            // 清空日志通道（tracing 发来的日志）
            while let Ok(msg) = log_rx.try_recv() {
                self.add_output(strip_ansi(&msg));
            }

            // 检查键盘输入（带超时，让位给通道检查）
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) => {
                        self.handle_key(key);
                    }
                    Event::Resize(_, _) => {
                        // ratatui 自动处理终端尺寸变化
                    }
                    _ => {}
                }
            }

            // 检查输出通道是否已断开（CLI 处理器已退出）
            if !self.running {
                break;
            }

            // 渲染
            terminal.draw(|frame| self.render(frame))?;
        }
        Ok(())
    }

    /// 添加一行输出
    fn add_output(&mut self, msg: String) {
        for line in msg.lines() {
            self.output_lines.push(line.to_string());
        }
        // 限制缓冲区大小，防止内存无限增长
        if self.output_lines.len() > 10000 {
            self.output_lines.drain(0..self.output_lines.len() - 8000);
        }
        if self.auto_scroll {
            self.scroll_offset = self.output_lines.len().saturating_sub(1);
        }
    }

    /// 处理键盘事件
    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+C: 发送 exit 命令，退出 TUI
                let _ = self.cmd_tx.send("exit".to_string());
                self.add_output("> exit".to_string());
                self.add_output("  ⟳ 正在关闭服务器...".to_string());
                self.running = false;
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+D: 强制退出（不发送 exit）
                self.running = false;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+U: 清除当前输入
                self.input.clear();
                self.cursor_pos = 0;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+L: 清屏
                self.output_lines.clear();
                self.scroll_offset = 0;
                self.auto_scroll = true;
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor_pos, c);
                self.cursor_pos += 1;
            }
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.input.remove(self.cursor_pos);
                }
            }
            KeyCode::Delete => {
                if self.cursor_pos < self.input.len() {
                    self.input.remove(self.cursor_pos);
                }
            }
            KeyCode::Enter => {
                let cmd = self.input.trim().to_string();
                if !cmd.is_empty() {
                    self.history.push(cmd.clone());
                    self.history_idx = None;
                    // 回显命令到输出
                    self.add_output(format!("> {}", cmd));
                    // 发送到 CLI 处理器
                    let _ = self.cmd_tx.send(cmd);
                }
                self.input.clear();
                self.cursor_pos = 0;
            }
            KeyCode::Up => {
                if self.history.is_empty() {
                    return;
                }
                let idx = self.history_idx.get_or_insert(self.history.len());
                if *idx > 0 {
                    *idx -= 1;
                    self.input = self.history[*idx].clone();
                    self.cursor_pos = self.input.len();
                }
            }
            KeyCode::Down => {
                if let Some(idx) = &mut self.history_idx {
                    if *idx + 1 < self.history.len() {
                        *idx += 1;
                        self.input = self.history[*idx].clone();
                    } else {
                        self.history_idx = None;
                        self.input.clear();
                    }
                    self.cursor_pos = self.input.len();
                }
            }
            KeyCode::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor_pos < self.input.len() {
                    self.cursor_pos += 1;
                }
            }
            KeyCode::Home => {
                self.cursor_pos = 0;
            }
            KeyCode::End => {
                self.cursor_pos = self.input.len();
            }
            KeyCode::PageUp => {
                let area_height = (self.scroll_offset > 0) as usize;
                let page_lines = 20.max(area_height);
                self.scroll_offset = self.scroll_offset.saturating_sub(page_lines);
                self.auto_scroll = false;
            }
            KeyCode::PageDown => {
                let max_scroll = self.output_lines.len().saturating_sub(1);
                self.scroll_offset = std::cmp::min(
                    self.scroll_offset.saturating_add(20),
                    max_scroll,
                );
                if self.scroll_offset >= max_scroll {
                    self.auto_scroll = true;
                }
            }
            _ => {}
        }
    }

    /// 渲染一帧
    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 20 || area.height < 5 {
            return; // 终端太小，无法渲染
        }

        // 布局：标题(1) | 输出(填充) | 输入(1) | 状态栏(1)
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // 标题栏
                Constraint::Min(0),    // 输出区域
                Constraint::Length(1), // 分隔线
                Constraint::Length(1), // 输入行
                Constraint::Length(1), // 状态栏
            ])
            .split(area);

        // ── 标题栏 ──
        let title = format!(
            " Phira-mp+ v{} 管理控制台",
            env!("CARGO_PKG_VERSION"),
        );
        let header = Paragraph::new(Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        frame.render_widget(header, chunks[0]);

        // ── 输出区域 ──
        let output_height = chunks[1].height.saturating_sub(2) as usize; // 减去边框
        let total_lines = self.output_lines.len();

        // 计算可见范围
        let scroll = if self.auto_scroll || total_lines <= output_height {
            total_lines.saturating_sub(output_height)
        } else {
            self.scroll_offset.min(total_lines.saturating_sub(1))
        };

        let visible_lines: Vec<Line> = self
            .output_lines
            .iter()
            .skip(scroll)
            .take(output_height)
            .map(|s| {
                if s.is_empty() {
                    Line::from("")
                } else if s.starts_with("> ") {
                    // 命令回显用青色加粗
                    Line::from(Span::styled(
                        s,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ))
                } else if s.contains("ERROR") || s.contains("✗") {
                    Line::from(Span::styled(s, Style::default().fg(Color::Red)))
                } else if s.contains("WARN") || s.contains("!") {
                    Line::from(Span::styled(s, Style::default().fg(Color::Yellow)))
                } else {
                    Line::from(Span::raw(s))
                }
            })
            .collect();

        let output_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let output_para = Paragraph::new(Text::from(visible_lines)).block(output_block);
        frame.render_widget(output_para, chunks[1]);

        // ── 分隔线 ──
        let sep_width = chunks[2].width as usize;
        let sep = Paragraph::new(Line::from(Span::styled(
            "─".repeat(sep_width),
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(sep, chunks[2]);

        // ── 输入行 ──
        let input_text = if self.input.is_empty() {
            Line::from(Span::styled(
                " 输入命令 (help 查看帮助)",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            Line::from(Span::raw(format!("> {}", self.input)))
        };
        let input_para = Paragraph::new(input_text);
        frame.render_widget(input_para, chunks[3]);

        // 设置光标位置（输入行）
        let cursor_x = chunks[3].x + 2 + self.cursor_pos as u16;
        let cursor_y = chunks[3].y;
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));

        // ── 状态栏 ──
        let scroll_info = if self.auto_scroll {
            format!("{} 行", total_lines)
        } else {
            format!("行 {}/{}", scroll + 1, total_lines)
        };
        let status = format!(
            " C-c:退出 ↑↓:历史 PgUp/Dn:滚动 Ctrl+L:清屏  |  {}",
            scroll_info,
        );
        let status_bar = Paragraph::new(Line::from(Span::styled(
            status,
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(status_bar, chunks[4]);
    }
}

/// 去除 ANSI 转义序列
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                // CSI 序列: ESC [ 参数字节(0x20-0x3F) 最终字节(0x40-0x7E)
                i += 1;
                while i < bytes.len() && bytes[i] >= 0x20 && bytes[i] <= 0x3F {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] >= 0x40 && bytes[i] <= 0x7E {
                    i += 1;
                }
            }
            // 其他 ESC 前缀序列（如 OSC）直接跳过
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}
