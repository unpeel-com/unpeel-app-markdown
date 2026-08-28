//! Vault picker built from App Kit's scoped Explorer. Folders remain
//! navigable, only Markdown files are admitted, Enter opens the selected
//! item, and Ctrl-N creates a note in the current folder.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use unpeel_app_kit::{
    DoubleClickTracker, DragSurface, Explorer, ExplorerEvent, ExplorerInput, ExplorerTheme,
};

use crate::theme::Theme;

pub struct Picker {
    root: PathBuf,
    explorer: Explorer,
    drags: DragSurface,
    clicks: DoubleClickTracker<PathBuf>,
    status: Option<String>,
    theme: Theme,
}

impl Picker {
    pub fn open(root: PathBuf, theme: Theme) -> io::Result<Self> {
        let mut explorer = Explorer::scoped(root)?
            .with_file_extensions(["md"])?
            .with_theme(ExplorerTheme::for_color_scheme(theme.kit.scheme));
        explorer.set_show_path(false);
        explorer.set_filter_placeholder("Filter notes");
        let root = explorer
            .navigation_root()
            .expect("scoped Explorer always has a root")
            .to_path_buf();
        Ok(Self {
            root,
            explorer,
            drags: DragSurface::detect(),
            clicks: DoubleClickTracker::new(),
            status: None,
            theme,
        })
    }

    /// Runs the picker until the user chooses a file (`Some`) or quits (`None`).
    pub fn pick(&mut self, terminal: &mut DefaultTerminal) -> io::Result<Option<PathBuf>> {
        self.rescan()?;
        loop {
            self.drags.begin_frame();
            terminal.draw(|frame| self.draw(frame))?;
            self.drags.commit()?;
            while !event::poll(Duration::from_millis(250))? {
                self.drags.heartbeat()?;
            }
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    if let Some(choice) = self.on_key(key) {
                        match choice {
                            Choice::Open(path) => return Ok(Some(path)),
                            Choice::Create => {
                                self.clear_drag_map()?;
                                let folder = self.explorer.cwd().to_path_buf();
                                if let Some(path) = prompt_new_note(terminal, &folder, self.theme)?
                                {
                                    return Ok(Some(path));
                                }
                                self.rescan()?;
                            }
                            Choice::Quit => return Ok(None),
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    if let Some(choice) = self.on_mouse(mouse) {
                        match choice {
                            Choice::Open(path) => return Ok(Some(path)),
                            Choice::Create => {
                                self.clear_drag_map()?;
                                let folder = self.explorer.cwd().to_path_buf();
                                if let Some(path) = prompt_new_note(terminal, &folder, self.theme)?
                                {
                                    return Ok(Some(path));
                                }
                                self.rescan()?;
                            }
                            Choice::Quit => return Ok(None),
                        }
                    }
                }
                Event::Paste(text) => {
                    self.clicks.reset();
                    self.explorer.insert_filter_text(text);
                    self.status = None;
                }
                _ => {}
            }
        }
    }

    fn rescan(&mut self) -> io::Result<()> {
        self.explorer.refresh()?;
        self.explorer.set_filter_focused(false);
        Ok(())
    }

