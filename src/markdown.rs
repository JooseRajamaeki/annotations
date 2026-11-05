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

/// Colorize a line of Markdown text (not inside a code block): headings, rules,
/// quote and list markers, then inline code, links, bold and italic.
pub fn markdown_highlight(line: &str) -> Vec<Span<'_>> {
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
}
