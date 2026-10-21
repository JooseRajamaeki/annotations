use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{Frame, Terminal};

use crate::annotation::{read_text_file, save_annotation, save_path_annotation};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// What the comment being written will be attached to.
#[derive(Clone, PartialEq, Eq, Debug)]
enum CommentTarget {
    /// The painted line range (or the cursor line) of the open file.
    Lines,
    /// A whole file or directory.
    Path(PathBuf),
}

/// Every key, grouped by mode, for the `?` help screen.
const HELP: &[(&str, &[(&str, &str)])] = &[
    ("File view", &[
        ("j/k ↑/↓", "move"),
        ("g / G", "top / bottom"),
        ("v", "paint (select) lines"),
        ("c / Enter", "comment the line(s)"),
        ("f", "comment the whole file"),
        ("b", "browse files"),
        ("q", "quit"),
    ]),
    ("Painting lines", &[
        ("j/k ↑/↓", "extend the selection"),
        ("c / Enter", "comment the selection"),
        ("v / Esc", "stop painting"),
    ]),
    ("File browser", &[
        ("j/k ↑/↓ g/G", "move"),
        ("Enter / l / →", "open file or directory"),
        ("h / ← / Bksp", "parent directory"),
        ("c", "comment the selected file or directory"),
        ("C", "comment this directory"),
        ("Esc", "back to the open file"),
        ("q", "quit"),
    ]),
    ("Writing a comment", &[
        ("Esc", "save"),
        ("Ctrl+Q", "discard"),
        ("Enter", "new line"),
        ("← / →", "move the cursor"),
    ]),
    ("Mouse", &[
        ("wheel", "scroll"),
        ("click", "move to a line / select an entry"),
        ("drag", "paint lines"),
        ("click again", "open the selected entry"),
    ]),
];

/// Lines (or browser entries) scrolled per mouse wheel step.
const WHEEL_STEP: isize = 3;

/// `path` relative to the working directory when it is inside it, so
/// annotations record `src/main.rs` rather than an absolute path.
fn relative_to_cwd(path: &Path) -> PathBuf {
    let Ok(cwd) = std::env::current_dir() else {
        return path.to_path_buf();
    };
    match path.strip_prefix(&cwd) {
        Ok(rel) if rel.as_os_str().is_empty() => PathBuf::from("."),
        Ok(rel) => rel.to_path_buf(),
        Err(_) => path.to_path_buf(),
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
    syntax: Syntax,
    /// Code block state of each line, for Markdown files (empty otherwise).
    md_lines: Vec<MdLine>,
    output_dir: PathBuf,
    cursor: usize,
    /// Start of the painted selection (0-based). `None` = no painting active.
    anchor: Option<usize>,
    mode: Mode,
    /// The note being written, kept as chars so cursor movement is safe.
    note: Vec<char>,
    note_cursor: usize,
    /// What the note being written is about.
    target: CommentTarget,
    /// Mode to go back to when the note is saved or discarded.
    return_mode: Mode,
    show_help: bool,
    scroll: usize,
    status: String,
    should_quit: bool,
    /// File browser state
    browser_path: PathBuf,
    browser_entries: Vec<FileInfo>,
    browser_cursor: usize,
    browser_scroll: usize,
    /// Where the file or browser list was last drawn, for mouse hit-testing.
    body_area: Rect,
    /// Line (0-based) the left mouse button was pressed on, while it is held.
    drag_from: Option<usize>,
}

/// Colorize a line of Rust code with basic syntax highlighting
fn syntax_highlight(line: &str) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    // Byte offset of each char (plus the end), for slicing `line` by char index.
    let byte: Vec<usize> = line.char_indices().map(|(b, _)| b).chain([line.len()]).collect();
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
                spans.push(Span::raw(&line[byte[current_span_start]..byte[i]]));
            }
            let comment_start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            spans.push(Span::styled(&line[byte[comment_start]..byte[i]], Style::default().fg(Color::DarkGray)));
            current_span_start = i;
            continue;
        }

        // Check for block comments
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            // Push anything before the comment
            if i > current_span_start {
                spans.push(Span::raw(&line[byte[current_span_start]..byte[i]]));
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
            spans.push(Span::styled(&line[byte[comment_start]..byte[i]], Style::default().fg(Color::DarkGray)));
            current_span_start = i;
            continue;
        }

        // Check for strings
        if c == '"' || c == '\'' {
            // Push anything before the string
            if i > current_span_start {
                spans.push(Span::raw(&line[byte[current_span_start]..byte[i]]));
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
            spans.push(Span::styled(&line[byte[string_start]..byte[i]], Style::default().fg(Color::Green)));
            current_span_start = i;
            continue;
        }

        // Check for keywords
        if c.is_alphabetic() {
            let word_start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word = &line[byte[word_start]..byte[i]];
            if keywords.contains(word) {
                // Push anything before the keyword
                if word_start > current_span_start {
                    spans.push(Span::raw(&line[byte[current_span_start]..byte[word_start]]));
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
    if current_span_start < chars.len() {
        spans.push(Span::raw(&line[byte[current_span_start]..]));
    }

    if spans.is_empty() {
        spans.push(Span::raw(line));
    }

    spans
}

/// How a file's lines are colored, chosen from its extension.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Syntax {
    Rust,
    Markdown,
}

impl Syntax {
    fn for_path(path: &Path) -> Syntax {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") {
            Syntax::Markdown
        } else {
            Syntax::Rust
        }
    }
}

/// Where a Markdown line sits relative to fenced code blocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MdLine {
    Text,
    /// An opening or closing ``` / ~~~ fence.
    Fence,
    /// Inside a fenced code block; `rust` when the block is tagged rs/rust.
    Code { rust: bool },
}

/// A code fence: up to 3 spaces, then 3 or more backticks or tildes.
/// Returns the fence char, the fence length and the info string after it.
fn code_fence(line: &str) -> Option<(char, usize, &str)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let c = rest.chars().next().filter(|&c| c == '`' || c == '~')?;
    let len = rest.len() - rest.trim_start_matches(c).len();
    let info = rest[len..].trim();
    if len < 3 || (c == '`' && info.contains('`')) {
        return None;
    }
    Some((c, len, info))
}

