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
    widgets::{Clear, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use unicode_width::UnicodeWidthStr;

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

    /// 滚动加速：连续按键次数（长按变快）
    scroll_repeat: usize,
    /// 上次滚动时间
    scroll_last_time: std::time::Instant,
    /// S 键按下标志：S+↑↓ 滚动模式
    scroll_key_pressed: bool,
    /// 压测执行中（禁用输入，显示进度）
    benchmark_running: bool,
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
            scroll_repeat: 0,
            scroll_last_time: std::time::Instant::now(),
            scroll_key_pressed: false,
            benchmark_running: false,
        }
    }

    /// TUI 主循环
    fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        out_rx: &mut mpsc::UnboundedReceiver<String>,
        log_rx: &mut mpsc::UnboundedReceiver<String>,
    ) -> io::Result<()> {
        // 简洁欢迎
        self.add_output(format!("Phira-mp+ v{} 管理控制台 — 输入 help 查看命令", env!("CARGO_PKG_VERSION")));
        self.add_output(String::new());

        while self.running {
            // 清空输出通道（CLI 处理器发来的结果）
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
            // 检测压测结束
            if self.benchmark_running {
                let last_lines: Vec<String> = self.output_lines.iter().rev().take(5).cloned().collect();
                let done = last_lines.iter().any(|l| l.contains("压测完成") || l.contains("BUILD EXIT") || l.contains("cleanup"));
                if done { self.benchmark_running = false; }
            }
            // 清空日志通道（tracing 发来的日志）
            while let Ok(msg) = log_rx.try_recv() {
                self.add_output(strip_ansi(&msg));
            }

            // CLI 端已断开 → 退出 TUI
            if !self.running {
                break;
            }

            // 检查键盘输入（带超时，让位给通道检查）
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) => {
                        self.handle_key(key);
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }

            // 检查运行状态
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
        if key.kind != KeyEventKind::Press { return; }
        // 压测中只允许 Ctrl+C
        if self.benchmark_running {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                let _ = self.cmd_tx.send("exit".to_string());
                self.add_output("> exit".to_string());
                self.benchmark_running = false;
                self.running = false;
            }
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
            // S 键标记（输入为空时，不干扰打字）
            KeyCode::Char('s') if self.input.is_empty() => { self.scroll_key_pressed = true; }
            KeyCode::Char('S') if self.input.is_empty() => { self.scroll_key_pressed = true; }
            KeyCode::Char(c) => {
                let mut chars: Vec<char> = self.input.chars().collect();
                let pos = self.cursor_pos.min(chars.len());
                chars.insert(pos, c);
                self.input = chars.into_iter().collect();
                self.cursor_pos = pos + 1;
            }
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    let mut chars: Vec<char> = self.input.chars().collect();
                    let pos = self.cursor_pos.saturating_sub(1).min(chars.len().saturating_sub(1));
                    if pos < chars.len() {
                        chars.remove(pos);
                        self.input = chars.into_iter().collect();
                        self.cursor_pos = pos;
                    }
                }
            }
            KeyCode::Delete => {
                let mut chars: Vec<char> = self.input.chars().collect();
                let pos = self.cursor_pos.min(chars.len().saturating_sub(1));
                if pos < chars.len() {
                    chars.remove(pos);
                    self.input = chars.into_iter().collect();
                }
            }
            KeyCode::Enter => {
                let cmd = self.input.trim().to_string();
                if !cmd.is_empty() {
                    self.history.push(cmd.clone());
                    self.history_idx = None;
                    self.auto_scroll = true;
                    self.add_output(format!("> {}", cmd));
                    let is_bench = cmd.starts_with("benchmark") || cmd == "bench";
                    let _ = self.cmd_tx.send(cmd);
                    if is_bench {
                        self.benchmark_running = true;
                    }
                }
                self.input.clear();
                self.cursor_pos = 0;
            }
            // ↑↓：S 键按下时滚动，否则命令历史
            KeyCode::Up => {
                if self.scroll_key_pressed { self.scroll_key_pressed = false; self.scroll_up(); return; }
                if self.history.is_empty() { return; }
                let idx = self.history_idx.get_or_insert(self.history.len());
                if *idx > 0 { *idx -= 1; self.input = self.history[*idx].clone(); self.cursor_pos = self.input.chars().count(); }
            }
            KeyCode::Down => {
                if self.scroll_key_pressed { self.scroll_key_pressed = false; self.scroll_down(); return; }
                if let Some(idx) = &mut self.history_idx {
                    if *idx + 1 < self.history.len() { *idx += 1; self.input = self.history[*idx].clone(); }
                    else { self.history_idx = None; self.input.clear(); }
                    self.cursor_pos = self.input.chars().count();
                }
            }
            KeyCode::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor_pos < self.input.chars().count() {
                    self.cursor_pos += 1;
                }
            }
            KeyCode::Home => {
                self.cursor_pos = 0;
            }
            KeyCode::End => {
                self.cursor_pos = self.input.chars().count();
            }
            KeyCode::PageUp => {
                let page_lines = 20.max(1);
                self.scroll_offset = self.scroll_offset.saturating_sub(page_lines);
                self.auto_scroll = false;
            }
            KeyCode::PageDown => {
                let max_scroll = self.output_lines.len().saturating_sub(1);
                self.scroll_offset = self
                    .scroll_offset
                    .saturating_add(20)
                    .min(max_scroll);
                if self.scroll_offset >= max_scroll {
                    self.auto_scroll = true;
                }
            }
            _ => {}
        }
    }

    /// 向上滚动（含加速）
    fn scroll_up(&mut self) {
        let now = std::time::Instant::now();
        if now.duration_since(self.scroll_last_time).as_millis() < 200 {
            self.scroll_repeat += 1;
        } else {
            self.scroll_repeat = 0;
        }
        self.scroll_last_time = now;
        let step = 1 + self.scroll_repeat.min(10); // 最多一次跳 11 行
        if self.scroll_offset >= step {
            self.scroll_offset -= step;
        } else {
            self.scroll_offset = 0;
        }
        self.auto_scroll = false;
    }

    /// 向下滚动（含加速）
    fn scroll_down(&mut self) {
        let now = std::time::Instant::now();
        if now.duration_since(self.scroll_last_time).as_millis() < 200 {
            self.scroll_repeat += 1;
        } else {
            self.scroll_repeat = 0;
        }
        self.scroll_last_time = now;
        let step = 1 + self.scroll_repeat.min(10);
        let max = self.output_lines.len().saturating_sub(1);
        if self.scroll_offset + step <= max {
            self.scroll_offset += step;
        } else {
            self.scroll_offset = max;
            self.auto_scroll = true;
        }
    }

    /// 渲染一帧 — 简洁 Claude Code 风格
    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 20 || area.height < 5 { return; }

        // 三个区域：输出(填充) + 输入行 + 状态栏
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let total_lines = self.output_lines.len();
        let output_h = chunks[0].height.saturating_sub(1) as usize;
        let output_w = chunks[0].width.saturating_sub(2) as usize; // 留出滚动指示器空间

        let scroll = if self.auto_scroll || total_lines <= output_h {
            total_lines.saturating_sub(output_h)
        } else {
            self.scroll_offset.min(total_lines.saturating_sub(1))
        };

        // ── 输出区域 — 纯文本，CJK 宽度感知 ──
        let hide_progress = self.benchmark_running;
        let visible_lines: Vec<Line> = self.output_lines.iter().skip(scroll).take(output_h).map(|s| {
            if s.is_empty() { return Line::from(""); }
            let style = if s.starts_with("> ") {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else if s.contains("✗") || s.contains("ERROR") {
                Style::default().fg(Color::Red)
            } else if s.contains("✓") || s.contains("◆") {
                Style::default().fg(Color::Green)
            } else if s.contains("!") || s.contains("WARN") {
                Style::default().fg(Color::Yellow)
            } else if s.contains("⟳") {
                Style::default().fg(Color::Cyan)
            } else if s.contains("▸") || s.contains("│") || s.contains("─") {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            // CJK 宽度截断，防止溢出换行打乱布局
            let truncated = truncate_line(s, output_w);
            Line::from(if style != Style::default() { Span::styled(truncated, style) } else { Span::raw(truncated) })
        }).collect();

        // 全区域清除 + 重绘
        frame.render_widget(Clear, chunks[0]);
        frame.render_widget(Paragraph::new(Text::from(visible_lines)), chunks[0]);

        // 压测进度条
        if hide_progress {
            let bar_w = output_w.min(40);
            let filled = self.scroll_repeat % (bar_w + 1);
            let bar = format!("  ⟳ 压测中 [{}{}] 等待完成...", "█".repeat(filled), "░".repeat(bar_w.saturating_sub(filled)));
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(bar, Style::default().fg(Color::Cyan)))),
                ratatui::layout::Rect::new(chunks[0].x + 1, chunks[0].y + chunks[0].height.saturating_sub(2), bar_w as u16 + 6, 1),
            );
        }

        // ── 滚动指示器 ──
        if !self.auto_scroll && total_lines > output_h && output_h > 1 {
            let pct = scroll as f64 / (total_lines.saturating_sub(output_h)).max(1) as f64;
            let max_y = chunks[0].y + chunks[0].height.saturating_sub(1);
            let bar_y = (chunks[0].y as f64 + pct * (output_h as f64 - 1.0)).round() as u16;
            if bar_y <= max_y {
                frame.render_widget(
                    Paragraph::new(Span::styled("┃", Style::default().fg(Color::Cyan))),
                    ratatui::layout::Rect::new(chunks[0].x + chunks[0].width.saturating_sub(1), bar_y, 1, 1),
                );
            }
        }

        // ── 输入行（压测中禁用） ──
        let input_prompt = if hide_progress {
            Span::styled("⏳ ", Style::default().fg(Color::DarkGray))
        } else if self.input.is_empty() {
            Span::styled("→ ", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled("→ ", Style::default().fg(Color::Cyan))
        };
        let input_content = if hide_progress {
            Span::styled("压测执行中，请等待...", Style::default().fg(Color::DarkGray))
        } else {
            Span::raw(&self.input)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![input_prompt, input_content])),
            chunks[1],
        );
        // 光标（压测中隐藏）
        if !hide_progress {
            frame.set_cursor_position(Position::new(
                chunks[1].x + 2 + self.cursor_pos.min(self.input.chars().count()) as u16,
                chunks[1].y,
            ));
        }

        // ── 状态栏 ──
        let scroll_info = match total_lines {
            0 => "".into(),
            _ if self.auto_scroll => format!("{} 行", total_lines),
            _ => format!("{:.0}%  ↑", scroll as f64 / (total_lines - 1).max(1) as f64 * 100.0),
        };
        let suffix = if hide_progress { "  ⟳ 压测中...  Ctrl+C强制退出" } else { "  ↑↓历史  S+↑↓滚动  Ctrl+C退出" };
        let status_text = format!("  {scroll_info}{suffix}");
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status_text, Style::default().fg(Color::DarkGray)))),
            chunks[2],
        );
    }
}

