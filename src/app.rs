use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, Terminal};

use crate::annotation::save_annotation;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Visual,
    Editing,
    FileBrowser,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Visual => "VISUAL (painting)",
            Mode::Editing => "EDITING",
            Mode::FileBrowser => "FILE BROWSER",
        }
    }
}

/// Information about a file or directory entry
#[derive(Clone)]
struct FileInfo {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

impl FileInfo {
    fn new(path: PathBuf) -> FileInfo {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let is_dir = path.is_dir();
        FileInfo { path, name, is_dir }
    }
}

pub struct App {
    file: Option<PathBuf>,
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
    /// File browser state
    browser_path: PathBuf,
    browser_entries: Vec<FileInfo>,
    browser_cursor: usize,
    browser_scroll: usize,
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

/// The comment text laid out in rows of at most `width` characters.
struct WrappedNote {
    rows: Vec<String>,
    /// Cursor position as (row, column) within `rows`.
    cursor: (usize, usize),
}

/// Word-wrap `note` to `width` columns, breaking after the last space in a
/// row when there is one and mid-word otherwise. Explicit newlines always
/// start a new row. Also maps the char index `cursor` to its (row, column).
fn wrap_note(note: &[char], cursor: usize, width: usize) -> WrappedNote {
    let width = width.max(1);
    // Rows as char index ranges into `note`, newlines excluded.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    let mut last_space: Option<usize> = None;
    let mut i = 0;
    while i < note.len() {
        if note[i] == '\n' {
            ranges.push((start, i));
            start = i + 1;
            last_space = None;
            i += 1;
            continue;
        }
        if i - start == width {
            // Row is full. A space here is the break itself (it hangs past the
            // edge and is not drawn); otherwise break after the last space, or
            // mid-word if there is none, and look at char `i` again.
            let end = if note[i] == ' ' { i + 1 } else { last_space.map_or(i, |sp| sp + 1) };
            ranges.push((start, end));
            start = end;
            last_space = (start..i).rev().find(|&k| note[k] == ' ');
            i = i.max(end);
            continue;
        }
        if note[i] == ' ' {
            last_space = Some(i);
        }
        i += 1;
    }
    ranges.push((start, note.len()));

    // The cursor belongs to the last row starting at or before it, so at a
    // soft break it sits at the start of the next row.
    let row = ranges.iter().rposition(|&(s, _)| s <= cursor).unwrap_or(0);
    let mut cursor_pos = (row, cursor - ranges[row].0);
    let mut rows: Vec<String> = ranges.iter().map(|&(s, e)| note[s..e].iter().take(width).collect()).collect();
    if cursor_pos.1 >= width {
        // Cursor after a full row: show it where the next char will go.
        cursor_pos = (row + 1, 0);
        if rows.len() == row + 1 {
            rows.push(String::new());
        }
    }
    WrappedNote { rows, cursor: cursor_pos }
}

impl App {
    pub fn new(file: &Path, contents: &str, output_dir: &Path) -> App {
        let lines: Vec<String> = contents.lines().map(str::to_string).collect();
        let lines = if lines.is_empty() { vec![String::new()] } else { lines };
        App {
            file: Some(file.to_path_buf()),
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
            browser_path: output_dir.to_path_buf(),
            browser_entries: Vec::new(),
            browser_cursor: 0,
            browser_scroll: 0,
        }
    }

    /// Create a new App in file browser mode
    pub fn new_browser(start_path: &Path, output_dir: &Path) -> App {
        let mut app = App {
            file: None,
            contents: String::new(),
            lines: vec![String::new()],
            output_dir: output_dir.to_path_buf(),
            cursor: 0,
            anchor: None,
            mode: Mode::FileBrowser,
            note: Vec::new(),
            note_cursor: 0,
            scroll: 0,
            status: String::new(),
            should_quit: false,
            browser_path: start_path.to_path_buf(),
            browser_entries: Vec::new(),
            browser_cursor: 0,
            browser_scroll: 0,
        };
        app.load_browser_entries();
        app
    }

