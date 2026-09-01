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
    Button, ButtonRole, DoubleClickTracker, DragSurface, Explorer, ExplorerEvent, ExplorerInput,
    ExplorerTheme, Input, List, Page, ThemeMonitor, UiBridge, UiBridgeEvent, UiEvent, UiEventKind,
    UiEventOutcome, UiEventValue, UiNode, display_path_from_root, tree_delta_operations,
};

use crate::theme::Theme;

const UI_VIEW_ID: &str = "main";
const UI_TREE_ID: &str = "notes-tree";
const UI_NEW_NOTE_ID: &str = "new-note";
const UI_NEW_NOTE_ACTION: &str = "new-note";
const UI_NEW_NOTE_INPUT_ID: &str = "new-note-name";
const UI_NEW_NOTE_SET_VALUE: &str = "set-new-note-name";
const UI_NEW_NOTE_SUBMIT: &str = "create-new-note";
const UI_NEW_NOTE_CANCEL: &str = "cancel-new-note";

pub struct Picker {
    root: PathBuf,
    explorer: Explorer,
    create_area: Rect,
    drags: DragSurface,
    clicks: DoubleClickTracker<PathBuf>,
    status: Option<String>,
    create: Option<CreateState>,
    theme: Theme,
    theme_monitor: ThemeMonitor,
}

impl Picker {
    pub fn open(root: PathBuf, theme: Theme) -> io::Result<Self> {
        let mut explorer = Explorer::scoped(root)?
            .with_file_extensions(["md"])?
            .with_theme(ExplorerTheme::for_theme(theme.kit));
        explorer.set_show_path(false);
        explorer.set_filter_placeholder("Filter notes");
        let root = explorer
            .navigation_root()
            .expect("scoped Explorer always has a root")
            .to_path_buf();
        Ok(Self {
            root,
            explorer,
            create_area: Rect::default(),
            drags: DragSurface::detect(),
            clicks: DoubleClickTracker::new(),
            status: None,
            create: None,
            theme,
            theme_monitor: ThemeMonitor::from_theme(theme.kit),
        })
    }

    /// Runs the picker until the user chooses a file (`Some`) or quits (`None`).
    pub fn pick(
        &mut self,
        terminal: &mut DefaultTerminal,
        bridge: &mut UiBridge,
        revision_counter: &mut u64,
    ) -> io::Result<Option<PathBuf>> {
        self.rescan()?;
        let mut revision = revision_counter
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Markdown UI revision space is exhausted"))?;
        let mut published = self.ui_node();
        bridge
            .publish(UI_VIEW_ID, revision, published.clone())
            .map_err(ui_bridge_error)?;
        loop {
            if let Some(result) = self.drain_bridge(bridge, &mut revision, &mut published)? {
                *revision_counter = revision;
                return Ok(result);
            }
            self.publish_projection(bridge, &mut revision, &mut published)?;
            if bridge.should_render_terminal() {
                self.drags.begin_frame();
                terminal.draw(|frame| self.draw(frame))?;
                self.drags.commit()?;
            }
            while !event::poll(Duration::from_millis(250))? {
                self.drags.heartbeat()?;
                if let Some(result) = self.drain_bridge(bridge, &mut revision, &mut published)? {
                    *revision_counter = revision;
                    return Ok(result);
                }
                if self.theme_monitor.refresh() {
                    self.theme = Theme::from_kit(self.theme_monitor.theme());
                    self.explorer
                        .set_theme(ExplorerTheme::for_theme(self.theme.kit));
                }
            }
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    let choice = if self.create.is_some() {
                        self.on_create_key(key)
                    } else {
                        self.on_key(key)
                    };
                    if let Some(choice) = choice
                        && let Some(result) = self.apply_choice(choice)?
                    {
                        self.publish_projection(bridge, &mut revision, &mut published)?;
                        *revision_counter = revision;
                        return Ok(result);
                    }
                }
                Event::Mouse(mouse) if self.create.is_none() => {
                    if let Some(choice) = self.on_mouse(mouse) {
                        if let Some(result) = self.apply_choice(choice)? {
                            self.publish_projection(bridge, &mut revision, &mut published)?;
                            *revision_counter = revision;
                            return Ok(result);
                        }
                    }
                }
                Event::Paste(text) => {
                    self.clicks.reset();
                    if let Some(create) = self.create.as_mut() {
                        create.insert(&text);
                    } else {
                        self.explorer.insert_filter_text(text);
                        self.status = None;
                    }
                }
                _ => {}
            }
            self.publish_projection(bridge, &mut revision, &mut published)?;
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

