use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use tui_textarea::{CursorMove, Input, Key};
use unicode_width::UnicodeWidthChar;
use unpeel_app_kit::{MarkdownTextArea, MarkdownTextAreaStyle};

use crate::block::{self, BlockKind, EnterAction};
use crate::clipboard;
use crate::format::{self, Mark};
use crate::frontmatter::{self, Metadata};
use crate::heading;
use crate::highlight;
use crate::mouse;
use crate::slash::{self, ItemId, MenuHit, MenuOrigin};
use crate::theme::Theme;

const COVER_HEIGHT: u16 = 5;
const CARD_DETAILS_HEIGHT: u16 = 4;
const CARD_HEIGHT: u16 = COVER_HEIGHT + CARD_DETAILS_HEIGHT;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Edit,
    Menu,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DocumentView {
    Card,
    Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CardField {
    Cover,
    Title,
    Description,
}

#[derive(Clone, Copy, Default)]
struct FieldHit {
    area: Rect,
    start_col: usize,
}

#[derive(Clone, Copy, Default)]
struct CardHit {
    area: Rect,
    cover_area: Rect,
    cover: FieldHit,
    title: FieldHit,
    description: FieldHit,
}

struct BlockMenu {
    origin: MenuOrigin,
    selected: usize,
    hit: Option<MenuHit>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Char,
    Word,
    Line,
}

struct DragState {
    kind: DragKind,
    anchor_start: (usize, usize),
    anchor_end: (usize, usize),
}

pub struct App<'a> {
    path: PathBuf,
    textarea: MarkdownTextArea<'a>,
    frontmatter: Metadata,
    view: DocumentView,
    card_focus: Option<CardField>,
    card_cursor: usize,
    card_select_all: bool,
    card_hit: CardHit,
    theme: Theme,
    mode: Mode,
    menu: Option<BlockMenu>,
    dirty: bool,
    exit: bool,
    status: Option<(String, Instant)>,
    drag: Option<DragState>,
    last_click: Option<(Instant, u16, u16, u8)>,
}

