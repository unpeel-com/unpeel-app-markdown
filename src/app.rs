use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::{DefaultTerminal, Frame};
use tui_textarea::{CursorMove, CursorRenderMode, Input, Key, TextArea};

use crate::block::{self, BlockKind, EnterAction};
use crate::clipboard;
use crate::format::{self, Mark};
use crate::heading;
use crate::highlight;
use crate::mouse;
use crate::slash::{self, ItemId, MenuHit, MenuOrigin};
use crate::theme::Theme;

const EDITOR_LEFT_PADDING: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Edit,
    Menu,
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
    textarea: TextArea<'a>,
    theme: Theme,
    mode: Mode,
    menu: Option<BlockMenu>,
    dirty: bool,
    exit: bool,
    status: Option<(String, Instant)>,
    editor_inner: Rect,
    scroll_top: (u16, u16),
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
        let mut textarea = TextArea::from(contents.lines());
        configure_textarea(&mut textarea, theme);
        highlight::refresh(&mut textarea, theme);

        let is_new = !path.exists();
        let mut app = Self {
            path,
            textarea,
            theme,
            mode: Mode::Edit,
            menu: None,
            dirty: false,
            exit: false,
            status: None,
            editor_inner: Rect::default(),
            scroll_top: (0, 0),
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
        let (row, _) = self.textarea.cursor();
        let selection_lines = self.textarea.selection_range().map(|(start, end)| {
            let (low, high) = if start <= end { (start, end) } else { (end, start) };
            [low.0 + 1, high.0 + 1]
        });
        status.set_context(
            crate::install::APP_ID,
            &serde_json::json!({
                "file": file.display().to_string(),
                "folder": folder,
                "cursor_line": row + 1,
                "selection_lines": selection_lines,
                "dirty": self.dirty,
            }),
        );
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [main, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

        self.draw_editor(frame, main);
        self.draw_empty_hint(frame, main);
        self.draw_footer(frame, footer);

        if self.mode == Mode::Menu {
            self.draw_menu(frame, main);
        }
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(self.footer_line()).style(Style::new().fg(self.theme.text)),
            area,
        );
    }

    fn draw_editor(&mut self, frame: &mut Frame, area: Rect) {
        let left_padding = EDITOR_LEFT_PADDING.min(area.width);
        let content = Rect {
            x: area.x.saturating_add(left_padding),
            width: area.width.saturating_sub(left_padding),
            ..area
        };
        let gutter = mouse::gutter_width(self.textarea.lines().len());
        let viewport = usize::from(content.height);
        let preview_rows = mouse::visual_rows(
            self.textarea.lines(),
            mouse::wrap_width(content.width, gutter),
            self.textarea.tab_length(),
        );
        let overflow = preview_rows.len() > viewport && viewport > 0;
        let body = if overflow {
            Rect {
                width: content.width.saturating_sub(1),
                ..content
            }
        } else {
            content
        };
        self.editor_inner = body;
        self.clamp_scroll();

        let gutter_area = Rect {
            x: body.x,
            y: body.y,
            width: gutter.min(body.width),
            height: body.height,
        };
        let text_area = Rect {
            x: body.x.saturating_add(gutter_area.width),
            y: body.y,
            width: body.width.saturating_sub(gutter_area.width),
            height: body.height,
        };
        highlight::refresh(&mut self.textarea, self.theme);
        frame.render_widget(&self.textarea, text_area);
        self.sync_scroll();
        self.draw_gutter(frame, gutter_area);
        if overflow {
            self.draw_scrollbar(frame, area, viewport);
        }

        let show_cursor = self.mode == Mode::Edit
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.origin == MenuOrigin::Slash);
        if show_cursor && let Some(position) = self.textarea.rendered_cursor_position() {
            frame.set_cursor_position(position);
        }
    }

    fn draw_gutter(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let lines = self.textarea.lines();
        let gutter = mouse::gutter_width(lines.len());
        let width = mouse::wrap_width(self.editor_inner.width, gutter);
        let rows = mouse::visual_rows(lines, width, self.textarea.tab_length());
        let top = usize::from(self.scroll_top.0);
        let digits = usize::from(mouse::line_number_digits(lines.len()));
        let current = self.textarea.cursor().0;

        let numbered: Vec<Line> = (0..usize::from(area.height))
            .map(|i| {
                let Some(row) = rows.get(top + i) else {
                    return Line::from("");
                };
                if row.start_col != 0 {
                    return Line::from("");
                }
                let label = format!("{:>digits$}  ", row.line + 1);
                if row.line == current {
                    Line::from(Span::styled(label, Style::new().fg(self.theme.muted)))
                } else {
                    Line::from(Span::styled(label, Style::new().fg(self.theme.faint)))
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(numbered), area);
    }

    fn draw_scrollbar(&self, frame: &mut Frame, area: Rect, viewport: usize) {
        let gutter = mouse::gutter_width(self.textarea.lines().len());
        let rows = mouse::visual_rows(
            self.textarea.lines(),
            mouse::wrap_width(self.editor_inner.width, gutter),
            self.textarea.tab_length(),
        );
        let Some(content_length) = scrollbar_content_length(rows.len(), viewport) else {
            return;
        };
        let position = usize::from(self.scroll_top.0).min(content_length.saturating_sub(1));
        let mut state = ScrollbarState::new(content_length)
            .position(position)
            .viewport_content_length(viewport);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .track_style(Style::default().fg(self.theme.faint))
                .thumb_symbol("┃")
                .thumb_style(Style::default().fg(self.theme.muted)),
            area,
            &mut state,
        );
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
        let mut spans: Vec<Span> = vec![
            " ".into(),
            Span::styled(file_name(&self.path), Style::new().fg(self.theme.strong)),
            if self.dirty {
                " ●  ".into()
            } else {
                Span::styled(" ✓  ", Style::new().fg(self.theme.faint))
            },
            Span::styled(
                format!("{}:{}", row + 1, col + 1),
                Style::new().fg(self.theme.muted),
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
        } else if self.mode == Mode::Menu {
            spans.extend([
                "↑↓".bold(),
                Span::styled(" select  ", Style::new().fg(self.theme.muted)),
                "⏎".bold(),
                Span::styled(" apply  ", Style::new().fg(self.theme.muted)),
                "Esc".bold(),
                Span::styled(" close", Style::new().fg(self.theme.muted)),
            ]);
        } else {
            spans.extend([
                "/".bold(),
                Span::styled(" insert  ", Style::new().fg(self.theme.muted)),
                "^S".bold(),
                Span::styled(" save  ", Style::new().fg(self.theme.muted)),
                "Esc".bold(),
                Span::styled(" quit", Style::new().fg(self.theme.muted)),
            ]);
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
            MouseEventKind::ScrollDown if self.editor_inner.contains(point) => {
                if self.scroll_top.0 < self.max_scroll() {
                    let _ = self.textarea.input(Input::from(mouse));
                    self.scroll_top.0 = self.scroll_top.0.saturating_add(1);
                    self.clamp_scroll();
                }
            }
            MouseEventKind::ScrollUp if self.editor_inner.contains(point) => {
                let _ = self.textarea.input(Input::from(mouse));
                self.scroll_top.0 = self.scroll_top.0.saturating_sub(1);
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
        if !self.editor_inner.contains(point) {
            return;
        }
        let (row, col) = self.hit(point.x, point.y);
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
        if point.y < self.editor_inner.y {
            self.textarea.scroll((-1i16, 0i16));
            self.scroll_top.0 = self.scroll_top.0.saturating_sub(1);
        } else if point.y >= self.editor_inner.bottom() && self.scroll_top.0 < self.max_scroll() {
            self.textarea.scroll((1i16, 0i16));
            self.scroll_top.0 = self.scroll_top.0.saturating_add(1);
            self.clamp_scroll();
        }
        let (row, col) = self.hit(point.x, point.y);
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

    fn hit(&self, column: u16, row: u16) -> (usize, usize) {
        let lines = self.textarea.lines();
        let gutter = mouse::gutter_width(lines.len());
        let width = mouse::wrap_width(self.editor_inner.width, gutter);
        mouse::hit_test(
            mouse::HitContext {
                lines,
                inner: self.editor_inner,
                scroll_top: self.scroll_top,
                gutter,
                width,
                tab_len: self.textarea.tab_length(),
            },
            column,
            row,
        )
    }

    fn sync_scroll(&mut self) {
        let Some(rendered) = self.textarea.rendered_cursor_position() else {
            self.clamp_scroll();
            return;
        };
        let lines = self.textarea.lines();
        let gutter = mouse::gutter_width(lines.len());
        let width = mouse::wrap_width(self.editor_inner.width, gutter);
        if let Some(scroll) = mouse::infer_scroll(
            lines,
            self.editor_inner,
            gutter,
            width,
            self.textarea.tab_length(),
            self.textarea.cursor(),
            rendered,
        ) {
            self.scroll_top = scroll;
        }
        self.clamp_scroll();
    }

    fn max_scroll(&self) -> u16 {
        if self.editor_inner.height == 0 {
            return 0;
        }
        let gutter = mouse::gutter_width(self.textarea.lines().len());
        let width = mouse::wrap_width(self.editor_inner.width, gutter);
        let rows = mouse::visual_rows(self.textarea.lines(), width, self.textarea.tab_length());
        max_scroll_offset(rows.len(), usize::from(self.editor_inner.height)) as u16
    }

    fn clamp_scroll(&mut self) {
        let max = self.max_scroll();
        if self.scroll_top.0 <= max {
            return;
        }
        let extra = self.scroll_top.0 - max;
        self.textarea.scroll((-(extra as i16), 0i16));
        self.scroll_top.0 = max;
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
        if !is_command_modifier(key.modifiers) {
            return false;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter => {
                self.toggle_or_make_todo();
                true
            }
            KeyCode::Char(ch) => self.dispatch_command(ch, shift),
            _ => false,
        }
    }

    fn handle_command(&mut self, input: &Input) -> bool {
        if input.ctrl && matches!(input.key, Key::Enter) {
            self.toggle_or_make_todo();
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
        match ch.to_ascii_lowercase() {
            'q' | 'w' => self.exit = true,
            'n' => self.new_file(),
            's' if shift => self.toggle_mark(Mark::Strike),
            's' => self.save(),
            'b' => self.toggle_mark(Mark::Bold),
            'i' => self.toggle_mark(Mark::Italic),
            'e' => self.toggle_mark(Mark::Code),
            'd' => self.duplicate_line(),
            'a' => self.textarea.select_all(),
            'c' => self.copy_selection(),
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

    fn new_file(&mut self) {
        if self.dirty {
            self.flash("save first (⌘S), then ⌘N");
            return;
        }
        self.path = PathBuf::from("untitled.md");
        self.textarea = TextArea::from([""]);
        configure_textarea(&mut self.textarea, self.theme);
        highlight::refresh(&mut self.textarea, self.theme);
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
        if self.textarea.paste() {
            self.after_edit();
            self.sync_slash_menu();
        }
    }

    fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
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
        if self.textarea.undo() {
            self.after_edit();
            self.sync_slash_menu();
            self.flash("undo");
        } else {
            self.flash("nothing to undo");
        }
    }

    fn redo_edit(&mut self) {
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
        let mut text = self.textarea.lines().join("\n");
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

fn configure_textarea(textarea: &mut TextArea<'_>, theme: Theme) {
    textarea.set_cursor_render_mode(CursorRenderMode::Hidden);
    textarea.set_wrap_mode(tui_textarea::WrapMode::WordOrGlyph);
    textarea.set_tab_length(2);
    textarea.remove_line_number();
    textarea.set_cursor_line_style(Style::new().bg(theme.cursor_line));
    textarea.set_cursor_style(Style::new().bg(theme.cursor).fg(theme.cursor_text));
    // Built-in selection is unstyled; rainbow paint lives in highlight::refresh.
    textarea.set_selection_style(Style::new());
    textarea.set_placeholder_text("Type '/' for commands");
    textarea.set_max_histories(500);
}

/// Scroll positions, not rows: last thumb sits on the bottom when the last
/// visual row is visible. Matches unpeel-tui's sidebar scrollbar.
fn scrollbar_content_length(row_count: usize, viewport: usize) -> Option<usize> {
    if row_count <= viewport || viewport == 0 {
        None
    } else {
        Some(row_count - viewport + 1)
    }
}

fn max_scroll_offset(row_count: usize, viewport: usize) -> usize {
    row_count.saturating_sub(viewport)
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
    fn scrollbar_length_is_scroll_positions() {
        assert_eq!(scrollbar_content_length(10, 10), None);
        assert_eq!(scrollbar_content_length(10, 20), None);
        assert_eq!(scrollbar_content_length(12, 10), Some(3));
        assert_eq!(scrollbar_content_length(11, 10), Some(2));
    }

    #[test]
    fn scroll_stops_when_last_row_is_visible() {
        assert_eq!(max_scroll_offset(10, 10), 0);
        assert_eq!(max_scroll_offset(10, 20), 0);
        assert_eq!(max_scroll_offset(15, 10), 5);
    }

    #[test]
    fn footer_places_document_and_shortcuts_at_the_bottom_left() {
        let width = 80;
        let buffer = render_app(Theme::dark(), width, 8);
        let top = row_text(&buffer, 0);
        let footer = row_text(&buffer, 7);
        let shortcuts = "/ insert  ^S save  Esc quit";

        assert!(!top.contains("demo.md"));
        assert!(footer.starts_with(" demo.md ✓  1:1"));
        assert!(footer.contains(shortcuts));
    }

    #[test]
    fn light_and_dark_renders_use_their_contrast_palettes() {
        for theme in [Theme::light(), Theme::dark()] {
            let buffer = render_app(theme, 80, 8);
            assert_eq!(buffer[(1, 7)].fg, theme.strong, "document title color");
            let heading_x = (0..buffer.area.width)
                .find(|&x| buffer[(x, 0)].symbol() == "#")
                .expect("first editor row contains a heading");
            assert_eq!(buffer[(heading_x, 0)].fg, theme.strong, "heading color");
            let body_x = (0..buffer.area.width)
                .find(|&x| buffer[(x, 2)].symbol() == "O")
                .expect("third editor row contains body copy");
            assert_eq!(buffer[(body_x, 2)].fg, theme.text, "body color");
        }
    }
}
