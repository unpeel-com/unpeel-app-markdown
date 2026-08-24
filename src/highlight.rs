//! Lightweight markdown source highlighting for the editor buffer.

use ratatui::style::{Color, Modifier, Style};
use tui_textarea::TextArea;

use crate::block::{self, BlockKind};
use crate::theme::Theme;

pub fn refresh(textarea: &mut TextArea<'_>, theme: Theme) {
    textarea.clear_custom_highlight();
    let lines: Vec<String> = textarea.lines().to_vec();
    let selection = textarea.selection_range();
    for (range, style, priority) in collect(&lines, theme) {
        // tui-textarea treats custom_highlight columns as UTF-8 byte offsets and
        // slices the line with them. Cursor/selection columns are character indexes.
        // A later custom span replaces the previous style entirely, so markdown
        // colors must not start inside the selection or they hide the white fg.
        for clipped in exclude_selection(range, selection) {
            textarea.custom_highlight(range_to_bytes(&lines, clipped), style, priority);
        }
    }
    if let Some(range) = selection {
        for (span, style) in rainbow_selection(&lines, range) {
            textarea.custom_highlight(range_to_bytes(&lines, span), style, 200);
        }
    }
}

fn char_col_to_byte(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

fn range_to_bytes(lines: &[String], range: Range) -> Range {
    let ((start_row, start_col), (end_row, end_col)) = range;
    let start = lines
        .get(start_row)
        .map(|line| char_col_to_byte(line, start_col))
        .unwrap_or(0);
    let end = lines
        .get(end_row)
        .map(|line| char_col_to_byte(line, end_col))
        .unwrap_or(0);
    ((start_row, start), (end_row, end))
}

/// Agent accents from unpeel-mascot, ramp order:
/// claude orange → codex teal → green → kimi blue → gemini → cursor purple.
const RAINBOW: [(u8, u8, u8); 6] = [
    (217, 119, 87),
    (0, 196, 196),
    (67, 194, 81),
    (79, 168, 255),
    (76, 125, 247),
    (155, 97, 234),
];

fn rainbow_selection(
    lines: &[String],
    range: ((usize, usize), (usize, usize)),
) -> Vec<(Range, Style)> {
    let (mut start, mut end) = range;
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    if start == end {
        return Vec::new();
    }
    // Left -> right per selected row segment (not top -> bottom over the whole
    // selection). Each row's selected span gets t=0 at its left edge and t=1
    // at its right edge.
    let mut out = Vec::new();
    for row in start.0..=end.0 {
        let len = lines.get(row).map(|line| line.chars().count()).unwrap_or(0);
        let from = if row == start.0 { start.1.min(len) } else { 0 };
        let to = if row == end.0 { end.1.min(len) } else { len };
        if from >= to {
            continue;
        }
        let width = to - from;
        let mut run_s = from;
        let mut run_e = from + 1;
        let mut run_st = rainbow_style_for_t(0.0);
        for col in from..to {
            let t = if width <= 1 {
                0.0
            } else {
                (col - from) as f32 / (width - 1) as f32
            };
            let st = rainbow_style_for_t(t);
            if col == from {
                run_s = col;
                run_e = col + 1;
                run_st = st;
                continue;
            }
            if col == run_e && st == run_st {
                run_e = col + 1;
            } else {
                out.push((((row, run_s), (row, run_e)), run_st));
                run_s = col;
                run_e = col + 1;
                run_st = st;
            }
        }
        out.push((((row, run_s), (row, run_e)), run_st));
    }
    out
}

#[allow(dead_code)]
fn selection_cells(
    lines: &[String],
    range: ((usize, usize), (usize, usize)),
) -> Vec<(usize, usize)> {
    let (mut start, mut end) = range;
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    if start == end {
        return Vec::new();
    }
    let mut cells = Vec::new();
    for row in start.0..=end.0 {
        let len = lines.get(row).map(|line| line.chars().count()).unwrap_or(0);
        let from = if row == start.0 { start.1.min(len) } else { 0 };
        let to = if row == end.0 { end.1.min(len) } else { len };
        for col in from..to {
            cells.push((row, col));
        }
    }
    cells
}

#[allow(dead_code)]
fn rainbow_style(index: usize, last: usize) -> Style {
    let t = if last == 0 {
        0.5
    } else {
        index as f32 / last as f32
    };
    rainbow_style_for_t(t)
}

fn rainbow_style_for_t(t: f32) -> Style {
    let (r, g, b) = lerp_stops(t);
    Style::new().bg(Color::Rgb(r, g, b)).fg(Color::White)
}

fn lerp_stops(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let segs = (RAINBOW.len() - 1) as f32;
    let x = t * segs;
    let i = (x as usize).min(RAINBOW.len() - 2);
    let f = x - i as f32;
    let a = RAINBOW[i];
    let b = RAINBOW[i + 1];
    (lerp(a.0, b.0, f), lerp(a.1, b.1, f), lerp(a.2, b.2, f))
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8
}

type Range = ((usize, usize), (usize, usize));

fn exclude_selection(range: Range, selection: Option<Range>) -> Vec<Range> {
    let Some(sel) = selection else {
        return vec![range];
    };
    let (sel_start, sel_end) = if sel.0 <= sel.1 {
        (sel.0, sel.1)
    } else {
        (sel.1, sel.0)
    };
    let ((row, start), (end_row, end)) = range;
    if row != end_row || row < sel_start.0 || row > sel_end.0 || start >= end {
        return vec![range];
    }
    let sel_from = if row == sel_start.0 { sel_start.1 } else { 0 };
    let sel_to = if row == sel_end.0 {
        sel_end.1
    } else {
        usize::MAX
    };
    let mut parts = Vec::new();
    if start < end.min(sel_from) {
        parts.push(((row, start), (row, end.min(sel_from))));
    }
    if start.max(sel_to) < end {
        parts.push(((row, start.max(sel_to)), (row, end)));
    }
    parts
}

fn collect(lines: &[String], theme: Theme) -> Vec<(Range, Style, u8)> {
    let mut out = Vec::new();
    let mut in_fence = false;

    for (row, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            push_span(
                &mut out,
                row,
                0,
                line.chars().count(),
                fence_style(theme),
                20,
            );
            continue;
        }
        if in_fence {
            push_span(
                &mut out,
                row,
                0,
                line.chars().count(),
                code_style(theme),
                20,
            );
            continue;
        }

        let parsed = block::parse(line);
        match parsed.kind {
            BlockKind::Heading(_) => {
                push_span(
                    &mut out,
                    row,
                    0,
                    line.chars().count(),
                    heading_style(theme),
                    10,
                );
            }
            BlockKind::Divider => {
                push_span(
                    &mut out,
                    row,
                    0,
                    line.chars().count(),
                    divider_style(theme),
                    10,
                );
            }
            BlockKind::Todo => {
                push_span(
                    &mut out,
                    row,
                    0,
                    line.chars().count(),
                    paragraph_style(theme),
                    1,
                );
                if let Some((start, end_inclusive)) = block::checkbox_cols(line) {
                    push_span(
                        &mut out,
                        row,
                        start,
                        end_inclusive + 1,
                        checkbox_style(theme),
                        12,
                    );
                }
                if parsed.checked && !parsed.body.is_empty() {
                    push_span(
                        &mut out,
                        row,
                        parsed.prefix_cols,
                        line.chars().count(),
                        done_style(theme),
                        11,
                    );
                }
            }
            _ if !line.is_empty() => {
                push_span(
                    &mut out,
                    row,
                    0,
                    line.chars().count(),
                    paragraph_style(theme),
                    1,
                );
            }
            _ => {}
        }

        highlight_inlines(row, line, &mut out, theme);
    }

    out
}

