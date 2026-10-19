use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::annotation::save_annotation;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Visual,
    Editing,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Visual => "VISUAL (painting)",
            Mode::Editing => "EDITING",
        }
    }
}

pub struct App {
    file: PathBuf,
    contents: String,
    lines: Vec<String>,
    output_dir: PathBuf,
    cursor: usize,
    /// Start of the painted selection (0-based). `None` = no painting active.
    anchor: Option<usize>,
    mode: Mode,
    /// The note being written, kept as chars so cursor movement is safe.
    note: Vec<char>,
    note_cursor: usize,
    scroll: usize,
    status: String,
    should_quit: bool,
}

/// Colorize a line of Rust code with basic syntax highlighting
fn syntax_highlight(line: &str) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let keywords: std::collections::HashSet<_> = [
        "fn", "let", "mut", "const", "static", "pub", "crate", "mod", "use", "extern",
        "type", "struct", "enum", "union", "trait", "impl", "dyn", "async", "await",
        "unsafe", "if", "else", "match", "while", "loop", "for", "in", "return",
        "break", "continue", "where", "as", "super", "self", "Self", "true", "false",
    ].iter().cloned().collect();

    let mut current_span_start = 0;

    while i < chars.len() {
        let c = chars[i];

        // Check for line comments
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            // Push anything before the comment
            if i > current_span_start {
                spans.push(Span::raw(&line[current_span_start..i]));
            }
            let comment_start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            spans.push(Span::styled(&line[comment_start..i], Style::default().fg(Color::DarkGray)));
            current_span_start = i;
            continue;
        }

        // Check for block comments
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            // Push anything before the comment
            if i > current_span_start {
                spans.push(Span::raw(&line[current_span_start..i]));
            }
            let comment_start = i;
            i += 2;
            while i < chars.len() {
                if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            spans.push(Span::styled(&line[comment_start..i], Style::default().fg(Color::DarkGray)));
            current_span_start = i;
            continue;
        }

        // Check for strings
        if c == '"' || c == '\'' {
            // Push anything before the string
            if i > current_span_start {
                spans.push(Span::raw(&line[current_span_start..i]));
            }
            let string_start = i;
            let delim = c;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == delim {
                    i += 1;
                    break;
                }
                i += 1;
            }
            spans.push(Span::styled(&line[string_start..i], Style::default().fg(Color::Green)));
            current_span_start = i;
            continue;
        }

        // Check for keywords
        if c.is_alphabetic() {
            let word_start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word = &line[word_start..i];
            if keywords.contains(word) {
                // Push anything before the keyword
                if word_start > current_span_start {
                    spans.push(Span::raw(&line[current_span_start..word_start]));
                }
                spans.push(Span::styled(word, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
                current_span_start = i;
                continue;
            }
            // Not a keyword, just continue (i is already at the end of the word)
            continue;
        }

        i += 1;
    }

    // Push any remaining text at the end
    if current_span_start < line.len() {
        spans.push(Span::raw(&line[current_span_start..]));
    }

    if spans.is_empty() {
        spans.push(Span::raw(line));
    }

    spans
}

impl App {
    pub fn new(file: &Path, contents: &str, output_dir: &Path) -> App {
        let lines: Vec<String> = contents.lines().map(str::to_string).collect();
        let lines = if lines.is_empty() { vec![String::new()] } else { lines };
        App {
            file: file.to_path_buf(),
            contents: contents.to_string(),
            lines,
            output_dir: output_dir.to_path_buf(),
            cursor: 0,
            anchor: None,
            mode: Mode::Normal,
            note: Vec::new(),
            note_cursor: 0,
            scroll: 0,
            status: String::new(),
            should_quit: false,
        }
    }

