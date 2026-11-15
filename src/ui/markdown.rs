use crate::ui::theme::Theme;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme as SyntectTheme, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn syntect_theme(is_light: bool) -> &'static SyntectTheme {
    let ts = THEME_SET.get_or_init(ThemeSet::load_defaults);
    let key = if is_light {
        "InspiredGitHub"
    } else {
        "base16-ocean.dark"
    };
    ts.themes
        .get(key)
        .or_else(|| ts.themes.values().next())
        .expect("syntect default themes present")
}

/// Strip ANSI escape sequences from raw terminal/command output so Ratatui layout stays exact.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ('@'..='~').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' {
                            break;
                        }
                        if ch == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// Word-wrap styled spans to `max_width`, applying `first_prefix` to line 0 and `cont_prefix` to wrapped lines.
pub fn wrap_styled_spans(
    spans: Vec<Span<'static>>,
    max_width: usize,
    first_prefix: Vec<Span<'static>>,
    cont_prefix: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let width = max_width.max(12);
    let first_prefix_w: usize = first_prefix
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let cont_prefix_w: usize = cont_prefix
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();

    if spans.is_empty() {
        return vec![Line::from(first_prefix)];
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur_spans = first_prefix;
    let mut cur_w = first_prefix_w;

    for span in spans {
        let style = span.style;
        let content = span.content.into_owned();
        if content.is_empty() {
            continue;
        }

        // Split preserving whitespace runs so spacing stays faithful
        let mut tokens: Vec<String> = Vec::new();
        let mut buf = String::new();
        let mut in_ws: Option<bool> = None;
        for ch in content.chars() {
            let is_ws = ch == ' ' || ch == '\t';
            if let Some(prev) = in_ws
                && prev != is_ws
            {
                tokens.push(std::mem::take(&mut buf));
            }
            in_ws = Some(is_ws);
            if ch == '\t' {
                buf.push_str("    ");
            } else {
                buf.push(ch);
            }
        }
        if !buf.is_empty() {
            tokens.push(buf);
        }

        for tok in tokens {
            let tok_w = UnicodeWidthStr::width(tok.as_str());
            let active_prefix_w = if lines.is_empty() {
                first_prefix_w
            } else {
                cont_prefix_w
            };

            if cur_w > active_prefix_w && cur_w + tok_w > width {
                lines.push(Line::from(std::mem::take(&mut cur_spans)));
                cur_spans = cont_prefix.clone();
                cur_w = cont_prefix_w;
                if tok.trim().is_empty() {
                    continue;
                }
            }

            if tok_w + cur_w <= width {
                cur_spans.push(Span::styled(tok, style));
                cur_w += tok_w;
            } else {
                // Hard-wrap an extra-long single token (like a long URL or path) at char boundaries
                let mut chunk = String::new();
                let mut chunk_w = 0usize;
                for ch in tok.chars() {
                    let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]));
                    if cur_w + chunk_w + cw > width && (cur_w + chunk_w) > cont_prefix_w {
                        if !chunk.is_empty() {
                            cur_spans.push(Span::styled(std::mem::take(&mut chunk), style));
                        }
                        lines.push(Line::from(std::mem::take(&mut cur_spans)));
                        cur_spans = cont_prefix.clone();
                        cur_w = cont_prefix_w;
                        chunk_w = 0;
                    }
                    chunk.push(ch);
                    chunk_w += cw;
                }
                if !chunk.is_empty() {
                    cur_spans.push(Span::styled(chunk, style));
                    cur_w += chunk_w;
                }
            }
        }
    }

    if !cur_spans.is_empty() {
        lines.push(Line::from(cur_spans));
    }

    lines
}

