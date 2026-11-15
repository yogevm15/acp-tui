use crate::acp::terminal_mgr::TerminalSnapshot;
use crate::acp::client::RpcDirection;
use crate::app::{App, short_id};
use crate::state::*;
use crate::ui::markdown::{render_markdown, render_unified_diff, strip_ansi, wrap_styled_spans};
use crate::ui::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    StatefulWidget, Widget,
};
use std::collections::HashMap;
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const SIDEBAR_WIDTH: u16 = 38;
const COLLAPSED_OUTPUT_LINES: usize = 10;
const COLLAPSED_DIFF_LINES: usize = 16;

/// Ratatui widgets such as `Paragraph` and `Clear` panic when `area` reaches outside the frame,
/// so every widget is drawn through these helpers.
fn render<W: Widget>(f: &mut Frame, widget: W, area: Rect) {
    let area = area.intersection(f.area());
    if !area.is_empty() {
        f.render_widget(widget, area);
    }
}

fn render_stateful<W: StatefulWidget>(f: &mut Frame, widget: W, area: Rect, state: &mut W::State) {
    let area = area.intersection(f.area());
    if !area.is_empty() {
        f.render_stateful_widget(widget, area, state);
    }
}

struct RenderCtx<'a> {
    theme: &'a Theme,
    terminals: &'a HashMap<String, TerminalSnapshot>,
    spinner: &'static str,
    cwd: &'a std::path::Path,
}

impl RenderCtx<'_> {
    fn rel(&self, p: &str) -> String {
        std::path::Path::new(p)
            .strip_prefix(self.cwd)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string())
    }
}

pub fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn shorten_home(p: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let h = home.to_string_lossy();
        if let Some(rest) = p.strip_prefix(h.as_ref()) {
            return format!("~{rest}");
        }
    }
    p.to_string()
}

pub fn truncate_w(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

fn pad_bg(mut line: Line<'static>, width: usize, bg: Color) -> Line<'static> {
    let w = line.width();
    for span in line.spans.iter_mut() {
        if span.style.bg.is_none() {
            span.style = span.style.bg(bg);
        }
    }
    if w < width {
        line.spans
            .push(Span::styled(" ".repeat(width - w), Style::default().bg(bg)));
    }
    line
}

fn plain_wrap(
    text: &str,
    width: usize,
    style: Style,
    first: Vec<Span<'static>>,
    cont: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut first_line = true;
    for raw in text.split('\n') {
        let pre = if first_line { first.clone() } else { cont.clone() };
        first_line = false;
        out.extend(wrap_styled_spans(
            vec![Span::styled(raw.to_string(), style)],
            width,
            pre,
            cont.clone(),
        ));
    }
    out
}

fn prefix_lines(lines: Vec<Line<'static>>, first: Span<'static>, cont: Span<'static>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, mut l)| {
            l.spans.insert(0, if i == 0 { first.clone() } else { cont.clone() });
            l
        })
        .collect()
}

fn collapse(
    lines: Vec<Line<'static>>,
    max: usize,
    expand: bool,
    keep_tail: bool,
    theme: &Theme,
    collapsible: &mut bool,
) -> Vec<Line<'static>> {
    if lines.len() > max {
        *collapsible = true;
    }
    if expand || lines.len() <= max {
        return lines;
    }
    let hidden = lines.len() - max;
    let hint = Line::from(Span::styled(
        format!("… +{hidden} lines (click or ctrl+o to expand)"),
        Style::default()
            .fg(theme.text_dim)
            .add_modifier(Modifier::ITALIC),
    ));
    if keep_tail {
        let mut out = vec![hint];
        out.extend(lines.into_iter().skip(hidden));
        out
    } else {
        let mut out: Vec<Line<'static>> = lines.into_iter().take(max).collect();
        out.push(hint);
        out
    }
}

fn format_nonempty_json(val: &serde_json::Value) -> Option<String> {
    match val {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Array(a) if a.is_empty() => None,
        serde_json::Value::Object(o) if o.is_empty() => None,
        other => serde_json::to_string_pretty(other).ok(),
    }
}

// ============================================================================
// Timeline item renderers
// ============================================================================