/// 按 CJK 显示宽度截断字符串，防止溢出换行打乱布局
fn truncate_line(s: &str, max_width: usize) -> String {
    if max_width < 3 { return s.chars().take(max_width).collect(); }
    let w = UnicodeWidthStr::width(s);
    if w <= max_width { return s.to_string(); }
    let mut out = String::with_capacity(max_width);
    let mut cur = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthStr::width(c.to_string().as_str());
        if cur + cw > max_width.saturating_sub(2) {
            out.push_str("…");
            break;
        }
        out.push(c);
        cur += cw;
    }
    out
}

/// 去除 ANSI 转义序列
///
/// 先将字符串解码为 char 数组（正确处理多字节 UTF-8），
/// 再用索引遍历跳过转义序列。
fn strip_ansi(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\x1b' {
            i += 1; // 跳过 ESC
            if i < chars.len() && chars[i] == '[' {
                // CSI: ESC [ param* intermediate* final
                // param: 0x30-0x3F, intermediate: 0x20-0x2F, final: 0x40-0x7E
                i += 1;
                while i < chars.len() {
                    let c = chars[i];
                    i += 1;
                    if c >= '\x40' && c <= '\x7E' {
                        break; // final byte
                    }
                }
            } else if i < chars.len() && chars[i] == ']' {
                // OSC: ESC ] ... ST (\x1b\\) 或 BEL (\x07)
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\x07' {
                        i += 1;
                        break;
                    }
                    if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else if i < chars.len() && chars[i] >= '\x40' && chars[i] <= '\x5F' {
                // 两字符 ESC 序列
                i += 1;
            }
            // 其他未知前缀：忽略 ESC 及其后的内容
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// 简单 stdin CLI（screen/tmux 等无 TUI 环境使用）
pub fn run_stdin_cli(cmd_tx: tokio::sync::mpsc::UnboundedSender<String>, mut out_rx: tokio::sync::mpsc::UnboundedReceiver<String>) {
    use std::io::Write;
    // 启动输出读取线程
    std::thread::spawn(move || {
        while let Some(line) = out_rx.blocking_recv() {
            println!("{line}");
        }
    });
    let stdin = std::io::stdin();
    let mut line_buf = String::new();
    loop {
        print!("> ");
        let _ = std::io::stdout().flush();
        line_buf.clear();
        match stdin.read_line(&mut line_buf) {
            Ok(0) => break, // EOF
            Err(e) => {
                // screen 下中文 IME 可能发送转义干扰 read_line，记录后跳过
                eprintln!("\n[input error: {e}, type help for commands]");
                continue;
            }
            Ok(_) => {
                // 清理可能混入的 ANSI 转义（IME 有时会残留 ESC 序列）
                let raw = strip_ansi(&line_buf);
                let trimmed = raw.trim().to_string();
                if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") || trimmed == "q" {
                    let _ = cmd_tx.send(trimmed);
                    break;
                }
                if !trimmed.is_empty() {
                    let _ = cmd_tx.send(trimmed);
                }
            }
        }
    }
}