impl App<'_> {
    pub fn open(path: PathBuf, theme: Theme) -> io::Result<Self> {
        let contents = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let document = frontmatter::parse(&contents, &document_title(&path));
        let mut textarea = MarkdownTextArea::new(document.body, markdown_text_area_style(theme));
        highlight::refresh(&mut textarea, theme);

        let is_new = !path.exists();
        let mut app = Self {
            path,
            textarea,
            frontmatter: document.metadata,
            view: DocumentView::Card,
            card_focus: None,
            card_cursor: 0,
            card_select_all: false,
            card_hit: CardHit::default(),
            theme,
            mode: Mode::Edit,
            menu: None,
            dirty: false,
            exit: false,
            status: None,
            drag: None,
            last_click: None,
        };
        if is_new {
            app.flash("new file — press Ctrl+S to create it");
        }
        Ok(app)
    }

    pub fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        status: &mut crate::unpeel::StatusReporter,
    ) -> io::Result<()> {
        while !self.exit {
            terminal.draw(|frame| self.draw(frame))?;
            self.publish_context(status);
            self.handle_events()?;
        }
        status.flush();
        Ok(())
    }

    /// Live context for agents: which note is open, where the cursor is,
    /// what is selected, and whether the buffer has unsaved edits. Debounced
    /// and deduplicated by the reporter, so per-iteration calls are cheap.
    fn publish_context(&self, status: &mut crate::unpeel::StatusReporter) {
        let file = std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone());
        let folder = file.parent().map(|parent| parent.display().to_string());
        let (buffer_row, _) = self.textarea.cursor();
        let row = if self.view == DocumentView::Card {
            frontmatter::body_start(&self.frontmatter) + buffer_row
        } else {
            buffer_row
        };
        let selection_lines = self.textarea.selection_range().map(|(start, end)| {
            let (low, high) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            let offset = if self.view == DocumentView::Card {
                frontmatter::body_start(&self.frontmatter)
            } else {
                0
            };
            [low.0 + offset + 1, high.0 + offset + 1]
        });
        status.set_context(
            crate::install::APP_ID,
            &serde_json::json!({
                "file": file.display().to_string(),
                "folder": folder,
                "cursor_line": row + 1,
                "selection_lines": selection_lines,
                "dirty": self.dirty,
                "view": if self.view == DocumentView::Card { "card" } else { "source" },
                "title": self.frontmatter.title,
                "description": self.frontmatter.description,
                "cover": self.frontmatter.cover,
            }),
        );
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [main, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

        let editor = if self.view == DocumentView::Card {
            self.draw_frontmatter_card(frame, main)
        } else {
            self.card_hit = CardHit::default();
            main
        };
        self.draw_editor(frame, editor);
        self.draw_empty_hint(frame, editor);
        self.draw_footer(frame, footer);

        if self.mode == Mode::Menu {
            self.draw_menu(frame, editor);
        }
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(self.footer_line()).style(Style::new().fg(self.theme.text)),
            area,
        );
    }

    fn draw_frontmatter_card(&mut self, frame: &mut Frame, area: Rect) -> Rect {
        let card_height = CARD_HEIGHT.min(area.height.saturating_sub(1));
        let [card, editor] =
            Layout::vertical([Constraint::Length(card_height), Constraint::Fill(1)]).areas(area);
        if card.height == 0 {
            self.card_hit = CardHit::default();
            return editor;
        }

        let cover_height = card.height.saturating_sub(CARD_DETAILS_HEIGHT);
        let [cover, title_row, description_row, _, divider] = Layout::vertical([
            Constraint::Length(cover_height),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(card);
        let field_area = |row: Rect| Rect {
            x: row.x.saturating_add(2),
            width: row.width.saturating_sub(4),
            ..row
        };
        let cover_field = field_area(Rect {
            y: cover.bottom().saturating_sub(1),
            height: cover.height.min(1),
            ..cover
        });
        let title = field_area(title_row);
        let description = field_area(description_row);

        let cover_source = frontmatter::cover_source(&self.frontmatter.cover);
        let (cover_bg, cover_fg) = cover_colors(cover_source, self.theme);
        draw_cover_surface(frame, cover, cover_source, cover_bg, cover_fg);

        let cover_hit = draw_card_field(
            frame,
            cover_field,
            &self.frontmatter.cover,
            "#cccc",
            Style::new().bg(cover_bg).fg(cover_fg),
            self.card_focus == Some(CardField::Cover),
            self.card_cursor,
            self.card_select_all,
        );
        let title_hit = draw_card_field(
            frame,
            title,
            &self.frontmatter.title,
            "Untitled",
            Style::new().fg(self.theme.strong).bold(),
            self.card_focus == Some(CardField::Title),
            self.card_cursor,
            self.card_select_all,
        );
        let description_hit = draw_card_field(
            frame,
            description,
            &self.frontmatter.description,
            "Add a description…",
            Style::new().fg(self.theme.muted),
            self.card_focus == Some(CardField::Description),
            self.card_cursor,
            self.card_select_all,
        );
        frame.render_widget(
            Paragraph::new("─".repeat(usize::from(divider.width)))
                .style(Style::new().fg(self.theme.faint)),
            divider,
        );
        self.card_hit = CardHit {
            area: card,
            cover_area: cover,
            cover: cover_hit,
            title: title_hit,
            description: description_hit,
        };
        editor
    }

    fn draw_editor(&mut self, frame: &mut Frame, area: Rect) {
        highlight::refresh(&mut self.textarea, self.theme);
        let show_cursor = self.card_focus.is_none()
            && (self.mode == Mode::Edit
                || self
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.origin == MenuOrigin::Slash));
        self.textarea.render(frame, area, show_cursor);
    }

    fn draw_menu(&mut self, frame: &mut Frame, area: Rect) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let items = self.visible_menu_items();
        let selected = if items.is_empty() {
            0
        } else {
            menu.selected.min(items.len() - 1)
        };
        let anchor = self
            .textarea
            .rendered_cursor_position()
            .unwrap_or(Position::new(
                area.x.saturating_add(2),
                area.y.saturating_add(1),
            ));
        let hit = slash::render_menu(frame, area, anchor, &items, selected, self.theme);
        if let Some(menu) = self.menu.as_mut() {
            menu.selected = selected;
            menu.hit = Some(hit);
        }
    }

    fn footer_line(&self) -> Line<'_> {
        let (row, col) = self.textarea.cursor();
        let location = if let Some(field) = self.card_focus {
            format!("{} {}", card_field_label(field), self.card_cursor + 1)
        } else {
            format!("{}:{}", row + 1, col + 1)
        };
        let mut spans: Vec<Span> = vec![
            " ".into(),
            Span::styled(file_name(&self.path), Style::new().fg(self.theme.strong)),
            if self.dirty {
                " ●  ".into()
            } else {
                Span::styled(" ✓  ", Style::new().fg(self.theme.faint))
            },
            Span::styled(location, Style::new().fg(self.theme.muted)),
            "  ".into(),
            Span::styled(
                if self.view == DocumentView::Card {
                    "CARD"
                } else {
                    "MD"
                },
                Style::new().fg(self.theme.faint),
            ),
            "   ".into(),
        ];
        let message = self
            .status
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(3))
            .map(|(text, _)| text.as_str());
        if let Some(message) = message {
            spans.push(message.into());
        }
        Line::from(spans)
    }

    fn handle_events(&mut self) -> io::Result<()> {
        if !event::poll(Duration::from_millis(120))? {
            return Ok(());
        }
        // Drain the whole queue before the next draw. Touch terminals send
        // scroll flicks as bursts of wheel events; redrawing per event turns
        // one flick into dozens of full diff frames — an output storm that
        // remote viewers have to replay.
        loop {
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    if self.handle_key_command(&key) {
                        // command handled
                    } else if key.kind == KeyEventKind::Press {
                        self.handle_input(input_from_key(key));
                    }
                }
                Event::Paste(text) => self.paste_text(&text),
                Event::Mouse(mouse) => self.handle_mouse(mouse),
                Event::Resize(_, _) => {}
                _ => {}
            }
            if self.exit || !event::poll(Duration::ZERO)? {
                return Ok(());
            }
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let point = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.on_mouse_down(point, mouse.modifiers),
            MouseEventKind::Moved => {
                self.hover_menu(point);
                if self.drag.is_some() {
                    self.on_mouse_drag(point);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.drag.is_some() => {
                self.on_mouse_drag(point);
            }
            MouseEventKind::Up(MouseButton::Left) => self.on_mouse_up(),
            MouseEventKind::ScrollDown if self.menu_contains(point) => self.move_menu(1),
            MouseEventKind::ScrollUp if self.menu_contains(point) => self.move_menu(-1),
            MouseEventKind::ScrollDown if self.textarea.contains(point) => {
                self.textarea
                    .scroll_lines_with_selection(1, mouse.modifiers.contains(KeyModifiers::SHIFT));
            }
            MouseEventKind::ScrollUp if self.textarea.contains(point) => {
                self.textarea
                    .scroll_lines_with_selection(-1, mouse.modifiers.contains(KeyModifiers::SHIFT));
            }
            _ => {}
        }
    }

    fn on_mouse_down(&mut self, point: Position, modifiers: KeyModifiers) {
        if self.mode == Mode::Menu {
            if let Some(index) = self.menu_item_at(point) {
                self.apply_menu_index(index);
                return;
            }
            if self.menu_contains(point) {
                return;
            }
            self.close_menu(false);
        }
        if self.view == DocumentView::Card {
            if let Some((field, cursor)) = self.card_field_at(point) {
                self.focus_card_field(field, Some(cursor));
                return;
            }
            if self.card_hit.area.contains(point) {
                self.card_focus = None;
                self.card_select_all = false;
                return;
            }
            self.card_focus = None;
            self.card_select_all = false;
        }
        if !self.textarea.contains(point) {
            return;
        }
        let (row, col) = self.textarea.hit_test(point);
        if self.click_checkbox(row, col) {
            return;
        }
        let count = self.register_click(point.x, point.y);
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        match (count, shift) {
            (1, true) => {
                if !self.textarea.is_selecting() {
                    self.textarea.start_selection();
                }
                self.jump(row, col);
                self.drag = Some(DragState {
                    kind: DragKind::Char,
                    anchor_start: self.selection_anchor(),
                    anchor_end: self.selection_anchor(),
                });
            }
            (2, _) => {
                let (start, end) = self.select_word_at(row, col);
                self.drag = Some(DragState {
                    kind: DragKind::Word,
                    anchor_start: (row, start),
                    anchor_end: (row, end),
                });
            }
            (3, _) => {
                let end = self.select_line_at(row);
                self.drag = Some(DragState {
                    kind: DragKind::Line,
                    anchor_start: (row, 0),
                    anchor_end: (row, end),
                });
            }
            _ => {
                self.textarea.cancel_selection();
                self.jump(row, col);
                self.textarea.start_selection();
                self.drag = Some(DragState {
                    kind: DragKind::Char,
                    anchor_start: (row, col),
                    anchor_end: (row, col),
                });
            }
        }
    }

    fn on_mouse_drag(&mut self, point: Position) {
        let Some(drag) = self.drag.as_ref() else {
            return;
        };
        let kind = drag.kind;
        let anchor_start = drag.anchor_start;
        let anchor_end = drag.anchor_end;
        self.textarea.auto_scroll(point);
        let (row, col) = self.textarea.hit_test(point);
        match kind {
            DragKind::Char => self.jump(row, col),
            DragKind::Word => {
                let line = self.textarea.lines().get(row).cloned().unwrap_or_default();
                let (word_start, word_end) = mouse::word_bounds(&line, col);
                let start = if mouse::pos_le((row, col), anchor_start) {
                    (row, word_start)
                } else {
                    anchor_start
                };
                let end = if mouse::pos_le(anchor_end, (row, col)) {
                    (row, word_end)
                } else {
                    anchor_end
                };
                self.set_selection(start, end);
            }
            DragKind::Line => {
                let start_row = anchor_start.0.min(row);
                let end_row = anchor_start.0.max(row);
                let end_col = self.line_len(end_row);
                self.set_selection((start_row, 0), (end_row, end_col));
            }
        }
    }

    fn on_mouse_up(&mut self) {
        if let Some(range) = self.textarea.selection_range()
            && range.0 == range.1
        {
            self.textarea.cancel_selection();
        }
        self.drag = None;
    }

    fn register_click(&mut self, x: u16, y: u16) -> u8 {
        let now = Instant::now();
        let count = match self.last_click {
            Some((at, lx, ly, n))
                if at.elapsed() < Duration::from_millis(400)
                    && x.abs_diff(lx) <= 1
                    && y.abs_diff(ly) <= 1 =>
            {
                n.saturating_add(1).min(3)
            }
            _ => 1,
        };
        self.last_click = Some((now, x, y, count));
        count
    }

    fn jump(&mut self, row: usize, col: usize) {
        self.textarea.move_cursor(CursorMove::Jump(
            row.min(usize::from(u16::MAX)) as u16,
            col.min(usize::from(u16::MAX)) as u16,
        ));
    }

    fn set_selection(&mut self, start: (usize, usize), end: (usize, usize)) {
        self.textarea.cancel_selection();
        self.jump(start.0, start.1);
        self.textarea.start_selection();
        self.jump(end.0, end.1);
    }

    fn select_word_at(&mut self, row: usize, col: usize) -> (usize, usize) {
        let line = self.textarea.lines().get(row).cloned().unwrap_or_default();
        let (start, end) = mouse::word_bounds(&line, col);
        self.set_selection((row, start), (row, end));
        (start, end)
    }

    fn select_line_at(&mut self, row: usize) -> usize {
        let end = self.line_len(row);
        self.set_selection((row, 0), (row, end));
        end
    }

    fn line_len(&self, row: usize) -> usize {
        self.textarea
            .lines()
            .get(row)
            .map(|line| line.chars().count())
            .unwrap_or(0)
    }

    fn selection_anchor(&self) -> (usize, usize) {
        self.textarea
            .selection_range()
            .map(|(start, _)| start)
            .unwrap_or_else(|| self.textarea.cursor())
    }

    fn handle_input(&mut self, input: Input) {
        if self.handle_command(&input) {
            return;
        }
        if self.mode == Mode::Menu {
            self.handle_menu_input(input);
            return;
        }
        if self.card_focus.is_some() {
            self.handle_card_input(input);
            return;
        }
        if self.view == DocumentView::Card
            && matches!(input.key, Key::Up)
            && self.textarea.cursor() == (0, 0)
            && !self.textarea.is_selecting()
        {
            self.focus_card_field(CardField::Description, None);
            return;
        }
        match input {
            Input { key: Key::Esc, .. } => self.exit = true,
            Input {
                key: Key::Enter, ..
            } => self.handle_enter(),
            Input {
                key: Key::Backspace,
                ..
            } => self.handle_backspace(),
            Input {
                key: Key::Tab,
                shift: true,
                ..
            } if !self.in_fence() => {
                block::indent_lines(&mut self.textarea, -2);
                self.after_edit();
            }
            Input { key: Key::Tab, .. } if !self.in_fence() => {
                block::indent_lines(&mut self.textarea, 2);
                self.after_edit();
            }
            Input {
                key: Key::Char('/' | '7'),
                ctrl: false,
                ..
            } if is_slash_key(&input) && self.can_open_slash() => {
                self.textarea.insert_char('/');
                self.after_edit();
                self.open_menu(MenuOrigin::Slash);
            }
            Input {
                key: Key::Char('/' | '7'),
                ctrl: false,
                ..
            } if is_slash_key(&input) => {
                self.textarea.insert_char('/');
                self.after_edit();
                self.apply_markdown_shortcut();
            }
            Input {
                key: Key::Char('\\'),
                ctrl: false,
                ..
            } if self.can_open_slash() => {
                self.open_menu(MenuOrigin::Palette);
            }
            Input {
                key: Key::Char(ch),
                ctrl: false,
                ..
            } if !ch.is_control() => {
                self.textarea.insert_char(ch);
                self.after_edit();
                self.apply_markdown_shortcut();
            }
            other => {
                if self.textarea.input(other) {
                    self.after_edit();
                    self.apply_markdown_shortcut();
                }
            }
        }
    }

    fn handle_key_command(&mut self, key: &KeyEvent) -> bool {
        if key.code == KeyCode::F(2) {
            self.toggle_document_view();
            return true;
        }
        if matches!(key.code, KeyCode::Char('v' | 'V'))
            && key
                .modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        {
            self.toggle_document_view();
            return true;
        }
        if !is_command_modifier(key.modifiers) {
            return false;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter => {
                if self.card_focus.is_some() {
                    self.advance_card_focus(1);
                } else {
                    self.toggle_or_make_todo();
                }
                true
            }
            KeyCode::Char(ch) => self.dispatch_command(ch, shift),
            _ => false,
        }
    }

    fn handle_command(&mut self, input: &Input) -> bool {
        if input.ctrl && matches!(input.key, Key::Enter) {
            if self.card_focus.is_some() {
                self.advance_card_focus(1);
            } else {
                self.toggle_or_make_todo();
            }
            return true;
        }
        if !input.ctrl {
            return false;
        }
        let Key::Char(ch) = input.key else {
            return false;
        };
        self.dispatch_command(ch, input.shift)
    }

    fn dispatch_command(&mut self, ch: char, shift: bool) -> bool {
        let ch = ch.to_ascii_lowercase();
        if ch == 't' {
            self.focus_title();
            return true;
        }
        if self.card_focus.is_some() && matches!(ch, 'b' | 'd' | 'e' | 'i' | 'y' | 'z') {
            self.flash("frontmatter fields use plain text");
            return true;
        }
        match ch {
            'q' | 'w' => self.exit = true,
            'n' => self.new_file(),
            's' if shift && self.card_focus.is_some() => {
                self.flash("frontmatter fields use plain text")
            }
            's' if shift => self.toggle_mark(Mark::Strike),
            's' => self.save(),
            'b' => self.toggle_mark(Mark::Bold),
            'i' => self.toggle_mark(Mark::Italic),
            'e' => self.toggle_mark(Mark::Code),
            'd' => self.duplicate_line(),
            'a' if self.card_focus.is_some() => self.select_card_field(),
            'a' => self.textarea.select_all(),
            'c' => self.copy_selection(),
            'x' if shift && self.card_focus.is_some() => {
                self.flash("frontmatter fields use plain text")
            }
            'x' if shift => self.toggle_mark(Mark::Strike),
            'x' => self.cut_selection(),
            'v' => self.paste_clipboard(),
            'y' => self.redo_edit(),
            'z' if shift => self.redo_edit(),
            'z' => self.undo_edit(),
            'f' => self.flash("find: select text, then search in the file"),
            _ => return false,
        }
        true
    }

    fn toggle_document_view(&mut self) {
        if self.mode == Mode::Menu {
            self.close_menu(false);
        }
        match self.view {
            DocumentView::Card => {
                let body_cursor = self.textarea.cursor();
                let body_start = frontmatter::body_start(&self.frontmatter);
                let lines = frontmatter::compose_lines(&self.frontmatter, self.textarea.lines());
                let cursor = if let Some(field) = self.card_focus {
                    let row = source_row_for_field(field);
                    let col = lines.get(row).map(|line| line.chars().count()).unwrap_or(0);
                    (row, col)
                } else {
                    (body_start + body_cursor.0, body_cursor.1)
                };
                self.textarea.set_lines(lines, cursor);
                self.view = DocumentView::Source;
                self.card_focus = None;
                self.card_select_all = false;
                self.card_hit = CardHit::default();
                highlight::refresh(&mut self.textarea, self.theme);
                self.flash("Markdown source — Ctrl+Shift+V returns to the card");
            }
            DocumentView::Source => {
                let source = self.textarea.lines().to_vec();
                let source_cursor = self.textarea.cursor();
                let Some(document) =
                    frontmatter::parse_source_lines(&source, &document_title(&self.path))
                else {
                    self.flash("add opening and closing --- fences to use card view");
                    return;
                };
                let field = source_field_at(&source, source_cursor.0);
                let body_cursor = if source_cursor.0 >= document.body_start {
                    (source_cursor.0 - document.body_start, source_cursor.1)
                } else {
                    (0, 0)
                };
                self.frontmatter = document.metadata;
                self.textarea.set_lines(document.body, body_cursor);
                self.view = DocumentView::Card;
                highlight::refresh(&mut self.textarea, self.theme);
                if let Some(field) = field {
                    self.focus_card_field(field, None);
                } else {
                    self.card_focus = None;
                    self.card_select_all = false;
                }
                self.flash("Card view — Ctrl+T edits the title");
            }
        }
    }

    fn focus_title(&mut self) {
        if self.view == DocumentView::Source {
            self.toggle_document_view();
        }
        if self.view == DocumentView::Card {
            self.focus_card_field(CardField::Title, None);
        }
    }

    fn focus_card_field(&mut self, field: CardField, cursor: Option<usize>) {
        self.mode = Mode::Edit;
        self.menu = None;
        self.drag = None;
        self.card_focus = Some(field);
        let len = self.card_value(field).chars().count();
        self.card_cursor = cursor.unwrap_or(len).min(len);
        self.card_select_all = false;
        self.textarea.cancel_selection();
    }

    fn select_card_field(&mut self) {
        let Some(field) = self.card_focus else { return };
        self.card_cursor = self.card_value(field).chars().count();
        self.card_select_all = true;
    }

    fn handle_card_input(&mut self, input: Input) {
        let Some(field) = self.card_focus else { return };
        match input {
            Input { key: Key::Esc, .. } => {
                self.card_focus = None;
                self.card_select_all = false;
            }
            Input {
                key: Key::Enter | Key::Down,
                ..
            } => self.advance_card_focus(1),
            Input { key: Key::Up, .. } => self.advance_card_focus(-1),
            Input {
                key: Key::Tab,
                shift: true,
                ..
            } => self.advance_card_focus(-1),
            Input { key: Key::Tab, .. } => self.advance_card_focus(1),
            Input { key: Key::Home, .. } => {
                self.card_cursor = 0;
                self.card_select_all = false;
            }
            Input { key: Key::End, .. } => {
                self.card_cursor = self.card_value(field).chars().count();
                self.card_select_all = false;
            }
            Input { key: Key::Left, .. } => {
                if self.card_select_all {
                    self.card_cursor = 0;
                } else {
                    self.card_cursor = self.card_cursor.saturating_sub(1);
                }
                self.card_select_all = false;
            }
            Input {
                key: Key::Right, ..
            } => {
                if self.card_select_all {
                    self.card_cursor = self.card_value(field).chars().count();
                } else {
                    let len = self.card_value(field).chars().count();
                    self.card_cursor = self.card_cursor.saturating_add(1).min(len);
                }
                self.card_select_all = false;
            }
            Input {
                key: Key::Backspace,
                ..
            } => self.delete_card_character(field, true),
            Input {
                key: Key::Delete, ..
            } => self.delete_card_character(field, false),
            Input {
                key: Key::Char(character),
                ctrl: false,
                alt: false,
                ..
            } if !character.is_control() => self.insert_card_text(&character.to_string()),
            _ => {}
        }
    }

    fn advance_card_focus(&mut self, direction: i8) {
        let Some(field) = self.card_focus else { return };
        let next = match (field, direction.signum()) {
            (CardField::Cover, 1) => Some(CardField::Title),
            (CardField::Title, 1) => Some(CardField::Description),
            (CardField::Description, 1) => None,
            (CardField::Description, -1) => Some(CardField::Title),
            (CardField::Title, -1) => Some(CardField::Cover),
            (CardField::Cover, -1) => Some(CardField::Cover),
            _ => Some(field),
        };
        if let Some(next) = next {
            self.focus_card_field(next, None);
        } else {
            self.card_focus = None;
            self.card_select_all = false;
            self.jump(0, 0);
        }
    }

    fn insert_card_text(&mut self, text: &str) {
        let Some(field) = self.card_focus else { return };
        let limit = card_field_limit(field);
        if self.card_select_all {
            self.card_value_mut(field).clear();
            self.card_cursor = 0;
            self.card_select_all = false;
        }
        let len = self.card_value(field).chars().count();
        let available = limit.saturating_sub(len);
        let inserted: String = text
            .chars()
            .map(|character| {
                if matches!(character, '\n' | '\r' | '\0') {
                    ' '
                } else {
                    character
                }
            })
            .filter(|character| !character.is_control())
            .take(available)
            .collect();
        if inserted.is_empty() {
            return;
        }
        let cursor = self.card_cursor.min(len);
        let byte = char_to_byte(self.card_value(field), cursor);
        self.card_value_mut(field).insert_str(byte, &inserted);
        self.card_cursor = cursor + inserted.chars().count();
        self.dirty = true;
    }

    fn delete_card_character(&mut self, field: CardField, backward: bool) {
        if self.card_select_all {
            if !self.card_value(field).is_empty() {
                self.card_value_mut(field).clear();
                self.dirty = true;
            }
            self.card_cursor = 0;
            self.card_select_all = false;
            return;
        }
        let len = self.card_value(field).chars().count();
        let cursor = self.card_cursor.min(len);
        let target = if backward {
            let Some(target) = cursor.checked_sub(1) else {
                return;
            };
            target
        } else {
            if cursor >= len {
                return;
            }
            cursor
        };
        let start = char_to_byte(self.card_value(field), target);
        let end = char_to_byte(self.card_value(field), target + 1);
        self.card_value_mut(field).replace_range(start..end, "");
        if backward {
            self.card_cursor = target;
        }
        self.dirty = true;
    }

    fn card_field_at(&self, point: Position) -> Option<(CardField, usize)> {
        for (field, hit) in [
            (CardField::Cover, self.card_hit.cover),
            (CardField::Title, self.card_hit.title),
            (CardField::Description, self.card_hit.description),
        ] {
            if !hit.area.contains(point) {
                continue;
            }
            let local_x = usize::from(point.x.saturating_sub(hit.area.x));
            let cursor =
                hit.start_col + char_col_at_width(self.card_value(field), hit.start_col, local_x);
            return Some((field, cursor));
        }
        if self.card_hit.cover_area.contains(point) {
            return Some((CardField::Cover, self.frontmatter.cover.chars().count()));
        }
        None
    }

    fn card_value(&self, field: CardField) -> &str {
        match field {
            CardField::Cover => &self.frontmatter.cover,
            CardField::Title => &self.frontmatter.title,
            CardField::Description => &self.frontmatter.description,
        }
    }

    fn card_value_mut(&mut self, field: CardField) -> &mut String {
        match field {
            CardField::Cover => &mut self.frontmatter.cover,
            CardField::Title => &mut self.frontmatter.title,
            CardField::Description => &mut self.frontmatter.description,
        }
    }

    fn new_file(&mut self) {
        if self.dirty {
            self.flash("save first (⌘S), then ⌘N");
            return;
        }
        self.path = PathBuf::from("untitled.md");
        self.textarea = MarkdownTextArea::new([""], markdown_text_area_style(self.theme));
        highlight::refresh(&mut self.textarea, self.theme);
        self.frontmatter = Metadata::new("Untitled");
        self.view = DocumentView::Card;
        self.card_focus = Some(CardField::Title);
        self.card_cursor = self.frontmatter.title.chars().count();
        self.card_select_all = true;
        self.dirty = false;
        self.flash("new file");
    }

    fn duplicate_line(&mut self) {
        let (row, col) = self.textarea.cursor();
        let Some(line) = self.textarea.lines().get(row).cloned() else {
            return;
        };
        self.textarea.move_cursor(CursorMove::End);
        self.textarea.insert_newline();
        self.textarea.insert_str(&line);
        self.jump(row + 1, col);
        self.after_edit();
    }

    fn copy_selection(&mut self) {
        if let Some(field) = self.card_focus {
            let text = self.card_value(field).to_string();
            if !text.is_empty() {
                let _ = clipboard::write(&text);
            }
            return;
        }
        if !self.textarea.is_selecting() {
            return;
        }
        self.textarea.copy();
        let text = self.textarea.yank_text();
        if !text.is_empty() {
            let _ = clipboard::write(&text);
        }
    }

    fn cut_selection(&mut self) {
        if let Some(field) = self.card_focus {
            let text = self.card_value(field).to_string();
            if !text.is_empty() {
                let _ = clipboard::write(&text);
                self.card_value_mut(field).clear();
                self.card_cursor = 0;
                self.card_select_all = false;
                self.dirty = true;
            }
            return;
        }
        if !self.textarea.is_selecting() {
            return;
        }
        if self.textarea.cut() {
            let text = self.textarea.yank_text();
            if !text.is_empty() {
                let _ = clipboard::write(&text);
            }
            self.after_edit();
            self.sync_slash_menu();
        }
    }

    fn paste_clipboard(&mut self) {
        if let Some(text) = clipboard::read() {
            self.paste_text(&text);
            return;
        }
        if self.card_focus.is_some() {
            self.flash("clipboard unavailable");
            return;
        }
        if self.textarea.paste() {
            self.after_edit();
            self.sync_slash_menu();
        }
    }

    fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.card_focus.is_some() {
            self.insert_card_text(text);
            return;
        }
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.origin == MenuOrigin::Palette)
        {
            self.close_menu(false);
        }
        if self.textarea.insert_str(text) {
            self.after_edit();
            self.sync_slash_menu();
        }
    }

    fn undo_edit(&mut self) {
        if self.card_focus.is_some() {
            self.flash("field undo is not available yet");
            return;
        }
        if self.textarea.undo() {
            self.after_edit();
            self.sync_slash_menu();
            self.flash("undo");
        } else {
            self.flash("nothing to undo");
        }
    }

    fn redo_edit(&mut self) {
        if self.card_focus.is_some() {
            self.flash("field redo is not available yet");
            return;
        }
        if self.textarea.redo() {
            self.after_edit();
            self.sync_slash_menu();
            self.flash("redo");
        } else {
            self.flash("nothing to redo");
        }
    }

    fn handle_menu_input(&mut self, input: Input) {
        let origin = self.menu.as_ref().map(|menu| menu.origin);
        let selected = self.menu.as_ref().map(|menu| menu.selected).unwrap_or(0);
        match input {
            Input { key: Key::Esc, .. } => {
                self.close_menu(origin == Some(MenuOrigin::Slash));
            }
            Input { key: Key::Up, .. } => self.move_menu(-1),
            Input { key: Key::Down, .. } => self.move_menu(1),
            Input { key: Key::Home, .. } => self.set_menu_selected(0),
            Input { key: Key::End, .. } => self.set_menu_selected(usize::MAX),
            Input {
                key: Key::Enter | Key::Tab,
                ..
            } => self.apply_menu_index(selected),
            Input {
                key: Key::Char(ch @ '1'..='6'),
                ..
            } => self.apply_kind(BlockKind::Heading(ch.to_digit(10).unwrap_or(1) as u8)),
            Input {
                key: Key::Char('0'),
                ..
            } => self.apply_kind(BlockKind::Paragraph),
            other if origin == Some(MenuOrigin::Slash) => {
                if self.textarea.input(other) {
                    self.after_edit();
                }
                self.sync_slash_menu();
            }
            _ => {}
        }
    }

    fn can_open_slash(&self) -> bool {
        slash::can_open_slash(
            self.textarea.lines(),
            self.textarea.cursor().0,
            self.textarea.is_selecting(),
        )
    }

    fn in_fence(&self) -> bool {
        slash::in_code_fence(self.textarea.lines(), self.textarea.cursor().0)
    }

    fn open_menu(&mut self, origin: MenuOrigin) {
        self.mode = Mode::Menu;
        self.menu = Some(BlockMenu {
            origin,
            selected: 0,
            hit: None,
        });
        self.drag = None;
    }

    fn close_menu(&mut self, revert_slash: bool) {
        if revert_slash
            && self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.origin == MenuOrigin::Slash)
        {
            heading::clear_slash_command(&mut self.textarea);
            self.after_edit();
        }
        self.mode = Mode::Edit;
        self.menu = None;
    }

    fn current_slash_query(&self) -> Option<String> {
        let (row, col) = self.textarea.cursor();
        let line = self.textarea.lines().get(row)?;
        slash::slash_query(line, col).map(str::to_string)
    }

    fn visible_menu_items(&self) -> Vec<&'static slash::MenuItem> {
        let Some(menu) = self.menu.as_ref() else {
            return Vec::new();
        };
        let query = match menu.origin {
            MenuOrigin::Slash => self.current_slash_query().unwrap_or_default(),
            MenuOrigin::Palette => String::new(),
        };
        slash::visible_items(menu.origin, &query)
    }

    fn move_menu(&mut self, delta: i32) {
        let count = self.visible_menu_items().len();
        if count == 0 {
            return;
        }
        if let Some(menu) = self.menu.as_mut() {
            let current = menu.selected.min(count - 1) as i32;
            menu.selected = (current + delta).rem_euclid(count as i32) as usize;
        }
    }

    fn set_menu_selected(&mut self, index: usize) {
        let count = self.visible_menu_items().len();
        if let Some(menu) = self.menu.as_mut() {
            menu.selected = if count == 0 { 0 } else { index.min(count - 1) };
        }
    }

    fn hover_menu(&mut self, point: Position) {
        if let Some(index) = self.menu_item_at(point)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.selected = index;
        }
    }

    fn menu_item_at(&self, point: Position) -> Option<usize> {
        self.menu.as_ref()?.hit.as_ref()?.item_at(point)
    }

    fn menu_contains(&self, point: Position) -> bool {
        self.menu
            .as_ref()
            .and_then(|menu| menu.hit.as_ref())
            .is_some_and(|hit| hit.contains(point))
    }

    fn sync_slash_menu(&mut self) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        if menu.origin != MenuOrigin::Slash {
            return;
        }
        if self.current_slash_query().is_none() {
            self.mode = Mode::Edit;
            self.menu = None;
            return;
        }
        let count = self.visible_menu_items().len();
        if let Some(menu) = self.menu.as_mut() {
            menu.selected = if count == 0 {
                0
            } else {
                menu.selected.min(count - 1)
            };
        }
    }

    fn apply_menu_index(&mut self, index: usize) {
        let Some(item) = self.visible_menu_items().get(index).copied() else {
            return;
        };
        match item.id {
            ItemId::Block(kind) => self.apply_kind(kind),
            ItemId::LiteralBackslash => {
                self.close_menu(false);
                self.textarea.insert_char('\\');
                self.after_edit();
            }
        }
    }

    fn apply_kind(&mut self, kind: BlockKind) {
        let slash = self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.origin == MenuOrigin::Slash);
        if slash {
            block::apply_slash(&mut self.textarea, kind);
        } else {
            block::apply_to_textarea(&mut self.textarea, kind);
        }
        self.mode = Mode::Edit;
        self.menu = None;
        self.after_edit();
        self.flash(format!("applied {}", kind.label()));
    }

    fn handle_enter(&mut self) {
        if self.textarea.is_selecting() || self.in_fence() {
            let _ = self.textarea.input(Input {
                key: Key::Enter,
                ..Input::default()
            });
            self.after_edit();
            return;
        }
        let (row, _) = self.textarea.cursor();
        let line = self.textarea.lines().get(row).cloned().unwrap_or_default();
        match block::enter_action(&line) {
            EnterAction::Continue => {
                let prefix = block::continue_prefix(&line);
                self.textarea.insert_newline();
                self.textarea.insert_str(prefix);
                self.after_edit();
            }
            EnterAction::ExitList => {
                let indent = heading::split_indent(&line).0.to_string();
                heading::replace_current_line(&mut self.textarea, &indent);
                self.after_edit();
            }
            EnterAction::Default => {
                let _ = self.textarea.input(Input {
                    key: Key::Enter,
                    ..Input::default()
                });
                self.after_edit();
            }
        }
    }

    fn handle_backspace(&mut self) {
        if self.textarea.is_selecting() {
            let _ = self.textarea.input(Input {
                key: Key::Backspace,
                ..Input::default()
            });
            self.after_edit();
            return;
        }
        let (row, col) = self.textarea.cursor();
        let line = self.textarea.lines().get(row).cloned().unwrap_or_default();
        if let Some(new_line) = block::backspace_to_paragraph(&line, col) {
            heading::replace_current_line(&mut self.textarea, &new_line);
            let indent_cols = heading::split_indent(&new_line).0.chars().count();
            self.jump(row, indent_cols);
            self.after_edit();
            return;
        }
        if self.textarea.input(Input {
            key: Key::Backspace,
            ..Input::default()
        }) {
            self.after_edit();
        }
    }

    fn apply_markdown_shortcut(&mut self) {
        let (row, _) = self.textarea.cursor();
        let Some(line) = self.textarea.lines().get(row).cloned() else {
            return;
        };
        let Some(new_line) = block::markdown_shortcut(&line) else {
            return;
        };
        heading::replace_current_line(&mut self.textarea, &new_line);
        self.after_edit();
    }

    fn toggle_mark(&mut self, mark: Mark) {
        if format::toggle(&mut self.textarea, mark) {
            self.after_edit();
            self.flash(mark.label());
        }
    }

    fn toggle_or_make_todo(&mut self) {
        let (row, _) = self.textarea.cursor();
        if slash::in_code_fence(self.textarea.lines(), row) {
            return;
        }
        let Some(line) = self.textarea.lines().get(row).cloned() else {
            return;
        };
        if let Some(new_line) = block::toggle_todo(&line) {
            heading::replace_current_line(&mut self.textarea, &new_line);
            self.after_edit();
            return;
        }
        block::apply_to_textarea(&mut self.textarea, BlockKind::Todo);
        self.after_edit();
        self.flash("to-do");
    }

    fn click_checkbox(&mut self, row: usize, col: usize) -> bool {
        let Some(line) = self.textarea.lines().get(row) else {
            return false;
        };
        let Some((start, end)) = block::checkbox_cols(line) else {
            return false;
        };
        if col < start || col > end {
            return false;
        }
        let Some(new_line) = block::toggle_todo(line) else {
            return false;
        };
        self.jump(row, col);
        heading::replace_current_line(&mut self.textarea, &new_line);
        self.after_edit();
        true
    }

    fn draw_empty_hint(&self, frame: &mut Frame, area: Rect) {
        if self.mode != Mode::Edit || self.menu.is_some() {
            return;
        }
        let (row, _) = self.textarea.cursor();
        let Some(line) = self.textarea.lines().get(row) else {
            return;
        };
        if !line.is_empty() || slash::in_code_fence(self.textarea.lines(), row) {
            return;
        }
        let lines = self.textarea.lines();
        if lines.len() == 1 && lines[0].is_empty() {
            return;
        }
        let Some(position) = self.textarea.rendered_cursor_position() else {
            return;
        };
        let hint = "Type '/' for commands";
        let width = (hint.chars().count() as u16).min(area.right().saturating_sub(position.x));
        if width < 4 {
            return;
        }
        frame.render_widget(
            Paragraph::new(hint).style(Style::new().fg(self.theme.faint)),
            Rect::new(position.x, position.y, width, 1),
        );
    }

    fn after_edit(&mut self) {
        self.dirty = true;
        highlight::refresh(&mut self.textarea, self.theme);
    }

    fn save(&mut self) {
        let lines = if self.view == DocumentView::Card {
            frontmatter::compose_lines(&self.frontmatter, self.textarea.lines())
        } else {
            self.textarea.lines().to_vec()
        };
        let mut text = lines.join("\n");
        if !text.ends_with('\n') {
            text.push('\n');
        }
        match std::fs::write(&self.path, text) {
            Ok(()) => {
                self.dirty = false;
                self.flash(format!("saved {}", self.path.display()));
            }
            Err(error) => self.flash(format!("save failed: {error}")),
        }
    }

    fn flash(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }
}

