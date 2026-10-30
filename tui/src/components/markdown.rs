//! An agent's Markdown as terminal lines, for the feed: headings bold,
//! emphasis italic, strong bold, strikethrough crossed out, inline code and
//! code blocks in the code colour, lists with bullets or numbers (nested,
//! with task boxes), quotes behind a bar, rules, tables as aligned columns,
//! and links as their text with the address after it when that differs.
//! Raw HTML shows as the text it is; an image as `[image: alt]`. Everything
//! is wrapped to a width in a base style, and the text is stripped of what a
//! terminal would act on before it's parsed.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::feed::plain;
use crate::layout::str_width;
use crate::theme;

/// The widest a rule is drawn.
const RULE: usize = 40;
/// The narrowest a table's column is cut to.
const MIN_COLUMN: usize = 3;

/// `text` (Markdown) as lines of at most `width` columns in `base`.
pub fn render(text: &str, width: usize, base: Style) -> Vec<Line<'static>> {
    let clean = plain(text);
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::new(width.max(8), base);
    for event in Parser::new_ext(&clean, options) {
        builder.event(event);
    }
    builder.finish()
}

/// A list being drawn: the number of its next item, when it's ordered, and
/// how wide its current item's marker is.
struct List {
    next: Option<u64>,
    indent: usize,
}

/// A table being collected: its rows of cells (plain text) and how many of
/// them are its head.
#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    head: usize,
    cell: Option<String>,
}

enum Token {
    Word(String, Style),
    Space,
    Break,
}

struct Builder {
    width: usize,
    base: Style,
    lines: Vec<Line<'static>>,
    /// The current block's text, as styled pieces ("\n" a hard break).
    inline: Vec<(String, Style)>,
    styles: Vec<Style>,
    quote: usize,
    lists: Vec<List>,
    /// The current item's marker, until its first line is drawn.
    marker: Option<String>,
    code: Option<String>,
    links: Vec<(String, usize)>,
    table: Option<Table>,
}

impl Builder {
    fn new(width: usize, base: Style) -> Self {
        Self {
            width,
            base,
            lines: Vec::new(),
            inline: Vec::new(),
            styles: vec![base],
            quote: 0,
            lists: Vec::new(),
            marker: None,
            code: None,
            links: Vec::new(),
            table: None,
        }
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or(self.base)
    }