fn highlight_inlines(row: usize, line: &str, out: &mut Vec<(Range, Style, u8)>, theme: Theme) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`'
            && let Some(rel) = chars[i + 1..].iter().position(|&c| c == '`')
        {
            let end = i + 1 + rel;
            push_span(out, row, i, end + 1, code_style(theme), 30);
            i = end + 1;
            continue;
        }
        if chars[i] == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some(rel) = find_closing(&chars, i + 2, &['*', '*'])
        {
            let end = i + 2 + rel + 2;
            push_span(out, row, i, end, bold_style(theme), 15);
            i = end;
            continue;
        }
        if chars[i] == '*'
            && let Some(rel) = chars[i + 1..].iter().position(|&c| c == '*')
        {
            let end = i + 1 + rel;
            if end > i + 1 {
                push_span(out, row, i, end + 1, italic_style(theme), 12);
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
}

fn find_closing(chars: &[char], start: usize, needle: &[char]) -> Option<usize> {
    if start + needle.len() > chars.len() {
        return None;
    }
    chars[start..]
        .windows(needle.len())
        .position(|window| window == needle)
}

fn push_span(
    out: &mut Vec<(Range, Style, u8)>,
    row: usize,
    start: usize,
    end: usize,
    style: Style,
    priority: u8,
) {
    if end > start {
        out.push((((row, start), (row, end)), style, priority));
    }
}

fn heading_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.strong)
        .add_modifier(Modifier::BOLD)
}

fn paragraph_style(theme: Theme) -> Style {
    Style::default().fg(theme.text)
}

fn code_style(theme: Theme) -> Style {
    Style::default().fg(theme.muted)
}