struct FieldWindow {
    text: String,
    start_col: usize,
    cursor_x: u16,
}

#[allow(clippy::too_many_arguments)]
fn draw_card_field(
    frame: &mut Frame,
    area: Rect,
    value: &str,
    placeholder: &str,
    style: Style,
    focused: bool,
    cursor: usize,
    select_all: bool,
) -> FieldHit {
    if area.width == 0 || area.height == 0 {
        return FieldHit::default();
    }
    let window = field_window(value, if focused { cursor } else { 0 }, area.width);
    let empty = value.is_empty();
    let shown = if empty {
        placeholder.to_string()
    } else {
        window.text
    };
    let style = if focused && select_all && !empty {
        style.reversed()
    } else if empty {
        style.italic()
    } else {
        style
    };
    frame.render_widget(Paragraph::new(Span::styled(shown, style)), area);
    if focused {
        frame.set_cursor_position(Position {
            x: area.x + window.cursor_x.min(area.width.saturating_sub(1)),
            y: area.y,
        });
    }
    FieldHit {
        area,
        start_col: window.start_col,
    }
}

fn field_window(value: &str, cursor: usize, width: u16) -> FieldWindow {
    let chars: Vec<char> = value.chars().collect();
    let cursor = cursor.min(chars.len());
    let available = usize::from(width.max(1));
    let before_limit = available.saturating_sub(1);
    let mut start = cursor;
    let mut before_width = 0usize;
    while start > 0 {
        let char_width = chars[start - 1].width().unwrap_or(0);
        if before_width + char_width > before_limit {
            break;
        }
        start -= 1;
        before_width += char_width;
    }

    let mut shown = String::new();
    let mut shown_width = 0usize;
    for character in chars.iter().skip(start) {
        let char_width = character.width().unwrap_or(0);
        if shown_width + char_width > available {
            break;
        }
        shown.push(*character);
        shown_width += char_width;
    }
    FieldWindow {
        text: shown,
        start_col: start,
        cursor_x: before_width.min(usize::from(u16::MAX)) as u16,
    }
}

