//! Slash-command block menu (`/` on an empty line).

use ratatui::Frame;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::block::BlockKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuOrigin {
    /// Typed `/` on an empty line. Query lives in the buffer after `/`.
    Slash,
    /// Typed `\`. Applies to the current line or selection.
    Palette,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemId {
    Block(BlockKind),
    LiteralBackslash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub id: ItemId,
    pub shortcut: char,
    pub name: &'static str,
    pub sample: &'static str,
    pub aliases: &'static [&'static str],
    pub primary: bool,
}

const ITEMS: &[MenuItem] = &[
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(1)),
        shortcut: '1',
        name: "Heading 1",
        sample: "#",
        aliases: &["h1", "1", "#", "heading 1", "heading1"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(2)),
        shortcut: '2',
        name: "Heading 2",
        sample: "##",
        aliases: &["h2", "2", "##", "heading 2", "heading2"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(3)),
        shortcut: '3',
        name: "Heading 3",
        sample: "###",
        aliases: &["h3", "3", "###", "heading 3", "heading3"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(4)),
        shortcut: '4',
        name: "Heading 4",
        sample: "####",
        aliases: &["h4", "4", "####", "heading 4", "heading4"],
        primary: false,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(5)),
        shortcut: '5',
        name: "Heading 5",
        sample: "#####",
        aliases: &["h5", "5", "#####", "heading 5", "heading5"],
        primary: false,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(6)),
        shortcut: '6',
        name: "Heading 6",
        sample: "######",
        aliases: &["h6", "6", "######", "heading 6", "heading6"],
        primary: false,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Paragraph),
        shortcut: '0',
        name: "Text",
        sample: "paragraph",
        aliases: &["p", "0", "text", "body", "paragraph"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Bullet),
        shortcut: 'b',
        name: "Bulleted list",
        sample: "-",
        aliases: &["bullet", "bulleted", "ul", "list", "-"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Numbered),
        shortcut: 'n',
        name: "Numbered list",
        sample: "1.",
        aliases: &["numbered", "ol", "number", "1"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Todo),
        shortcut: 't',
        name: "To-do",
        sample: "[]",
        aliases: &["todo", "to-do", "task", "check", "checkbox"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Quote),
        shortcut: 'q',
        name: "Quote",
        sample: ">",
        aliases: &["quote", "blockquote", ">"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Code),
        shortcut: 'c',
        name: "Code",
        sample: "```",
        aliases: &["code", "fence", "pre"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Divider),
        shortcut: '-',
        name: "Divider",
        sample: "---",
        aliases: &["divider", "hr", "line", "---"],
        primary: true,
    },
];

const BACKSLASH: MenuItem = MenuItem {
    id: ItemId::LiteralBackslash,
    shortcut: '\\',
    name: "Backslash",
    sample: "insert \\",
    aliases: &["\\", "backslash"],
    primary: false,
};

pub fn items_for(origin: MenuOrigin) -> Vec<&'static MenuItem> {
    match origin {
        MenuOrigin::Slash => ITEMS.iter().collect(),
        MenuOrigin::Palette => ITEMS.iter().chain(std::iter::once(&BACKSLASH)).collect(),
    }
}

pub fn visible_items(origin: MenuOrigin, query: &str) -> Vec<&'static MenuItem> {
    let query = query.trim();
    items_for(origin)
        .into_iter()
        .filter(|item| {
            if query.is_empty() {
                item.primary || origin == MenuOrigin::Palette
            } else {
                item.matches(query)
            }
        })
        .collect()
}

impl MenuItem {
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim();
        if query.is_empty() {
            return true;
        }
        let query = query.to_ascii_lowercase();
        if self.name.to_ascii_lowercase().contains(&query) {
            return true;
        }
        self.aliases
            .iter()
            .any(|alias| alias_matches(alias, &query))
    }
}

fn alias_matches(alias: &str, query: &str) -> bool {
    if alias == query {
        return true;
    }
    if query.starts_with('#') {
        return false;
    }
    alias.starts_with(query)
}

/// `/` opens the menu when the current line is blank and not inside a fence.
pub fn can_open_slash(lines: &[String], row: usize, selecting: bool) -> bool {
    if selecting {
        return false;
    }
    let Some(line) = lines.get(row) else {
        return false;
    };
    line.trim().is_empty() && !in_code_fence(lines, row)
}

/// Text after `/` on the current line, if that line is still a slash command.
pub fn slash_query(line: &str, col: usize) -> Option<&str> {
    let indent_bytes = line.len() - line.trim_start().len();
    let rest = &line[indent_bytes..];
    let query = rest.strip_prefix('/')?;
    let indent_cols = line[..indent_bytes].chars().count();
    if col < indent_cols {
        return None;
    }
    Some(query)
}

pub fn in_code_fence(lines: &[String], row: usize) -> bool {
    let mut in_fence = false;
    for (i, line) in lines.iter().enumerate() {
        if i > row {
            break;
        }
        if line.trim_start().starts_with("```") {
            if i == row {
                return true;
            }
            in_fence = !in_fence;
        }
    }
    in_fence
}

#[derive(Clone, Debug, Default)]
pub struct MenuHit {
    pub area: Rect,
    pub items: Vec<Rect>,
}