    fn clear_drag_map(&mut self) -> io::Result<()> {
        self.drags.begin_frame();
        self.drags.commit()
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Choice> {
        self.clicks.reset();
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alternate = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let command = key
            .modifiers
            .intersects(KeyModifiers::SUPER | KeyModifiers::META);
        let non_text_modifier = key.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SUPER
                | KeyModifiers::HYPER
                | KeyModifiers::META,
        );
        if key.code == KeyCode::Char('c') && control {
            return Some(Choice::Quit);
        }
        if key.code == KeyCode::Esc {
            if self.explorer.cwd() == self.root {
                return Some(Choice::Quit);
            }
            return self.apply_explorer(ExplorerInput::Parent);
        }
        if key.code == KeyCode::Enter {
            return self.apply_explorer(ExplorerInput::Open);
        }
        if key.code == KeyCode::Char('n') && control {
            return Some(Choice::Create);
        }
        if self.explorer.filter_focused() {
            return match key.code {
                KeyCode::Tab | KeyCode::Down => self.apply_explorer(ExplorerInput::BlurFilter),
                KeyCode::Up => self.apply_explorer(ExplorerInput::Up),
                KeyCode::Char('p') if control => self.apply_explorer(ExplorerInput::Up),
                KeyCode::PageUp => self.apply_explorer(ExplorerInput::PageUp),
                KeyCode::PageDown => self.apply_explorer(ExplorerInput::PageDown),
                KeyCode::Left if command => {
                    self.apply_explorer(ExplorerInput::FilterHome { extend: shift })
                }
                KeyCode::Right if command => {
                    self.apply_explorer(ExplorerInput::FilterEnd { extend: shift })
                }
                KeyCode::Left => self.apply_explorer(ExplorerInput::FilterLeft {
                    extend: shift,
                    word: control || alternate,
                }),
                KeyCode::Right => self.apply_explorer(ExplorerInput::FilterRight {
                    extend: shift,
                    word: control || alternate,
                }),
                KeyCode::Home => self.apply_explorer(ExplorerInput::FilterHome { extend: shift }),
                KeyCode::End => self.apply_explorer(ExplorerInput::FilterEnd { extend: shift }),
                KeyCode::Backspace => self.apply_explorer(ExplorerInput::FilterBackspace),
                KeyCode::Delete => self.apply_explorer(ExplorerInput::FilterDelete),
                KeyCode::Char('a') if control || command => {
                    self.apply_explorer(ExplorerInput::FilterSelectAll)
                }
                KeyCode::Char('u') if control => self.apply_explorer(ExplorerInput::ClearFilter),
                KeyCode::Char('r') if control => self.apply_explorer(ExplorerInput::Refresh),
                KeyCode::Char(character) if !non_text_modifier => {
                    self.apply_explorer(ExplorerInput::FilterCharacter(character))
                }
                _ => None,
            };
        }
        match key.code {
            KeyCode::Tab | KeyCode::Char('/') => {
                return self.apply_explorer(ExplorerInput::FocusFilter);
            }
            KeyCode::Up if self.explorer.selected_index() == 0 => {
                return self.apply_explorer(ExplorerInput::FocusFilter);
            }
            KeyCode::Up => return self.apply_explorer(ExplorerInput::Up),
            KeyCode::Char('p') if control => {
                return self.apply_explorer(ExplorerInput::Up);
            }
            KeyCode::Down => return self.apply_explorer(ExplorerInput::Down),
            KeyCode::PageUp => return self.apply_explorer(ExplorerInput::PageUp),
            KeyCode::PageDown => return self.apply_explorer(ExplorerInput::PageDown),
            KeyCode::Home => return self.apply_explorer(ExplorerInput::First),
            KeyCode::End => return self.apply_explorer(ExplorerInput::Last),
            KeyCode::Left | KeyCode::Backspace => {
                return self.apply_explorer(ExplorerInput::Parent);
            }
            KeyCode::Right => return self.apply_explorer(ExplorerInput::Open),
            KeyCode::Char('r') if control => {
                return self.apply_explorer(ExplorerInput::Refresh);
            }
            KeyCode::Char(character) if !non_text_modifier => {
                return self.apply_explorer(ExplorerInput::FilterCharacter(character));
            }
            _ => {}
        }
        None
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> Option<Choice> {
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.clicks.reset();
                self.apply_explorer(ExplorerInput::Up)
            }
            MouseEventKind::ScrollDown => {
                self.clicks.reset();
                self.apply_explorer(ExplorerInput::Down)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.explorer.filter_area().contains(position) {
                    self.clicks.reset();
                    self.explorer
                        .filter_mouse_down(position, mouse.modifiers.contains(KeyModifiers::SHIFT));
                    self.status = None;
                } else if let Some(path) = self
                    .explorer
                    .entry_at(position)
                    .map(|entry| entry.path().to_path_buf())
                {
                    self.explorer.set_filter_focused(false);
                    self.explorer.select_at(position);
                    if self.clicks.click(path) {
                        return self.apply_explorer(ExplorerInput::Open);
                    }
                } else {
                    self.clicks.reset();
                }
                None
            }
            MouseEventKind::Drag(MouseButton::Left) if self.explorer.filter_dragging() => {
                self.explorer.filter_mouse_drag(position);
                None
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.explorer.filter_mouse_up();
                None
            }
            _ => None,
        }
    }

    fn apply_explorer(&mut self, input: ExplorerInput) -> Option<Choice> {
        match self.explorer.handle(input) {
            Ok(ExplorerEvent::FileActivated(path)) => Some(Choice::Open(path)),
            Ok(ExplorerEvent::DirectoryChanged(_)) => {
                self.explorer.set_filter_focused(false);
                self.status = None;
                None
            }
            Ok(ExplorerEvent::FilterChanged | ExplorerEvent::Refreshed) => {
                self.status = None;
                None
            }
            Ok(_) => None,
            Err(error) => {
                self.status = Some(error.to_string());
                None
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let footer_rows = u16::from(area.height >= 2);
        let explorer_area = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.saturating_sub(footer_rows),
        );
        frame.render_widget(self.explorer.widget(&mut self.drags), explorer_area);

        if footer_rows == 1 {
            let footer = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
            let (message, style) = self.status.as_ref().map_or_else(
                || {
                    self.drags.register(footer, self.explorer.cwd());
                    (
                        self.explorer.cwd().display().to_string(),
                        Style::new().fg(self.theme.muted),
                    )
                },
                |message| (message.clone(), Style::new().fg(self.theme.kit.danger)),
            );
            frame.render_widget(Paragraph::new(format!("  {message}")).style(style), footer);
        }

        if let Some(position) = self.explorer.filter_cursor_position() {
            frame.set_cursor_position(position);
        }
    }
}

