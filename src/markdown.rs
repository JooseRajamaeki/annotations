use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Where a Markdown line sits relative to fenced code blocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MdLine {
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
pub fn markdown_lines(lines: &[String]) -> Vec<MdLine> {
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

/// The level of a heading line (with its indentation removed): 1 to 6 `#`s,
/// then a space or nothing.
fn heading_level(trimmed: &str) -> Option<usize> {
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    let rest = &trimmed[hashes..];
    ((1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with(' '))).then_some(hashes)
}

/// A thematic break: three or more `-`, `*` or `_`, maybe with spaces between.
fn is_rule(trimmed: &str) -> bool {
    let compact: String = trimmed.chars().filter(|c| *c != ' ').collect();
    compact.len() >= 3 && ['-', '*', '_'].iter().any(|&c| compact.trim_start_matches(c).is_empty())
}

/// Byte length of the list marker (with its space) that `rest` starts with.
fn list_marker(rest: &str) -> Option<usize> {
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") {
        Some(2)
    } else if (1..=9).contains(&digits) && (rest[digits..].starts_with(". ") || rest[digits..].starts_with(") ")) {
        Some(digits + 2)
    } else {
        None
    }
}

/// Colorize a line of Markdown text (not inside a code block): headings, rules,
/// quote and list markers, then inline code, links, bold and italic.
pub fn markdown_highlight(line: &str) -> Vec<Span<'_>> {
    let marker = Style::default().fg(Color::Magenta);
    let dim = Style::default().fg(Color::DarkGray);
    let trimmed = line.trim_start();
    if heading_level(trimmed).is_some() {
        return vec![Span::styled(line, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))];
    }
    if is_rule(trimmed) {
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
    if let Some(len) = list_marker(&line[pos..]) {
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

/// A code span opening with the backtick run at byte `pos`: its run length
/// and end (exclusive). `Err(run)` when no run of the same length closes it.
fn code_span(line: &str, pos: usize) -> Result<(usize, usize), usize> {
    let run = line[pos..].len() - line[pos..].trim_start_matches('`').len();
    let fence = &line[pos..pos + run];
    let mut from = pos + run;
    while let Some(i) = line[from..].find(fence) {
        let at = from + i;
        let len = line[at..].len() - line[at..].trim_start_matches('`').len();
        if len == run {
            return Ok((run, at + run));
        }
        from = at + len;
    }
    Err(run)
}

/// A link `[text](url)` or image `![alt](url)` at byte `pos`: the byte of its
/// `[`, of its `(` and its end (exclusive).
fn link_at(line: &str, pos: usize) -> Option<(usize, usize, usize)> {
    let text_start = if line[pos..].starts_with("![") { pos + 1 } else { pos };
    if !line[text_start..].starts_with('[') {
        return None;
    }
    let url_start = text_start + line[text_start..].find("](")? + 1;
    let end = url_start + line[url_start..].find(')')? + 1;
    Some((text_start, url_start, end))
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
            b'`' => match code_span(line, pos) {
                Ok((_, end)) => Some((end, vec![(end, code)])),
                Err(run) => {
                    pos += run;
                    continue;
                }
            },
            b'[' | b'!' => link_at(line, pos).map(|(_, url_start, end)| (end, vec![(url_start, link), (end, dim)])),
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

// Rendering keeps one screen line per source line (it may still wrap), so
// line numbers, painting and saved scopes refer to the file as written.
// Paragraphs are therefore not reflowed and setext headings are not joined.

/// Rendered code blocks are indented so they stand apart from the text.
pub const CODE_INDENT: &str = "  ";

/// Alignment of a table column, from its delimiter row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Align {
    Left,
    Center,
    Right,
}

/// Columns of one table, shared by its rows so the cells line up.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Table {
    widths: Vec<usize>,
    aligns: Vec<Align>,
}

/// The table line `idx` belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct TableRow {
    table: usize,
    delimiter: bool,
}

/// What each line of a Markdown file is, for highlighting and rendering it.
#[derive(Default)]
pub struct Markdown {
    kinds: Vec<MdLine>,
    table_rows: Vec<Option<TableRow>>,
    tables: Vec<Table>,
}