    /// Selected line range, 1-based inclusive.
    fn selection(&self) -> (usize, usize) {
        match self.anchor {
            Some(a) => {
                let (lo, hi) = if a <= self.cursor {
                    (a, self.cursor)
                } else {
                    (self.cursor, a)
                };
                (lo + 1, hi + 1)
            }
            None => (self.cursor + 1, self.cursor + 1),
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let max = self.lines.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    fn start_editing(&mut self) {
        self.mode = Mode::Editing;
        self.note.clear();
        self.note_cursor = 0;
    }

    fn cancel_editing(&mut self) {
        self.mode = Mode::Normal;
        self.note.clear();
        self.note_cursor = 0;
        self.status = "comment discarded".to_string();
    }

    fn save(&mut self) {
        let note: String = self.note.iter().collect::<String>().trim().to_string();
        self.mode = Mode::Normal;
        self.note.clear();
        self.note_cursor = 0;
        self.anchor = None;
        if note.is_empty() {
            self.status = "empty comment — nothing saved".to_string();
            return;
        }
        let (start, end) = self.selection();
        match save_annotation(&self.file, &self.contents, start, end, &note, &self.output_dir) {
            Ok(path) => self.status = format!("saved: {}", path.display()),
            Err(e) => self.status = format!("save failed: {e}"),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Normal => match key.code {
                KeyCode::Char('q') => self.should_quit = true,
                KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
                KeyCode::Char('g') => self.cursor = 0,
                KeyCode::Char('G') => self.cursor = self.lines.len().saturating_sub(1),
                KeyCode::Char('V') => {
                    self.anchor = Some(self.cursor);
                    self.mode = Mode::Visual;
                }
                KeyCode::Char('c') | KeyCode::Enter => self.start_editing(),
                _ => {}
            },
            Mode::Visual => match key.code {
                KeyCode::Esc => {
                    self.anchor = None;
                    self.mode = Mode::Normal;
                }
                KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
                KeyCode::Char('g') => self.cursor = 0,
                KeyCode::Char('G') => self.cursor = self.lines.len().saturating_sub(1),
                KeyCode::Char('c') | KeyCode::Enter => self.start_editing(),
                _ => {}
            },
            Mode::Editing => match key.code {
                KeyCode::Esc => self.save(),
                KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => self.cancel_editing(),
                KeyCode::Enter => {
                    self.note.insert(self.note_cursor, '\n');
                    self.note_cursor += 1;
                }
                KeyCode::Backspace => {
                    if self.note_cursor > 0 {
                        self.note_cursor -= 1;
                        self.note.remove(self.note_cursor);
                    }
                }
                KeyCode::Left => self.note_cursor = self.note_cursor.saturating_sub(1),
                KeyCode::Right => self.note_cursor = (self.note_cursor + 1).min(self.note.len()),
                KeyCode::Char(c) => {
                    self.note.insert(self.note_cursor, c);
                    self.note_cursor += 1;
                }
                _ => {}
            },
        }
    }
}

pub fn run(file: &Path, contents: &str, output_dir: &Path) -> std::io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = App::new(file, contents, output_dir);
    let result = loop {
        terminal.draw(|f| render(f, &mut app))?;
        if app.should_quit {
            break Ok(());
        }
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => app.handle_key(key),
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let editing = app.mode == Mode::Editing;

    let chunks = if editing {
        Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(area)
    } else {
        Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area)
    };
    let (header_area, body_area, status_area, input_area) = if editing {
        (chunks[0], chunks[1], chunks[2], Some(chunks[3]))
    } else {
        (chunks[0], chunks[1], chunks[2], None)
    };

    // Keep the cursor line visible.
    let visible = body_area.height as usize;
    if app.cursor < app.scroll {
        app.scroll = app.cursor;
    }
    if visible > 0 && app.cursor >= app.scroll + visible {
        app.scroll = app.cursor + 1 - visible;
    }

    let (sel_start, sel_end) = app.selection();

    // Header
    let header = Line::from(vec![
        Span::styled(
            format!(" {} ", app.file.display()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} lines ", app.lines.len()),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("[{}]", app.mode.label()),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!(" · saves to: {}", app.output_dir.display()),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            "   j/k move · V paint multiple lines · c comment · Ctrl+Q cancel · q quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), header_area);

    // Body: painted lines are highlighted.
    let gutter = app.lines.len().to_string().len().max(3);
    let mut body_lines: Vec<Line> = Vec::new();
    for row in 0..visible {
        let idx = app.scroll + row;
        if idx >= app.lines.len() {
            break;
        }
        let lineno = idx + 1;
        let selected = app.anchor.is_some() && lineno >= sel_start && lineno <= sel_end;
        let mut style = Style::default();
        if selected {
            style = style.bg(Color::Rgb(45, 70, 110)).fg(Color::White);
        }
        if idx == app.cursor && !selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        let marker = if selected { ">" } else { " " };
        
        // Apply syntax highlighting
        let highlighted = syntax_highlight(&app.lines[idx]);
        let mut line_spans = vec![Span::styled(format!("{marker}{lineno:>gutter$} "), Style::default().fg(Color::DarkGray))];
        
        // Apply selection/cursor style to highlighted spans
        if selected {
            for span in highlighted {
                line_spans.push(Span::styled(span.content, style));
            }
        } else if idx == app.cursor {
            for span in highlighted {
                line_spans.push(Span::styled(span.content, span.style.patch(style)));
            }
        } else {
            for span in highlighted {
                line_spans.push(span);
            }
        }
        
        body_lines.push(Line::from(line_spans));
    }
    frame.render_widget(Paragraph::new(body_lines), body_area);

    // Status bar
    let status = Line::from(vec![
        Span::styled(
            format!(" {} ", app.mode.label()),
            Style::default().fg(Color::Black).bg(Color::Gray),
        ),
        Span::styled(
            format!(" lines {sel_start}-{sel_end} of {} ", app.lines.len()),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(&app.status, Style::default().fg(Color::Green)),
    ]);
    frame.render_widget(Paragraph::new(status), status_area);

    // Note editor
    if let Some(input_area) = input_area {
        let note: String = app.note.iter().collect();
        let input = Paragraph::new(note.as_str())
            .block(Block::bordered().title("Comment — Esc to save · Ctrl+Q cancel · Enter for newline"))
            .wrap(Wrap { trim: false });
        frame.render_widget(input, input_area);
        let cursor_x = input_area.x + 1 + app.note_cursor as u16;
        let cursor_y = input_area.y + 1;
        frame.set_cursor_position((cursor_x.min(input_area.right().saturating_sub(1)), cursor_y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn syntax_highlight_empty_line() {
        let spans = syntax_highlight("");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "");
    }

    #[test]
    fn syntax_highlight_keyword() {
        let spans = syntax_highlight("fn");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "fn");
        // Check style has cyan color
        assert!(spans[0].style.fg == Some(Color::Cyan));
    }

    #[test]
    fn syntax_highlight_keyword_in_line() {
        let spans = syntax_highlight("fn main()");
        assert!(spans.len() >= 1);
        // First span should be the keyword
        assert_eq!(spans[0].content, "fn");
    }

    #[test]
    fn syntax_highlight_string() {
        let spans = syntax_highlight("\"hello\"");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "\"hello\"");
        // Check style has green color
        assert!(spans[0].style.fg == Some(Color::Green));
    }

    #[test]
    fn syntax_highlight_comment() {
        let spans = syntax_highlight("// comment");
        assert!(spans.len() >= 1);
        // Should have a span with dark gray color
        let has_comment = spans.iter().any(|s| s.style.fg == Some(Color::DarkGray));
        assert!(has_comment, "Should find comment span");
    }

    #[test]
    fn syntax_highlight_mixed() {
        let spans = syntax_highlight("fn test() { // comment");
        assert!(spans.len() >= 1);
        // Should have at least one keyword
        let has_keyword = spans.iter().any(|s| s.content == "fn");
        assert!(has_keyword, "Should find fn keyword");
    }
}