fn char_col_at_width(value: &str, start_col: usize, target_width: usize) -> usize {
    let mut width = 0usize;
    let mut columns = 0usize;
    for character in value.chars().skip(start_col) {
        let character_width = character.width().unwrap_or(0);
        if width + character_width > target_width {
            break;
        }
        width += character_width;
        columns += 1;
    }
    columns
}

fn char_to_byte(value: &str, column: usize) -> usize {
    value
        .char_indices()
        .nth(column)
        .map(|(byte, _)| byte)
        .unwrap_or(value.len())
}

fn draw_cover_surface(
    frame: &mut Frame,
    area: Rect,
    source: frontmatter::CoverSource<'_>,
    background: Color,
    foreground: Color,
) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new("").style(Style::new().bg(background).fg(foreground)),
        area,
    );

    // This dedicated edge-to-edge rectangle is the seam for a future Kitty
    // graphics backend: paint a URL image here, then retain the field overlay.
    if matches!(source, frontmatter::CoverSource::Url(_)) && area.height >= 3 {
        let label = Rect {
            y: area.y + area.height / 2,
            height: 1,
            ..area
        };
        frame.render_widget(
            Paragraph::new("remote image cover")
                .alignment(Alignment::Center)
                .style(Style::new().bg(background).fg(foreground).italic()),
            label,
        );
    }
}

