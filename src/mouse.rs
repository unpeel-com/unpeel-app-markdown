//! Map terminal mouse coordinates onto the wrapped editor buffer.

use ratatui::layout::{Position, Rect};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualRow {
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub last_in_line: bool,
}

pub fn line_number_digits(line_count: usize) -> u16 {
    if line_count == 0 {
        1
    } else {
        line_count.ilog10() as u16 + 1
    }
}

pub fn gutter_width(line_count: usize) -> u16 {
    line_number_digits(line_count) + 3
}

pub fn wrap_width(inner_width: u16, gutter: u16) -> usize {
    usize::from(inner_width.saturating_sub(gutter)).max(1)
}

pub fn visual_rows(lines: &[String], width: usize, tab_len: u8) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    for (line_idx, line) in lines.iter().enumerate() {
        let ranges = wrap_ranges(line, width, tab_len);
        let last = ranges.len().saturating_sub(1);
        for (i, (start_col, end_col)) in ranges.into_iter().enumerate() {
            rows.push(VisualRow {
                line: line_idx,
                start_col,
                end_col,
                last_in_line: i == last,
            });
        }
    }
    if rows.is_empty() {
        rows.push(VisualRow {
            line: 0,
            start_col: 0,
            end_col: 0,
            last_in_line: true,
        });
    }
    rows
}

#[derive(Clone, Copy)]
pub struct HitContext<'a> {
    pub lines: &'a [String],
    pub inner: Rect,
    pub scroll_top: (u16, u16),
    pub gutter: u16,
    pub width: usize,
    pub tab_len: u8,
}

pub fn hit_test(ctx: HitContext<'_>, column: u16, row: u16) -> (usize, usize) {
    let rows = visual_rows(ctx.lines, ctx.width, ctx.tab_len);
    let visual_y =
        (u32::from(ctx.scroll_top.0) + u32::from(row.saturating_sub(ctx.inner.y))) as usize;
    let visual_row = rows[visual_y.min(rows.len().saturating_sub(1))];

    if column < ctx.inner.x.saturating_add(ctx.gutter) {
        return (visual_row.line, visual_row.start_col);
    }

    let local_x = u32::from(column.saturating_sub(ctx.inner.x.saturating_add(ctx.gutter)))
        + u32::from(ctx.scroll_top.1);
    let fragment = slice_cols(
        ctx.lines
            .get(visual_row.line)
            .map(String::as_str)
            .unwrap_or(""),
        visual_row.start_col,
        visual_row.end_col,
    );
    let mut col = visual_row.start_col + col_at_width(&fragment, local_x as usize, ctx.tab_len);
    if !visual_row.last_in_line {
        col = col.min(visual_row.end_col.saturating_sub(1));
    } else {
        col = col.min(visual_row.end_col);
    }
    (visual_row.line, col)
}

pub fn infer_scroll(
    lines: &[String],
    inner: Rect,
    _gutter: u16,
    width: usize,
    tab_len: u8,
    cursor: (usize, usize),
    rendered: Position,
) -> Option<(u16, u16)> {
    if !inner.contains(rendered) {
        return None;
    }
    let (visual_row, _) = data_to_visual(lines, width, tab_len, cursor)?;
    let top_row = visual_row.saturating_sub(usize::from(rendered.y.saturating_sub(inner.y)));
    Some((clamp_u16(top_row), 0))
}

pub fn word_bounds(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let idx = col.min(chars.len() - 1);
    let class = char_class(chars[idx]);
    let start = (0..=idx)
        .rev()
        .take_while(|&i| char_class(chars[i]) == class)
        .last()
        .unwrap_or(idx);
    let end = (idx..chars.len())
        .take_while(|&i| char_class(chars[i]) == class)
        .last()
        .map(|i| i + 1)
        .unwrap_or(idx + 1);
    (start, end)
}

pub fn pos_le(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1)
}

fn data_to_visual(
    lines: &[String],
    width: usize,
    tab_len: u8,
    cursor: (usize, usize),
) -> Option<(usize, usize)> {
    let rows = visual_rows(lines, width, tab_len);
    let (line, col) = cursor;
    for (idx, row) in rows.iter().enumerate() {
        if row.line != line {
            continue;
        }
        let contains = if row.last_in_line {
            col >= row.start_col && col <= row.end_col
        } else {
            col >= row.start_col && col < row.end_col
        };
        if contains {
            let fragment = slice_cols(&lines[line], row.start_col, col);
            return Some((idx, display_width(&fragment, tab_len)));
        }
    }
    rows.last().map(|row| {
        let fragment = slice_cols(
            lines.get(row.line).map(String::as_str).unwrap_or(""),
            row.start_col,
            row.end_col,
        );
        (rows.len() - 1, display_width(&fragment, tab_len))
    })
}