impl Markdown {
    pub fn parse(lines: &[String]) -> Markdown {
        let kinds = markdown_lines(lines);
        let mut table_rows = vec![None; lines.len()];
        let mut tables = Vec::new();
        let text = |k: usize| kinds.get(k) == Some(&MdLine::Text);
        let mut i = 0;
        while i + 1 < lines.len() {
            let aligns = (text(i) && text(i + 1)).then(|| delimiter_row(&lines[i + 1])).flatten();
            let Some(aligns) = aligns.filter(|a| split_cells(&lines[i]).len() == a.len()) else {
                i += 1;
                continue;
            };
            let mut end = i + 2;
            while text(end) && lines[end].contains('|') {
                end += 1;
            }
            let mut widths = vec![1; aligns.len()];
            for line in (i..end).filter(|&k| k != i + 1).map(|k| &lines[k]) {
                for (width, cell) in widths.iter_mut().zip(split_cells(line)) {
                    *width = (*width).max(inline_width(cell));
                }
            }
            for (k, row) in table_rows.iter_mut().enumerate().take(end).skip(i) {
                *row = Some(TableRow { table: tables.len(), delimiter: k == i + 1 });
            }
            tables.push(Table { widths, aligns });
            i = end;
        }
        Markdown { kinds, table_rows, tables }
    }

    /// Where line `idx` sits relative to code blocks; `None` for other files.
    pub fn kind(&self, idx: usize) -> Option<MdLine> {
        self.kinds.get(idx).copied()
    }

    /// Line `idx` (`line`) rendered for reading, `width` columns wide. `None`
    /// for code block lines, which are drawn as they are.
    pub fn render(&self, idx: usize, line: &str, width: usize) -> Option<Vec<Span<'static>>> {
        let dim = Style::default().fg(Color::DarkGray);
        if let Some(row) = self.table_rows.get(idx).copied().flatten() {
            return Some(render_table_row(line, &self.tables[row.table], row.delimiter));
        }
        match self.kind(idx)? {
            MdLine::Text => Some(render_text(line, width)),
            // The opening fence shows the language; the closing one is blank.
            MdLine::Fence => Some(vec![Span::styled(format!("{CODE_INDENT}{}", code_fence(line).map_or("", |f| f.2)), dim)]),
            MdLine::Code { .. } => None,
        }
    }
}

/// The cells of a table row, without the outer pipes. An escaped `\|` does
/// not split cells.
fn split_cells(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = match t.strip_suffix('|') {
        Some(inner) if !inner.ends_with('\\') => inner,
        _ => t,
    };
    let bytes = t.as_bytes();
    let mut cells = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'|' => {
                cells.push(t[start..i].trim());
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    cells.push(t[start..].trim());
    cells
}

/// The column alignments of a table delimiter row such as `|:--|--:|`.
/// It needs a pipe, so that `---` under a line stays a rule.
fn delimiter_row(line: &str) -> Option<Vec<Align>> {
    if !line.contains('|') {
        return None;
    }
    split_cells(line)
        .into_iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.len() > 1 && cell.ends_with(':');
            let dashes = cell.trim_start_matches(':').trim_end_matches(':');
            if dashes.is_empty() || !dashes.chars().all(|c| c == '-') {
                return None;
            }
            Some(match (left, right) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            })
        })
        .collect()
}

fn inline_width(text: &str) -> usize {
    let mut spans = Vec::new();
    render_inline(text, Style::default(), &mut spans);
    spans.iter().map(Span::width).sum()
}

fn render_table_row(line: &str, table: &Table, delimiter: bool) -> Vec<Span<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    if delimiter {
        let rule: Vec<String> = table.widths.iter().map(|&w| "─".repeat(w)).collect();
        return vec![Span::styled(rule.join("─┼─"), dim)];
    }
    let cells = split_cells(line);
    let mut spans = Vec::new();
    for (col, (&width, &align)) in table.widths.iter().zip(&table.aligns).enumerate() {
        if col > 0 {
            spans.push(Span::styled(" │ ", dim));
        }
        let mut cell = Vec::new();
        render_inline(cells.get(col).copied().unwrap_or(""), Style::default(), &mut cell);
        let pad = width.saturating_sub(cell.iter().map(Span::width).sum());
        let before = match align {
            Align::Left => 0,
            Align::Center => pad / 2,
            Align::Right => pad,
        };
        spans.push(Span::raw(" ".repeat(before)));
        spans.extend(cell);
        spans.push(Span::raw(" ".repeat(pad - before)));
    }
    spans
}