    fn apply_choice(&mut self, choice: Choice) -> io::Result<Option<Option<PathBuf>>> {
        match choice {
            Choice::Open(path) => Ok(Some(Some(path))),
            Choice::Create(folder) => {
                self.clear_drag_map()?;
                self.create = Some(CreateState::new(folder));
                Ok(None)
            }
            Choice::Update => Ok(None),
            Choice::Quit => Ok(Some(None)),
        }
    }

    fn on_create_key(&mut self, key: KeyEvent) -> Option<Choice> {
        let create = self.create.as_mut()?;
        match key.code {
            KeyCode::Esc => {
                self.create = None;
                None
            }
            KeyCode::Enter => {
                let folder = create.folder.clone();
                let name = create.name.clone();
                match create_note(&folder, &name) {
                    Ok(path) => Some(Choice::Open(path)),
                    Err(message) => {
                        create.error = Some(message);
                        None
                    }
                }
            }
            KeyCode::Backspace => {
                create.name.pop();
                create.error = None;
                None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                create.name.clear();
                create.error = None;
                None
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                    && create.name.chars().count() < 120 =>
            {
                create.name.push(character);
                create.error = None;
                None
            }
            _ => None,
        }
    }

    fn ui_node(&mut self) -> UiNode {
        if let Some(create) = &self.create {
            let message = create.error.clone().unwrap_or_else(|| {
                note_stem(&create.name)
                    .map(|stem| format!("Creates {stem}.md"))
                    .unwrap_or_else(|_| "Creates a Markdown file in this folder".to_string())
            });
            let page = Page::new(
                "New note",
                List::new("new-note-details", Vec::new()).empty_message(message),
            )
            .input(
                Input::new(UI_NEW_NOTE_INPUT_ID, "Name")
                    .value(create.name.clone())
                    .placeholder("Note name")
                    .set_value_action(UI_NEW_NOTE_SET_VALUE)
                    .submit_action(UI_NEW_NOTE_SUBMIT),
            )
            .back_action(UI_NEW_NOTE_CANCEL);
            return UiNode::page(UI_TREE_ID, page);
        }
        let tree = self.explorer.semantic_tree("Notes").primary_action(
            Button::new(UI_NEW_NOTE_ID, "New Markdown file", UI_NEW_NOTE_ACTION)
                .role(ButtonRole::Primary),
        );
        UiNode::tree(UI_TREE_ID, tree)
    }

    fn publish_projection(
        &mut self,
        bridge: &mut UiBridge,
        revision: &mut u64,
        published: &mut UiNode,
    ) -> io::Result<()> {
        let next = self.ui_node();
        if next == *published {
            return Ok(());
        }
        let operations = tree_delta_operations(published, &next);
        let next_revision = revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Markdown UI revision space is exhausted"))?;
        bridge
            .publish_delta(UI_VIEW_ID, *revision, next_revision, operations)
            .map_err(ui_bridge_error)?;
        *revision = next_revision;
        *published = next;
        Ok(())
    }

    fn drain_bridge(
        &mut self,
        bridge: &mut UiBridge,
        revision: &mut u64,
        published: &mut UiNode,
    ) -> io::Result<Option<Option<PathBuf>>> {
        while let Some(message) = bridge.poll().map_err(ui_bridge_error)? {
            let UiBridgeEvent::Action { event, .. } = message else {
                continue;
            };
            let decision = self.handle_ui_event(*revision, &event);
            let (outcome, result) = match decision {
                Ok(Some(choice)) => match self.apply_choice(choice) {
                    Ok(result) => (UiEventOutcome::Applied, result),
                    Err(error) => (UiEventOutcome::Rejected(error.to_string()), None),
                },
                Ok(None) => (
                    UiEventOutcome::Rejected(
                        "Action targets a different picker component".to_string(),
                    ),
                    None,
                ),
                Err(message) => (UiEventOutcome::Rejected(message), None),
            };
            self.publish_projection(bridge, revision, published)?;
            bridge
                .acknowledge(&event, outcome, *revision)
                .map_err(ui_bridge_error)?;
            if result.is_some() {
                return Ok(result);
            }
        }
        Ok(None)
    }

    fn handle_ui_event(
        &mut self,
        revision: u64,
        event: &UiEvent,
    ) -> Result<Option<Choice>, String> {
        if event.base_revision != revision {
            return Err(format!(
                "Picker changed from revision {} to {revision}; retry the action",
                event.base_revision
            ));
        }
        if let Some(create) = self.create.as_mut() {
            match (
                event.action.node_id.as_str(),
                event.action.action.as_str(),
                event.action.kind,
                &event.action.value,
            ) {
                (
                    UI_NEW_NOTE_INPUT_ID,
                    UI_NEW_NOTE_SET_VALUE,
                    UiEventKind::Change,
                    UiEventValue::Text(value),
                ) => {
                    create.set(value);
                    Ok(Some(Choice::Update))
                }
                (
                    UI_NEW_NOTE_INPUT_ID,
                    UI_NEW_NOTE_SUBMIT,
                    UiEventKind::Submit,
                    UiEventValue::Text(value),
                ) => {
                    create.set(value);
                    match create_note(&create.folder, &create.name) {
                        Ok(path) => Ok(Some(Choice::Open(path))),
                        Err(message) => {
                            create.error = Some(message.clone());
                            Err(message)
                        }
                    }
                }
                (UI_TREE_ID, UI_NEW_NOTE_CANCEL, UiEventKind::Cancel, UiEventValue::None) => {
                    self.create = None;
                    Ok(Some(Choice::Update))
                }
                _ => Ok(None),
            }
        } else if event.action.node_id.as_str() == UI_NEW_NOTE_ID
            && event.action.action.as_str() == UI_NEW_NOTE_ACTION
            && event.action.kind == UiEventKind::Activate
            && event.action.value == UiEventValue::None
        {
            Ok(Some(Choice::Create(self.explorer.cwd().to_path_buf())))
        } else {
            self.explorer
                .handle_ui_event(revision, UI_TREE_ID, event)
                .map(|event| match event {
                    Some(ExplorerEvent::FileActivated(path)) => Some(Choice::Open(path)),
                    Some(_) => Some(Choice::Update),
                    None => None,
                })
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Choice> {
        self.clicks.reset();
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('c') && control {
            return Some(Choice::Quit);
        }
        if key.code == KeyCode::Esc {
            if self.explorer.cwd() == self.root {
                return Some(Choice::Quit);
            }
            let input = self.explorer.input_for_key(&key)?;
            return self.apply_explorer(input);
        }
        if key.code == KeyCode::Char('n') && control {
            return Some(Choice::Create(self.explorer.cwd().to_path_buf()));
        }
        if key.code == KeyCode::Char('p') && control {
            return self.apply_explorer(ExplorerInput::Up);
        }
        if key.code == KeyCode::Char('r') && control {
            return self.apply_explorer(ExplorerInput::Refresh);
        }
        let input = self.explorer.input_for_key(&key)?;
        self.apply_explorer(input)
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
                if self.create_area.contains(position) {
                    self.clicks.reset();
                    return Some(Choice::Create(self.explorer.cwd().to_path_buf()));
                } else if self.explorer.filter_area().contains(position) {
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
        self.create_area = Rect::default();
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
            let action_width = footer
                .width
                .min("  + New Markdown file  Ctrl+N".len() as u16);
            self.create_area = Rect::new(footer.x, footer.y, action_width, 1);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  + ", Style::new().fg(self.theme.accent).bold()),
                    Span::styled("New Markdown file", Style::new().fg(self.theme.strong)),
                    Span::styled("  Ctrl+N", Style::new().fg(self.theme.muted)),
                ])),
                self.create_area,
            );

            let details = Rect::new(
                footer.x.saturating_add(action_width),
                footer.y,
                footer.width.saturating_sub(action_width),
                1,
            );
            let (message, style) = self.status.as_ref().map_or_else(
                || {
                    self.drags.register(details, self.explorer.cwd());
                    (
                        display_path_from_root(self.explorer.cwd(), &self.root),
                        Style::new().fg(self.theme.muted),
                    )
                },
                |message| (message.clone(), Style::new().fg(self.theme.kit.danger)),
            );
            frame.render_widget(Paragraph::new(format!("  {message}")).style(style), details);
        }