/// Highlight a code block into styled Ratatui lines.
pub fn highlight_code_block(
    code: &str,
    lang: Option<&str>,
    max_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let ps = syntax_set();
    let st = syntect_theme(theme.is_light);
    let syntax = lang
        .and_then(|l| {
            let clean = l.split_whitespace().next().unwrap_or(l).trim();
            ps.find_syntax_by_token(clean)
        })
        .unwrap_or_else(|| ps.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, st);
    let inner_w = max_width.saturating_sub(4).max(12);
    let lang_label = lang
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("text");

    let mut out: Vec<Line<'static>> = Vec::new();
    let rule_len = inner_w.saturating_sub(lang_label.len() + 5);
    out.push(Line::from(vec![
        Span::styled(
            format!("╭─ {lang_label} "),
            Style::default().fg(theme.text_dim),
        ),
        Span::styled(
            "─".repeat(rule_len),
            Style::default().fg(theme.border),
        ),
    ]));

    let cleaned = strip_ansi(code);
    for raw_line in cleaned.lines() {
        let expanded = raw_line.replace('\t', "    ");
        let ranges = highlighter
            .highlight_line(&expanded, ps)
            .unwrap_or_default();

        let mut line_spans: Vec<Span<'static>> = Vec::new();
        if ranges.is_empty() {
            line_spans.push(Span::styled(
                expanded.clone(),
                Style::default().fg(theme.text).bg(theme.code_bg),
            ));
        } else {
            for (syn_style, slice) in ranges {
                let fg = Color::Rgb(
                    syn_style.foreground.r,
                    syn_style.foreground.g,
                    syn_style.foreground.b,
                );
                line_spans.push(Span::styled(
                    slice.to_string(),
                    Style::default().fg(fg).bg(theme.code_bg),
                ));
            }
        }

        let wrapped = wrap_styled_spans(
            line_spans,
            max_width,
            vec![Span::styled("│ ", Style::default().fg(theme.border))],
            vec![Span::styled("│ ↪ ", Style::default().fg(theme.text_dim))],
        );
        out.extend(wrapped);
    }

    if cleaned.is_empty() {
        out.push(Line::from(vec![
            Span::styled("│ ", Style::default().fg(theme.border)),
            Span::styled("(empty)", Style::default().fg(theme.text_dim)),
        ]));
    }

    out.push(Line::from(vec![Span::styled(
        format!("╰{}", "─".repeat(inner_w.saturating_sub(1))),
        Style::default().fg(theme.border),
    )]));

    out
}