    /// Load directory entries for file browser
    fn load_browser_entries(&mut self) {
        self.browser_entries.clear();
        self.browser_cursor = 0;
        self.browser_scroll = 0;
        
        if let Ok(entries) = fs::read_dir(&self.browser_path) {
            let mut entries_vec: Vec<FileInfo> = entries
                .filter_map(|e| e.ok())
                .map(|e| FileInfo::new(e.path()))
                .collect();
            
            // Sort directories first, then files
            entries_vec.sort_by(|a, b| {
                if a.is_dir == b.is_dir {
                    a.name.cmp(&b.name)
                } else if a.is_dir {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            });
            
            self.browser_entries = entries_vec;
        }
        
        self.status = format!("Browsing: {}", self.browser_path.display());
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
        // Read the painted range before clearing the anchor below.
        let (start, end) = self.selection();
        self.mode = Mode::Normal;
        self.note.clear();
        self.note_cursor = 0;
        self.anchor = None;
        if note.is_empty() {
            self.status = "empty comment — nothing saved".to_string();
            return;
        }
        if let Some(file) = &self.file {
            match save_annotation(file, &self.contents, start, end, &note, &self.output_dir) {
                Ok(path) => self.status = format!("saved: {}", path.display()),
                Err(e) => self.status = format!("save failed: {e}"),
            }
        } else {
            self.status = "no file loaded".to_string();
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
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    self.anchor = Some(self.cursor);
                    self.mode = Mode::Visual;
                }
                KeyCode::Char('c') | KeyCode::Enter => self.start_editing(),
                _ => {}
            },
            Mode::Visual => match key.code {
                KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('V') => {
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
            Mode::FileBrowser => match key.code {
                KeyCode::Char('q') => self.should_quit = true,
                KeyCode::Esc => {
                    // Go back to parent directory or quit if at root
                    if self.browser_path.parent().is_some() {
                        self.browser_path = self.browser_path.parent().unwrap().to_path_buf();
                        self.load_browser_entries();
                    } else {
                        self.should_quit = true;
                    }
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.browser_cursor = (self.browser_cursor + 1).min(self.browser_entries.len().saturating_sub(1));
                    // Scroll if cursor goes off screen
                    let visible = 10; // Approximate visible items
                    if self.browser_cursor >= self.browser_scroll + visible {
                        self.browser_scroll = self.browser_cursor + 1 - visible;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if self.browser_cursor > 0 {
                        self.browser_cursor -= 1;
                        // Scroll up if needed
                        if self.browser_cursor < self.browser_scroll {
                            self.browser_scroll = self.browser_cursor;
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(entry) = self.browser_entries.get(self.browser_cursor) {
                        if entry.is_dir {
                            // Enter directory
                            self.browser_path = entry.path.clone();
                            self.load_browser_entries();
                        } else {
                            // Select file - load it for editing
                            if let Ok(contents) = fs::read_to_string(&entry.path) {
                                self.file = Some(entry.path.clone());
                                self.contents = contents;
                                self.lines = self.contents.lines().map(str::to_string).collect();
                                if self.lines.is_empty() {
                                    self.lines = vec![String::new()];
                                }
                                self.mode = Mode::Normal;
                                self.cursor = 0;
                                self.anchor = None;
                                self.scroll = 0;
                                self.status = String::new();
                            } else {
                                self.status = format!("error: cannot read {}", entry.path.display());
                            }
                        }
                    }
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    // Same as Enter for directories
                    if let Some(entry) = self.browser_entries.get(self.browser_cursor) {
                        if entry.is_dir {
                            self.browser_path = entry.path.clone();
                            self.load_browser_entries();
                        }
                    }
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    // Go to parent directory
                    if self.browser_path.parent().is_some() {
                        self.browser_path = self.browser_path.parent().unwrap().to_path_buf();
                        self.load_browser_entries();
                    }
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

/// Run the file browser mode
pub fn run_browser(start_path: &Path, output_dir: &Path) -> std::io::Result<Option<PathBuf>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = App::new_browser(start_path, output_dir);
    let result = loop {
        terminal.draw(|f| render(f, &mut app))?;
        if app.should_quit {
            break Ok(app.file.clone());
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
    let file_browser = app.mode == Mode::FileBrowser;

    // The comment box grows with its wrapped text, up to half the screen.
    let wrapped_note = editing.then(|| {
        let inner_width = area.width.saturating_sub(2).max(1) as usize;
        wrap_note(&app.note, app.note_cursor, inner_width)
    });
    let max_note_rows = (area.height / 2).saturating_sub(2).max(1) as usize;
    let note_rows = wrapped_note.as_ref().map_or(1, |w| w.rows.len().clamp(1, max_note_rows));

    let chunks = if editing {
        Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(note_rows as u16 + 2),
        ])
        .split(area)
    } else if file_browser {
        Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
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

    // Handle file browser mode separately
    if file_browser {
        // Keep the cursor line visible.
        let visible = body_area.height as usize;
        if app.browser_cursor < app.browser_scroll {
            app.browser_scroll = app.browser_cursor;
        }
        if visible > 0 && app.browser_cursor >= app.browser_scroll + visible {
            app.browser_scroll = app.browser_cursor + 1 - visible;
        }

        // Header
        let header = Line::from(vec![
            Span::styled(
                format!(" Browse: {} ", app.browser_path.display()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("[{}]", app.mode.label()),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                "   j/k move · Enter select · h/Left parent · q quit",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(header), header_area);

        // Body: file/directory list
        let mut body_lines: Vec<Line> = Vec::new();
        for row in 0..visible {
            let idx = app.browser_scroll + row;
            if idx >= app.browser_entries.len() {
                break;
            }
            let entry = &app.browser_entries[idx];
            let is_selected = idx == app.browser_cursor;
            let mut style = Style::default();
            if is_selected {
                style = style.bg(Color::Rgb(45, 70, 110)).fg(Color::White);
            }
            if entry.is_dir {
                style = style.add_modifier(Modifier::BOLD);
            }
            let marker = if entry.is_dir { "📁" } else { "📄" };
            body_lines.push(Line::from(vec![
                Span::styled(format!("{marker} "), style),
                Span::styled(&entry.name, style),
            ]));
        }
        frame.render_widget(Paragraph::new(body_lines), body_area);
        
        // Status bar for file browser
        let status = Line::from(vec![
            Span::styled(
                format!(" {} ", app.mode.label()),
                Style::default().fg(Color::Black).bg(Color::Gray),
            ),
            Span::styled(
                format!(" {} entries ", app.browser_entries.len()),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(&app.status, Style::default().fg(Color::Green)),
        ]);
        frame.render_widget(Paragraph::new(status), status_area);
    } else {
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
                format!(" {} ", app.file.as_ref().map_or("No file".to_string(), |f| f.display().to_string())),
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
                "   j/k move · v paint multiple lines · c comment · Ctrl+Q cancel · q quit",
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
        let (sel_start, sel_end) = app.selection();
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
        if let (Some(input_area), Some(wrapped)) = (input_area, wrapped_note) {
            // Scroll so the cursor row stays visible.
            let (cursor_row, cursor_col) = wrapped.cursor;
            let scroll = cursor_row.saturating_sub(note_rows - 1);
            let lines: Vec<Line> = wrapped.rows.into_iter().skip(scroll).take(note_rows).map(Line::from).collect();
            let input = Paragraph::new(lines)
                .block(Block::bordered().title("Comment — Esc to save · Ctrl+Q cancel · Enter for newline"));
            frame.render_widget(input, input_area);
            let cursor_x = input_area.x + 1 + cursor_col as u16;
            let cursor_y = input_area.y + 1 + (cursor_row - scroll) as u16;
            frame.set_cursor_position((cursor_x.min(input_area.right().saturating_sub(2)), cursor_y));
        }
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

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn wrap_note_breaks_at_spaces() {
        let note = chars("hello world foo");
        let w = wrap_note(&note, note.len(), 11);
        assert_eq!(w.rows, vec!["hello world", "foo"]);
        assert_eq!(w.cursor, (1, 3));
    }

    #[test]
    fn wrap_note_breaks_long_words() {
        let note = chars("abcdefghij");
        let w = wrap_note(&note, 0, 4);
        assert_eq!(w.rows, vec!["abcd", "efgh", "ij"]);
        assert_eq!(w.cursor, (0, 0));
    }

    #[test]
    fn wrap_note_respects_newlines() {
        let note = chars("ab\ncd");
        let w = wrap_note(&note, 2, 10);
        assert_eq!(w.rows, vec!["ab", "cd"]);
        assert_eq!(w.cursor, (0, 2));
        assert_eq!(wrap_note(&note, 3, 10).cursor, (1, 0));
    }

    #[test]
    fn wrap_note_cursor_after_full_row_moves_to_next_row() {
        let note = chars("abcd");
        let w = wrap_note(&note, 4, 4);
        assert_eq!(w.rows, vec!["abcd", ""]);
        assert_eq!(w.cursor, (1, 0));
    }

    #[test]
    fn wrap_note_rows_never_exceed_width() {
        let note = chars("the quick brown fox jumps over the lazy dog again and again");
        for width in 1..20 {
            let w = wrap_note(&note, note.len(), width);
            assert!(w.rows.iter().all(|r| r.chars().count() <= width), "width {width}: {:?}", w.rows);
            let no_spaces = |s: String| s.replace(' ', "");
            assert_eq!(no_spaces(w.rows.concat()), no_spaces(note.iter().collect()));
        }
    }

    #[test]
    fn lowercase_v_toggles_visual_mode() {
        let mut app = App::new(Path::new("x.rs"), "a\nb\n", Path::new("out"));
        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        assert!(app.mode == Mode::Visual);
        assert_eq!(app.anchor, Some(0));
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.selection(), (1, 2));
        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        assert!(app.mode == Mode::Normal);
        assert_eq!(app.anchor, None);
    }

    // --- Key handling and rendering, driven through `handle_key` and `render`. ---

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("annotator-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::from(code));
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) });
        }
    }

    fn saved_files(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir).map_or(Vec::new(), |d| d.map(|e| e.unwrap().path()).collect())
    }

    /// Render one frame into an in-memory terminal; returns screen rows and cursor.
    fn draw(app: &mut App, width: u16, height: u16) -> (Vec<String>, (u16, u16)) {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let backend = terminal.backend();
        let rows = backend
            .buffer()
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect();
        let pos = backend.cursor_position();
        (rows, (pos.x, pos.y))
    }

    #[test]
    fn esc_saves_painted_selection() {
        let dir = temp_dir();
        let file = dir.join("a.rs");
        fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let out = dir.join("out");
        let mut app = App::new(&file, &fs::read_to_string(&file).unwrap(), &out);

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('c'));
        assert!(app.mode == Mode::Editing);
        type_text(&mut app, "  looks off  ");
        press(&mut app, KeyCode::Esc);

        assert!(app.mode == Mode::Normal);
        assert_eq!(app.anchor, None);
        assert!(app.status.starts_with("saved: "), "status: {}", app.status);
        let files = saved_files(&out);
        assert_eq!(files.len(), 1);
        let md = fs::read_to_string(&files[0]).unwrap();
        assert!(md.contains("- scope: lines"));
        assert!(md.contains("- lines: 2-3"));
        assert!(md.ends_with("## Comment\n\nlooks off\n"), "{md}");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ctrl_q_discards_comment() {
        let dir = temp_dir();
        let out = dir.join("out");
        let mut app = App::new(Path::new("x.rs"), "a\n", &out);
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "never mind");
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));

        assert!(app.mode == Mode::Normal);
        assert_eq!(app.status, "comment discarded");
        assert!(app.note.is_empty());
        assert!(saved_files(&out).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn plain_q_while_editing_is_text_not_quit() {
        let mut app = App::new(Path::new("x.rs"), "a\n", Path::new("out"));
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "q");
        assert!(!app.should_quit);
        assert_eq!(app.note, vec!['q']);
    }

    #[test]
    fn empty_comment_is_not_saved() {
        let dir = temp_dir();
        let out = dir.join("out");
        let mut app = App::new(Path::new("x.rs"), "a\n", &out);
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "  \n ");
        press(&mut app, KeyCode::Esc);

        assert_eq!(app.status, "empty comment — nothing saved");
        assert!(saved_files(&out).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn editor_cursor_movement_and_backspace() {
        let mut app = App::new(Path::new("x.rs"), "a\n", Path::new("out"));
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "ac");
        press(&mut app, KeyCode::Left);
        type_text(&mut app, "b");
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right); // already at end: stays
        type_text(&mut app, "dx");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.note.iter().collect::<String>(), "abcd");
        assert_eq!(app.note_cursor, 4);
    }

    #[test]
    fn cursor_moves_are_clamped_to_file() {
        let mut app = App::new(Path::new("x.rs"), "a\nb\nc\n", Path::new("out"));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.cursor, 0);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.cursor, 2);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor, 2);
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn selection_works_when_painting_upwards() {
        let mut app = App::new(Path::new("x.rs"), "a\nb\nc\nd\n", Path::new("out"));
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selection(), (2, 4));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.selection(), (2, 2));
    }