        if let Some(position) = self.explorer.filter_cursor_position() {
            frame.set_cursor_position(position);
        }
        if self.create.is_some() {
            self.draw_create_prompt(frame);
        }
    }

    fn draw_create_prompt(&self, frame: &mut Frame) {
        let Some(create) = &self.create else { return };
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
                Style::new().fg(self.theme.accent).bold(),
            ))
            .border_style(Style::new().fg(self.theme.faint));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [description, _spacer, input, target, error_row, _] = Layout::vertical([
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
                .style(Style::new().fg(self.theme.muted)),
            description,
        );
        let prefix = "Name  ";
        let available = input.width.saturating_sub(prefix.len() as u16).max(1) as usize;
        let chars: Vec<char> = create.name.chars().collect();
        let from = chars.len().saturating_sub(available);
        let shown: String = chars[from..].iter().collect();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, Style::new().fg(self.theme.muted)),
                Span::styled(shown.clone(), Style::new().fg(self.theme.strong)),
            ])),
            input,
        );
        frame.set_cursor_position(Position {
            x: input.x + prefix.len() as u16 + shown.chars().count() as u16,
            y: input.y,
        });
        let target_name = note_stem(&create.name)
            .map(|stem| format!("Creates {stem}.md"))
            .unwrap_or_else(|_| "Creates a Markdown file in this folder".to_string());
        frame.render_widget(
            Paragraph::new(target_name).style(Style::new().fg(self.theme.faint)),
            target,
        );
        if let Some(message) = create.error.as_deref() {
            frame.render_widget(
                Paragraph::new(message).style(Style::new().fg(ratatui::style::Color::Red)),
                error_row,
            );
        }
    }
}

