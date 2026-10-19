use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
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
            "   j/k move · V paint · c comment · q quit",
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
        if idx == app.cursor {
            style = style.add_modifier(Modifier::BOLD);
        }
        let marker = if selected { ">" } else { " " };
        body_lines.push(Line::from(vec![
            Span::styled(format!("{marker}{lineno:>gutter$} "), Style::default().fg(Color::DarkGray)),
            Span::styled(app.lines[idx].clone(), style),
        ]));
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
            .block(Block::bordered().title("Comment — Esc to save · Enter for newline"))
            .wrap(Wrap { trim: false });
        frame.render_widget(input, input_area);
        let cursor_x = input_area.x + 1 + app.note_cursor as u16;
        let cursor_y = input_area.y + 1;
        frame.set_cursor_position((cursor_x.min(input_area.right().saturating_sub(1)), cursor_y));
    }
}