enum Choice {
    Open(PathBuf),
    Create,
    Quit,
}

fn note_stem(name: &str) -> Result<String, String> {
    let name = name.trim().trim_end_matches(".md").trim();
    if name.is_empty() {
        return Err("enter a note name".to_string());
    }
    let mut stem = String::new();
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            stem.push(character);
        } else if (character.is_whitespace() || matches!(character, '-' | '_'))
            && !stem.is_empty()
            && !stem.ends_with('-')
        {
            stem.push('-');
        }
    }
    let stem = stem.trim_end_matches('-').to_string();
    if stem.is_empty() {
        Err("use at least one letter or number".to_string())
    } else {
        Ok(stem)
    }
}

fn create_note(root: &Path, name: &str) -> Result<PathBuf, String> {
    let title = name.trim().trim_end_matches(".md").trim();
    let path = root.join(format!("{}.md", note_stem(name)?));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                "a note with that name already exists".to_string()
            } else {
                error.to_string()
            }
        })?;
    if let Err(error) = writeln!(file, "# {title}\n") {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error.to_string());
    }
    Ok(path)
}

fn prompt_new_note(
    terminal: &mut DefaultTerminal,
    root: &Path,
    theme: Theme,
) -> io::Result<Option<PathBuf>> {
    let mut name = String::new();
    let mut error: Option<String> = None;
    loop {
        terminal.draw(|frame| {
            let width = frame.area().width.saturating_sub(4).clamp(20, 62);
            let height = 9.min(frame.area().height);
            let area = Rect::new(
                frame.area().width.saturating_sub(width) / 2,
                frame.area().height.saturating_sub(height) / 2,
                width,
                height,
            );
            let block = Block::bordered()
                .title(Span::styled(
                    " New note ",
                    Style::new().fg(theme.accent).bold(),
                ))
                .border_style(Style::new().fg(theme.faint));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let [description, spacer, input, target, error_row, _] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .areas(inner);
            frame.render_widget(
                Paragraph::new("Name the note you want to create.")
                    .style(Style::new().fg(theme.muted)),
                description,
            );
            let prefix = "Name  ";
            let available = input.width.saturating_sub(prefix.len() as u16).max(1) as usize;
            let chars: Vec<char> = name.chars().collect();
            let from = chars.len().saturating_sub(available);
            let shown: String = chars[from..].iter().collect();
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(prefix, Style::new().fg(theme.muted)),
                    Span::styled(shown.clone(), Style::new().fg(theme.strong)),
                ])),
                input,
            );
            frame.set_cursor_position(Position {
                x: input.x + prefix.len() as u16 + shown.chars().count() as u16,
                y: input.y,
            });
            let target_name = note_stem(&name)
                .map(|stem| format!("Creates {stem}.md"))
                .unwrap_or_else(|_| "Creates a Markdown file in this folder".to_string());
            frame.render_widget(
                Paragraph::new(target_name).style(Style::new().fg(theme.faint)),
                target,
            );
            if let Some(message) = error.as_deref() {
                frame.render_widget(
                    Paragraph::new(message).style(Style::new().fg(ratatui::style::Color::Red)),
                    error_row,
                );
            }
            let _ = spacer;
        })?;

        match event::read()? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Enter => match create_note(root, &name) {
                        Ok(path) => return Ok(Some(path)),
                        Err(message) => error = Some(message),
                    },
                    KeyCode::Backspace => {
                        name.pop();
                        error = None;
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        name.clear();
                        error = None;
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                            && name.chars().count() < 120 =>
                    {
                        name.push(character);
                        error = None;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                let pasted: String = text
                    .chars()
                    .filter(|character| !matches!(character, '\n' | '\r' | '\0'))
                    .take(120usize.saturating_sub(name.chars().count()))
                    .collect();
                name.push_str(&pasted);
                error = None;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_notes_are_named_safely_and_never_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let path = create_note(root.path(), "Project Brief").expect("create note");
        assert_eq!(path, root.path().join("project-brief.md"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Project Brief\n\n"
        );
        assert!(create_note(root.path(), "Project Brief.md").is_err());
        assert_eq!(note_stem("../../Secrets"), Ok("secrets".to_string()));
        assert!(note_stem("---").is_err());
    }

    #[test]
    fn empty_vault_still_exposes_the_new_note_command() {
        let root = tempfile::tempdir().unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();
        assert!(picker.explorer.entries().is_empty());
        assert!(matches!(
            picker.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(Choice::Create)
        ));
    }

    #[test]
    fn picker_is_scoped_and_only_lists_folders_and_markdown_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("notes")).unwrap();
        std::fs::write(root.path().join("demo.md"), "# Demo\n").unwrap();
        std::fs::write(root.path().join("README.MD"), "# Readme\n").unwrap();
        std::fs::write(root.path().join("ignore.txt"), "ignore\n").unwrap();

        let picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();
        let labels = picker
            .explorer
            .entries()
            .iter()
            .map(|entry| entry.display_name())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["notes/", "demo.md", "README.MD"]);
        assert_eq!(
            picker.explorer.navigation_root(),
            Some(picker.root.as_path())
        );
        assert!(
            picker
                .explorer
                .entries()
                .iter()
                .all(|entry| !entry.is_parent())
        );
    }

    #[test]
    fn typing_uses_the_shared_filter_and_escape_is_back() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("README.md"), "# Readme\n").unwrap();
        std::fs::write(root.path().join("demo.md"), "# Demo\n").unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();

        assert!(!picker.explorer.filter_focused());
        for ch in "demo".chars() {
            assert!(
                picker
                    .on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
                    .is_none()
            );
        }
        assert_eq!(picker.explorer.filter(), "demo");
        assert_eq!(picker.explorer.entries().len(), 1);
        assert_eq!(picker.explorer.entries()[0].display_name(), "demo.md");
        assert!(picker.explorer.filter_focused());

        picker.explorer.clear_filter();
        let nested = std::fs::canonicalize(root.path().join("nested")).unwrap();
        picker.explorer.select_path(&nested);
        picker.apply_explorer(ExplorerInput::Open);
        assert_eq!(picker.explorer.cwd(), nested);
        assert!(
            picker
                .on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .is_none()
        );
        assert_eq!(picker.explorer.cwd(), picker.root);
        assert!(matches!(
            picker.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Choice::Quit)
        ));
    }

    #[test]
    fn arrow_focus_can_move_between_the_first_row_and_filter() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("demo.md"), "# Demo\n").unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();

        assert_eq!(picker.explorer.selected_index(), 0);
        assert!(!picker.explorer.filter_focused());
        assert!(
            picker
                .on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
                .is_none()
        );
        assert!(picker.explorer.filter_focused());
        assert!(
            picker
                .on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
                .is_none()
        );
        assert!(!picker.explorer.filter_focused());
    }

    #[test]
    fn startup_picker_uses_the_borderless_app_kit_list_pattern() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("demo.md"), "# Demo\n").unwrap();
        let theme = Theme::dark();
        let mut picker = Picker::open(root.path().to_path_buf(), theme).unwrap();
        let width = 40;
        let mut terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();

        let buffer = terminal.backend().buffer();
        let row_text = |row| {
            (0..width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        };
        assert!(row_text(0).starts_with("  / Filter notes"));
        assert!(row_text(1).starts_with("  demo.md"));
        assert_eq!(picker.explorer.list_area(), Rect::new(0, 1, width, 4));
        let selected_background = theme
            .kit
            .selected_row
            .bg
            .expect("App Kit selection has a background");
        assert!((0..width).all(|column| buffer[(column, 1)].bg == selected_background));
        assert_ne!(buffer[(0, 0)].symbol(), "┌");
        assert_ne!(buffer[(width - 1, 4)].symbol(), "┘");
        let root_prefix = picker
            .root
            .display()
            .to_string()
            .chars()
            .take(20)
            .collect::<String>();
        assert!(row_text(5).contains(&root_prefix));

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        assert!(picker.on_mouse(click).is_none(), "one click only selects");
        assert!(
            matches!(picker.on_mouse(click), Some(Choice::Open(_))),
            "double click opens"
        );
    }
}