/// Classify every line of a Markdown file. A block is closed by a fence of the
/// same char that is at least as long as the opening one and has no info string.
fn markdown_lines(lines: &[String]) -> Vec<MdLine> {
    let mut open: Option<(char, usize, bool)> = None;
    lines
        .iter()
        .map(|line| match (open, code_fence(line)) {
            (None, Some((c, len, info))) => {
                let lang = info.split_whitespace().next().unwrap_or("");
                open = Some((c, len, lang == "rs" || lang == "rust"));
                MdLine::Fence
            }
            (Some((c, len, _)), Some((fc, flen, ""))) if fc == c && flen >= len => {
                open = None;
                MdLine::Fence
            }
            (Some((_, _, rust)), _) => MdLine::Code { rust },
            (None, None) => MdLine::Text,
        })
        .collect()
}

/// Colorize a line of Markdown text (not inside a code block): headings, rules,
/// quote and list markers, then inline code, links, bold and italic.
fn markdown_highlight(line: &str) -> Vec<Span<'_>> {
    let marker = Style::default().fg(Color::Magenta);
    let dim = Style::default().fg(Color::DarkGray);
    let trimmed = line.trim_start();
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) && (trimmed.len() == hashes || trimmed[hashes..].starts_with(' ')) {
        return vec![Span::styled(line, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))];
    }
    let compact: String = trimmed.chars().filter(|c| *c != ' ').collect();
    if compact.len() >= 3 && ['-', '*', '_'].iter().any(|&c| compact.trim_start_matches(c).is_empty()) {
        return vec![Span::styled(line, dim)];
    }

    let mut spans = Vec::new();
    // Indentation, then any `>` quote markers, then a list marker.
    let mut pos = line.len() - trimmed.len();
    if pos > 0 {
        spans.push(Span::raw(&line[..pos]));
    }
    while line[pos..].starts_with('>') {
        let end = if line[pos + 1..].starts_with(' ') { pos + 2 } else { pos + 1 };
        spans.push(Span::styled(&line[pos..end], dim));
        pos = end;
    }
    let rest = &line[pos..];
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let list_marker = if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") {
        Some(2)
    } else if (1..=9).contains(&digits) && (rest[digits..].starts_with(". ") || rest[digits..].starts_with(") ")) {
        Some(digits + 2)
    } else {
        None
    };
    if let Some(len) = list_marker {
        spans.push(Span::styled(&line[pos..pos + len], marker));
        pos += len;
    }
    markdown_inline(line, pos, &mut spans);
    if spans.is_empty() {
        spans.push(Span::raw(line));
    }
    spans
}

/// End (exclusive) of an emphasis run that `delim` opens at byte `start`, or
/// `None` if it does not open one. Like CommonMark, the text must not start or
/// end with a space, and `_` does not work inside words (`snake_case`).
fn emphasis_end(line: &str, start: usize, delim: &str) -> Option<usize> {
    let inner = start + delim.len();
    let underscore = delim.starts_with('_');
    let before = line[..start].chars().next_back();
    if line[inner..].starts_with([' ', '\t']) || (underscore && before.is_some_and(char::is_alphanumeric)) {
        return None;
    }
    // A single `*` or `_` must not close on half of a `**` or `__`.
    let single = (delim.len() == 1).then(|| delim.chars().next().unwrap_or('*'));
    let mut from = inner;
    while let Some(i) = line[from..].find(delim) {
        let close = from + i;
        let after = line[close + delim.len()..].chars().next();
        let doubled = single.is_some_and(|c| after == Some(c) || line[..close].ends_with(c));
        if close > inner
            && !doubled
            && !line[..close].ends_with([' ', '\t'])
            && !(underscore && after.is_some_and(char::is_alphanumeric))
        {
            return Some(close + delim.len());
        }
        from = close + 1;
    }
    None
}

/// Inline Markdown from byte `pos` of `line` to its end, appended to `spans`.
fn markdown_inline<'a>(line: &'a str, mut pos: usize, spans: &mut Vec<Span<'a>>) {
    let code = Style::default().fg(Color::Yellow);
    let link = Style::default().fg(Color::Blue).add_modifier(Modifier::UNDERLINED);
    let dim = Style::default().fg(Color::DarkGray);
    let bytes = line.as_bytes();
    // Start of the plain text not yet pushed.
    let mut plain = pos;
    while pos < line.len() {
        // Every marker is ASCII, so `pos` is a char boundary whenever it matches.
        let styled: Option<(usize, Vec<(usize, Style)>)> = match bytes[pos] {
            b'\\' => {
                pos += 2;
                continue;
            }
            b'`' => {
                let run = line[pos..].len() - line[pos..].trim_start_matches('`').len();
                let fence = &line[pos..pos + run];
                // Closing run of exactly the same length.
                let mut close = None;
                let mut from = pos + run;
                while let Some(i) = line[from..].find(fence) {
                    let at = from + i;
                    let len = line[at..].len() - line[at..].trim_start_matches('`').len();
                    if len == run {
                        close = Some(at + run);
                        break;
                    }
                    from = at + len;
                }
                match close {
                    Some(end) => Some((end, vec![(end, code)])),
                    None => {
                        pos += run;
                        continue;
                    }
                }
            }
            b'[' | b'!' if line[pos..].starts_with('[') || line[pos..].starts_with("![") => {
                let text_start = pos + line[pos..].find('[').unwrap_or(0);
                line[text_start..].find("](").and_then(|close| {
                    let url_start = text_start + close + 1;
                    let end = url_start + line[url_start..].find(')')? + 1;
                    Some((end, vec![(url_start, link), (end, dim)]))
                })
            }
            b'*' | b'_' => {
                let delim = if bytes.get(pos + 1) == Some(&bytes[pos]) { &line[pos..pos + 2] } else { &line[pos..pos + 1] };
                let modifier = if delim.len() == 2 { Modifier::BOLD } else { Modifier::ITALIC };
                emphasis_end(line, pos, delim).map(|end| (end, vec![(end, Style::default().add_modifier(modifier))]))
            }
            _ => None,
        };
        match styled {
            Some((end, parts)) => {
                if plain < pos {
                    spans.push(Span::raw(&line[plain..pos]));
                }
                let mut from = pos;
                for (to, style) in parts {
                    spans.push(Span::styled(&line[from..to], style));
                    from = to;
                }
                pos = end;
                plain = end;
            }
            None => pos += 1,
        }
    }
    if plain < line.len() {
        spans.push(Span::raw(&line[plain..]));
    }
}