fn fence_style(theme: Theme) -> Style {
    Style::default().fg(theme.faint)
}

fn bold_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.strong)
        .add_modifier(Modifier::BOLD)
}

fn italic_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.text)
        .add_modifier(Modifier::ITALIC)
}

fn divider_style(theme: Theme) -> Style {
    Style::default().fg(theme.faint)
}

fn checkbox_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.strong)
        .add_modifier(Modifier::BOLD)
}

fn done_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.faint)
        .add_modifier(Modifier::CROSSED_OUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_heading_and_inline_code() {
        let marks = collect(&["# Title".into(), "use `code` here".into()], Theme::dark());
        assert!(marks.iter().any(|(range, _, _)| *range == ((0, 0), (0, 7))));
        assert!(
            marks
                .iter()
                .any(|(range, _, _)| *range == ((1, 4), (1, 10)))
        );
    }

    #[test]
    fn checkbox_highlight_includes_the_closing_bracket() {
        for line in ["- [ ] open", "- [x] done"] {
            let theme = Theme::dark();
            let marks = collect(&[line.into()], theme);
            assert!(marks.iter().any(|(range, style, priority)| {
                *range == ((0, 2), (0, 5)) && *style == checkbox_style(theme) && *priority == 12
            }));
        }
    }

    #[test]
    fn checked_todo_highlight_includes_the_first_body_character() {
        let theme = Theme::dark();
        let marks = collect(&["- [x] done".into()], theme);
        assert!(marks.iter().any(|(range, style, priority)| {
            *range == ((0, 6), (0, 10)) && *style == done_style(theme) && *priority == 11
        }));
    }

    #[test]
    fn rainbow_selection_spans_the_range() {
        let lines = ["hello world".into()];
        let spans = rainbow_selection(&lines, ((0, 0), (0, 5)));
        assert!(!spans.is_empty());
        assert_eq!(spans.first().map(|(range, _)| range.0), Some((0, 0)));
        assert_eq!(spans.last().map(|(range, _)| range.1), Some((0, 5)));
        assert!(spans.iter().any(|(_, style)| style.bg.is_some()));
        assert!(
            spans
                .iter()
                .all(|(_, style)| style.fg == Some(Color::White))
        );
    }

    #[test]
    fn markdown_highlights_do_not_cover_the_selection() {
        let range = ((0, 0), (0, 10));
        assert_eq!(
            exclude_selection(range, Some(((0, 2), (0, 8)))),
            vec![((0, 0), (0, 2)), ((0, 8), (0, 10))]
        );
        assert!(exclude_selection(range, Some(((0, 0), (0, 10)))).is_empty());
        assert_eq!(exclude_selection(range, None), vec![range]);
    }

    #[test]
    fn rainbow_empty_selection_is_blank() {
        let lines = ["hello".into()];
        assert!(rainbow_selection(&lines, ((0, 2), (0, 2))).is_empty());
    }

    const CMD_LINE: &str =
        "Type `[] ` to start a to-do. Click the box or press ⌘↩ to check it off.";

    #[test]
    fn highlight_ranges_are_char_boundaries_on_multibyte_line() {
        let lines = [CMD_LINE.to_string()];
        let cmd = CMD_LINE.find('⌘').expect("demo line has ⌘");
        assert!(!CMD_LINE.is_char_boundary(cmd + 1));

        for (range, _, _) in collect(&lines, Theme::dark()) {
            let ((sr, sb), (er, eb)) = range_to_bytes(&lines, range);
            assert!(lines[sr].is_char_boundary(sb), "start {sb} in {range:?}");
            assert!(lines[er].is_char_boundary(eb), "end {eb} in {range:?}");
        }

        let n = CMD_LINE.chars().count();
        for (range, _) in rainbow_selection(&lines, ((0, 0), (0, n))) {
            let ((sr, sb), (er, eb)) = range_to_bytes(&lines, range);
            assert!(lines[sr].is_char_boundary(sb), "rainbow start {sb}");
            assert!(lines[er].is_char_boundary(eb), "rainbow end {eb}");
        }
    }

    #[test]
    fn refresh_renders_multibyte_line_and_selection() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;
        use tui_textarea::CursorMove;

        let mut textarea = TextArea::from([CMD_LINE]);
        refresh(&mut textarea, Theme::dark());
        let area = Rect::new(0, 0, 80, 3);
        let mut buf = Buffer::empty(area);
        Widget::render(&textarea, area, &mut buf);

        textarea.move_cursor(CursorMove::Head);
        textarea.start_selection();
        textarea.move_cursor(CursorMove::End);
        refresh(&mut textarea, Theme::dark());
        let mut buf = Buffer::empty(area);
        Widget::render(&textarea, area, &mut buf);
    }
}