enum Choice {
    Open(PathBuf),
    Create(PathBuf),
    Update,
    Quit,
}

struct CreateState {
    folder: PathBuf,
    name: String,
    error: Option<String>,
}

impl CreateState {
    fn new(folder: PathBuf) -> Self {
        Self {
            folder,
            name: String::new(),
            error: None,
        }
    }

    fn set(&mut self, value: &str) {
        self.name = value
            .chars()
            .filter(|character| !matches!(character, '\n' | '\r' | '\0'))
            .take(120)
            .collect();
        self.error = None;
    }

    fn insert(&mut self, value: &str) {
        let remaining = 120usize.saturating_sub(self.name.chars().count());
        self.name.extend(
            value
                .chars()
                .filter(|character| !matches!(character, '\n' | '\r' | '\0'))
                .take(remaining),
        );
        self.error = None;
    }
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

fn ui_bridge_error(error: unpeel_app_kit::UiBridgeError) -> io::Error {
    io::Error::other(error)
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
            Some(Choice::Create(folder)) if folder == picker.root
        ));
    }

    #[test]
    fn new_note_command_targets_the_current_nested_folder() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let nested = std::fs::canonicalize(nested).unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();
        picker.explorer.select_path(&nested);
        picker.apply_explorer(ExplorerInput::Open);

        assert!(matches!(
            picker.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(Choice::Create(folder)) if folder == nested
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
        let footer = row_text(5);
        assert!(footer.contains("+ New Markdown file"));
        assert!(footer.contains("Ctrl+N"));
        assert!(footer.trim_end().ends_with('.'));

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

        let create_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: picker.create_area.x + 2,
            row: picker.create_area.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(
            picker.on_mouse(create_click),
            Some(Choice::Create(folder)) if folder == picker.root
        ));
    }
}