fn cover_colors(source: frontmatter::CoverSource<'_>, theme: Theme) -> (Color, Color) {
    let frontmatter::CoverSource::Color(r, g, b) = source else {
        return (theme.cursor_line, theme.muted);
    };
    let lightness = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1_000;
    let foreground = if lightness >= 150 {
        Color::Rgb(24, 24, 27)
    } else {
        Color::Rgb(250, 250, 250)
    };
    (Color::Rgb(r, g, b), foreground)
}

fn card_field_label(field: CardField) -> &'static str {
    match field {
        CardField::Cover => "cover",
        CardField::Title => "title",
        CardField::Description => "description",
    }
}

fn card_field_limit(field: CardField) -> usize {
    match field {
        CardField::Cover => 2_048,
        CardField::Title => 240,
        CardField::Description => 500,
    }
}

fn source_row_for_field(field: CardField) -> usize {
    match field {
        CardField::Cover => 1,
        CardField::Title => 2,
        CardField::Description => 3,
    }
}

fn source_field_at(lines: &[String], row: usize) -> Option<CardField> {
    let (key, _) = lines.get(row)?.split_once(':')?;
    match key.trim() {
        "cover" => Some(CardField::Cover),
        "title" => Some(CardField::Title),
        "description" => Some(CardField::Description),
        _ => None,
    }
}