    #[test]
    fn header_shows_lowercase_v_hint_and_output_dir() {
        let mut app = App::new(Path::new("x.rs"), "a\n", Path::new("my-notes"));
        let (rows, _) = draw(&mut app, 200, 10);
        assert!(rows[0].contains("v paint multiple lines"), "{}", rows[0]);
        assert!(rows[0].contains("saves to: my-notes"), "{}", rows[0]);
    }

    #[test]
    fn comment_box_wraps_text_and_tracks_cursor() {
        let mut app = App::new(Path::new("x.rs"), "fn main() {}\n", Path::new("out"));
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "aaaa bbbb cccc dddd eeee ffff gggg");
        // 30 wide → 28 inner columns: the text needs two rows, so the box is
        // 4 tall at the bottom of a 20 row screen (rows 16..=19).
        let (rows, cursor) = draw(&mut app, 30, 20);
        assert!(rows[16].starts_with("┌Comment"), "{:#?}", rows);
        assert_eq!(rows[17], "│aaaa bbbb cccc dddd eeee    │");
        assert_eq!(rows[18], "│ffff gggg                   │");
        assert!(rows[19].starts_with("└"));
        assert_eq!(cursor, (10, 18));
        assert!(rows[2].contains("fn main() {}"), "file body still shown: {}", rows[2]);
    }