    fn push_style(&mut self, change: impl FnOnce(Style) -> Style) {
        let next = change(self.style());
        self.styles.push(next);
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    fn code_style(&self) -> Style {
        self.base.fg(theme::palette().accent_orange)
    }

    fn muted(&self) -> Style {
        Style::default().fg(theme::palette().text_muted)
    }

    fn text(&mut self, text: &str) {
        if let Some(code) = self.code.as_mut() {
            code.push_str(text);
            return;
        }
        if let Some(cell) = self.table.as_mut().and_then(|table| table.cell.as_mut()) {
            cell.push_str(text);
            return;
        }
        let style = self.style();
        self.inline.push((text.to_owned(), style));
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) | Event::InlineMath(code) => {
                if let Some(cell) = self.table.as_mut().and_then(|table| table.cell.as_mut()) {
                    cell.push_str(&code);
                } else {
                    let style = self.code_style();
                    self.inline.push((code.to_string(), style));
                }
            }
            Event::DisplayMath(math) => {
                self.flush();
                self.code_block(&math);
            }
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
            Event::FootnoteReference(label) => self.text(&format!("[^{label}]")),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.text("\n"),
            Event::Rule => {
                self.flush();
                self.gap();
                let rule = "─".repeat(self.width.saturating_sub(self.prefix_width()).min(RULE));
                let mut spans = self.prefix(false);
                spans.push(Span::styled(rule, self.muted()));
                self.lines.push(Line::from(spans));
            }
            Event::TaskListMarker(done) => {
                let style = self.style();
                self.inline
                    .push((if done { "☑ " } else { "☐ " }.to_owned(), style));
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                if self.lists.is_empty() {
                    self.gap();
                }
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.gap();
                let p = theme::palette();
                self.push_style(|style| {
                    let style = style.add_modifier(Modifier::BOLD).fg(p.text_bright);
                    if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                        style.add_modifier(Modifier::UNDERLINED)
                    } else {
                        style
                    }
                });
            }
            Tag::BlockQuote(_) => {
                self.flush();
                if self.quote == 0 {
                    self.gap();
                }
                self.quote += 1;
                let muted = self.muted();
                self.push_style(|style| style.patch(muted).add_modifier(Modifier::ITALIC));
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                if self.lists.is_empty() {
                    self.gap();
                }
                let _ = matches!(kind, CodeBlockKind::Fenced(_));
                self.code = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                if self.lists.is_empty() {
                    self.gap();
                }
                self.lists.push(List {
                    next: start,
                    indent: 2,
                });
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len();
                if let Some(list) = self.lists.last_mut() {
                    let marker = match list.next.as_mut() {
                        Some(number) => {
                            let marker = format!("{number}. ");
                            *number += 1;
                            marker
                        }
                        None => match depth {
                            1 => "• ".to_owned(),
                            2 => "◦ ".to_owned(),
                            _ => "▪ ".to_owned(),
                        },
                    };
                    list.indent = str_width(&marker);
                    self.marker = Some(marker);
                }
            }
            Tag::Table(_) => {
                self.flush();
                self.gap();
                self.table = Some(Table::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.cell = Some(String::new());
                }
            }
            Tag::Emphasis => self.push_style(|style| style.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(|style| style.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(|style| style.add_modifier(Modifier::CROSSED_OUT))
            }
            Tag::Link { dest_url, .. } => {
                let p = theme::palette();
                self.links.push((dest_url.to_string(), self.inline.len()));
                self.push_style(|style| style.fg(p.label_blue).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { .. } => self.text("[image: "),
            Tag::FootnoteDefinition(label) => {
                self.flush();
                self.gap();
                self.text(&format!("[^{label}]: "));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.pop_style();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
                self.pop_style();
            }
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                self.code_block(&code);
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Item => self.flush(),
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    let cell = table.cell.take().unwrap_or_default();
                    if let Some(row) = table.rows.last_mut() {
                        row.push(cell.trim().to_owned());
                    }
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.head = table.rows.len();
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.draw_table(table);
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                if let Some((url, from)) = self.links.pop() {
                    let label: String = self.inline[from.min(self.inline.len())..]
                        .iter()
                        .map(|(text, _)| text.as_str())
                        .collect();
                    if !url.is_empty() && label.trim() != url.trim() {
                        let muted = self.muted();
                        self.inline.push((format!(" <{url}>"), muted));
                    }
                }
            }
            TagEnd::Image => self.text("]"),
            TagEnd::FootnoteDefinition => self.flush(),
            _ => {}
        }
    }

    /// A blank line between blocks (behind the quote's bar inside one).
    fn gap(&mut self) {
        if self.lines.is_empty() {
            return;
        }
        if self.lines.last().is_some_and(|line| line.width() == 0) {
            return;
        }
        let spans = self.quote_bars();
        self.lines.push(Line::from(spans));
    }