fn document_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Untitled")
        .to_string()
}

fn is_slash_key(input: &Input) -> bool {
    match input.key {
        Key::Char('/') => true,
        Key::Char('7') if input.shift => true,
        _ => false,
    }
}

fn is_command_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(
        KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META | KeyModifiers::HYPER,
    )
}

fn input_from_key(key: KeyEvent) -> Input {
    let mut input = Input::from(key);
    if is_command_modifier(key.modifiers) {
        input.ctrl = true;
    }
    input
}

fn markdown_text_area_style(theme: Theme) -> MarkdownTextAreaStyle {
    MarkdownTextAreaStyle {
        cursor_line: Style::new().bg(theme.cursor_line),
        cursor: Style::new().bg(theme.cursor).fg(theme.cursor_text),
        // Built-in selection is unstyled; rainbow paint lives in highlight::refresh.
        selection: Style::new(),
        gutter: Style::new().fg(theme.faint),
        current_gutter: Style::new().fg(theme.muted),
        scrollbar_track: Style::new().fg(theme.faint),
        scrollbar_thumb: Style::new().fg(theme.muted),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("untitled")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_app(theme: Theme, width: u16, height: u16) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo.md");
        let mut app = App::open(path, theme).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn footer_keeps_document_status_without_shortcut_help() {
        let width = 140;
        let buffer = render_app(Theme::dark(), width, 8);
        let top = row_text(&buffer, 0);
        let footer = row_text(&buffer, 7);

        assert!(!top.contains("demo.md"));
        assert!(footer.starts_with(" demo.md ✓  1:1  CARD"));
        for shortcut in ["/ insert", "^T title", "^⇧V source", "^S save", "Esc quit"] {
            assert!(
                !footer.contains(shortcut),
                "unexpected {shortcut:?} in {footer:?}"
            );
        }
    }

    #[test]
    fn light_and_dark_renders_use_their_contrast_palettes() {
        for theme in [Theme::light(), Theme::dark()] {
            let buffer = render_app(theme, 120, 24);
            assert_eq!(buffer[(1, 23)].fg, theme.strong, "document title color");
            for y in 0..COVER_HEIGHT {
                for x in [0, buffer.area.width - 1] {
                    assert_eq!(
                        buffer[(x, y)].bg,
                        Color::Rgb(204, 204, 204),
                        "cover color at ({x}, {y})"
                    );
                }
            }
            assert_eq!(
                buffer[(2, COVER_HEIGHT)].fg,
                theme.strong,
                "card title color"
            );

            let heading = (CARD_HEIGHT..buffer.area.height - 1)
                .find_map(|y| {
                    (0..buffer.area.width)
                        .find(|&x| buffer[(x, y)].symbol() == "#")
                        .map(|x| (x, y))
                })
                .expect("visible editor rows contain a heading");
            assert_eq!(buffer[heading].fg, theme.strong, "heading color");
            let body = (CARD_HEIGHT..buffer.area.height - 1)
                .find_map(|y| {
                    (0..buffer.area.width)
                        .find(|&x| buffer[(x, y)].symbol() == "O")
                        .map(|x| (x, y))
                })
                .expect("visible editor rows contain body copy");
            assert_eq!(buffer[body].fg, theme.text, "body color");
        }
    }

    #[test]
    fn frontmatter_renders_as_a_card_instead_of_yaml() {
        let buffer = render_app(Theme::dark(), 100, 16);
        let frame = (0..buffer.area.height)
            .map(|row| row_text(&buffer, row))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(frame.contains("Markdown editor demo"));
        assert!(frame.contains("A hands-on tour"));
        assert!(!frame.contains("cover:"));
        assert!(!frame.contains("title:"));
        assert!(!frame.contains("description:"));
    }

    #[test]
    fn url_cover_uses_the_full_banner_and_remains_editable() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("remote.md");
        let url = "https://images.example.com/wide-cover.jpg";
        std::fs::write(
            &path,
            format!("---\ncover: {url:?}\ntitle: \"Remote\"\ndescription: \"\"\n---\nBody\n"),
        )
        .unwrap();
        let theme = Theme::dark();
        let mut app = App::open(path, theme).unwrap();
        let width = 100;
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();

        assert!(row_text(buffer, COVER_HEIGHT / 2).contains("remote image cover"));
        assert!(row_text(buffer, COVER_HEIGHT - 1).contains(url));
        for y in 0..COVER_HEIGHT {
            assert_eq!(buffer[(0, y)].bg, theme.cursor_line);
            assert_eq!(buffer[(width - 1, y)].bg, theme.cursor_line);
        }
        assert_eq!(
            app.card_field_at(Position::new(width - 1, 0)),
            Some((CardField::Cover, url.chars().count()))
        );
    }

    #[test]
    fn source_toggle_round_trips_frontmatter_and_body() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo.md");
        let mut app = App::open(path, Theme::dark()).unwrap();
        let body = app.textarea.lines().to_vec();

        assert!(app.handle_key_command(&KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )));
        assert!(app.view == DocumentView::Source);
        assert_eq!(app.textarea.lines()[0], "---");
        assert_eq!(app.textarea.lines()[1], "cover: \"#cccc\"");
        assert_eq!(app.textarea.lines()[2], "title: \"Markdown editor demo\"");

        assert!(app.handle_key_command(&KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE,)));
        assert!(app.view == DocumentView::Card);
        assert_eq!(app.frontmatter.title, "Markdown editor demo");
        assert_eq!(app.textarea.lines(), body);
    }

    #[test]
    fn title_edits_inline_and_saves_as_frontmatter() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        std::fs::write(&path, "Body\n").unwrap();
        let mut app = App::open(path.clone(), Theme::dark()).unwrap();

        app.focus_card_field(CardField::Title, None);
        app.select_card_field();
        app.insert_card_text("Renamed inline");
        app.save();

        let saved = std::fs::read_to_string(path).unwrap();
        assert!(saved.contains("cover: \"#cccc\""));
        assert!(saved.contains("title: \"Renamed inline\""));
        assert!(saved.contains("description: \"\""));
        assert!(saved.ends_with("---\nBody\n"));
    }
}