/// A heading of `level` 1 to 6. A terminal cannot change the font size, so
/// the levels differ in how much of the row they claim: H1 is a full-width
/// bar, H2 and H3 are followed by a heavy and a thin line, and H4 to H6 fade
/// from colored to plain to dim.
fn render_heading(level: usize, content: &str, width: usize) -> Vec<Span<'static>> {
    let cyan = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    // Reversed rather than a background color, so the bar stays readable
    // when the cursor line's background is drawn over it.
    let bar = cyan.add_modifier(Modifier::REVERSED);
    let style = match level {
        1 => bar,
        2..=4 => cyan,
        5 => Style::default().add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
    };
    let mut spans = Vec::new();
    if level == 1 {
        spans.push(Span::styled(" ", bar));
    }
    render_inline(content, style, &mut spans);
    let used: usize = spans.iter().map(Span::width).sum();
    // The fill only goes on headings that fit on one row; on a wrapped one it
    // would add a row of its own.
    let fill = width.saturating_sub(used + 1);
    match level {
        1 => spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), bar)),
        2 if fill > 0 => spans.push(Span::styled(format!(" {}", "━".repeat(fill)), cyan)),
        3 if fill > 0 => spans.push(Span::styled(format!(" {}", "─".repeat(fill)), Style::default().fg(Color::DarkGray))),
        _ => {}
    }
    spans
}

/// A line of Markdown text (not in a code block or a table) with its markup
/// replaced by styles: headings lose their `#`s, bullets become `•`, quotes a
/// bar, rules span `width` columns.
fn render_text(line: &str, width: usize) -> Vec<Span<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let trimmed = line.trim_start();
    if let Some(level) = heading_level(trimmed) {
        let content = trimmed[level..].trim();
        // An optional closing run of `#`s is not part of the heading.
        let unclosed = content.trim_end_matches('#');
        let content = if unclosed.is_empty() || unclosed.ends_with(' ') { unclosed.trim_end() } else { content };
        return render_heading(level, content, width);
    }
    if is_rule(trimmed) {
        return vec![Span::styled("─".repeat(width), dim)];
    }

    let mut spans = Vec::new();
    let mut pos = line.len() - trimmed.len();
    if pos > 0 {
        spans.push(Span::raw(line[..pos].to_string()));
    }
    while line[pos..].starts_with('>') {
        spans.push(Span::styled("│ ", dim));
        pos += if line[pos + 1..].starts_with(' ') { 2 } else { 1 };
    }
    if let Some(len) = list_marker(&line[pos..]) {
        let marker = &line[pos..pos + len];
        let bullet = if len == 2 && !marker.starts_with(|c: char| c.is_ascii_digit()) { "• " } else { marker };
        spans.push(Span::styled(bullet.to_string(), Style::default().fg(Color::Magenta)));
        pos += len;
    }
    render_inline(&line[pos..], Style::default(), &mut spans);
    spans
}