    #[test]
    fn comment_box_scrolls_when_taller_than_half_the_screen() {
        let mut app = App::new(Path::new("x.rs"), "a\n", Path::new("out"));
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9");
        // 12 rows high → at most 12/2 - 2 = 4 text rows, scrolled to the cursor.
        let (rows, cursor) = draw(&mut app, 30, 12);
        let shown: Vec<&str> = rows[7..11].iter().map(|r| r.trim_matches(['│', ' '])).collect();
        assert_eq!(shown, ["l6", "l7", "l8", "l9"]);
        assert!(rows[11].starts_with("└"));
        assert_eq!(cursor, (3, 10));
    }

    #[test]
    fn file_browser_lists_dirs_first_and_opens_files() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("zdir")).unwrap();
        fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let mut app = App::new_browser(&dir, Path::new("out"));
        let names: Vec<&str> = app.browser_entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zdir", "a.rs"]);

        press(&mut app, KeyCode::Enter); // into zdir
        assert_eq!(app.browser_path, dir.join("zdir"));
        press(&mut app, KeyCode::Char('h')); // back to parent
        assert_eq!(app.browser_path, dir);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter); // open a.rs
        assert!(app.mode == Mode::Normal);
        assert_eq!(app.file.as_deref(), Some(dir.join("a.rs").as_path()));
        assert_eq!(app.lines, vec!["fn a() {}"]);

        fs::remove_dir_all(&dir).unwrap();
    }
}