/// Scroll a list of `len` items, `visible` of them on screen, by `delta` rows
/// and pull `cursor` along so it stays on screen.
fn scroll_view(scroll: &mut usize, cursor: &mut usize, len: usize, visible: usize, delta: isize) {
    if visible == 0 {
        return;
    }
    let max_scroll = len.saturating_sub(visible) as isize;
    *scroll = (*scroll as isize + delta).clamp(0, max_scroll) as usize;
    *cursor = (*cursor).clamp(*scroll, *scroll + visible - 1).min(len.saturating_sub(1));
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
    fn empty(output_dir: &Path, mode: Mode, browser_path: PathBuf) -> App {
        App {
            file: None,
            contents: String::new(),
            lines: vec![String::new()],
            syntax: Syntax::Rust,
            md_lines: Vec::new(),
            output_dir: output_dir.to_path_buf(),
            cursor: 0,
            anchor: None,
            mode,
            note: Vec::new(),
            note_cursor: 0,
            target: CommentTarget::Lines,
            return_mode: mode,
            show_help: false,
            scroll: 0,
            status: "press ? for help".to_string(),
            should_quit: false,
            browser_path,
            browser_entries: Vec::new(),
            browser_cursor: 0,
            browser_scroll: 0,
            body_area: Rect::default(),
            drag_from: None,
        }
    }

    /// Start in the file view; the browser (`b`) opens in the file's directory.
    pub fn new(file: &Path, contents: &str, output_dir: &Path) -> App {
        let browser_path = std::path::absolute(file)
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let mut app = App::empty(output_dir, Mode::Normal, browser_path);
        app.load_file(file, contents.to_string());
        app
    }

    /// Start in the file browser at `start_path`.
    pub fn new_browser(start_path: &Path, output_dir: &Path) -> App {
        let mut app = App::empty(output_dir, Mode::FileBrowser, start_path.to_path_buf());
        app.load_browser_entries();
        app
    }

    fn load_file(&mut self, path: &Path, contents: String) {
        self.file = Some(relative_to_cwd(path));
        self.lines = contents.lines().map(str::to_string).collect();
        if self.lines.is_empty() {
            self.lines = vec![String::new()];
        }
        self.syntax = Syntax::for_path(path);
        self.md_lines = match self.syntax {
            Syntax::Markdown => markdown_lines(&self.lines),
            Syntax::Rust => Vec::new(),
        };
        self.contents = contents;
        self.mode = Mode::Normal;
        self.cursor = 0;
        self.anchor = None;
        self.scroll = 0;
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
    }

    /// Show `dir` in the browser with the cursor on `select`, if it is listed.
    fn browse(&mut self, dir: PathBuf, select: Option<&Path>) {
        self.mode = Mode::FileBrowser;
        self.browser_path = dir;
        self.load_browser_entries();
        if let Some(select) = select {
            if let Some(i) = self.browser_entries.iter().position(|e| e.path == select) {
                self.browser_cursor = i;
            }
        }
    }

    /// Open the browser from the file view, with the open file selected.
    fn open_browser(&mut self) {
        let current = self.file.as_deref().and_then(|f| std::path::absolute(f).ok());
        self.browse(self.browser_path.clone(), current.as_deref());
        self.status = String::new();
    }

    fn browser_parent(&mut self) {
        if let Some(parent) = self.browser_path.parent().map(Path::to_path_buf) {
            let from = self.browser_path.clone();
            self.browse(parent, Some(&from));
        }
    }

    fn browser_open_selected(&mut self, open_files: bool) {
        let Some(entry) = self.browser_entries.get(self.browser_cursor).cloned() else {
            return;
        };
        if entry.is_dir {
            self.browse(entry.path, None);
        } else if open_files {
            match read_text_file(&entry.path) {
                Ok(contents) => {
                    self.load_file(&entry.path, contents);
                    self.status = "opened — v to paint lines, c to comment, b to browse".to_string();
                }
                Err(e) => self.status = format!("cannot open {}: {e}", entry.name),
            }
        }
    }

    fn move_browser_cursor(&mut self, delta: isize) {
        let max = self.browser_entries.len().saturating_sub(1) as isize;
        self.browser_cursor = (self.browser_cursor as isize + delta).clamp(0, max) as usize;
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

    /// Line `idx` of the open file, colored for its syntax.
    fn highlight(&self, idx: usize) -> Vec<Span<'_>> {
        let line = &self.lines[idx];
        match (self.syntax, self.md_lines.get(idx)) {
            (Syntax::Markdown, Some(MdLine::Text)) => markdown_highlight(line),
            (Syntax::Markdown, Some(MdLine::Fence)) => vec![Span::styled(line.as_str(), Style::default().fg(Color::DarkGray))],
            (Syntax::Markdown, Some(MdLine::Code { rust: false })) => {
                vec![Span::styled(line.as_str(), Style::default().fg(Color::Yellow))]
            }
            _ => syntax_highlight(line),
        }
    }

    /// Whether the body shows the file browser (also while commenting on an entry from it).
    fn browsing(&self) -> bool {
        self.mode == Mode::FileBrowser || (self.mode == Mode::Editing && self.return_mode == Mode::FileBrowser)
    }

    fn start_editing(&mut self, target: CommentTarget) {
        self.return_mode = self.mode;
        self.target = target;
        self.mode = Mode::Editing;
        self.note.clear();
        self.note_cursor = 0;
        self.status.clear();
    }

    /// What the comment being written is about, for the editor title.
    fn target_label(&self) -> String {
        match &self.target {
            CommentTarget::Lines => {
                let (start, end) = self.selection();
                let file = self.file.as_deref().unwrap_or(Path::new("?")).display();
                if start == end {
                    format!("line {start} of {file}")
                } else {
                    format!("lines {start}-{end} of {file}")
                }
            }
            CommentTarget::Path(p) => {
                let kind = if p.is_dir() { "directory" } else { "file" };
                format!("{kind} {}", relative_to_cwd(p).display())
            }
        }
    }

    fn cancel_editing(&mut self) {
        self.mode = self.return_mode;
        self.note.clear();
        self.note_cursor = 0;
        self.status = "comment discarded".to_string();
    }

    fn save(&mut self) {
        let note: String = self.note.iter().collect::<String>().trim().to_string();
        // Read the painted range before clearing the anchor below.
        let (start, end) = self.selection();
        self.note.clear();
        self.note_cursor = 0;
        if note.is_empty() {
            self.mode = self.return_mode;
            self.status = "empty comment — nothing saved".to_string();
            return;
        }
        let result = match &self.target {
            CommentTarget::Lines => {
                self.anchor = None;
                self.mode = Mode::Normal;
                match &self.file {
                    Some(file) => save_annotation(file, &self.contents, start, end, &note, &self.output_dir),
                    None => Err(std::io::Error::other("no file loaded")),
                }
            }
            CommentTarget::Path(path) => {
                self.mode = self.return_mode;
                save_path_annotation(&relative_to_cwd(path), &note, &self.output_dir)
            }
        };
        self.status = match result {
            Ok(path) => format!("saved: {}", path.display()),
            Err(e) => format!("save failed: {e}"),
        };
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.show_help {
            // Any key closes the help screen.
            self.show_help = false;
            return;
        }
        match self.mode {
            Mode::Normal => match key.code {
                KeyCode::Char('q') => self.should_quit = true,
                KeyCode::Char('?') => self.show_help = true,
                KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
                KeyCode::Char('g') => self.cursor = 0,
                KeyCode::Char('G') => self.cursor = self.lines.len().saturating_sub(1),
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    self.anchor = Some(self.cursor);
                    self.mode = Mode::Visual;
                }
                KeyCode::Char('c') | KeyCode::Enter => self.start_editing(CommentTarget::Lines),
                KeyCode::Char('f') => {
                    if let Some(file) = self.file.clone() {
                        self.start_editing(CommentTarget::Path(file));
                    }
                }
                KeyCode::Char('b') => self.open_browser(),
                _ => {}
            },
            Mode::Visual => match key.code {
                KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('V') => {
                    self.anchor = None;
                    self.mode = Mode::Normal;
                }
                KeyCode::Char('?') => self.show_help = true,
                KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
                KeyCode::Char('g') => self.cursor = 0,
                KeyCode::Char('G') => self.cursor = self.lines.len().saturating_sub(1),
                KeyCode::Char('c') | KeyCode::Enter => self.start_editing(CommentTarget::Lines),
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
                KeyCode::Char('?') => self.show_help = true,
                KeyCode::Esc => {
                    if self.file.is_some() {
                        self.mode = Mode::Normal;
                    }
                }
                KeyCode::Char('j') | KeyCode::Down => self.move_browser_cursor(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_browser_cursor(-1),
                KeyCode::Char('g') => self.browser_cursor = 0,
                KeyCode::Char('G') => self.browser_cursor = self.browser_entries.len().saturating_sub(1),
                KeyCode::Enter => self.browser_open_selected(true),
                KeyCode::Char('l') | KeyCode::Right => self.browser_open_selected(false),
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => self.browser_parent(),
                KeyCode::Char('c') => {
                    if let Some(entry) = self.browser_entries.get(self.browser_cursor) {
                        let path = entry.path.clone();
                        self.start_editing(CommentTarget::Path(path));
                    }
                }
                KeyCode::Char('C') => self.start_editing(CommentTarget::Path(self.browser_path.clone())),
                _ => {}
            },
        }
    }

    /// List index under screen row `row` when the body is scrolled by `scroll`.
    /// Negative or past the end when the row is outside the list.
    fn index_at_row(&self, row: u16, scroll: usize) -> isize {
        scroll as isize + row as isize - self.body_area.y as isize
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.show_help {
            // Any click closes the help screen, like any key.
            if let MouseEventKind::Down(_) = mouse.kind {
                self.show_help = false;
            }
            return;
        }
        let visible = self.body_area.height as usize;
        let in_body = self.body_area.contains(Position::new(mouse.column, mouse.row));
        match self.mode {
            Mode::Normal | Mode::Visual => match mouse.kind {
                MouseEventKind::ScrollDown => {
                    scroll_view(&mut self.scroll, &mut self.cursor, self.lines.len(), visible, WHEEL_STEP)
                }
                MouseEventKind::ScrollUp => {
                    scroll_view(&mut self.scroll, &mut self.cursor, self.lines.len(), visible, -WHEEL_STEP)
                }
                // A click moves to the line and stops painting; dragging from it paints.
                MouseEventKind::Down(MouseButton::Left) if in_body => {
                    let line = self.index_at_row(mouse.row, self.scroll);
                    if line < self.lines.len() as isize {
                        self.anchor = None;
                        self.mode = Mode::Normal;
                        self.cursor = line as usize;
                        self.drag_from = Some(self.cursor);
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(from) = self.drag_from {
                        // Dragging above or below the body moves past the edge,
                        // and the view scrolls to follow.
                        let max = self.lines.len().saturating_sub(1) as isize;
                        self.cursor = self.index_at_row(mouse.row, self.scroll).clamp(0, max) as usize;
                        self.anchor = Some(from);
                        self.mode = Mode::Visual;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => self.drag_from = None,
                _ => {}
            },
            Mode::FileBrowser => match mouse.kind {
                MouseEventKind::ScrollDown => scroll_view(
                    &mut self.browser_scroll,
                    &mut self.browser_cursor,
                    self.browser_entries.len(),
                    visible,
                    WHEEL_STEP,
                ),
                MouseEventKind::ScrollUp => scroll_view(
                    &mut self.browser_scroll,
                    &mut self.browser_cursor,
                    self.browser_entries.len(),
                    visible,
                    -WHEEL_STEP,
                ),
                // Click selects an entry; clicking the selected entry opens it.
                MouseEventKind::Down(MouseButton::Left) if in_body => {
                    let idx = self.index_at_row(mouse.row, self.browser_scroll);
                    if (0..self.browser_entries.len() as isize).contains(&idx) {
                        if idx as usize == self.browser_cursor {
                            self.browser_open_selected(true);
                        } else {
                            self.browser_cursor = idx as usize;
                        }
                    }
                }
                _ => {}
            },
            // The comment is about the painted lines, so they stay put while writing.
            Mode::Editing => {}
        }
    }

    /// The most useful keys for the current mode, always shown under the title.
    /// Kept within 80 columns; `?` lists everything.
    fn key_hints(&self) -> String {
        match self.mode {
            Mode::Normal => "? help · v paint lines · c comment line(s) · f comment file · b browse · q quit".to_string(),
            Mode::Visual => "? help · j/k extend · c comment selection · v/Esc stop painting".to_string(),
            Mode::Editing => "Esc save · Ctrl+Q discard · Enter new line · ←/→ move cursor".to_string(),
            Mode::FileBrowser => {
                let back = if self.file.is_some() { " · Esc back" } else { "" };
                format!("? help · Enter open · h up · c comment entry · C comment dir{back} · q quit")
            }
        }
    }
}

/// Run the TUI until the user quits.
pub fn run(mut app: App) -> std::io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = loop {
        terminal.draw(|f| render(f, &mut app))?;
        if app.should_quit {
            break Ok(());
        }
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => app.handle_key(key),
            Ok(Event::Mouse(mouse)) => app.handle_mouse(mouse),
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let editing = app.mode == Mode::Editing;

    // The comment box grows with its wrapped text, up to half the screen.
    let wrapped_note = editing.then(|| {
        let inner_width = area.width.saturating_sub(2).max(1) as usize;
        wrap_note(&app.note, app.note_cursor, inner_width)
    });
    let max_note_rows = (area.height / 2).saturating_sub(2).max(1) as usize;
    let note_rows = wrapped_note.as_ref().map_or(1, |w| w.rows.len().clamp(1, max_note_rows));

    let mut constraints = vec![Constraint::Length(2), Constraint::Min(1), Constraint::Length(1)];
    if editing {
        constraints.push(Constraint::Length(note_rows as u16 + 2));
    }
    let chunks = Layout::vertical(constraints).split(area);
    let (header_area, body_area, status_area) = (chunks[0], chunks[1], chunks[2]);

    // Header: what is shown, then the key hints for the current mode.
    let title = if app.browsing() {
        format!(" Browse: {} ", app.browser_path.display())
    } else {
        format!(
            " {}  {} lines ",
            app.file.as_ref().map_or("No file".to_string(), |f| f.display().to_string()),
            app.lines.len()
        )
    };
    let header = vec![
        Line::from(vec![
            Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("[{}]", app.mode.label()), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!(" · saves to: {}", app.output_dir.display()),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(Span::styled(app.key_hints(), Style::default().fg(Color::Yellow))),
    ];
    frame.render_widget(Paragraph::new(header), header_area);
    app.body_area = body_area;

    let position = if app.browsing() {
        render_browser(frame, app, body_area);
        format!(" {} entries ", app.browser_entries.len())
    } else {
        render_file(frame, app, body_area);
        let (sel_start, sel_end) = app.selection();
        format!(" lines {sel_start}-{sel_end} of {} ", app.lines.len())
    };

    // Status bar
    let status = Line::from(vec![
        Span::styled(
            format!(" {} ", app.mode.label()),
            Style::default().fg(Color::Black).bg(Color::Gray),
        ),
        Span::styled(position, Style::default().fg(Color::DarkGray)),
        Span::styled(&app.status, Style::default().fg(Color::Green)),
    ]);
    frame.render_widget(Paragraph::new(status), status_area);

    // Note editor
    if let Some(wrapped) = wrapped_note {
        let input_area = chunks[3];
        // Scroll so the cursor row stays visible.
        let (cursor_row, cursor_col) = wrapped.cursor;
        let scroll = cursor_row.saturating_sub(note_rows - 1);
        let lines: Vec<Line> = wrapped.rows.into_iter().skip(scroll).take(note_rows).map(Line::from).collect();
        let title = format!("Comment on {} — Esc save · Ctrl+Q discard", app.target_label());
        let input = Paragraph::new(lines).block(Block::bordered().title(title));
        frame.render_widget(input, input_area);
        let cursor_x = input_area.x + 1 + cursor_col as u16;
        let cursor_y = input_area.y + 1 + (cursor_row - scroll) as u16;
        frame.set_cursor_position((cursor_x.min(input_area.right().saturating_sub(2)), cursor_y));
    }

    if app.show_help {
        render_help(frame, area);
    }
}

fn render_browser(frame: &mut Frame, app: &mut App, body_area: Rect) {
    // Keep the cursor line visible.
    let visible = body_area.height as usize;
    if app.browser_cursor < app.browser_scroll {
        app.browser_scroll = app.browser_cursor;
    }
    if visible > 0 && app.browser_cursor >= app.browser_scroll + visible {
        app.browser_scroll = app.browser_cursor + 1 - visible;
    }

    let mut body_lines: Vec<Line> = Vec::new();
    if app.browser_entries.is_empty() {
        body_lines.push(Line::from(Span::styled(
            "  (empty directory — h to go up)",
            Style::default().fg(Color::DarkGray),
        )));
    }
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
        let suffix = if entry.is_dir { "/" } else { "" };
        body_lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), style),
            Span::styled(format!("{}{suffix}", entry.name), style),
        ]));
    }
    frame.render_widget(Paragraph::new(body_lines), body_area);
}