fn render_item(
    item: &TimelineItem,
    width: usize,
    ctx: &RenderCtx,
    live: bool,
    expanded: bool,
) -> (Vec<Line<'static>>, bool) {
    let t = ctx.theme;
    match item {
        TimelineItem::UserMessage {
            text,
            attachments,
            steered,
            ..
        } => {
            let bar = Span::styled("▌ ", Style::default().fg(t.user_bar).bg(t.surface));
            let mut lines = plain_wrap(
                text,
                width.saturating_sub(1),
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                vec![bar.clone()],
                vec![bar.clone()],
            );
            if !attachments.is_empty() {
                lines.push(Line::from(vec![
                    bar.clone(),
                    Span::styled(
                        format!("📎 {}", attachments.join("  ")),
                        Style::default().fg(t.accent),
                    ),
                ]));
            }
            if *steered {
                lines.push(Line::from(vec![
                    bar.clone(),
                    Span::styled(
                        "↳ sent into the running turn",
                        Style::default().fg(t.text_dim).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
            (
                lines.into_iter().map(|l| pad_bg(l, width, t.surface)).collect(),
                false,
            )
        }
        TimelineItem::AssistantMessage { text, .. } => {
            let md = render_markdown(text, width.saturating_sub(2), t);
            (
                prefix_lines(
                    md,
                    Span::styled("● ", Style::default().fg(t.assistant_bar)),
                    Span::raw("  "),
                ),
                false,
            )
        }
        TimelineItem::AssistantThought { text, .. } => {
            let header_style = Style::default()
                .fg(t.thought_bar)
                .add_modifier(Modifier::ITALIC);
            let body_style = Style::default()
                .fg(t.text_dim)
                .add_modifier(Modifier::ITALIC);
            let hint_style = Style::default().fg(t.text_dim);
            let bar = || vec![Span::styled("│ ", Style::default().fg(t.thought_bar))];
            let trimmed = text.trim();
            if live {
                let body = if trimmed.is_empty() {
                    Vec::new()
                } else {
                    plain_wrap(trimmed, width, body_style, bar(), bar())
                };
                if expanded {
                    let mut out = vec![Line::from(vec![
                        Span::styled(format!("{} Thinking…", ctx.spinner), header_style),
                        Span::styled("  click to collapse ▾", hint_style),
                    ])];
                    out.extend(body);
                    return (out, true);
                }
                let hidden = body.len().saturating_sub(3);
                let hint = if hidden > 0 {
                    format!("  +{hidden} lines · click to expand ▸")
                } else {
                    "  click to expand ▸".to_string()
                };
                let mut out = vec![Line::from(vec![
                    Span::styled(format!("{} Thinking…", ctx.spinner), header_style),
                    Span::styled(hint, hint_style),
                ])];
                out.extend(body.into_iter().skip(hidden));
                return (out, true);
            }
            if expanded {
                let mut out = vec![Line::from(vec![
                    Span::styled("✻ Thinking", header_style),
                    Span::styled("  click · ctrl+t ▾", hint_style),
                ])];
                if !trimmed.is_empty() {
                    out.extend(plain_wrap(trimmed, width, body_style, bar(), bar()));
                }
                return (out, true);
            }
            let first = text
                .split(['\n', '.'])
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            (
                vec![Line::from(vec![
                    Span::styled("✻ Thought  ", header_style),
                    Span::styled(
                        truncate_w(first, width.saturating_sub(32)),
                        body_style,
                    ),
                    Span::styled("  click · ctrl+t ▸", hint_style),
                ])],
                true,
            )
        }
        TimelineItem::ToolCall(card) => render_tool_card(card, width, ctx, expanded),
        TimelineItem::Notice {
            severity,
            title,
            description,
        } => {
            let (icon, color) = match severity.as_str() {
                "error" => ("✗", t.error),
                "warning" => ("⚠", t.warning),
                _ => ("ℹ", t.accent),
            };
            let mut lines = plain_wrap(
                title,
                width,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
                vec![Span::styled(format!("{icon} "), Style::default().fg(color))],
                vec![Span::raw("  ")],
            );
            if let Some(d) = description.as_ref().filter(|d| !d.trim().is_empty()) {
                lines.extend(plain_wrap(
                    d,
                    width,
                    Style::default().fg(t.text_muted),
                    vec![Span::raw("  ")],
                    vec![Span::raw("  ")],
                ));
            }
            (lines, false)
        }
        TimelineItem::Compaction {
            status,
            summary,
            error,
            ..
        } => {
            let icon = match status.as_str() {
                "completed" => "✓",
                "failed" => "✗",
                "cancelled" => "⊘",
                _ => ctx.spinner,
            };
            let mut collapsible = status == "in_progress";
            let mut body_lines = Vec::new();
            if !summary.trim().is_empty() {
                let md = render_markdown(summary, width.saturating_sub(4), t);
                body_lines = collapse(
                    prefix_lines(
                        md,
                        Span::styled("  ⎿ ", Style::default().fg(t.text_dim)),
                        Span::raw("    "),
                    ),
                    6,
                    expanded,
                    false,
                    t,
                    &mut collapsible,
                );
            }
            let expandable = collapsible || expanded;
            let mut header_spans = vec![
                Span::styled(format!("{icon} "), Style::default().fg(t.secondary)),
                Span::styled(
                    "Context compaction",
                    Style::default().fg(t.secondary).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" · {status}"), Style::default().fg(t.text_dim)),
            ];
            if expandable {
                header_spans.push(Span::styled(
                    if expanded { " ▾" } else { " ▸" },
                    Style::default().fg(t.text_dim),
                ));
            }
            let mut lines = vec![Line::from(header_spans)];
            lines.extend(body_lines);
            if let Some(e) = error {
                lines.push(Line::from(Span::styled(
                    format!("  ⎿ {e}"),
                    Style::default().fg(t.error),
                )));
            }
            (lines, expandable)
        }
        TimelineItem::SubagentMessage {
            subagent_title,
            text,
            to_subagent,
            ..
        } => {
            let mut collapsible = false;
            let md = render_markdown(text, width.saturating_sub(4), t);
            let body_lines = collapse(
                prefix_lines(
                    md,
                    Span::styled("  ⎿ ", Style::default().fg(t.text_dim)),
                    Span::raw("    "),
                ),
                6,
                expanded,
                false,
                t,
                &mut collapsible,
            );
            let expandable = collapsible || expanded;
            let header = if *to_subagent {
                format!("↳ Task → {subagent_title}")
            } else {
                format!("↲ {subagent_title} → result")
            };
            let mut header_spans = vec![Span::styled(
                header,
                Style::default().fg(t.secondary).add_modifier(Modifier::BOLD),
            )];
            if expandable {
                header_spans.push(Span::styled(
                    if expanded { " ▾" } else { " ▸" },
                    Style::default().fg(t.text_dim),
                ));
            }
            let mut lines = vec![Line::from(header_spans)];
            lines.extend(body_lines);
            (lines, expandable)
        }
        TimelineItem::SystemInfo { text, is_error } => {
            let (icon, color) = if *is_error {
                ("✗ ", t.error)
            } else {
                ("• ", t.text_muted)
            };
            (
                plain_wrap(
                    text,
                    width,
                    Style::default().fg(color),
                    vec![Span::styled(icon, Style::default().fg(color))],
                    vec![Span::raw("  ")],
                ),
                false,
            )
        }
    }
}

fn tool_status_icon(card: &ToolCallCard, ctx: &RenderCtx) -> Span<'static> {
    let t = ctx.theme;
    match card.status.as_str() {
        "completed" => Span::styled("✓", Style::default().fg(t.success)),
        "failed" => Span::styled("✗", Style::default().fg(t.error)),
        "cancelled" => Span::styled("⊘", Style::default().fg(t.text_dim)),
        "in_progress" => Span::styled(ctx.spinner, Style::default().fg(t.warning)),
        _ => Span::styled("○", Style::default().fg(t.text_muted)),
    }
}

fn kind_color(kind: &str, t: &Theme) -> Color {
    match kind {
        "edit" | "delete" | "move" => t.warning,
        "execute" => t.secondary,
        "read" | "search" => t.accent,
        "fetch" => t.primary,
        "think" => t.thought_bar,
        _ => t.text_muted,
    }
}

fn render_tool_card(
    card: &ToolCallCard,
    width: usize,
    ctx: &RenderCtx,
    expanded: bool,
) -> (Vec<Line<'static>>, bool) {
    let t = ctx.theme;
    let body_w = width.saturating_sub(4);
    let mut body: Vec<Line<'static>> = Vec::new();
    let mut collapsible = card.is_running();

    // Diffs
    for d in &card.diffs {
        let summary = render_unified_diff(
            d.old_text.as_deref(),
            &d.new_text,
            body_w,
            if expanded { None } else { Some(COLLAPSED_DIFF_LINES) },
            t,
        );
        if summary.total_lines > COLLAPSED_DIFF_LINES {
            collapsible = true;
        }
        let label = if d.old_text.is_none() { "created" } else { "edited" };
        body.push(Line::from(vec![
            Span::styled(ctx.rel(&d.path), Style::default().fg(t.text).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {label} "), Style::default().fg(t.text_dim)),
            Span::styled(format!("+{}", summary.added), Style::default().fg(t.diff_add_fg)),
            Span::raw(" "),
            Span::styled(format!("-{}", summary.removed), Style::default().fg(t.diff_del_fg)),
        ]));
        body.extend(summary.lines);
    }

    // Terminals
    for tid in &card.terminal_ids {
        let Some(term) = ctx.terminals.get(tid) else {
            continue;
        };
        if !term.command.is_empty() && term.command.trim() != card.title.trim() {
            body.push(Line::from(vec![
                Span::styled("$ ", Style::default().fg(t.secondary)),
                Span::styled(truncate_w(&term.command, body_w.saturating_sub(2)), Style::default().fg(t.text_muted)),
            ]));
        }
        if term.truncated {
            body.push(Line::from(Span::styled(
                "… earlier output truncated",
                Style::default().fg(t.text_dim).add_modifier(Modifier::ITALIC),
            )));
        }
        let output = strip_ansi(&term.output);
        let out_lines: Vec<Line<'static>> = output
            .trim_end()
            .lines()
            .flat_map(|l| {
                wrap_styled_spans(
                    vec![Span::styled(l.to_string(), Style::default().fg(t.text_muted))],
                    body_w,
                    vec![],
                    vec![],
                )
            })
            .collect();
        if out_lines.is_empty() && term.exit_status.is_none() {
            body.push(Line::from(Span::styled(
                format!("{} running…", ctx.spinner),
                Style::default().fg(t.text_dim),
            )));
        }
        body.extend(collapse(
            out_lines,
            COLLAPSED_OUTPUT_LINES,
            expanded,
            true,
            t,
            &mut collapsible,
        ));
        if let Some(exit) = &term.exit_status {
            let ok = exit.exit_code == Some(0);
            if ok && card.is_failed() {
                continue;
            }
            let label = match (&exit.exit_code, &exit.signal) {
                (Some(c), _) => format!("exit {c}"),
                (None, Some(s)) => format!("killed by {s}"),
                _ => "exited".to_string(),
            };
            body.push(Line::from(Span::styled(
                label,
                Style::default().fg(if ok { t.success } else { t.error }),
            )));
        }
    }

    // Text content. File reads collapse to a line count unless expanded.
    if card.kind == "read" && !card.is_failed() && !card.text_outputs.is_empty() {
        collapsible = true;
        if !expanded {
            let n: usize = card
                .text_outputs
                .iter()
                .map(|o| o.lines().filter(|l| !l.trim_start().starts_with("```")).count())
                .sum();
            body.push(Line::from(Span::styled(
                format!("Read {n} lines (click or ctrl+o to show)"),
                Style::default().fg(t.text_muted),
            )));
        } else {
            for text in &card.text_outputs {
                let rendered = match strip_fence(text) {
                    Some(inner) => plain_wrap(
                        inner.trim_end(),
                        body_w,
                        Style::default().fg(t.text_muted),
                        vec![],
                        vec![],
                    ),
                    None => render_markdown(text, body_w, t),
                };
                body.extend(rendered);
            }
        }
    } else if card.terminal_ids.is_empty() || card.text_outputs.iter().any(|o| o.contains("```")) {
        for text in &card.text_outputs {
            let rendered = match strip_fence(text) {
                Some(inner) => plain_wrap(
                    inner.trim_end(),
                    body_w,
                    Style::default().fg(if card.is_failed() { t.error } else { t.text_muted }),
                    vec![],
                    vec![],
                ),
                None => render_markdown(text, body_w, t),
            };
            body.extend(collapse(
                rendered,
                COLLAPSED_OUTPUT_LINES,
                expanded,
                false,
                t,
                &mut collapsible,
            ));
        }
    }

    let has_primary_body = !body.is_empty();

    // Locations for reads/searches without other output
    if !has_primary_body && !card.locations.is_empty() {
        if card.title.len() >= 3 || card.locations.len() > 3 {
            collapsible = true;
        }
        if card.title.len() < 3 || expanded {
            let limit = if expanded { card.locations.len() } else { 3 };
            for loc in card.locations.iter().take(limit) {
                let s = match loc.line {
                    Some(l) if l > 0 => format!("{}:{l}", ctx.rel(&loc.path)),
                    _ => ctx.rel(&loc.path),
                };
                body.push(Line::from(Span::styled(s, Style::default().fg(t.text_muted))));
            }
        }
    }

    // Raw output for failures, raw input/output when expanded and no primary output exists
    if !has_primary_body {
        let raw_in = card.raw_input.as_ref().and_then(format_nonempty_json);
        let raw_out = card.raw_output.as_ref().and_then(format_nonempty_json);
        if card.is_failed()
            && let Some(s) = raw_out
        {
            let wrapped = plain_wrap(&s, body_w, Style::default().fg(t.error), vec![], vec![]);
            body.extend(collapse(wrapped, 6, expanded, false, t, &mut collapsible));
            if raw_in.is_some() {
                collapsible = true;
                if expanded
                    && let Some(inp) = raw_in
                {
                    body.extend(plain_wrap(&inp, body_w, Style::default().fg(t.text_dim), vec![], vec![]));
                }
            }
        } else {
            if raw_in.is_some() || raw_out.is_some() {
                collapsible = true;
            }
            if expanded {
                if let Some(inp) = raw_in {
                    body.extend(plain_wrap(&inp, body_w, Style::default().fg(t.text_dim), vec![], vec![]));
                }
                if let Some(out) = raw_out {
                    body.extend(plain_wrap(&out, body_w, Style::default().fg(t.text_muted), vec![], vec![]));
                }
            }
        }
    }

    let badge_color = kind_color(&card.kind, t);
    let mut header = vec![
        tool_status_icon(card, ctx),
        Span::raw(" "),
        Span::styled(
            card.kind_badge().to_string(),
            Style::default().fg(badge_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    let title = card
        .title
        .replace('\n', " ")
        .replace(&format!("{}/", ctx.cwd.display()), "");
    let dur = card
        .duration_ms
        .map(|ms| {
            if ms >= 1000 {
                format!(" · {:.1}s", ms as f64 / 1000.0)
            } else {
                format!(" · {ms}ms")
            }
        })
        .unwrap_or_default();
    let expandable = collapsible || expanded;
    let chevron = if expandable {
        if expanded { " ▾" } else { " ▸" }
    } else {
        ""
    };
    let used = 3 + card.kind_badge().len() + dur.len() + chevron.width();
    header.push(Span::styled(
        truncate_w(&title, width.saturating_sub(used)),
        Style::default().fg(t.text),
    ));
    if !dur.is_empty() {
        header.push(Span::styled(dur, Style::default().fg(t.text_dim)));
    }
    if !chevron.is_empty() {
        header.push(Span::styled(chevron, Style::default().fg(t.text_dim)));
    }

    let mut lines = vec![Line::from(header)];
    lines.extend(prefix_lines(
        body,
        Span::styled("  ⎿ ", Style::default().fg(t.text_dim)),
        Span::raw("    "),
    ));
    (lines, expandable)
}

/// The inside of a text that is one fenced code block, as tool results often are.
fn strip_fence(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") || !trimmed.ends_with("```") || trimmed.len() < 6 {
        return None;
    }
    let after_open = trimmed.find('\n')? + 1;
    let close = trimmed.rfind('\n')?;
    (close >= after_open).then(|| &trimmed[after_open..close])
}

fn welcome_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut lines = vec![Line::raw("")];
    let logo = [
        "  ▄▀█ █▀▀ █▀█   ▀█▀ █ █ █",
        "  █▀█ █▄▄ █▀▀    █  █▄█ █",
    ];
    for l in logo {
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::raw(""));
    let status = match app.status {
        ConnectionStatus::Connecting => format!("{} connecting…", app.spinner()),
        ConnectionStatus::Connected if app.opening => format!("{} opening session…", app.spinner()),
        ConnectionStatus::Connected => "● connected".to_string(),
        ConnectionStatus::AuthRequired => "● authentication required (/login)".to_string(),
        ConnectionStatus::Disconnected => "● disconnected".to_string(),
    };
    let status_color = match app.status {
        ConnectionStatus::Connected if !app.opening => t.success,
        ConnectionStatus::Disconnected => t.error,
        ConnectionStatus::AuthRequired => t.warning,
        _ => t.text_muted,
    };
    lines.push(Line::from(vec![
        Span::styled("  Agent    ", Style::default().fg(t.text_dim)),
        Span::styled(app.agent_display_name(), Style::default().fg(t.text).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {status}"), Style::default().fg(status_color)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Command  ", Style::default().fg(t.text_dim)),
        Span::styled(
            truncate_w(&app.profile.command_line_display(), width.saturating_sub(12)),
            Style::default().fg(t.text_muted),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Folder   ", Style::default().fg(t.text_dim)),
        Span::styled(
            shorten_home(&app.cwd.to_string_lossy()),
            Style::default().fg(t.text_muted),
        ),
    ]));
    let others: Vec<String> = app
        .profiles
        .iter()
        .filter(|p| p.id != app.profile.id)
        .map(|p| p.id.clone())
        .collect();
    lines.push(Line::from(vec![
        Span::styled("  Agents   ", Style::default().fg(t.text_dim)),
        Span::styled(
            truncate_w(
                &format!(
                    "{}{}",
                    shorten_home(&app.acp_json_path.to_string_lossy()),
                    if others.is_empty() { String::new() } else { format!("  (also: {})", others.join(", ")) }
                ),
                width.saturating_sub(12),
            ),
            Style::default().fg(t.text_muted),
        ),
    ]));
    lines.push(Line::raw(""));
    let tips: [(&str, &str); 8] = [
        ("enter", "send · shift+enter or \\ for a new line"),
        ("/", "commands · /model, /mode, /agent complete their values"),
        ("@", "attach a file (fuzzy search)"),
        ("shift+tab", "cycle the session mode"),
        ("ctrl+p", "command palette"),
        ("ctrl+r", "search prompt history"),
        ("ctrl+s / ctrl+g", "sessions / switch agent"),
        ("esc", "interrupt the agent · ? for all shortcuts"),
    ];
    for (k, v) in tips {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<16}"), Style::default().fg(t.accent)),
            Span::styled(v.to_string(), Style::default().fg(t.text_muted)),
        ]));
    }
    lines
}

fn build_timeline(app: &mut App, width: u16) -> (Vec<Line<'static>>, Vec<TimelineHitSpan>) {
    if app.timeline.is_empty() {
        return (welcome_lines(app, width as usize), Vec::new());
    }
    let item_expands: Vec<bool> = (0..app.timeline.len())
        .map(|i| app.is_item_expanded(i))
        .collect();
    let spinner = app.spinner();
    let ctx = RenderCtx {
        theme: &app.theme,
        terminals: &app.terminals,
        spinner,
        cwd: &app.cwd,
    };
    let epoch = app.render_epoch;
    let w = width as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<TimelineHitSpan> = Vec::with_capacity(app.timeline.len());
    for (idx, item) in app.timeline.iter().enumerate() {
        let is_last = idx + 1 == app.timeline.len();
        let live_thought = app.busy && is_last && matches!(item, TimelineItem::AssistantThought { .. });
        let live = live_thought
            || match item {
                TimelineItem::ToolCall(c) => c.is_running(),
                TimelineItem::Compaction { status, .. } => status == "in_progress",
                _ => false,
            };
        let expanded = item_expands.get(idx).copied().unwrap_or(false);
        let has_override = app.item_expanded.get(idx).copied().flatten().is_some();
        let rev = app.item_revs.get(idx).copied().unwrap_or(0);
        let cached = app
            .render_cache
            .get(idx)
            .and_then(Option::as_ref)
            .filter(|(r, cw, e, exp, _, _)| {
                !live && *r == rev && *cw == width && *e == epoch && *exp == expanded
            })
            .map(|(_, _, _, _, lines, expandable)| (lines.clone(), *expandable));
        let (lines, rendered_expandable) = match cached {
            Some(hit) => hit,
            None => {
                let (l, exp_flag) = render_item(item, w, &ctx, live_thought, expanded);
                if !live
                    && let Some(slot) = app.render_cache.get_mut(idx)
                {
                    *slot = Some((rev, width, epoch, expanded, l.clone(), exp_flag));
                }
                (l, exp_flag)
            }
        };
        if idx > 0 {
            out.push(Line::raw(""));
        }
        let start_line = out.len();
        out.extend(lines);
        let end_line = out.len();
        spans.push(TimelineHitSpan {
            start_line,
            end_line,
            item_idx: idx,
            expandable: rendered_expandable || has_override,
        });
    }
    (out, spans)
}

// ============================================================================
// Top-level layout
// ============================================================================

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.timeline_hit.view_rect = Rect::default();
    app.timeline_hit.activity_rect = Rect::default();
    app.timeline_hit.jump_pill_rect = None;
    app.timeline_hit.completion_rect = None;
    app.timeline_hit.toast_rects.clear();
    app.timeline_hit.spans.clear();

    let t = app.theme.clone();
    render(f, Block::default().style(Style::default().bg(t.bg).fg(t.text)), area);

    let composer_inner_w = area.width.saturating_sub(4).max(10) as usize;
    let (comp_rows, comp_cursor) = wrap_composer(&app.composer.lines, app.composer.row, app.composer.col, composer_inner_w);
    let comp_h = (comp_rows.len().clamp(1, 10) as u16) + 2;

    let queue_h = app.queued.len().min(3) as u16;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(queue_h),
            Constraint::Length(1),
            Constraint::Length(comp_h),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, app, rows[0]);

    let show_sidebar = app.show_sidebar && area.width >= 100;
    let body = if show_sidebar {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Length(SIDEBAR_WIDTH)])
            .split(rows[1]);
        draw_sidebar(f, app, cols[1]);
        cols[0]
    } else {
        rows[1]
    };
    draw_timeline(f, app, body);
    draw_queue(f, app, rows[2]);
    draw_activity(f, app, rows[3], show_sidebar);
    app.timeline_hit.activity_rect = rows[3].intersection(area);
    draw_composer(f, app, rows[4], &comp_rows, comp_cursor);
    draw_footer(f, app, rows[5]);

    if app.completion.is_some() && app.modal.is_none() {
        app.timeline_hit.completion_rect = draw_completion(f, app, rows[4]);
    }
    app.timeline_hit.toast_rects = draw_toasts(f, app, rows[1]);
    if app.modal.is_some() {
        draw_modal(f, app, area);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let (dot, dot_color) = match app.status {
        ConnectionStatus::Connected if !app.opening => ("●", t.success),
        ConnectionStatus::Connecting => (app.spinner(), t.warning),
        ConnectionStatus::Connected => (app.spinner(), t.warning),
        ConnectionStatus::AuthRequired => ("●", t.warning),
        ConnectionStatus::Disconnected => ("●", t.error),
    };
    let right_text = format!("{} ", shorten_home(&app.cwd.to_string_lossy()));
    let agent = app.agent_name();
    let title_room = (area.width as usize)
        .saturating_sub(7 + 3 + agent.width() + 2 + right_text.width().min(40) + 2);
    let left = vec![
        Span::styled(
            " ◆ ACP ",
            Style::default().fg(t.bg).bg(t.primary).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {dot} "), Style::default().fg(dot_color)),
        Span::styled(agent, Style::default().fg(t.text).add_modifier(Modifier::BOLD)),
        Span::styled("  ", Style::default()),
        Span::styled(
            truncate_w(
                &app.session_title.clone().unwrap_or_else(|| "New session".to_string()),
                title_room.max(8),
            ),
            Style::default().fg(t.text_muted),
        ),
    ];
    let lw: usize = left.iter().map(|s| s.width()).sum();
    let avail = (area.width as usize).saturating_sub(lw + 1);
    let right = truncate_w(&right_text, avail);
    let mut spans = left;
    let pad = (area.width as usize).saturating_sub(lw + right.width());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, Style::default().fg(t.text_dim)));
    render(
        f,
        Paragraph::new(Line::from(spans)).style(Style::default().bg(t.panel)),
        area,
    );
}

fn draw_timeline(f: &mut Frame, app: &mut App, area: Rect) {
    let inner = area.inner(Margin { horizontal: 2, vertical: 0 });
    let content_w = inner.width.saturating_sub(1).max(10);
    let (lines, spans) = build_timeline(app, content_w);
    let total = lines.len();
    let height = inner.height as usize;
    let max_scroll = total.saturating_sub(height);

    if let Some(anchor) = app.pending_timeline_anchor.take() {
        if let Some(span) = spans.iter().find(|s| s.item_idx == anchor.item_idx) {
            let mut desired_start = match anchor.old_item_start.checked_sub(anchor.old_start) {
                Some(offset) => span.start_line.saturating_sub(offset),
                None => span.start_line,
            };
            if anchor.expanding && span.end_line > desired_start.saturating_add(height) {
                desired_start = desired_start
                    .max(span.end_line.saturating_sub(height).min(span.start_line));
            }
            app.scroll_from_bottom = total
                .saturating_sub(height.saturating_add(desired_start))
                .min(max_scroll);
        } else {
            app.scroll_from_bottom = app.scroll_from_bottom.min(max_scroll);
        }
    } else {
        if app.scroll_from_bottom > 0 && total > app.last_total_lines {
            app.scroll_from_bottom += total - app.last_total_lines;
        }
        app.scroll_from_bottom = app.scroll_from_bottom.min(max_scroll);
    }
    app.last_total_lines = total;
    app.last_timeline_height = height;

    let start = total.saturating_sub(height + app.scroll_from_bottom);
    let end = (start + height).min(total);
    let visible: Vec<Line<'static>> = lines[start..end].to_vec();
    render(f, Paragraph::new(visible), Rect { width: content_w, ..inner });

    let has_scrollbar = total > height;
    app.timeline_hit.view_rect = Rect {
        width: area.width.saturating_sub(u16::from(has_scrollbar)),
        ..area
    }
    .intersection(f.area());
    app.timeline_hit.inner_y = inner.y;
    app.timeline_hit.visible_start = start;
    app.timeline_hit.spans = spans;

    if has_scrollbar {
        let mut state = ScrollbarState::new(max_scroll).position(max_scroll - app.scroll_from_bottom);
        render_stateful(
            f,
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some(" "))
                .thumb_style(Style::default().fg(app.theme.border)),
            area,
            &mut state,
        );
    }
    if app.scroll_from_bottom > 0 {
        let label = format!(" ↓ {} more lines · esc to jump ", app.scroll_from_bottom);
        let w = label.width() as u16;
        let r = Rect {
            x: area.x + area.width.saturating_sub(w + 2),
            y: area.y + area.height.saturating_sub(1),
            width: w.min(area.width),
            height: 1,
        };
        app.timeline_hit.jump_pill_rect = Some(r.intersection(f.area()));
        render(
            f,
            Paragraph::new(Span::styled(
                label,
                Style::default().fg(app.theme.bg).bg(app.theme.accent),
            )),
            r,
        );
    }
}

fn section(title: &str, t: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        title.to_uppercase(),
        Style::default().fg(t.text_dim).add_modifier(Modifier::BOLD),
    ))
}

fn kv(k: &str, v: String, w: usize, t: &Theme, vcolor: Color) -> Line<'static> {
    let label = format!("{k:<9}");
    Line::from(vec![
        Span::styled(label, Style::default().fg(t.text_muted)),
        Span::styled(truncate_w(&v, w.saturating_sub(9)), Style::default().fg(vcolor)),
    ])
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(t.border))
        .style(Style::default().bg(t.panel));
    let inner = block.inner(area).inner(Margin { horizontal: 1, vertical: 1 });
    render(f, block, area);
    let w = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(section("Session", t));
    let title = app.session_title.clone().unwrap_or_else(|| "New session".to_string());
    lines.extend(plain_wrap(&title, w, Style::default().fg(t.text).add_modifier(Modifier::BOLD), vec![], vec![]).into_iter().take(2));
    if let Some(sid) = &app.session_id {
        lines.push(Line::from(Span::styled(short_id(sid), Style::default().fg(t.text_dim))));
    }
    lines.push(Line::raw(""));

    lines.push(section("Agent", t));
    lines.push(kv("agent", app.profile.id.clone(), w, t, t.text));
    if let Some(m) = app.current_model_label() {
        lines.push(kv("model", m, w, t, t.text));
    }
    if let Some(mode) = app.current_mode_label() {
        let color = t.mode_color(&app.current_mode_id().unwrap_or_default());
        lines.push(kv("mode", mode, w, t, color));
    }
    if let Some(th) = app.thinking_option() {
        lines.push(kv("thinking", th.display_value(), w, t, t.text));
    }
    for opt in &app.config_options {
        let skip = ["mode", "model"].contains(&opt.category.as_deref().unwrap_or(&opt.id))
            || app.thinking_option().is_some_and(|th| th.id == opt.id)
            || opt.id == "mode"
            || opt.id == "model";
        if !skip {
            lines.push(kv(&truncate_w(&opt.name.to_lowercase(), 8), opt.display_value(), w, t, t.text_muted));
        }
    }
    if app.auto_approve {
        lines.push(Line::from(Span::styled("⚠ auto-approve on", Style::default().fg(t.warning))));
    }
    lines.push(Line::raw(""));

    let u = &app.usage;
    if u.context_size > 0 || u.cost.is_some() || u.cumulative_tokens > 0 {
        lines.push(section("Context", t));
        if u.context_size > 0 {
            let pct = (u.context_used as f64 / u.context_size as f64).clamp(0.0, 1.0);
            let bar_w = w.saturating_sub(6).max(6);
            let filled = (pct * bar_w as f64).round() as usize;
            let color = if pct > 0.85 {
                t.error
            } else if pct > 0.6 {
                t.warning
            } else {
                t.success
            };
            lines.push(Line::from(vec![
                Span::styled("█".repeat(filled), Style::default().fg(color)),
                Span::styled("░".repeat(bar_w - filled), Style::default().fg(t.border)),
                Span::styled(format!(" {:>3.0}%", pct * 100.0), Style::default().fg(t.text_muted)),
            ]));
            lines.push(Line::from(Span::styled(
                format!("{} / {} tokens", fmt_tokens(u.context_used), fmt_tokens(u.context_size)),
                Style::default().fg(t.text_dim),
            )));
        } else if u.context_used > 0 {
            lines.push(Line::from(Span::styled(
                format!("{} tokens in context", fmt_tokens(u.context_used)),
                Style::default().fg(t.text_dim),
            )));
        }
        if let Some(last) = &u.last_turn_usage {
            lines.push(Line::from(Span::styled(
                format!(
                    "last turn ↑{} ↓{}",
                    fmt_tokens(last.input_tokens),
                    fmt_tokens(last.output_tokens)
                ),
                Style::default().fg(t.text_dim),
            )));
        }
        if let Some(c) = &u.cost {
            lines.push(Line::from(Span::styled(
                format!("cost {:.2} {}", c.amount, c.currency),
                Style::default().fg(t.text_muted),
            )));
        }
        lines.push(Line::raw(""));
    }

    if !app.plan.is_empty() {
        let done = app.plan.iter().filter(|e| e.status == "completed").count();
        lines.push(section(&format!("Todo {done}/{}", app.plan.len()), t));
        for e in &app.plan {
            let (icon, style) = match e.status.as_str() {
                "completed" => ("☑ ", Style::default().fg(t.text_dim).add_modifier(Modifier::CROSSED_OUT)),
                "in_progress" => ("◐ ", Style::default().fg(t.warning).add_modifier(Modifier::BOLD)),
                _ => ("☐ ", Style::default().fg(t.text)),
            };
            lines.extend(plain_wrap(
                &e.content,
                w,
                style,
                vec![Span::styled(icon, style.remove_modifier(Modifier::CROSSED_OUT))],
                vec![Span::raw("  ")],
            ));
        }
        lines.push(Line::raw(""));
    } else if let Some(md) = &app.plan_markdown {
        lines.push(section("Plan", t));
        lines.extend(render_markdown(md, w, t).into_iter().take(12));
        lines.push(Line::raw(""));
    }

    if !app.subagents.is_empty() {
        lines.push(section("Subagents", t));
        for s in &app.subagents {
            let icon = if s.is_running { app.spinner() } else { "✓" };
            let color = if s.is_running { t.warning } else { t.success };
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(truncate_w(&s.title, w.saturating_sub(2)), Style::default().fg(t.text)),
            ]));
            let detail = s
                .activity
                .clone()
                .filter(|_| s.is_running)
                .unwrap_or_else(|| s.state_label.clone());
            lines.push(Line::from(Span::styled(
                format!("  {}", truncate_w(&detail, w.saturating_sub(2))),
                Style::default().fg(t.text_dim),
            )));
        }
        lines.push(Line::raw(""));
    }

    let files = app.modified_files();
    if !files.is_empty() {
        lines.push(section("Modified files", t));
        for file in files {
            let stats = format!(" +{} -{}", file.added, file.removed);
            let name_w = w.saturating_sub(stats.width());
            lines.push(Line::from(vec![
                Span::styled(truncate_w(&file.path, name_w), Style::default().fg(t.text)),
                Span::styled(format!(" +{}", file.added), Style::default().fg(t.diff_add_fg)),
                Span::styled(format!(" -{}", file.removed), Style::default().fg(t.diff_del_fg)),
            ]));
        }
        lines.push(Line::raw(""));
    }

    let running_terms = app
        .terminals
        .values()
        .filter(|s| s.exit_status.is_none())
        .count();
    if running_terms > 0 {
        lines.push(section("Terminals", t));
        lines.push(Line::from(Span::styled(
            format!("{} {running_terms} running", app.spinner()),
            Style::default().fg(t.warning),
        )));
    }

    render(f, Paragraph::new(lines), inner);
}

fn activity_verb(app: &App) -> String {
    for item in app.timeline.iter().rev() {
        match item {
            TimelineItem::ToolCall(c) if c.is_running() => {
                return truncate_w(&c.title.replace('\n', " "), 50);
            }
            TimelineItem::AssistantThought { .. } => return "Thinking".to_string(),
            TimelineItem::AssistantMessage { .. } => return "Writing".to_string(),
            TimelineItem::UserMessage { .. } => return "Working".to_string(),
            _ => {}
        }
    }
    "Working".to_string()
}

fn draw_queue(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let t = &app.theme;
    let w = area.width as usize;
    let mut lines: Vec<Line<'static>> = app
        .queued
        .iter()
        .take(area.height as usize)
        .map(|q| {
            Line::from(vec![
                Span::styled("  ⏳ ", Style::default().fg(t.accent)),
                Span::styled(
                    truncate_w(&q.replace('\n', " ⏎ "), w.saturating_sub(24)),
                    Style::default().fg(t.text_muted),
                ),
            ])
        })
        .collect();
    if app.queued.len() > area.height as usize
        && let Some(last) = lines.last_mut()
    {
        last.spans.push(Span::styled(
            format!("  +{} more", app.queued.len() - area.height as usize + 1),
            Style::default().fg(t.text_dim),
        ));
    }
    render(f, Paragraph::new(lines), area);
}

fn draw_activity(f: &mut Frame, app: &App, area: Rect, sidebar_visible: bool) {
    let t = &app.theme;
    let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
    if app.quit_armed_at.is_some() {
        spans.push(Span::styled("Press Ctrl+C again to exit", Style::default().fg(t.warning)));
    } else if app.busy {
        let elapsed = app.turn_started.map(|s| s.elapsed()).unwrap_or_default();
        spans.push(Span::styled(format!("{} ", app.spinner()), Style::default().fg(t.primary)));
        spans.push(Span::styled(
            format!("{}…", activity_verb(app)),
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("  {} · esc to interrupt", fmt_duration(elapsed)),
            Style::default().fg(t.text_dim),
        ));
    } else if app.status == ConnectionStatus::Connecting {
        spans.push(Span::styled(
            format!("{} Starting {}…", app.spinner(), app.profile.id),
            Style::default().fg(t.text_muted),
        ));
    } else if app.opening {
        spans.push(Span::styled(
            format!("{} Opening session…", app.spinner()),
            Style::default().fg(t.text_muted),
        ));
    } else if app.status == ConnectionStatus::AuthRequired {
        spans.push(Span::styled("Authentication required — type /login", Style::default().fg(t.warning)));
    } else if app.status == ConnectionStatus::Disconnected {
        spans.push(Span::styled("Disconnected — ctrl+p → Reconnect", Style::default().fg(t.error)));
    } else if let Some(d) = app.last_turn_duration {
        let mut s = format!("✓ Done in {}", fmt_duration(d));
        if let Some(u) = &app.usage.last_turn_usage {
            s.push_str(&format!(" · {} tokens", fmt_tokens(u.total_tokens)));
        }
        spans.push(Span::styled(s, Style::default().fg(t.text_dim)));
    }
    if !app.queued.is_empty() {
        spans.push(Span::styled(
            format!("  · {} queued, sent after this turn (↑ on empty input edits)", app.queued.len()),
            Style::default().fg(t.accent),
        ));
    }
    if !sidebar_visible && !app.plan.is_empty() {
        let done = app.plan.iter().filter(|e| e.status == "completed").count();
        let current = app
            .plan
            .iter()
            .find(|e| e.status == "in_progress")
            .map(|e| format!(" · {}", truncate_w(&e.content, 30)))
            .unwrap_or_default();
        spans.push(Span::styled(
            format!("  ☑ {done}/{}{current}", app.plan.len()),
            Style::default().fg(t.text_muted),
        ));
    }
    render(f, Paragraph::new(Line::from(spans)), area);
}

/// Wrap composer lines to `width` columns and return the visual rows plus the cursor's (row, col).
fn wrap_composer(lines: &[String], row: usize, col: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let mut rows = Vec::new();
    let mut cursor = (0, 0);
    for (li, line) in lines.iter().enumerate() {
        let mut cur = String::new();
        let mut cur_w = 0;
        let start_row = rows.len();
        let mut found = false;
        for (bi, ch) in line.char_indices() {
            if li == row && bi == col && !found {
                cursor = (rows.len(), cur_w);
                found = true;
            }
            let cw = ch.width().unwrap_or(0);
            if cur_w + cw > width {
                rows.push(std::mem::take(&mut cur));
                cur_w = 0;
                if li == row && bi == col {
                    cursor = (rows.len(), 0);
                }
            }
            cur.push(ch);
            cur_w += cw;
        }
        if li == row && !found {
            if cur_w >= width {
                rows.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            cursor = (rows.len(), cur_w);
        }
        rows.push(cur);
        let _ = start_row;
    }
    (rows, cursor)
}

fn draw_composer(f: &mut Frame, app: &App, area: Rect, rows: &[String], cursor: (usize, usize)) {
    let t = &app.theme;
    let mode_id = app.current_mode_id().unwrap_or_default();
    let mode_color = t.mode_color(&mode_id);
    let border_color = if app.modal.is_some() { t.border } else { mode_color };
    let mut title = vec![Span::raw(" ")];
    if let Some(m) = app.current_mode_label() {
        title.push(Span::styled(m, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)));
        title.push(Span::raw(" "));
    }
    if app.auto_approve {
        title.push(Span::styled("⚠ auto-approve ", Style::default().fg(t.warning)));
    }
    let hint = if app.busy {
        " enter: steer/queue · esc: interrupt "
    } else {
        " enter send · shift+enter newline "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(title))
        .title_bottom(Line::from(Span::styled(hint, Style::default().fg(t.text_dim))).right_aligned())
        .style(Style::default().bg(t.bg));
    let inner = block.inner(area);
    render(f, block, area);

    let height = inner.height as usize;
    let scroll = cursor.0.saturating_sub(height.saturating_sub(1));
    let prompt_style = Style::default().fg(mode_color).add_modifier(Modifier::BOLD);
    let text_area = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    render(f, Paragraph::new(Span::styled("›", prompt_style)), Rect { width: 2, height: inner.height.min(1), ..inner });

    if app.composer.is_empty() {
        let placeholder = match app.status {
            ConnectionStatus::Connected if !app.opening => {
                if app.busy {
                    "Type to steer or queue a message…"
                } else {
                    "Ask anything…  / for commands · @ to attach files"
                }
            }
            ConnectionStatus::AuthRequired => "Authentication required — type /login",
            ConnectionStatus::Disconnected => "Agent disconnected — ctrl+p → Reconnect",
            _ => "Connecting… you can already type",
        };
        render(
            f,
            Paragraph::new(Span::styled(placeholder, Style::default().fg(t.text_dim))),
            text_area,
        );
    } else {
        let visible: Vec<Line<'static>> = rows
            .iter()
            .skip(scroll)
            .take(height)
            .map(|r| {
                let style = if r.trim_start().starts_with('/') && rows.len() == 1 {
                    Style::default().fg(t.accent)
                } else {
                    Style::default().fg(t.text)
                };
                Line::from(Span::styled(r.clone(), style))
            })
            .collect();
        render(f, Paragraph::new(visible), text_area);
    }
    if app.modal.is_none() {
        let cy = cursor.0.saturating_sub(scroll) as u16;
        f.set_cursor_position((
            text_area.x + (cursor.1 as u16).min(text_area.width.saturating_sub(1)),
            text_area.y + cy.min(text_area.height.saturating_sub(1)),
        ));
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let mut left: Vec<Span<'static>> = vec![Span::raw(" ")];
    if let Some(mode) = app.current_mode_label() {
        let color = t.mode_color(&app.current_mode_id().unwrap_or_default());
        left.push(Span::styled(
            format!(" {mode} "),
            Style::default().fg(t.bg).bg(color).add_modifier(Modifier::BOLD),
        ));
        left.push(Span::raw(" "));
    }
    if let Some(m) = app.current_model_label() {
        left.push(Span::styled(m, Style::default().fg(t.text)));
    }
    if let Some(th) = app.thinking_option() {
        left.push(Span::styled(format!(" · {}", th.display_value()), Style::default().fg(t.text_muted)));
    }
    let u = &app.usage;
    if u.context_size > 0 {
        let pct = u.context_used as f64 / u.context_size as f64 * 100.0;
        left.push(Span::styled(format!(" · ctx {pct:.0}%"), Style::default().fg(t.text_muted)));
    }
    if let Some(c) = &u.cost {
        left.push(Span::styled(format!(" · ${:.2}", c.amount), Style::default().fg(t.text_muted)));
    }
    let right = "ctrl+p commands · ctrl+s sessions · ctrl+g agents · ? help ";
    let lw: usize = left.iter().map(|s| s.width()).sum();
    let avail = (area.width as usize).saturating_sub(lw + 1);
    let right = if right.width() <= avail { right.to_string() } else { "? help ".to_string() };
    let pad = (area.width as usize).saturating_sub(lw + right.width());
    left.push(Span::raw(" ".repeat(pad)));
    left.push(Span::styled(right, Style::default().fg(t.text_dim)));
    render(f, Paragraph::new(Line::from(left)).style(Style::default().bg(t.panel)), area);
}

fn draw_completion(f: &mut Frame, app: &App, composer_area: Rect) -> Option<Rect> {
    let popup = app.completion.as_ref()?;
    let t = &app.theme;
    let max_items = 10usize;
    let n = popup.candidates.len().min(max_items);
    let h = n as u16 + 2;
    let w = composer_area.width.saturating_sub(4).min(100);
    let area = Rect {
        x: composer_area.x + 2,
        y: composer_area.y.saturating_sub(h),
        width: w,
        height: h,
    };
    let start = popup.selected.saturating_sub(max_items - 1);
    let title = match popup.kind {
        CompletionPopupKind::SlashCommand => "commands".to_string(),
        CompletionPopupKind::FileMention => "files".to_string(),
        CompletionPopupKind::CommandArg => app
            .composer
            .active_command_arg()
            .map(|(cmd, _)| format!("/{cmd}"))
            .unwrap_or_else(|| "values".to_string()),
    };
    let counter = format!(" {title} · {}/{} ", popup.selected + 1, popup.candidates.len());
    let hint = match popup.kind {
        CompletionPopupKind::CommandArg => " ↑↓ choose · enter apply · tab complete · esc close ",
        CompletionPopupKind::FileMention => " ↑↓ choose · tab/enter attach · esc close ",
        CompletionPopupKind::SlashCommand => " ↑↓ choose · enter run · tab complete · esc close ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_focused))
        .title(Span::styled(counter, Style::default().fg(t.text_muted)))
        .title_bottom(Line::from(Span::styled(hint, Style::default().fg(t.text_dim))).right_aligned())
        .style(Style::default().bg(t.surface));
    let inner = block.inner(area);
    render(f, Clear, area);
    render(f, block, area);
    let iw = inner.width as usize;
    let label_col = popup
        .candidates
        .iter()
        .map(|c| c.label.width())
        .max()
        .unwrap_or(10)
        .clamp(10, (iw * 3 / 5).max(10));
    let lines: Vec<Line<'static>> = popup
        .candidates
        .iter()
        .enumerate()
        .skip(start)
        .take(max_items)
        .map(|(i, c)| {
            let selected = i == popup.selected;
            let bg = if selected { t.selection_bg } else { t.surface };
            let tag = c.tag.clone().unwrap_or_default();
            let tag_color = match tag.as_str() {
                "current" => t.success,
                "agent" => t.accent,
                "tui" => t.secondary,
                "not found" => t.error,
                _ => t.text_dim,
            };
            let label = truncate_w(&c.label, label_col);
            let pad = label_col.saturating_sub(label.width());
            let detail_w = iw.saturating_sub(label_col + tag.width() + 5);
            let detail = truncate_w(&c.detail, detail_w);
            let fill = detail_w.saturating_sub(detail.width());
            let line = Line::from(vec![
                Span::styled(if selected { "›" } else { " " }, Style::default().fg(t.primary)),
                Span::styled(
                    format!("{label}{}", " ".repeat(pad)),
                    Style::default()
                        .fg(if selected { t.primary } else { t.text })
                        .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() }),
                ),
                Span::styled(format!("  {detail}{}", " ".repeat(fill)), Style::default().fg(t.text_dim)),
                Span::styled(format!(" {tag}"), Style::default().fg(tag_color)),
            ]);
            pad_bg(line, iw, bg)
        })
        .collect();
    render(f, Paragraph::new(lines), inner);
    Some(area.intersection(f.area()))
}

fn draw_toasts(f: &mut Frame, app: &App, area: Rect) -> Vec<Rect> {
    let t = &app.theme;
    let mut rects = Vec::new();
    let mut y = area.y + 1;
    for toast in app.toasts.iter().rev().take(3) {
        let text = truncate_w(&toast.text, 60);
        let w = (text.width() as u16 + 4).min(area.width);
        let r = Rect {
            x: area.x + area.width.saturating_sub(w + 1),
            y,
            width: w,
            height: 3,
        };
        if r.y + r.height > area.y + area.height {
            break;
        }
        let color = if toast.is_error { t.error } else { t.primary };
        rects.push(r.intersection(f.area()));
        render(f, Clear, r);
        render(
            f,
            Paragraph::new(Span::styled(text, Style::default().fg(t.text))).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(color))
                    .style(Style::default().bg(t.surface)),
            ),
            r,
        );
        y += 3;
    }
    rects
}

// ============================================================================
// Modals
// ============================================================================

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 3,
        width: w,
        height: h,
    }
}

fn modal_block(title: &str, hint: &str, t: &Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_focused))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(Span::styled(format!(" {hint} "), Style::default().fg(t.text_dim))).right_aligned())
        .style(Style::default().bg(t.surface))
}

fn query_line(query: &str, placeholder: &str, t: &Theme) -> Line<'static> {
    if query.is_empty() {
        Line::from(vec![
            Span::styled("› ", Style::default().fg(t.primary)),
            Span::styled(placeholder.to_string(), Style::default().fg(t.text_dim)),
        ])
    } else {
        Line::from(vec![
            Span::styled("› ", Style::default().fg(t.primary)),
            Span::styled(query.to_string(), Style::default().fg(t.text)),
            Span::styled("▏", Style::default().fg(t.primary)),
        ])
    }
}

fn list_window(selected: usize, total: usize, height: usize) -> usize {
    if total <= height {
        0
    } else {
        selected.saturating_sub(height / 2).min(total - height)
    }
}

fn draw_modal(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    match app.modal.as_ref() {
        Some(ActiveModal::CommandPalette { query, items, selected }) => {
            let r = centered(area, 80, 22);
            render(f, Clear, r);
            let block = modal_block("Commands", "↑↓ select · enter run · esc close", &t);
            let inner = block.inner(r);
            render(f, block, r);
            let filtered = App::filtered_palette(query, items);
            let mut lines = vec![query_line(query, "Search commands…", &t), Line::raw("")];
            let h = inner.height.saturating_sub(2) as usize;
            let start = list_window(*selected, filtered.len(), h);
            let iw = inner.width as usize;
            for (pos, &idx) in filtered.iter().enumerate().skip(start).take(h) {
                let it = &items[idx];
                let sel = pos == *selected;
                let bg = if sel { t.selection_bg } else { t.surface };
                let right = it.shortcut.clone().unwrap_or_else(|| it.category.clone());
                let left_w = iw.saturating_sub(right.width() + 2);
                let left_text = truncate_w(&format!(" {}  {}", it.title, it.subtitle), left_w);
                let title_len = it.title.width() + 1;
                let (title_part, sub_part) = if left_text.width() > title_len {
                    let split = left_text.char_indices().nth(it.title.chars().count() + 1).map(|(i, _)| i).unwrap_or(left_text.len());
                    (left_text[..split].to_string(), left_text[split..].to_string())
                } else {
                    (left_text.clone(), String::new())
                };
                let used = title_part.width() + sub_part.width();
                let line = Line::from(vec![
                    Span::styled(title_part, Style::default().fg(if sel { t.primary } else { t.text }).add_modifier(Modifier::BOLD)),
                    Span::styled(sub_part, Style::default().fg(t.text_muted)),
                    Span::raw(" ".repeat(iw.saturating_sub(used + right.width() + 1))),
                    Span::styled(right, Style::default().fg(t.text_dim)),
                ]);
                lines.push(pad_bg(line, iw, bg));
            }
            render(f, Paragraph::new(lines), inner);
        }
        Some(ActiveModal::Picker(p)) => {
            let r = centered(area, 76, (p.items.len().min(24) as u16 + 6).clamp(8, 24));
            render(f, Clear, r);
            let block = modal_block(&p.title, "type to filter · enter select · esc close", &t);
            let inner = block.inner(r);
            render(f, block, r);
            let filtered = p.filtered_indices();
            let mut lines = vec![query_line(&p.query, "Filter…", &t), Line::raw("")];
            let h = inner.height.saturating_sub(2) as usize;
            let start = list_window(p.selected, filtered.len(), h);
            let iw = inner.width as usize;
            for (pos, &idx) in filtered.iter().enumerate().skip(start).take(h) {
                let it = &p.items[idx];
                let sel = pos == p.selected;
                let bg = if sel { t.selection_bg } else { t.surface };
                let marker = if it.current { "● " } else { "  " };
                let badge = it.badge.clone().map(|b| format!(" [{b}]")).unwrap_or_default();
                let label_w = if it.description.is_empty() { iw.saturating_sub(4 + badge.width()) } else { iw / 2 };
                let label = truncate_w(&it.label, label_w);
                let desc = truncate_w(&it.description, iw.saturating_sub(label.width() + badge.width() + 6));
                let badge_color = if badge.contains("not found") { t.error } else { t.accent };
                let line = Line::from(vec![
                    Span::styled(format!(" {marker}"), Style::default().fg(t.success)),
                    Span::styled(label, Style::default().fg(if sel { t.primary } else { t.text }).add_modifier(Modifier::BOLD)),
                    Span::styled(badge, Style::default().fg(badge_color)),
                    Span::styled(format!("  {desc}"), Style::default().fg(t.text_dim)),
                ]);
                lines.push(pad_bg(line, iw, bg));
            }
            if filtered.is_empty() {
                lines.push(Line::from(Span::styled("  No matches", Style::default().fg(t.text_dim))));
            }
            render(f, Paragraph::new(lines), inner);
        }
        Some(ActiveModal::Sessions(s)) => {
            let r = centered(area, 96, 26);
            render(f, Clear, r);
            let block = modal_block("Sessions", "enter open · ctrl+d delete · esc close", &t);
            let inner = block.inner(r);
            render(f, block, r);
            let filtered = s.filtered_indices();
            let mut lines = vec![query_line(&s.query, "Search sessions…", &t), Line::raw("")];
            let iw = inner.width as usize;
            if s.loading {
                lines.push(Line::from(Span::styled(
                    format!("  {} Loading sessions…", app.spinner()),
                    Style::default().fg(t.text_muted),
                )));
            } else if filtered.is_empty() {
                lines.push(Line::from(Span::styled("  No sessions found", Style::default().fg(t.text_dim))));
            }
            let h = (inner.height.saturating_sub(2) / 2) as usize;
            let start = list_window(s.selected, filtered.len(), h);
            for (pos, &idx) in filtered.iter().enumerate().skip(start).take(h) {
                let info = &s.sessions[idx];
                let sel = pos == s.selected;
                let bg = if sel { t.selection_bg } else { t.surface };
                let current = app.session_id.as_deref() == Some(info.session_id.as_str());
                let title = info
                    .title
                    .clone()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| "Untitled session".to_string());
                let when = info.updated_at.as_deref().map(relative_time).unwrap_or_default();
                let l1 = Line::from(vec![
                    Span::styled(if current { " ● " } else { "   " }, Style::default().fg(t.success)),
                    Span::styled(
                        truncate_w(&title.replace('\n', " "), iw.saturating_sub(when.width() + 6)),
                        Style::default().fg(if sel { t.primary } else { t.text }).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {when}"), Style::default().fg(t.text_dim)),
                ]);
                let l2 = Line::from(Span::styled(
                    truncate_w(&format!("   {} · {}", short_id(&info.session_id), shorten_home(&info.cwd)), iw),
                    Style::default().fg(t.text_dim),
                ));
                lines.push(pad_bg(l1, iw, bg));
                lines.push(pad_bg(l2, iw, bg));
            }
            render(f, Paragraph::new(lines), inner);
        }
        Some(ActiveModal::Config(c)) => {
            let n = app.config_options.len().min(26) as u16;
            let r = centered(area, 80, (n + 6).clamp(8, 26));
            render(f, Clear, r);
            let block = modal_block("Session config", "↑↓ select · enter change · esc close", &t);
            let inner = block.inner(r);
            render(f, block, r);
            let iw = inner.width as usize;
            let mut lines = Vec::new();
            for (i, opt) in app.config_options.iter().enumerate() {
                let sel = i == c.selected;
                let bg = if sel { t.selection_bg } else { t.surface };
                let value = opt.display_value();
                let name = truncate_w(&opt.name, iw.saturating_sub(value.width() + 4));
                let pad = iw.saturating_sub(name.width() + value.width() + 3);
                let line = Line::from(vec![
                    Span::styled(format!(" {name}"), Style::default().fg(if sel { t.primary } else { t.text }).add_modifier(Modifier::BOLD)),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(value, Style::default().fg(t.accent)),
                ]);
                lines.push(pad_bg(line, iw, bg));
                if sel && let Some(d) = opt.description.as_ref().filter(|d| !d.is_empty()) {
                    lines.push(pad_bg(
                        Line::from(Span::styled(format!("   {}", truncate_w(d, iw.saturating_sub(4))), Style::default().fg(t.text_dim))),
                        iw,
                        bg,
                    ));
                }
            }
            render(f, Paragraph::new(lines), inner);
        }
        Some(ActiveModal::Permission(_)) => draw_permission(f, app, area),
        Some(ActiveModal::Elicitation(_)) => draw_elicitation(f, app, area),
        Some(ActiveModal::Logs(l)) => {
            let r = centered(area, area.width.saturating_sub(6), area.height.saturating_sub(4));
            render(f, Clear, r);
            let block = modal_block("ACP protocol log", "type to filter · ↑↓/pgup/pgdn scroll · esc close", &t);
            let inner = block.inner(r);
            render(f, block, r);
            let iw = inner.width as usize;
            let filter = l.filter.to_lowercase();
            let entries: Vec<&crate::acp::client::RpcLogEntry> = app
                .rpc_logs
                .iter()
                .filter(|e| filter.is_empty() || e.summary.to_lowercase().contains(&filter) || e.payload.to_lowercase().contains(&filter))
                .collect();
            let h = inner.height.saturating_sub(2) as usize;
            let total = entries.len();
            let end = total.saturating_sub(l.scroll.min(total.saturating_sub(h.min(total))));
            let start = end.saturating_sub(h);
            let mut lines = vec![
                query_line(&l.filter, &format!("Filter {} entries…", app.rpc_logs.len()), &t),
                Line::raw(""),
            ];
            for e in &entries[start..end] {
                let (arrow, color) = match e.direction {
                    RpcDirection::Outgoing => ("→", t.primary),
                    RpcDirection::Incoming => ("←", t.success),
                    RpcDirection::Stderr => ("!", t.warning),
                };
                let head = format!("{} {arrow} {}  ", e.timestamp, e.summary);
                let payload = truncate_w(&e.payload.replace('\n', " "), iw.saturating_sub(head.width()));
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", e.timestamp), Style::default().fg(t.text_dim)),
                    Span::styled(format!("{arrow} "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("{}  ", e.summary), Style::default().fg(t.text)),
                    Span::styled(payload, Style::default().fg(t.text_dim)),
                ]));
            }
            render(f, Paragraph::new(lines), inner);
        }
        Some(ActiveModal::Help(_)) => draw_help(f, app, area),
        None => {}
    }
}

const HELP_GROUPS: &[(&str, &[(&str, &str)])] = &[
    ("Composer", &[
        ("enter", "Send the message (steer or queue while the agent works)"),
        ("shift+enter · alt+enter · ctrl+j", "New line (or end the line with \\)"),
        ("↑ / ↓", "Prompt history · edit the last queued message"),
        ("ctrl+a / ctrl+e", "Start / end of the line"),
        ("ctrl+← / ctrl+→ · alt+b / alt+f", "Previous / next word"),
        ("ctrl+w · alt+d · ctrl+u · ctrl+k", "Delete word back · word forward · to line start · to line end"),
        ("ctrl+z", "Undo"),
        ("ctrl+r", "Search prompt history"),
        ("/", "Slash commands (agent + TUI); /model, /mode, /thinking, /agent, /theme complete values"),
        ("@path", "Attach a file (@file:10-20 for a line range)"),
    ]),
    ("Agent", &[
        ("esc", "Interrupt the running turn (esc esc clears the input)"),
        ("shift+tab", "Cycle the session mode"),
        ("ctrl+l", "Select the model"),
        ("ctrl+g", "Switch the ACP agent"),
        ("ctrl+n", "New session"),
        ("ctrl+s", "Browse and resume sessions"),
    ]),
    ("View", &[
        ("ctrl+p", "Command palette"),
        ("click step", "Expand / collapse a tool or thinking step"),
        ("ctrl+o", "Expand / collapse all tool outputs"),
        ("ctrl+t", "Expand / collapse all thinking"),
        ("ctrl+b", "Toggle the sidebar"),
        ("pgup / pgdn · shift+↑↓ · wheel", "Scroll the conversation"),
        ("ctrl+home / ctrl+end", "Jump to the top / bottom"),
        ("F12", "ACP protocol log"),
    ]),
    ("Permissions", &[
        ("1-9 · enter", "Pick an option"),
        ("y · a · n", "Allow once · always allow · reject"),
        ("esc", "Reject once"),
        ("/yolo", "Toggle auto-approve"),
    ]),
    ("App", &[
        ("ctrl+c", "Clear input · interrupt · press twice to quit"),
        ("ctrl+d", "Quit (empty input)"),
    ]),
];

fn help_key_col() -> usize {
    HELP_GROUPS
        .iter()
        .flat_map(|(_, items)| items.iter().map(|(k, _)| k.width()))
        .max()
        .unwrap_or(0)
        + 2
}

fn help_ideal_width(key_col: usize) -> u16 {
    let max_line_w = HELP_GROUPS
        .iter()
        .flat_map(|(_, items)| items.iter().map(|(_, v)| 3 + key_col + v.width()))
        .max()
        .unwrap_or(0);
    // Reserve space for borders, right breathing room, and an optional scrollbar.
    (max_line_w + 4).min(u16::MAX as usize) as u16
}

fn help_lines(content_w: usize, key_col: usize, t: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let key_style = Style::default().fg(t.accent);
    let desc_style = Style::default().fg(t.text_muted);
    let two_col = content_w >= 3 + key_col + 16;

    for (g, items) in HELP_GROUPS {
        lines.push(Line::from(Span::styled(
            format!(" {g}"),
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        )));
        for (k, v) in *items {
            if two_col {
                let pad = key_col.saturating_sub(k.width());
                let first = vec![Span::styled(format!("   {k}{}", " ".repeat(pad)), key_style)];
                let cont = vec![Span::raw(" ".repeat(3 + key_col))];
                lines.extend(wrap_styled_spans(
                    vec![Span::styled((*v).to_string(), desc_style)],
                    content_w,
                    first,
                    cont,
                ));
            } else {
                lines.extend(wrap_styled_spans(
                    vec![Span::styled((*k).to_string(), key_style)],
                    content_w,
                    vec![Span::raw("   ")],
                    vec![Span::raw("   ")],
                ));
                lines.extend(wrap_styled_spans(
                    vec![Span::styled((*v).to_string(), desc_style)],
                    content_w,
                    vec![Span::raw("     ")],
                    vec![Span::raw("     ")],
                ));
            }
        }
        lines.push(Line::raw(""));
    }
    if matches!(lines.last(), Some(l) if l.width() == 0) {
        lines.pop();
    }
    lines
}

fn draw_help(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let Some(ActiveModal::Help(h)) = app.modal.as_mut() else {
        return;
    };

    let key_col = help_key_col();
    let ideal_w = help_ideal_width(key_col);
    let max_h = if area.height > 6 {
        area.height - 4
    } else {
        area.height
    };

    let probe = centered(area, ideal_w, max_h);
    let inner_w = probe.width.saturating_sub(2) as usize;
    let max_inner_h = probe.height.saturating_sub(2) as usize;

    let mut lines = help_lines(inner_w.saturating_sub(1), key_col, &t);
    if lines.len() > max_inner_h && inner_w > 1 {
        lines = help_lines(inner_w.saturating_sub(2), key_col, &t);
    }

    let content_h = (lines.len().min(u16::MAX as usize - 2) as u16).saturating_add(2);
    let r = centered(area, ideal_w, content_h.min(max_h));
    let total = lines.len();
    let ih = r.height.saturating_sub(2) as usize;
    let hint = if total > ih {
        "↑↓ scroll · esc close"
    } else {
        "esc close"
    };
    let block = modal_block("Keyboard shortcuts", hint, &t);
    let inner = block.inner(r);
    render(f, Clear, r);
    render(f, block, r);

    let max_scroll = total.saturating_sub(ih);
    h.scroll = h.scroll.min(max_scroll);

    let visible: Vec<Line<'static>> = lines.into_iter().skip(h.scroll).take(ih).collect();
    let text_area = if total > ih && inner.width > 1 {
        Rect {
            width: inner.width - 1,
            ..inner
        }
    } else {
        inner
    };
    render(f, Paragraph::new(visible), text_area);

    if total > ih && inner.width > 0 && inner.height > 0 {
        let mut state = ScrollbarState::new(max_scroll).position(h.scroll);
        render_stateful(
            f,
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some(" "))
                .thumb_style(Style::default().fg(t.border)),
            inner,
            &mut state,
        );
    }
}

fn draw_permission(f: &mut Frame, app: &App, area: Rect) {
    let Some(ActiveModal::Permission(p)) = app.modal.as_ref() else {
        return;
    };
    let t = &app.theme;
    let req = &p.pending.request;
    let tc = &req.tool_call;
    let card = app
        .tool_index
        .get(&tc.tool_call_id)
        .and_then(|&i| app.timeline.get(i))
        .and_then(|item| match item {
            TimelineItem::ToolCall(c) => Some(c),
            _ => None,
        });
    let w = area.width.saturating_sub(8).min(110);
    let iw = w.saturating_sub(4) as usize;

    let mut body: Vec<Line<'static>> = Vec::new();
    let kind = tc
        .kind
        .clone()
        .or_else(|| card.map(|c| c.kind.clone()))
        .unwrap_or_else(|| "other".to_string());
    let title = tc
        .title
        .clone()
        .or_else(|| card.map(|c| c.title.clone()))
        .unwrap_or_else(|| "Tool call".to_string());
    body.push(Line::from(vec![
        Span::styled(
            format!("{} ", kind.to_uppercase()),
            Style::default().fg(kind_color(&kind, t)).add_modifier(Modifier::BOLD),
        ),
        Span::styled(truncate_w(&title.replace('\n', " "), iw.saturating_sub(10)), Style::default().fg(t.text).add_modifier(Modifier::BOLD)),
    ]));
    body.push(Line::raw(""));

    let raw_input = tc.raw_input.as_ref().or_else(|| card.and_then(|c| c.raw_input.as_ref()));
    let mut detail: Vec<Line<'static>> = Vec::new();
    if let Some(cmd) = raw_input.and_then(|r| r.get("command")).and_then(|c| c.as_str()) {
        detail.extend(plain_wrap(
            cmd,
            iw,
            Style::default().fg(t.text).bg(t.code_bg),
            vec![Span::styled("$ ", Style::default().fg(t.secondary))],
            vec![Span::raw("  ")],
        ));
    }
    if let Some(c) = card {
        for d in &c.diffs {
            let s = render_unified_diff(d.old_text.as_deref(), &d.new_text, iw, None, t);
            detail.push(Line::from(vec![
                Span::styled(app.rel_path(&d.path), Style::default().fg(t.text).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  +{}", s.added), Style::default().fg(t.diff_add_fg)),
                Span::styled(format!(" -{}", s.removed), Style::default().fg(t.diff_del_fg)),
            ]));
            detail.extend(s.lines);
        }
        if detail.is_empty() {
            for text in &c.text_outputs {
                detail.extend(render_markdown(text, iw, t));
            }
        }
    }
    if detail.is_empty()
        && let Some(raw) = raw_input
    {
        let s = serde_json::to_string_pretty(raw).unwrap_or_default();
        detail.extend(plain_wrap(&s, iw, Style::default().fg(t.text_muted), vec![], vec![]));
    }

    let max_detail = (area.height as usize).saturating_sub(req.options.len() + 12).max(4);
    let scroll = p.diff_scroll.min(detail.len().saturating_sub(max_detail));
    let more = detail.len().saturating_sub(scroll + max_detail);
    body.extend(detail.into_iter().skip(scroll).take(max_detail));
    if more > 0 {
        body.push(Line::from(Span::styled(
            format!("… +{more} lines (pgdn to scroll)"),
            Style::default().fg(t.text_dim).add_modifier(Modifier::ITALIC),
        )));
    }
    body.push(Line::raw(""));
    body.push(Line::from(Span::styled(
        "Allow this action?",
        Style::default().fg(t.text).add_modifier(Modifier::BOLD),
    )));
    for (i, o) in req.options.iter().enumerate() {
        let sel = i == p.selected;
        let (icon, color) = if o.kind.starts_with("allow") {
            ("✓", t.success)
        } else {
            ("✗", t.error)
        };
        let suffix = match o.kind.as_str() {
            "allow_always" | "reject_always" => " (always)",
            _ => "",
        };
        let line = Line::from(vec![
            Span::styled(if sel { " ❯ " } else { "   " }, Style::default().fg(t.primary)),
            Span::styled(format!("{}. ", i + 1), Style::default().fg(t.text_dim)),
            Span::styled(format!("{icon} "), Style::default().fg(color)),
            Span::styled(
                o.name.clone(),
                Style::default()
                    .fg(if sel { t.primary } else { t.text })
                    .add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() }),
            ),
            Span::styled(suffix, Style::default().fg(t.text_dim)),
        ]);
        body.push(if sel { pad_bg(line, iw, t.selection_bg) } else { line });
    }

    let h = (body.len().min(u16::MAX as usize - 2) as u16 + 2).min(area.height.saturating_sub(2));
    let r = centered(area, w, h);
    render(f, Clear, r);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.warning))
        .title(Span::styled(" ⚠ Permission required ", Style::default().fg(t.warning).add_modifier(Modifier::BOLD)))
        .title_bottom(Line::from(Span::styled(" ↑↓ select · enter confirm · 1-9 pick · esc reject ", Style::default().fg(t.text_dim))).right_aligned())
        .style(Style::default().bg(t.surface));
    let inner = block.inner(r).inner(Margin { horizontal: 1, vertical: 0 });
    render(f, block, r);
    render(f, Paragraph::new(body), inner);
}

fn draw_elicitation(f: &mut Frame, app: &App, area: Rect) {
    let Some(ActiveModal::Elicitation(e)) = app.modal.as_ref() else {
        return;
    };
    let t = &app.theme;
    let w = area.width.saturating_sub(8).min(90);
    let iw = w.saturating_sub(4) as usize;
    let mut body: Vec<Line<'static>> = plain_wrap(
        &e.pending.request.message,
        iw,
        Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        vec![],
        vec![],
    );
    body.push(Line::raw(""));
    let mut cursor: Option<(u16, u16)> = None;
    if e.is_url_mode {
        let url = e.pending.request.url.clone().unwrap_or_default();
        body.extend(plain_wrap(&url, iw, Style::default().fg(t.accent).add_modifier(Modifier::UNDERLINED), vec![], vec![]));
        body.push(Line::raw(""));
        body.push(Line::from(Span::styled(
            "Enter opens the link in your browser · Esc declines",
            Style::default().fg(t.text_muted),
        )));
    } else {
        for (i, field) in e.fields.iter().enumerate() {
            let focused = i == e.focused_field;
            let label_style = Style::default()
                .fg(if focused { t.primary } else { t.text })
                .add_modifier(Modifier::BOLD);
            body.push(Line::from(vec![
                Span::styled(if focused { "❯ " } else { "  " }, Style::default().fg(t.primary)),
                Span::styled(field.label.clone(), label_style),
                Span::styled(if field.required { " *" } else { "" }, Style::default().fg(t.error)),
            ]));
            if let Some(d) = &field.description {
                body.extend(plain_wrap(d, iw, Style::default().fg(t.text_dim), vec![Span::raw("  ")], vec![Span::raw("  ")]));
            }
            match &field.kind {
                ElicitationFieldKind::Text | ElicitationFieldKind::Number { .. } => {
                    let val = truncate_w(&field.text_val, iw.saturating_sub(6));
                    if focused {
                        cursor = Some((body.len() as u16, 4 + val.width() as u16));
                    }
                    body.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            format!(" {val:<width$}", width = iw.saturating_sub(4)),
                            Style::default().fg(t.text).bg(if focused { t.selection_bg } else { t.code_bg }),
                        ),
                    ]));
                }
                ElicitationFieldKind::Boolean => {
                    body.push(Line::from(Span::styled(
                        format!("  [{}] {}", if field.bool_val { "x" } else { " " }, if field.bool_val { "yes" } else { "no" }),
                        Style::default().fg(t.text),
                    )));
                }
                ElicitationFieldKind::SingleSelect { options } => {
                    let label = options.get(field.select_idx).map(|(_, l)| l.clone()).unwrap_or_default();
                    body.push(Line::from(vec![
                        Span::styled("  ◀ ", Style::default().fg(t.text_dim)),
                        Span::styled(label, Style::default().fg(t.accent).add_modifier(Modifier::BOLD)),
                        Span::styled(format!(" ▶  ({}/{})", field.select_idx + 1, options.len()), Style::default().fg(t.text_dim)),
                    ]));
                }
                ElicitationFieldKind::MultiSelect { options, checked, cursor: c } => {
                    for (oi, ((_, label), on)) in options.iter().zip(checked).enumerate() {
                        let here = focused && oi == *c;
                        body.push(Line::from(Span::styled(
                            format!("  {}[{}] {label}", if here { "›" } else { " " }, if *on { "x" } else { " " }),
                            Style::default().fg(if here { t.primary } else { t.text }),
                        )));
                    }
                }
            }
            body.push(Line::raw(""));
        }
    }
    let h = (body.len().min(u16::MAX as usize - 2) as u16 + 2).min(area.height.saturating_sub(2));
    let r = centered(area, w, h);
    render(f, Clear, r);
    let block = modal_block(
        "Agent needs input",
        if e.is_url_mode { "enter open · esc decline" } else { "tab/↑↓ field · ←→/space change · enter submit · esc decline" },
        t,
    );
    let inner = block.inner(r).inner(Margin { horizontal: 1, vertical: 0 });
    render(f, block, r);
    render(f, Paragraph::new(body), inner);
    if let Some((row, col)) = cursor
        && row < inner.height
    {
        f.set_cursor_position((inner.x + col.min(inner.width.saturating_sub(1)), inner.y + row));
    }
}

fn relative_time(ts: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return ts.to_string();
    };
    let secs = (chrono::Utc::now() - dt.with_timezone(&chrono::Utc)).num_seconds();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 86_400 * 30 {
        format!("{}d ago", secs / 86_400)
    } else {
        dt.format("%Y-%m-%d").to_string()
    }
}