/// Render Markdown text into styled, word-wrapped `ratatui::text::Line`s.
pub fn render_markdown(markdown: &str, max_width: usize, theme: &Theme) -> Vec<Line<'static>> {
    if markdown.is_empty() {
        return Vec::new();
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(markdown, options);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut inline_spans: Vec<Span<'static>> = Vec::new();

    let mut bold = false;
    let mut italic = false;
    let mut strikethrough = false;
    let mut in_link: Option<String> = None;
    let mut heading_level: Option<HeadingLevel> = None;
    let mut blockquote_depth: usize = 0;

    // List state: Vec<Option<u64>> where None = bullet list, Some(next_num) = ordered list
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut item_prefix_pending: Option<String> = None;

    // Code block state
    let mut in_code_block = false;
    let mut code_lang: Option<String> = None;
    let mut code_buf = String::new();

    // Table state
    let mut in_table = false;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();

    let flush_inline = |inline_spans: &mut Vec<Span<'static>>,
                        lines: &mut Vec<Line<'static>>,
                        heading_level: Option<HeadingLevel>,
                        blockquote_depth: usize,
                        list_depth: usize,
                        item_prefix: &mut Option<String>| {
        if inline_spans.is_empty() && item_prefix.is_none() {
            return;
        }
        let mut first_prefix: Vec<Span<'static>> = Vec::new();
        let mut cont_prefix: Vec<Span<'static>> = Vec::new();

        if blockquote_depth > 0 {
            let bar = "▎ ".repeat(blockquote_depth);
            first_prefix.push(Span::styled(bar.clone(), Style::default().fg(theme.accent)));
            cont_prefix.push(Span::styled(bar, Style::default().fg(theme.accent)));
        }

        if let Some(h) = heading_level {
            let marker = match h {
                HeadingLevel::H1 => "▍ ",
                HeadingLevel::H2 => "▎ ",
                _ => "▏ ",
            };
            let color = match h {
                HeadingLevel::H1 => theme.primary,
                HeadingLevel::H2 => theme.accent,
                _ => theme.secondary,
            };
            first_prefix.push(Span::styled(
                marker.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
            cont_prefix.push(Span::raw("  "));
        } else if list_depth > 0 {
            let indent = "  ".repeat(list_depth.saturating_sub(1));
            if let Some(pref) = item_prefix.take() {
                let pad = " ".repeat(UnicodeWidthStr::width(pref.as_str()));
                first_prefix.push(Span::raw(indent.clone()));
                first_prefix.push(Span::styled(pref, Style::default().fg(theme.primary)));
                cont_prefix.push(Span::raw(format!("{indent}{pad}")));
            } else {
                first_prefix.push(Span::raw(format!("{indent}  ")));
                cont_prefix.push(Span::raw(format!("{indent}  ")));
            }
        }

        let wrapped = wrap_styled_spans(
            std::mem::take(inline_spans),
            max_width,
            first_prefix,
            cont_prefix,
        );
        lines.extend(wrapped);
    };

    for event in parser {
        if in_code_block {
            match event {
                Event::Text(t) => code_buf.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                    let rendered = highlight_code_block(
                        &code_buf,
                        code_lang.as_deref(),
                        max_width,
                        theme,
                    );
                    lines.extend(rendered);
                    code_buf.clear();
                    code_lang = None;
                }
                _ => {}
            }
            continue;
        }

        if in_table {
            match event {
                Event::Start(Tag::TableHead | Tag::TableRow) => {
                    current_row.clear();
                }
                Event::Start(Tag::TableCell) => {
                    current_cell.clear();
                }
                Event::Text(t) | Event::Code(t) => {
                    current_cell.push_str(&t);
                }
                Event::End(TagEnd::TableCell) => {
                    current_row.push(std::mem::take(&mut current_cell));
                }
                Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                    if !current_row.is_empty() {
                        table_rows.push(std::mem::take(&mut current_row));
                    }
                }
                Event::End(TagEnd::Table) => {
                    in_table = false;
                    lines.extend(render_markdown_table(&table_rows, max_width, theme));
                    table_rows.clear();
                }
                _ => {}
            }
            continue;
        }

        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    if !lines.is_empty() {
                        lines.push(Line::raw(""));
                    }
                    heading_level = Some(level);
                }
                Tag::Paragraph => {}
                Tag::BlockQuote(_) => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    blockquote_depth += 1;
                }
                Tag::CodeBlock(kind) => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    in_code_block = true;
                    code_buf.clear();
                    code_lang = match kind {
                        CodeBlockKind::Fenced(l) if !l.trim().is_empty() => Some(l.into_string()),
                        _ => None,
                    };
                }
                Tag::List(start) => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    list_stack.push(start);
                }
                Tag::Item => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    if let Some(last) = list_stack.last_mut() {
                        match last {
                            Some(n) => {
                                item_prefix_pending = Some(format!("{n}. "));
                                *n += 1;
                            }
                            None => {
                                let bullet = if list_stack.len() % 2 == 1 {
                                    "• "
                                } else {
                                    "◦ "
                                };
                                item_prefix_pending = Some(bullet.to_string());
                            }
                        }
                    }
                }
                Tag::Table(_) => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    in_table = true;
                    table_rows.clear();
                }
                Tag::Strong => bold = true,
                Tag::Emphasis => italic = true,
                Tag::Strikethrough => strikethrough = true,
                Tag::Link { dest_url, .. } => in_link = Some(dest_url.into_string()),
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    heading_level = None;
                }
                TagEnd::Paragraph => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    if list_stack.is_empty() && blockquote_depth == 0 {
                        lines.push(Line::raw(""));
                    }
                }
                TagEnd::BlockQuote(_) => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    blockquote_depth = blockquote_depth.saturating_sub(1);
                }
                TagEnd::Item => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                }
                TagEnd::List(_) => {
                    flush_inline(
                        &mut inline_spans,
                        &mut lines,
                        heading_level,
                        blockquote_depth,
                        list_stack.len(),
                        &mut item_prefix_pending,
                    );
                    list_stack.pop();
                    if list_stack.is_empty() {
                        lines.push(Line::raw(""));
                    }
                }
                TagEnd::Strong => bold = false,
                TagEnd::Emphasis => italic = false,
                TagEnd::Strikethrough => strikethrough = false,
                TagEnd::Link => {
                    if let Some(url) = in_link.take()
                        && !url.is_empty()
                    {
                        inline_spans.push(Span::styled(
                            format!(" ({url})"),
                            Style::default().fg(theme.text_dim),
                        ));
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                let mut style = Style::default().fg(if blockquote_depth > 0 {
                    theme.text_muted
                } else {
                    theme.text
                });
                if let Some(h) = heading_level {
                    style = style
                        .fg(match h {
                            HeadingLevel::H1 => theme.primary,
                            HeadingLevel::H2 => theme.accent,
                            _ => theme.secondary,
                        })
                        .add_modifier(Modifier::BOLD);
                }
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if italic || blockquote_depth > 0 {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if strikethrough {
                    style = style.add_modifier(Modifier::CROSSED_OUT);
                }
                if in_link.is_some() {
                    style = style.fg(theme.accent).add_modifier(Modifier::UNDERLINED);
                }
                inline_spans.push(Span::styled(t.into_string(), style));
            }
            Event::Code(c) => {
                inline_spans.push(Span::styled(
                    format!("`{c}`"),
                    Style::default()
                        .fg(theme.accent)
                        .bg(theme.code_bg)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Event::TaskListMarker(checked) => {
                let (mark, col) = if checked {
                    ("[✓] ", theme.success)
                } else {
                    ("[ ] ", theme.text_muted)
                };
                item_prefix_pending = None;
                inline_spans.push(Span::styled(mark.to_string(), Style::default().fg(col)));
            }
            Event::SoftBreak => {
                inline_spans.push(Span::raw(" "));
            }
            Event::HardBreak => {
                flush_inline(
                    &mut inline_spans,
                    &mut lines,
                    heading_level,
                    blockquote_depth,
                    list_stack.len(),
                    &mut item_prefix_pending,
                );
            }
            Event::Rule => {
                flush_inline(
                    &mut inline_spans,
                    &mut lines,
                    heading_level,
                    blockquote_depth,
                    list_stack.len(),
                    &mut item_prefix_pending,
                );
                lines.push(Line::from(Span::styled(
                    "─".repeat(max_width.min(60)),
                    Style::default().fg(theme.border),
                )));
            }
            _ => {}
        }
    }

    if in_code_block && !code_buf.is_empty() {
        // Unclosed streaming code block! Render live with syntax highlighting!
        lines.extend(highlight_code_block(
            &code_buf,
            code_lang.as_deref(),
            max_width,
            theme,
        ));
    } else {
        flush_inline(
            &mut inline_spans,
            &mut lines,
            heading_level,
            blockquote_depth,
            list_stack.len(),
            &mut item_prefix_pending,
        );
    }

    // Trim trailing blank line
    while lines.last().is_some_and(|l| l.spans.is_empty() || l.width() == 0) {
        lines.pop();
    }

    lines
}

fn render_markdown_table(
    rows: &[Vec<String>],
    max_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if col_count == 0 {
        return Vec::new();
    }
    let mut widths = vec![3usize; col_count];
    for r in rows {
        for (i, cell) in r.iter().enumerate() {
            widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    let total_needed: usize = widths.iter().sum::<usize>() + col_count * 3 + 1;
    if total_needed > max_width && col_count > 0 {
        let per_col = max_width.saturating_sub(col_count * 3 + 1) / col_count;
        for w in &mut widths {
            *w = (*w).min(per_col.max(4));
        }
    }

    let mut out = Vec::new();
    for (row_idx, row) in rows.iter().enumerate() {
        let mut spans = vec![Span::styled("│ ", Style::default().fg(theme.border))];
        for (col_idx, &w) in widths.iter().enumerate() {
            let val = row.get(col_idx).map(String::as_str).unwrap_or("");
            let truncated: String = val.chars().take(w).collect();
            let pad = w.saturating_sub(UnicodeWidthStr::width(truncated.as_str()));
            let style = if row_idx == 0 {
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            spans.push(Span::styled(format!("{truncated}{}", " ".repeat(pad)), style));
            spans.push(Span::styled(" │ ", Style::default().fg(theme.border)));
        }
        out.push(Line::from(spans));
        if row_idx == 0 && rows.len() > 1 {
            let sep = widths
                .iter()
                .map(|w| "─".repeat(*w + 2))
                .collect::<Vec<_>>()
                .join("┼");
            out.push(Line::from(Span::styled(
                format!("├{sep}┤"),
                Style::default().fg(theme.border),
            )));
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct DiffSummary {
    pub added: usize,
    pub removed: usize,
    pub total_lines: usize,
    pub lines: Vec<Line<'static>>,
}

/// Compute a rich unified diff with line numbers and red/green styling using `similar`.
pub fn render_unified_diff(
    old_text: Option<&str>,
    new_text: &str,
    max_width: usize,
    max_display_lines: Option<usize>,
    theme: &Theme,
) -> DiffSummary {
    let old = old_text.unwrap_or("");
    let diff = TextDiff::from_lines(old, new_text);

    let mut added = 0usize;
    let mut removed = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }

    let mut rendered: Vec<Line<'static>> = Vec::new();
    let content_w = max_width.saturating_sub(14).max(16);

    for group in diff.grouped_ops(3) {
        if let (Some(first), Some(last)) = (group.first(), group.last()) {
            let old_start = first.old_range().start + 1;
            let old_len = last.old_range().end.saturating_sub(first.old_range().start);
            let new_start = first.new_range().start + 1;
            let new_len = last.new_range().end.saturating_sub(first.new_range().start);
            rendered.push(Line::from(Span::styled(
                format!("@@ -{old_start},{old_len} +{new_start},{new_len} @@"),
                Style::default().fg(theme.accent),
            )));
        }

        for op in group {
            for change in diff.iter_changes(&op) {
                let (sign, fg, bg) = match change.tag() {
                    ChangeTag::Delete => ("-", theme.diff_del_fg, Some(theme.diff_del_bg)),
                    ChangeTag::Insert => ("+", theme.diff_add_fg, Some(theme.diff_add_bg)),
                    ChangeTag::Equal => (" ", theme.text_muted, None),
                };

                let old_no = change
                    .old_index()
                    .map(|i| format!("{:>4}", i + 1))
                    .unwrap_or_else(|| "    ".to_string());
                let new_no = change
                    .new_index()
                    .map(|i| format!("{:>4}", i + 1))
                    .unwrap_or_else(|| "    ".to_string());

                let raw_val = change.value().trim_end_matches(&['\r', '\n'][..]).replace('\t', "    ");
                let clipped: String = if UnicodeWidthStr::width(raw_val.as_str()) > content_w {
                    let mut s = String::new();
                    let mut w = 0;
                    for ch in raw_val.chars() {
                        let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]));
                        if w + cw + 1 > content_w {
                            s.push('…');
                            break;
                        }
                        s.push(ch);
                        w += cw;
                    }
                    s
                } else {
                    raw_val
                };

                let mut line_style = Style::default().fg(fg);
                if let Some(bg_col) = bg {
                    line_style = line_style.bg(bg_col);
                }

                rendered.push(Line::from(vec![
                    Span::styled(
                        format!("{old_no} {new_no} │"),
                        Style::default().fg(theme.text_dim),
                    ),
                    Span::styled(format!(" {sign} {clipped}"), line_style),
                ]));
            }
        }
    }

    let total_lines = rendered.len();
    if let Some(limit) = max_display_lines
        && rendered.len() > limit
    {
        let hidden = rendered.len() - limit;
        rendered.truncate(limit);
        rendered.push(Line::from(Span::styled(
            format!("     … +{hidden} more diff lines (click or ctrl+o to expand)"),
            Style::default()
                .fg(theme.text_muted)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    DiffSummary {
        added,
        removed,
        total_lines,
        lines: rendered,
    }
}