/// Inline Markdown in `text` with its markup hidden: code spans, links,
/// images, autolinks, emphasis, strikethrough and escapes. Markup nests, so
/// `**see `x`**` is bold with `x` as code.
fn render_inline(text: &str, base: Style, spans: &mut Vec<Span<'static>>) {
    let code = base.patch(Style::default().fg(Color::Yellow));
    let link = base.patch(Style::default().fg(Color::Blue).add_modifier(Modifier::UNDERLINED));
    let dim = base.patch(Style::default().fg(Color::DarkGray));
    let bytes = text.as_bytes();
    let mut pos = 0;
    // Start of the plain text not yet pushed.
    let mut plain = 0;
    let flush = |spans: &mut Vec<Span<'static>>, to: usize, plain: usize| {
        if plain < to {
            spans.push(Span::styled(text[plain..to].to_string(), base));
        }
    };
    while pos < text.len() {
        // Every marker is ASCII, so `pos` is a char boundary whenever it matches.
        match bytes[pos] {
            b'\\' if bytes.get(pos + 1).is_some_and(u8::is_ascii_punctuation) => {
                flush(spans, pos, plain);
                // The escaped char starts the next plain text and is not markup.
                plain = pos + 1;
                pos += 2;
            }
            b'`' => match code_span(text, pos) {
                Ok((run, end)) => {
                    flush(spans, pos, plain);
                    let inner = &text[pos + run..end - run];
                    // CommonMark strips one space on each side, so `` ` `` can show a backtick.
                    let inner = match inner.strip_prefix(' ').and_then(|i| i.strip_suffix(' ')) {
                        Some(stripped) if !inner.trim().is_empty() => stripped,
                        _ => inner,
                    };
                    spans.push(Span::styled(inner.to_string(), code));
                    (pos, plain) = (end, end);
                }
                Err(run) => pos += run,
            },
            b'[' | b'!' => match link_at(text, pos) {
                Some((text_start, url_start, end)) => {
                    flush(spans, pos, plain);
                    if text_start > pos {
                        spans.push(Span::styled("▣ ", dim));
                    }
                    render_inline(&text[text_start + 1..url_start - 1], link, spans);
                    (pos, plain) = (end, end);
                }
                None => pos += 1,
            },
            b'<' => {
                let url = text[pos + 1..].find('>').map(|i| &text[pos + 1..pos + 1 + i]);
                match url.filter(|u| (u.starts_with("http://") || u.starts_with("https://")) && !u.contains(' ')) {
                    Some(url) => {
                        flush(spans, pos, plain);
                        spans.push(Span::styled(url.to_string(), link));
                        pos += url.len() + 2;
                        plain = pos;
                    }
                    None => pos += 1,
                }
            }
            b'*' | b'_' | b'~' => {
                let doubled = bytes.get(pos + 1) == Some(&bytes[pos]);
                let delim = &text[pos..pos + if doubled { 2 } else { 1 }];
                let modifier = match delim {
                    "~~" => Some(Modifier::CROSSED_OUT),
                    "**" | "__" => Some(Modifier::BOLD),
                    "*" | "_" => Some(Modifier::ITALIC),
                    _ => None,
                };
                match modifier.and_then(|m| emphasis_end(text, pos, delim).map(|end| (m, end))) {
                    Some((modifier, end)) => {
                        flush(spans, pos, plain);
                        let inner = &text[pos + delim.len()..end - delim.len()];
                        render_inline(inner, base.add_modifier(modifier), spans);
                        (pos, plain) = (end, end);
                    }
                    None => pos += delim.len(),
                }
            }
            _ => pos += 1,
        }
    }
    flush(spans, text.len(), plain);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The text of `spans`, joined.
    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn render_line(line: &str) -> Vec<Span<'static>> {
        let doc = Markdown::parse(&[line.to_string()]);
        doc.render(0, line, 20).unwrap()
    }

    fn inline(line: &str) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        render_inline(line, Style::default(), &mut spans);
        spans
    }

    #[test]
    fn rendered_headings_rules_quotes_and_lists_lose_their_markup() {
        assert_eq!(text(&render_line("#### Usage ##")), "Usage", "the closing #s go too");
        assert_eq!(text(&render_line("#### C#")), "C#", "a # inside a word stays");
        assert_eq!(text(&render_line("#hashtag")), "#hashtag");

        assert_eq!(text(&render_line("---")), "─".repeat(20), "a rule spans the width");
        assert_eq!(text(&render_line("  - item")), "  • item");
        assert_eq!(style_of(&render_line("* item"), "• ").fg, Some(Color::Magenta));
        assert_eq!(text(&render_line("12. item")), "12. item");
        assert_eq!(text(&render_line("> > - quoted")), "│ │ • quoted");
    }

    #[test]
    fn each_heading_level_looks_different() {
        // Rendered 20 columns wide.
        let shown: Vec<String> = (1..=6).map(|n| text(&render_line(&format!("{} Title", "#".repeat(n))))).collect();
        assert_eq!(
            shown,
            [
                " Title              ".to_string(),
                format!("Title {}", "━".repeat(14)),
                format!("Title {}", "─".repeat(14)),
                "Title".to_string(),
                "Title".to_string(),
                "Title".to_string(),
            ]
        );
        let style = |n: usize| style_of(&render_line(&format!("{} Title", "#".repeat(n))), "Title");
        let styles: Vec<Style> = (1..=6).map(style).collect();
        assert!(styles[0].add_modifier.contains(Modifier::REVERSED), "H1 is a bar");
        for (i, a) in styles.iter().enumerate() {
            assert!(a.add_modifier.contains(Modifier::BOLD) || i == 5, "H{} is bold", i + 1);
        }
        assert_eq!(styles[3].fg, Some(Color::Cyan));
        assert_eq!(styles[4].fg, None, "H5 is plain bold");
        assert!(styles[5].add_modifier.contains(Modifier::ITALIC) && styles[5].fg == Some(Color::DarkGray));

        // A heading too long for one row gets no line, which would take a row of its own.
        let long = render_line("## A heading longer than the row");
        assert_eq!(text(&long), "A heading longer than the row");
    }

    #[test]
    fn rendered_inline_markup_is_hidden_and_styled() {
        let spans = inline("use `a*b*c` or `` x ` y `` and **bold** and *it* and ~~old~~");
        assert_eq!(text(&spans), "use a*b*c or x ` y and bold and it and old");
        assert_eq!(style_of(&spans, "a*b*c").fg, Some(Color::Yellow), "no emphasis inside code");
        assert_eq!(style_of(&spans, "x ` y").fg, Some(Color::Yellow));
        assert!(style_of(&spans, "bold").add_modifier.contains(Modifier::BOLD));
        assert!(style_of(&spans, "it").add_modifier.contains(Modifier::ITALIC));
        assert!(style_of(&spans, "old").add_modifier.contains(Modifier::CROSSED_OUT));

        let links = inline("see [the docs](https://x.y), ![logo](a.png) and <https://a.b>");
        assert_eq!(text(&links), "see the docs, ▣ logo and https://a.b");
        assert_eq!(style_of(&links, "the docs").fg, Some(Color::Blue));
        assert_eq!(style_of(&links, "logo").fg, Some(Color::Blue));
        assert_eq!(style_of(&links, "https://a.b").fg, Some(Color::Blue));

        // Markup nests: code in bold keeps both styles, a link's text can be bold.
        let nested = inline("**see `x`** and [**b**](u)");
        assert_eq!(text(&nested), "see x and b");
        let code = style_of(&nested, "x");
        assert_eq!(code.fg, Some(Color::Yellow));
        assert!(code.add_modifier.contains(Modifier::BOLD));
        assert!(style_of(&nested, "b").add_modifier.contains(Modifier::BOLD | Modifier::UNDERLINED));

        // Not markup: snake_case, spaced-out stars, escapes, unclosed markers.
        for (line, shown) in [
            ("call snake_case_name here", "call snake_case_name here"),
            ("2 * 3 * 4", "2 * 3 * 4"),
            ("\\*not italic\\* \\d", "*not italic* \\d"),
            ("*open `open [a](b <c>", "*open `open [a](b <c>"),
        ] {
            let spans = inline(line);
            assert_eq!(text(&spans), shown);
            assert!(spans.iter().all(|s| s.style == Style::default()), "{line}: {spans:?}");
        }
    }

    #[test]
    fn rendering_keeps_non_ascii_text() {
        let spans = render_line("- ä **ö** `å` [ü](x) _é_ ~~ß~~");
        assert_eq!(text(&spans), "• ä ö å ü é ß");
    }

    #[test]
    fn code_blocks_are_left_to_the_caller_and_fences_show_the_language() {
        let lines: Vec<String> = ["```rs", "# not a heading", "```"].map(String::from).to_vec();
        let doc = Markdown::parse(&lines);
        assert_eq!(text(&doc.render(0, &lines[0], 20).unwrap()), format!("{CODE_INDENT}rs"));
        assert!(doc.render(1, &lines[1], 20).is_none());
        assert_eq!(text(&doc.render(2, &lines[2], 20).unwrap()), CODE_INDENT);
        assert!(Markdown::default().render(0, "# x", 20).is_none(), "not a Markdown file");
    }

    fn rendered(lines: &[&str]) -> Vec<String> {
        let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        let doc = Markdown::parse(&lines);
        (0..lines.len()).map(|i| text(&doc.render(i, &lines[i], 10).unwrap())).collect()
    }

    #[test]
    fn tables_line_up_their_columns() {
        let table = rendered(&[
            "| Key | Action | n |",
            "|:---|:---:|--:|",
            "| `j` | move | 1 |",
            "| a \\| b | **go** | 22 |",
            "| short |",
            "",
            "| not | a table |",
        ]);
        assert_eq!(
            table,
            [
                "Key   │ Action │  n",
                "──────┼────────┼───",
                "j     │  move  │  1",
                "a | b │   go   │ 22",
                "short │        │   ",
                "",
                "| not | a table |",
            ]
        );
    }

    #[test]
    fn only_real_delimiter_rows_start_a_table() {
        // `---` under a line is a rule (or a heading), not a one-column table.
        assert_eq!(rendered(&["a | b", "---"])[1], "─".repeat(10));
        assert_eq!(rendered(&["a | b", "-|x"]), ["a | b", "-|x"]);
        assert_eq!(rendered(&["a | b | c", "--|--"])[0], "a | b | c", "cell counts differ");
        let fenced: Vec<String> = ["```", "a | b", "--|--", "```"].map(String::from).to_vec();
        assert!(Markdown::parse(&fenced).render(1, &fenced[1], 10).is_none(), "tables in code stay code");
    }
}