    fn quote_bars(&self) -> Vec<Span<'static>> {
        (0..self.quote)
            .map(|_| Span::styled("▎ ", self.muted()))
            .collect()
    }

    fn prefix_width(&self) -> usize {
        self.quote * 2 + self.lists.iter().map(|list| list.indent).sum::<usize>()
    }

    /// What a line starts with: the quotes' bars, then the lists' indents;
    /// on an item's first line (`first`), its marker.
    fn prefix(&mut self, first: bool) -> Vec<Span<'static>> {
        let mut spans = self.quote_bars();
        let depth = self.lists.len();
        for (level, list) in self.lists.iter().enumerate() {
            if level + 1 == depth && first {
                if let Some(marker) = self.marker.take() {
                    let colour = theme::palette().accent_primary;
                    spans.push(Span::styled(marker, Style::default().fg(colour)));
                    continue;
                }
            }
            spans.push(Span::raw(" ".repeat(list.indent)));
        }
        spans
    }

    /// Draw the current block's text, wrapped.
    fn flush(&mut self) {
        let pieces = std::mem::take(&mut self.inline);
        if pieces.iter().all(|(text, _)| text.trim().is_empty()) {
            return;
        }
        let mut tokens = Vec::new();
        for (text, style) in pieces {
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    tokens.push(Token::Break);
                }
                let mut word = String::new();
                for c in line.chars() {
                    if c == ' ' || c == '\t' {
                        if !word.is_empty() {
                            tokens.push(Token::Word(std::mem::take(&mut word), style));
                        }
                        tokens.push(Token::Space);
                    } else {
                        word.push(c);
                    }
                }
                if !word.is_empty() {
                    tokens.push(Token::Word(word, style));
                }
            }
        }
        let mut line = self.prefix(true);
        let start = self.prefix_width();
        let room = self.width.saturating_sub(start).max(4);
        let mut used = 0;
        let mut space = false;
        let mut lines = Vec::new();
        let mut open_line = |line: &mut Vec<Span<'static>>, builder: &mut Self| {
            let next = builder.prefix(false);
            lines.push(Line::from(std::mem::replace(line, next)));
        };
        for token in tokens {
            match token {
                Token::Space => space = used > 0,
                Token::Break => {
                    open_line(&mut line, self);
                    used = 0;
                    space = false;
                }
                Token::Word(word, style) => {
                    let w = str_width(&word);
                    let gap = usize::from(space);
                    if used > 0 && used + gap + w > room {
                        open_line(&mut line, self);
                        used = 0;
                    } else if gap > 0 {
                        line.push(Span::styled(" ", style));
                        used += 1;
                    }
                    space = false;
                    // A word longer than a line is cut where it reaches the edge.
                    let mut piece = String::new();
                    for c in word.chars() {
                        let cw = str_width(c.encode_utf8(&mut [0; 4]));
                        if used + str_width(&piece) + cw > room && (used > 0 || !piece.is_empty()) {
                            line.push(Span::styled(std::mem::take(&mut piece), style));
                            open_line(&mut line, self);
                            used = 0;
                        }
                        piece.push(c);
                    }
                    used += str_width(&piece);
                    line.push(Span::styled(piece, style));
                }
            }
        }
        lines.push(Line::from(line));
        self.lines.extend(lines);
    }

    /// A code block's lines, each behind the prefix and two spaces, cut at
    /// the edge onto the next line.
    fn code_block(&mut self, code: &str) {
        let style = self.code_style();
        let room = self.width.saturating_sub(self.prefix_width() + 2).max(4);
        for raw in code.trim_end_matches('\n').split('\n') {
            let raw = raw.replace('\t', "    ");
            let mut rest: &str = &raw;
            loop {
                let mut cut = rest.len();
                let mut used = 0;
                for (index, c) in rest.char_indices() {
                    let cw = str_width(c.encode_utf8(&mut [0; 4]));
                    if used + cw > room {
                        cut = index;
                        break;
                    }
                    used += cw;
                }
                let mut spans = self.prefix(false);
                spans.push(Span::styled("  ", style));
                spans.push(Span::styled(rest[..cut].to_owned(), style));
                self.lines.push(Line::from(spans));
                rest = &rest[cut..];
                if rest.is_empty() {
                    break;
                }
            }
        }
    }

    /// A table's rows as columns separated by `│`, its head bold and ruled
    /// off; columns are cut, widest first, to fit.
    fn draw_table(&mut self, table: Table) {
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let mut widths = vec![0; columns];
        for row in &table.rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(str_width(cell));
            }
        }
        let room = self
            .width
            .saturating_sub(self.prefix_width() + 3 * (columns - 1));
        while widths.iter().sum::<usize>() > room {
            let Some((widest, width)) = widths.iter().copied().enumerate().max_by_key(|(_, w)| *w)
            else {
                break;
            };
            if width <= MIN_COLUMN {
                break;
            }
            widths[widest] = width - 1;
        }
        let muted = self.muted();
        let head = self.base.add_modifier(Modifier::BOLD);
        for (number, row) in table.rows.iter().enumerate() {
            let style = if number < table.head { head } else { self.base };
            let mut spans = self.prefix(false);
            for (index, width) in widths.iter().enumerate() {
                if index > 0 {
                    spans.push(Span::styled(" │ ", muted));
                }
                let cell = row.get(index).map_or("", String::as_str);
                let shown = crate::components::format::truncate(cell, *width);
                let pad = width.saturating_sub(str_width(&shown));
                spans.push(Span::styled(shown, style));
                if index + 1 < columns {
                    spans.push(Span::raw(" ".repeat(pad)));
                }
            }
            self.lines.push(Line::from(spans));
            if number + 1 == table.head {
                let mut rule = self.prefix(false);
                let line = widths
                    .iter()
                    .map(|width| "─".repeat(*width))
                    .collect::<Vec<_>>()
                    .join("─┼─");
                rule.push(Span::styled(line, muted));
                self.lines.push(Line::from(rule));
            }
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        if let Some(code) = self.code.take() {
            self.code_block(&code);
        }
        while self.lines.last().is_some_and(|line| line.width() == 0) {
            self.lines.pop();
        }
        self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn styled<'a>(lines: &'a [Line], needle: &str) -> &'a Span<'a> {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not in {:?}", text(lines)))
    }

    #[test]
    fn markers_become_styles_and_lists_get_bullets() {
        let lines = render(
            "# Done\n\nThe **parser** handles *quoted* commas and `a,b` now.\n\n- Fixed it\n- Added a test\n  1. unit\n  2. e2e\n- [x] Checked\n",
            60,
            Style::default(),
        );
        assert_eq!(
            text(&lines),
            [
                "Done",
                "",
                "The parser handles quoted commas and a,b now.",
                "",
                "• Fixed it",
                "• Added a test",
                "  1. unit",
                "  2. e2e",
                "• ☑ Checked",
            ]
        );
        assert!(styled(&lines, "Done")
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(styled(&lines, "parser")
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(styled(&lines, "quoted")
            .style
            .add_modifier
            .contains(Modifier::ITALIC));
        assert_eq!(
            styled(&lines, "a,b").style.fg,
            Some(theme::palette().accent_orange)
        );
    }

    #[test]
    fn long_items_wrap_under_their_text_and_code_keeps_its_lines() {
        let lines = render(
            "- one two three four five six\n\n```rust\nfn main() {\n    run();\n}\n```\n\n> quoted words here",
            16,
            Style::default(),
        );
        assert_eq!(
            text(&lines),
            [
                "• one two three",
                "  four five six",
                "",
                "  fn main() {",
                "      run();",
                "  }",
                "",
                "▎ quoted words",
                "▎ here",
            ]
        );
    }

    #[test]
    fn tables_line_up_and_links_say_where_they_go() {
        let lines = render(
            "| File | Lines |\n|---|---|\n| parser.rs | 120 |\n| a.rs | 3 |\n\nSee [the docs](https://docs.rakumux.com) or <https://x.dev>.",
            40,
            Style::default(),
        );
        assert_eq!(
            text(&lines),
            [
                "File      │ Lines",
                "──────────┼──────",
                "parser.rs │ 120",
                "a.rs      │ 3",
                "",
                "See the docs <https://docs.rakumux.com>",
                "or https://x.dev.",
            ]
        );
    }

    #[test]
    fn html_shows_as_text_and_controls_are_stripped() {
        let lines = render(
            "<b>bold?</b> \u{1b}[31mred\u{202e}\n\n![a chart](http://x/c.png)",
            40,
            Style::default(),
        );
        assert_eq!(
            text(&lines),
            ["<b>bold?</b> [31mred", "", "[image: a chart]"]
        );
    }
}