fn render_file(frame: &mut Frame, app: &mut App, body_area: Rect) {
    // Keep the cursor line visible.
    let visible = body_area.height as usize;
    if app.cursor < app.scroll {
        app.scroll = app.cursor;
    }
    if visible > 0 && app.cursor >= app.scroll + visible {
        app.scroll = app.cursor + 1 - visible;
    }

    let (sel_start, sel_end) = app.selection();

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
        let highlighted = app.highlight(idx);
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
}

/// Centered popup listing every key (`?`).
fn render_help(frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for (i, (section, keys)) in HELP.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(*section, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
        for (key, action) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<15}"), Style::default().fg(Color::Yellow)),
                Span::raw(*action),
            ]));
        }
    }
    let width = 60.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("Keys — press any key to close")),
        popup,
    );
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

    /// Lines with non-ASCII text before and around every kind of token.
    const NON_ASCII: &[&str] = &[
        "- 16:55 — Started: explored in env",
        "\"ä\" // x → y",
        "ä fn x /* ö */ let",
        "| `j`/`k`, `↑`/`↓` | move |",
        "# Pää — otsikko",
        "> lainaus **lihava ä** ja *kursiivi ö* [linkki ä](https://e.fi/ä) `koodi ä`",
        "\\*ä* *ä",
    ];

    #[test]
    fn highlighters_keep_text_intact_with_non_ascii() {
        // The Rust highlighter used to slice by char index and panic here.
        for line in NON_ASCII {
            let rust: String = syntax_highlight(line).iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(&rust, line);
            let md: String = markdown_highlight(line).iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(&md, line);
        }
    }

    /// The style of the span whose text is exactly `text`.
    fn style_of(spans: &[Span], text: &str) -> Style {
        spans
            .iter()
            .find(|s| s.content == text)
            .unwrap_or_else(|| panic!("no span {text:?} in {spans:?}"))
            .style
    }

    #[test]
    fn markdown_headings_rules_quotes_and_lists() {
        let heading = markdown_highlight("## Usage");
        assert_eq!(heading.len(), 1);
        assert_eq!(heading[0].style.fg, Some(Color::Cyan));
        assert!(heading[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(markdown_highlight("#hashtag")[0].style.fg, None, "needs a space after #");
        assert_eq!(markdown_highlight("---")[0].style.fg, Some(Color::DarkGray));
        assert_eq!(markdown_highlight("* * *")[0].style.fg, Some(Color::DarkGray));

        let item = markdown_highlight("  - item");
        assert_eq!(style_of(&item, "- ").fg, Some(Color::Magenta));
        let numbered = markdown_highlight("12. item");
        assert_eq!(style_of(&numbered, "12. ").fg, Some(Color::Magenta));
        let quote = markdown_highlight("> > - quoted item");
        assert_eq!(style_of(&quote, "> ").fg, Some(Color::DarkGray));
        assert_eq!(style_of(&quote, "- ").fg, Some(Color::Magenta));
        assert_eq!(markdown_highlight("-not a list")[0].style.fg, None);
    }

    #[test]
    fn markdown_inline_code_links_and_emphasis() {
        let spans = markdown_highlight("use `a*b*c` or ``x ` y`` and **bold** and *it* or _it_");
        assert_eq!(style_of(&spans, "`a*b*c`").fg, Some(Color::Yellow), "no emphasis inside code");
        assert_eq!(style_of(&spans, "``x ` y``").fg, Some(Color::Yellow));
        assert!(style_of(&spans, "**bold**").add_modifier.contains(Modifier::BOLD));
        assert!(style_of(&spans, "*it*").add_modifier.contains(Modifier::ITALIC));
        assert!(style_of(&spans, "_it_").add_modifier.contains(Modifier::ITALIC));

        let link = markdown_highlight("see [the docs](https://x.y) and ![logo](a.png)");
        assert_eq!(style_of(&link, "[the docs]").fg, Some(Color::Blue));
        assert_eq!(style_of(&link, "(https://x.y)").fg, Some(Color::DarkGray));
        assert_eq!(style_of(&link, "![logo]").fg, Some(Color::Blue));

        // Not emphasis: snake_case, spaced-out stars, escapes, unclosed markers.
        for line in ["call snake_case_name here", "2 * 3 * 4", "\\*not italic\\*", "*open", "`open", "[a](b"] {
            let spans = markdown_highlight(line);
            assert!(spans.iter().all(|s| s.style == Style::default()), "{line}: {spans:?}");
        }
        let nested = markdown_highlight("*a **b** c*");
        assert!(style_of(&nested, "*a **b** c*").add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn markdown_code_fences() {
        let lines: Vec<String> = [
            "````markdown", "```rs", "fn a() {}", "```", "````", // a 3-tick fence inside a 4-tick block
            "~~~ rust", "let x = 1;", "```", "~~~",              // ``` does not close ~~~
            "```", "# not a heading", "``` trailing", "```",     // a closing fence has no info string
            "text", "    ```", "text",                           // indented 4: not a fence
        ]
        .map(String::from)
        .to_vec();
        use MdLine::*;
        let rust = Code { rust: true };
        let other = Code { rust: false };
        assert_eq!(
            markdown_lines(&lines),
            [
                Fence, other, other, other, Fence,
                Fence, rust, rust, Fence,
                Fence, other, other, Fence,
                Text, Text, Text,
            ]
        );
        assert_eq!(markdown_lines(&["```".to_string(), "unclosed".to_string()]), [Fence, other]);
    }

    #[test]
    fn markdown_files_are_drawn_with_markdown_colors() {
        let text = "# Title\nfn is a word here\n```rs\nfn main() {}\n```\n";
        let mut app = App::new(Path::new("notes.MD"), text, Path::new("out"));
        app.cursor = 4; // keep the cursor's bold off the lines under test
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 10)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        // The body starts on row 2, after a 5-column gutter (marker + 3 digits + space).
        let fg = |row: u16, col: u16| buffer[(5 + col, 2 + row)].fg;
        assert_eq!(fg(0, 2), Color::Cyan, "heading");
        assert_eq!(fg(1, 0), Color::Reset, "`fn` in Markdown text is not a keyword");
        assert_eq!(fg(2, 0), Color::DarkGray, "fence");
        assert_eq!(fg(3, 0), Color::Cyan, "`fn` in a Rust code block is a keyword");

        let rust = App::new(Path::new("main.rs"), text, Path::new("out"));
        assert_eq!(rust.syntax, Syntax::Rust);
        assert!(rust.md_lines.is_empty());
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
        assert!(md.contains("a.rs#L2-3`\n"), "{md}");
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
    fn header_shows_output_dir_and_key_hints_on_their_own_line() {
        let mut app = App::new(Path::new("x.rs"), "a\n", Path::new("my-notes"));
        let (rows, _) = draw(&mut app, 120, 10);
        assert!(rows[0].contains("saves to: my-notes"), "{}", rows[0]);
        assert!(rows[1].starts_with("? help · v paint lines"), "{}", rows[1]);
        assert!(rows[1].contains("b browse"), "{}", rows[1]);
    }

    #[test]
    fn every_mode_shows_key_hints() {
        let dir = temp_dir();
        fs::write(dir.join("a.rs"), "a\n").unwrap();
        let mut app = App::new(&dir.join("a.rs"), "a\n", &dir.join("out"));
        let hint = |app: &mut App| draw(app, 200, 12).0[1].clone();

        assert!(hint(&mut app).contains("f comment file"));
        press(&mut app, KeyCode::Char('v'));
        assert!(hint(&mut app).contains("c comment selection"));
        press(&mut app, KeyCode::Char('c'));
        assert!(hint(&mut app).contains("Esc save · Ctrl+Q discard"));
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('b'));
        let browser = hint(&mut app);
        assert!(browser.contains("Enter open · h up · c comment entry · C comment dir"), "{browser}");
        assert!(browser.contains("Esc back"), "{browser}");

        // Started without a file there is nothing to go back to.
        let mut app = App::new_browser(&dir, &dir.join("out"));
        assert!(!hint(&mut app).contains("Esc back"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn key_hints_fit_in_80_columns() {
        let mut app = App::new(Path::new("x.rs"), "a\n", Path::new("out"));
        for mode in [Mode::Normal, Mode::Visual, Mode::Editing, Mode::FileBrowser] {
            app.mode = mode;
            let width = app.key_hints().chars().count();
            assert!(width <= 80, "{mode:?} hints are {width} columns");
        }
    }

    #[test]
    fn help_screen_lists_all_keys_and_any_key_closes_it() {
        let mut app = App::new(Path::new("x.rs"), "a\n", Path::new("out"));
        press(&mut app, KeyCode::Char('?'));
        let (rows, _) = draw(&mut app, 80, 40);
        let screen = rows.join("\n");
        for (section, keys) in HELP {
            assert!(screen.contains(section), "missing section {section}");
            for (_, action) in *keys {
                assert!(screen.contains(action), "missing {action:?} in help:\n{screen}");
            }
        }
        // The key that closes help is not acted on.
        press(&mut app, KeyCode::Char('q'));
        assert!(!app.show_help);
        assert!(!app.should_quit);
        assert!(!draw(&mut app, 80, 40).0.join("\n").contains("press any key to close"));
    }

    #[test]
    fn browse_from_file_view_and_switch_files() {
        let dir = temp_dir();
        fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        fs::write(dir.join("b.rs"), "fn b() {}\n").unwrap();
        let mut app = App::new(&dir.join("a.rs"), "fn a() {}\n", &dir.join("out"));

        press(&mut app, KeyCode::Char('b'));
        assert!(app.mode == Mode::FileBrowser);
        assert_eq!(app.browser_path, dir);
        assert_eq!(app.browser_entries[app.browser_cursor].name, "a.rs", "open file is selected");

        press(&mut app, KeyCode::Esc); // back to a.rs unchanged
        assert!(app.mode == Mode::Normal);
        assert_eq!(app.lines, vec!["fn a() {}"]);

        press(&mut app, KeyCode::Char('b'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert!(app.mode == Mode::Normal);
        assert_eq!(app.file.as_deref(), Some(dir.join("b.rs").as_path()));
        assert_eq!(app.lines, vec!["fn b() {}"]);
        assert!(!app.should_quit);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn browser_navigation_keys() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("sub/empty")).unwrap();
        fs::write(dir.join("a.rs"), "a\n").unwrap();
        fs::write(dir.join("z.rs"), "z\n").unwrap();
        let mut app = App::new_browser(&dir, Path::new("out"));

        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.browser_entries[app.browser_cursor].name, "z.rs");
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.browser_entries[app.browser_cursor].name, "z.rs", "clamped at the end");
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.browser_entries[app.browser_cursor].name, "sub");

        press(&mut app, KeyCode::Char('l')); // into sub
        assert_eq!(app.browser_path, dir.join("sub"));
        press(&mut app, KeyCode::Right); // into sub/empty
        assert_eq!(app.browser_path, dir.join("sub/empty"));
        assert!(draw(&mut app, 60, 10).0[2].contains("empty directory"));
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.browser_path, dir);
        assert_eq!(app.browser_entries[app.browser_cursor].name, "sub", "cursor on the dir we left");

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('l')); // l does not open files
        assert!(app.mode == Mode::FileBrowser);
        press(&mut app, KeyCode::Esc); // no file open: stays in browser
        assert!(app.mode == Mode::FileBrowser);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn comment_on_directory_from_browser() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.rs"), "a\n").unwrap();
        let out = dir.join("out");
        let mut app = App::new_browser(&dir, &out);

        press(&mut app, KeyCode::Char('c')); // "src" is the first entry
        assert!(app.mode == Mode::Editing);
        type_text(&mut app, "split this module");
        let (rows, _) = draw(&mut app, 100, 20);
        let screen = rows.join("\n");
        assert!(screen.contains("Comment on directory "), "{screen}");
        assert!(screen.contains("src/"), "browser stays visible while writing:\n{screen}");
        press(&mut app, KeyCode::Esc);

        assert!(app.mode == Mode::FileBrowser, "back in the browser after saving");
        let files = saved_files(&out);
        assert_eq!(files.len(), 1);
        let md = fs::read_to_string(&files[0]).unwrap();
        assert!(md.contains("src/`\n"), "{md}");
        assert!(md.contains("split this module"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn comment_on_current_directory_and_selected_file_from_browser() {
        let dir = temp_dir();
        fs::write(dir.join("a.rs"), "a\n").unwrap();
        let out = dir.join("out");
        let mut app = App::new_browser(&dir, &out);

        press(&mut app, KeyCode::Char('C'));
        assert_eq!(app.target, CommentTarget::Path(dir.clone()));
        type_text(&mut app, "whole dir");
        press(&mut app, KeyCode::Esc);

        press(&mut app, KeyCode::Char('c')); // a.rs
        assert_eq!(app.target, CommentTarget::Path(dir.join("a.rs")));
        type_text(&mut app, "whole file");
        press(&mut app, KeyCode::Esc);

        let mut scopes: Vec<String> = saved_files(&out)
            .iter()
            .map(|f| fs::read_to_string(f).unwrap().lines().nth(2).unwrap().to_string())
            .collect();
        scopes.sort();
        assert_eq!(scopes, [format!("- scope: `{}/`", dir.display()), format!("- scope: `{}`", dir.join("a.rs").display())]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f_comments_whole_open_file() {
        let dir = temp_dir();
        let file = dir.join("a.rs");
        fs::write(&file, "a\nb\n").unwrap();
        let out = dir.join("out");
        let mut app = App::new(&file, "a\nb\n", &out);

        press(&mut app, KeyCode::Char('f'));
        assert!(draw(&mut app, 100, 20).0.join("\n").contains("Comment on file "));
        type_text(&mut app, "rewrite");
        press(&mut app, KeyCode::Esc);

        assert!(app.mode == Mode::Normal);
        let md = fs::read_to_string(&saved_files(&out)[0]).unwrap();
        assert!(md.contains(&format!("- scope: `{}`\n", file.display())), "{md}");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn discarding_a_selection_comment_keeps_painting() {
        let mut app = App::new(Path::new("x.rs"), "a\nb\nc\n", Path::new("out"));
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('c'));
        assert!(draw(&mut app, 100, 20).0.join("\n").contains("Comment on lines 1-2 of x.rs"));
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(app.mode == Mode::Visual);
        assert_eq!(app.selection(), (1, 2));
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

    // --- Mouse: `draw` first so the app knows where the body is. ---

    fn mouse(app: &mut App, kind: MouseEventKind, row: u16) {
        app.handle_mouse(MouseEvent { kind, column: 8, row, modifiers: KeyModifiers::NONE });
    }

    fn numbered_lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn dragging_paints_lines_and_the_comment_saves_them_as_context() {
        let dir = temp_dir();
        let file = dir.join("a.rs");
        let contents = numbered_lines(10);
        fs::write(&file, &contents).unwrap();
        let out = dir.join("out");
        let mut app = App::new(&file, &contents, &out);
        draw(&mut app, 80, 12); // body is rows 2..=10

        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 3); // line 2
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 4);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 5); // line 4
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 5);
        assert!(app.mode == Mode::Visual);
        assert_eq!(app.selection(), (2, 4));

        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 8); // button released: no effect
        assert_eq!(app.selection(), (2, 4));

        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "dedupe");
        press(&mut app, KeyCode::Esc);
        let md = fs::read_to_string(&saved_files(&out)[0]).unwrap();
        assert!(md.contains("a.rs#L2-4`\n"), "{md}");
        assert!(md.contains("## Context\n\n```rs\nline 2\nline 3\nline 4\n```\n\n## Comment\n\ndedupe\n"), "{md}");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn click_moves_cursor_and_stops_painting() {
        let mut app = App::new(Path::new("x.rs"), &numbered_lines(5), Path::new("out"));
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('j'));
        draw(&mut app, 80, 12);

        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 5); // line 4
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 5);
        assert!(app.mode == Mode::Normal);
        assert_eq!(app.anchor, None);
        assert_eq!(app.cursor, 3);

        // Below the last line, in the header and in the status bar: ignored.
        for row in [8, 0, 11] {
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), row);
            assert_eq!(app.cursor, 3, "row {row}");
        }
    }

    #[test]
    fn wheel_scrolls_the_file_and_keeps_the_cursor_on_screen() {
        let mut app = App::new(Path::new("x.rs"), &numbered_lines(50), Path::new("out"));
        draw(&mut app, 80, 12); // 9 body rows

        mouse(&mut app, MouseEventKind::ScrollDown, 4);
        assert_eq!((app.scroll, app.cursor), (3, 3), "cursor pulled onto the screen");
        assert!(draw(&mut app, 80, 12).0[2].contains("  4 line 4"));

        for _ in 0..30 {
            mouse(&mut app, MouseEventKind::ScrollDown, 4);
        }
        assert_eq!(app.scroll, 41, "stops with the last line at the bottom");
        assert_eq!(app.cursor, 41);
        assert!(draw(&mut app, 80, 12).0[10].contains(" 50 line 50"));

        mouse(&mut app, MouseEventKind::ScrollUp, 4);
        assert_eq!((app.scroll, app.cursor), (38, 41), "cursor still visible: stays");
        draw(&mut app, 80, 12);
        assert_eq!(app.scroll, 38, "render keeps the wheel position");
    }

    #[test]
    fn dragging_past_the_bottom_scrolls_while_painting() {
        let mut app = App::new(Path::new("x.rs"), &numbered_lines(50), Path::new("out"));
        draw(&mut app, 80, 12);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 2); // line 1
        for _ in 0..3 {
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 11); // status bar
            draw(&mut app, 80, 12);
        }
        assert_eq!(app.selection(), (1, 12));
        assert_eq!(app.scroll, 3);
    }

    #[test]
    fn wheel_while_painting_extends_the_selection() {
        let mut app = App::new(Path::new("x.rs"), &numbered_lines(50), Path::new("out"));
        draw(&mut app, 80, 12);
        press(&mut app, KeyCode::Char('v'));
        mouse(&mut app, MouseEventKind::ScrollDown, 4);
        assert!(app.mode == Mode::Visual);
        assert_eq!(app.selection(), (1, 4));
    }

    #[test]
    fn browser_click_selects_then_opens_and_wheel_scrolls() {
        let dir = temp_dir();
        fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        fs::write(dir.join("b.rs"), "fn b() {}\n").unwrap();
        let mut app = App::new_browser(&dir, Path::new("out"));
        draw(&mut app, 80, 12);

        mouse(&mut app, MouseEventKind::ScrollDown, 4); // fewer entries than rows
        assert_eq!((app.browser_scroll, app.browser_cursor), (0, 0));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 6); // past the entries
        assert_eq!(app.browser_cursor, 0);

        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 3);
        assert!(app.mode == Mode::FileBrowser);
        assert_eq!(app.browser_entries[app.browser_cursor].name, "b.rs");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 3);
        assert!(app.mode == Mode::Normal);
        assert_eq!(app.lines, vec!["fn b() {}"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mouse_is_ignored_while_editing_and_click_closes_help() {
        let mut app = App::new(Path::new("x.rs"), &numbered_lines(50), Path::new("out"));
        press(&mut app, KeyCode::Char('c'));
        draw(&mut app, 80, 20);
        mouse(&mut app, MouseEventKind::ScrollDown, 4);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 5);
        assert!(app.mode == Mode::Editing);
        assert_eq!((app.scroll, app.cursor), (0, 0));

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('?'));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 5);
        assert!(!app.show_help);
        assert_eq!(app.cursor, 0, "the click that closes help is not acted on");
    }
}
