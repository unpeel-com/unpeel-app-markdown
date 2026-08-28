use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{DefaultTerminal, Frame};
use tui_textarea::{CursorMove, Input, Key};
use unpeel_app_kit::{MarkdownTextArea, MarkdownTextAreaStyle};

use crate::block::{self, BlockKind, EnterAction};
use crate::clipboard;
use crate::format::{self, Mark};
use crate::heading;
use crate::highlight;
use crate::mouse;
use crate::slash::{self, ItemId, MenuHit, MenuOrigin};
use crate::theme::Theme;

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
    textarea: MarkdownTextArea<'a>,
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
        let mut textarea = MarkdownTextArea::new(contents.lines(), markdown_text_area_style(theme));
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
            let (low, high) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
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
        highlight::refresh(&mut self.textarea, self.theme);
        let show_cursor = self.mode == Mode::Edit
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.origin == MenuOrigin::Slash);
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
        self.textarea = MarkdownTextArea::new([""], markdown_text_area_style(self.theme));
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

fn markdown_text_area_style(theme: Theme) -> MarkdownTextAreaStyle {
    MarkdownTextAreaStyle {
        cursor_line: Style::new().bg(theme.cursor_line),
        cursor: Style::new().bg(theme.cursor).fg(theme.cursor_text),
        selection: theme.kit.selected_row,
        gutter: Style::new().fg(theme.faint),
        current_gutter: Style::new().fg(theme.muted),
        scrollbar_track: theme.kit.scrollbar_track,
        scrollbar_thumb: theme.kit.scrollbar_thumb,
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
        assert!(footer.starts_with(" demo.md ✓  1:1"));
        assert!(!footer.contains("CARD"));
        for shortcut in ["/ insert", "^S save", "Esc quit"] {
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
            let heading = (0..buffer.area.height - 1)
                .find_map(|y| {
                    (0..buffer.area.width)
                        .find(|&x| buffer[(x, y)].symbol() == "#")
                        .map(|x| (x, y))
                })
                .expect("visible editor rows contain a heading");
            assert_eq!(buffer[heading].fg, theme.strong, "heading color");
            let body = (0..buffer.area.height - 1)
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
    fn text_selection_uses_the_shared_gray_app_kit_style() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        for theme in [Theme::light(), Theme::dark()] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("selection.md");
            let text = "# Selected text";
            std::fs::write(&path, format!("{text}\n")).unwrap();
            let mut app = App::open(path, theme).unwrap();
            app.textarea.start_selection();
            app.textarea.move_cursor(CursorMove::End);

            let mut terminal = Terminal::new(TestBackend::new(60, 5)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            let start = (0..buffer.area.width)
                .find(|column| buffer[(*column, 0)].symbol() == "#")
                .expect("selected line is visible");
            let selected_background = theme
                .kit
                .selected_row
                .bg
                .expect("App Kit selection has a background");
            let selected_foreground = theme
                .kit
                .selected_row
                .fg
                .expect("App Kit selection has a foreground");
            for column in start..start + text.len() as u16 {
                assert_eq!(
                    buffer[(column, 0)].bg,
                    selected_background,
                    "selection background at column {column}"
                );
                assert_eq!(
                    buffer[(column, 0)].fg,
                    selected_foreground,
                    "selection foreground at column {column}"
                );
            }
        }
    }

    #[test]
    fn yaml_frontmatter_is_ordinary_editable_markdown() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        let source = "---\ntitle: \"Plain source\"\n---\nBody\n";
        std::fs::write(&path, source).unwrap();
        let mut app = App::open(path.clone(), Theme::dark()).unwrap();

        assert_eq!(
            app.textarea.lines(),
            ["---", "title: \"Plain source\"", "---", "Body"]
        );
        assert!(!app.handle_key_command(&KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE,)));
        app.save();
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
}