fn wrap_ranges(line: &str, width: usize, tab_len: u8) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let chunks: Vec<(usize, usize)> = UnicodeSegmentation::split_word_bound_indices(line)
        .map(|(start, word)| (start, start + word.len()))
        .collect();

    let mut byte_ranges = Vec::new();
    if chunks.is_empty() {
        byte_ranges.push((0, 0));
    } else {
        let mut i = 0usize;
        let mut seg_start = chunks[0].0;
        let mut seg_end = seg_start;
        let mut seg_width = 0usize;

        while i < chunks.len() {
            let (start, end) = chunks[i];
            if seg_end == seg_start {
                seg_start = start;
            }
            let chunk = &line[start..end];
            let chunk_width = display_width_from(chunk, seg_width, tab_len);
            if seg_width + chunk_width <= width {
                seg_end = end;
                seg_width += chunk_width;
                i += 1;
                continue;
            }
            if seg_end > seg_start {
                byte_ranges.push((seg_start, seg_end));
                seg_start = seg_end;
                seg_width = 0;
                continue;
            }
            split_bytes_by_grapheme(line, start, end, width, tab_len, &mut byte_ranges);
            i += 1;
            seg_start = end;
            seg_end = end;
            seg_width = 0;
        }
        if seg_end > seg_start {
            byte_ranges.push((seg_start, seg_end));
        }
    }

    if byte_ranges.is_empty() {
        byte_ranges.push((0, 0));
    }

    byte_ranges
        .into_iter()
        .map(|(start, end)| (byte_to_col(line, start), byte_to_col(line, end)))
        .collect()
}

fn split_bytes_by_grapheme(
    line: &str,
    start: usize,
    end: usize,
    width: usize,
    tab_len: u8,
    out: &mut Vec<(usize, usize)>,
) {
    let mut segment_start = start;
    while segment_start < end {
        let mut segment_end = segment_start;
        let mut segment_width = 0usize;
        for (offset, grapheme) in
            UnicodeSegmentation::grapheme_indices(&line[segment_start..end], true)
        {
            let grapheme_start = segment_start + offset;
            let grapheme_end = grapheme_start + grapheme.len();
            let next_width = display_width_to(grapheme, segment_width, tab_len);
            let grapheme_width = next_width.saturating_sub(segment_width);
            if segment_end != segment_start && segment_width + grapheme_width > width {
                break;
            }
            segment_end = grapheme_end;
            segment_width = next_width;
            if segment_width > width {
                break;
            }
        }
        if segment_end == segment_start {
            if let Some(ch) = line[segment_start..end].chars().next() {
                segment_end = segment_start + ch.len_utf8();
            } else {
                break;
            }
        }
        out.push((segment_start, segment_end));
        segment_start = segment_end;
    }
}

fn slice_cols(line: &str, start_col: usize, end_col: usize) -> String {
    line.chars()
        .skip(start_col)
        .take(end_col.saturating_sub(start_col))
        .collect()
}

fn byte_to_col(line: &str, byte: usize) -> usize {
    line.get(..byte.min(line.len()))
        .map(|prefix| prefix.chars().count())
        .unwrap_or_else(|| line.chars().count())
}

fn col_at_width(text: &str, target: usize, tab_len: u8) -> usize {
    let mut col = 0usize;
    let mut chars = 0usize;
    for c in text.chars() {
        if col >= target {
            break;
        }
        let width = char_display_width(c, col, tab_len);
        if col + width > target {
            break;
        }
        col += width;
        chars += 1;
    }
    chars
}

fn display_width(text: &str, tab_len: u8) -> usize {
    display_width_to(text, 0, tab_len)
}

fn display_width_from(text: &str, start_width: usize, tab_len: u8) -> usize {
    display_width_to(text, start_width, tab_len).saturating_sub(start_width)
}

fn display_width_to(text: &str, mut width: usize, tab_len: u8) -> usize {
    for c in text.chars() {
        width += char_display_width(c, width, tab_len);
    }
    width
}

fn char_display_width(c: char, col: usize, tab_len: u8) -> usize {
    if c == '\t' {
        let tab = usize::from(tab_len.max(1));
        (tab - (col % tab)).max(1)
    } else {
        c.width().unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn char_class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Other
    }
}

fn clamp_u16(value: usize) -> u16 {
    value.min(usize::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gutter_counts_digits_and_margin() {
        assert_eq!(gutter_width(1), 4);
        assert_eq!(gutter_width(10), 5);
        assert_eq!(gutter_width(100), 6);
    }

    #[test]
    fn word_bounds_select_identifier() {
        assert_eq!(word_bounds("foo bar", 1), (0, 3));
        assert_eq!(word_bounds("foo bar", 4), (4, 7));
        assert_eq!(word_bounds("a,b", 1), (1, 2));
    }
}