impl MenuHit {
    pub fn item_at(&self, point: Position) -> Option<usize> {
        self.items.iter().position(|rect| rect.contains(point))
    }

    pub fn contains(&self, point: Position) -> bool {
        self.area.contains(point)
    }
}

pub fn popover_area(bounds: Rect, anchor: Position, width: u16, height: u16) -> Rect {
    let width = width.min(bounds.width.max(1));
    let height = height.min(bounds.height.max(1));

    let mut x = anchor.x.saturating_sub(1);
    if x < bounds.x {
        x = bounds.x;
    }
    if x.saturating_add(width) > bounds.right() {
        x = bounds.right().saturating_sub(width);
    }

    let below = anchor.y.saturating_add(1);
    let y = if u32::from(below) + u32::from(height) <= u32::from(bounds.bottom()) {
        below
    } else {
        let above = anchor.y.saturating_sub(height);
        if above >= bounds.y {
            above
        } else {
            bounds.bottom().saturating_sub(height)
        }
    };

    Rect {
        x,
        y,
        width,
        height,
    }
    .intersection(bounds)
}

pub fn render_menu(
    frame: &mut Frame,
    bounds: Rect,
    anchor: Position,
    items: &[&MenuItem],
    selected: usize,
) -> MenuHit {
    let rows = items.len().max(1) as u16;
    let height = rows.saturating_add(2);
    let width = 38;
    let area = popover_area(bounds, anchor, width, height);
    let inner = area.inner(Margin::new(1, 1));
    let selected = if items.is_empty() {
        0
    } else {
        selected.min(items.len() - 1)
    };

    let mut item_rects = Vec::with_capacity(items.len());
    let mut lines = Vec::with_capacity(items.len().max(1));
    if items.is_empty() {
        lines.push(Line::from(" no matching block".dark_gray()));
    } else {
        let row_width = usize::from(inner.width);
        for (i, item) in items.iter().enumerate() {
            if i < usize::from(inner.height) {
                item_rects.push(Rect {
                    x: inner.x,
                    y: inner.y.saturating_add(i as u16),
                    width: inner.width,
                    height: 1,
                });
            }
            let key = item.shortcut.to_string();
            let rest = format!(" {key}  {:<14}  {}", item.name, item.sample);
            let pad = row_width.saturating_sub(rest.chars().count());
            let text = format!("{rest}{}", " ".repeat(pad));
            if i == selected {
                lines.push(Line::from(Span::styled(
                    text,
                    Style::new().fg(Color::Black).bg(Color::Gray).bold(),
                )));
            } else {
                lines.push(Line::from(text.dark_gray()));
            }
        }
    }

    let widget = Paragraph::new(lines).block(
        Block::bordered()
            .title(" insert ")
            .title_style(Style::new().bold())
            .title_bottom(" ↑↓  ⏎ apply  esc ")
            .border_style(Style::new().dark_gray()),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(widget, area);
    MenuHit {
        area,
        items: item_rects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_only_opens_on_blank_unfenced_lines() {
        let lines = vec![
            "# Title".into(),
            "".into(),
            "```".into(),
            "".into(),
            "```".into(),
        ];
        assert!(!can_open_slash(&lines, 0, false));
        assert!(can_open_slash(&lines, 1, false));
        assert!(!can_open_slash(&lines, 1, true));
        assert!(!can_open_slash(&lines, 3, false));
    }

    #[test]
    fn query_reads_text_after_slash() {
        assert_eq!(slash_query("/", 1), Some(""));
        assert_eq!(slash_query("/h2", 3), Some("h2"));
        assert_eq!(slash_query("  /h3", 5), Some("h3"));
        assert_eq!(slash_query("  /h3", 0), None);
        assert_eq!(slash_query("not a command", 1), None);
    }

    #[test]
    fn filter_matches_aliases_and_names() {
        let h1 = visible_items(MenuOrigin::Slash, "h1");
        assert_eq!(h1.len(), 1);
        assert_eq!(h1[0].id, ItemId::Block(BlockKind::Heading(1)));

        let h2 = visible_items(MenuOrigin::Slash, "2");
        assert_eq!(h2.len(), 1);
        assert_eq!(h2[0].id, ItemId::Block(BlockKind::Heading(2)));

        let hashes = visible_items(MenuOrigin::Slash, "###");
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0].id, ItemId::Block(BlockKind::Heading(3)));

        let todo = visible_items(MenuOrigin::Slash, "todo");
        assert_eq!(todo.len(), 1);
        assert_eq!(todo[0].id, ItemId::Block(BlockKind::Todo));

        assert!(visible_items(MenuOrigin::Slash, "zzz").is_empty());
        assert!(
            visible_items(MenuOrigin::Slash, "")
                .iter()
                .any(|item| item.id == ItemId::Block(BlockKind::Bullet))
        );
        assert!(
            visible_items(MenuOrigin::Slash, "")
                .iter()
                .all(|item| item.id != ItemId::Block(BlockKind::Heading(4)))
        );
        assert!(
            visible_items(MenuOrigin::Palette, "")
                .iter()
                .any(|item| item.id == ItemId::Block(BlockKind::Paragraph))
        );
    }
}
