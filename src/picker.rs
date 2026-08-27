//! Vault picker: when the editor is pointed at a folder instead of a file,
//! this screen lists every Markdown file with fuzzy search plus an explicit
//! New note row. Enter opens or creates; quitting the editor returns here.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::frontmatter::{self, Metadata};
use crate::theme::Theme;

struct Entry {
    rel: String,
    path: PathBuf,
    modified: SystemTime,
}

pub struct Picker {
    root: PathBuf,
    name: String,
    entries: Vec<Entry>,
    /// Indices into `entries` matching the query, with matched char positions.
    matches: Vec<(usize, Vec<usize>)>,
    query: String,
    selected: usize,
    offset: usize,
    list_area: Rect,
    theme: Theme,
}

impl Picker {
    pub fn open(root: PathBuf, theme: Theme) -> io::Result<Self> {
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let mut picker = Self {
            root,
            name,
            entries: Vec::new(),
            matches: Vec::new(),
            query: String::new(),
            selected: 0,
            offset: 0,
            list_area: Rect::default(),
            theme,
        };
        picker.rescan()?;
        Ok(picker)
    }

    /// Runs the picker until the user chooses a file (`Some`) or quits (`None`).
    pub fn pick(&mut self, terminal: &mut DefaultTerminal) -> io::Result<Option<PathBuf>> {
        self.rescan()?;
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(choice) = self.on_key(key) {
                        match choice {
                            Choice::Open(path) => return Ok(Some(path)),
                            Choice::Create => {
                                if let Some(path) =
                                    prompt_new_note(terminal, &self.root, self.theme)?
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
                                if let Some(path) =
                                    prompt_new_note(terminal, &self.root, self.theme)?
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
                    self.query.push_str(text.trim());
                    self.selected = 0;
                    self.offset = 0;
                    self.refilter();
                }
                _ => {}
            }
        }
    }

    fn rescan(&mut self) -> io::Result<()> {
        self.entries.clear();
        let root = self.root.clone();
        collect_markdown(&root, &root, &mut self.entries)?;
        self.refilter();
        Ok(())
    }

    fn refilter(&mut self) {
        let mut scored: Vec<(usize, i64, Vec<usize>)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                fuzzy_match(&entry.rel, &self.query).map(|(score, positions)| (i, score, positions))
            })
            .collect();
        if self.query.is_empty() {
            // No query: recently modified first, like a CMS index.
            scored.sort_by(|a, b| {
                self.entries[b.0]
                    .modified
                    .cmp(&self.entries[a.0].modified)
                    .then_with(|| self.entries[a.0].rel.cmp(&self.entries[b.0].rel))
            });
        } else {
            scored.sort_by(|a, b| {
                a.1.cmp(&b.1)
                    .then_with(|| self.entries[a.0].rel.cmp(&self.entries[b.0].rel))
            });
        }
        self.matches = scored.into_iter().map(|(i, _, p)| (i, p)).collect();
        self.selected = self.selected.min(self.item_count().saturating_sub(1));
        self.offset = self.offset.min(self.selected);
    }

    fn show_create(&self) -> bool {
        self.query.is_empty()
    }

    fn item_count(&self) -> usize {
        self.matches.len() + usize::from(self.show_create())
    }

    fn choice_at(&self, row: usize) -> Option<Choice> {
        if let Some(&(entry, _)) = self.matches.get(row) {
            return Some(Choice::Open(self.entries[entry].path.clone()));
        }
        (self.show_create() && row == self.matches.len()).then_some(Choice::Create)
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Choice> {
        match key.code {
            KeyCode::Esc => {
                if self.query.is_empty() {
                    return Some(Choice::Quit);
                }
                self.query.clear();
                self.refilter();
            }
            KeyCode::Enter => {
                if let Some(choice) = self.choice_at(self.selected) {
                    return Some(choice);
                }
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(Choice::Create);
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-(self.page() as isize)),
            KeyCode::PageDown => self.move_selection(self.page() as isize),
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(-1);
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(1);
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(c);
                self.selected = 0;
                self.offset = 0;
                self.refilter();
            }
            _ => {}
        }
        None
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> Option<Choice> {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            MouseEventKind::Down(MouseButton::Left) => {
                let area = self.list_area;
                if area.contains(Position {
                    x: mouse.column,
                    y: mouse.row,
                }) {
                    let row = self.offset + (mouse.row - area.y) as usize;
                    if let Some(choice) = self.choice_at(row) {
                        self.selected = row;
                        return Some(choice);
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn move_selection(&mut self, delta: isize) {
        let Some(last) = self.item_count().checked_sub(1) else {
            return;
        };
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    fn page(&self) -> usize {
        (self.list_area.height as usize).max(1)
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [main, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let title = format!(" {} — {} notes ", self.name, self.entries.len());
        let block = Block::bordered()
            .title(Span::styled(title, Style::new().fg(self.theme.muted)))
            .border_style(Style::new().fg(self.theme.faint));
        let inner = block.inner(main).inner(Margin {
            horizontal: 1,
            vertical: 0,
        });
        frame.render_widget(block, main);

        let [search, divider, list] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(inner);
        self.list_area = list;

        let prompt = Line::from(vec![
            Span::styled("search ", Style::new().fg(self.theme.muted)),
            Span::styled(&self.query, Style::new().fg(self.theme.strong).bold()),
        ]);
        frame.render_widget(Paragraph::new(prompt), search);
        frame.set_cursor_position(Position {
            x: search.x + 7 + self.query.chars().count() as u16,
            y: search.y,
        });
        frame.render_widget(
            Paragraph::new(Span::styled(
                "─".repeat(divider.width as usize),
                Style::new().fg(self.theme.faint),
            )),
            divider,
        );

        let height = list.height as usize;
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if height > 0 && self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }

        if self.matches.is_empty() && !self.show_create() {
            frame.render_widget(
                Paragraph::new("no matches").style(Style::new().fg(self.theme.muted).italic()),
                list,
            );
        }
        for row in self.offset..self.item_count().min(self.offset + height) {
            let area = Rect {
                y: list.y + (row - self.offset) as u16,
                height: 1,
                ..list
            };
            let is_selected = row == self.selected;
            if let Some(&(entry, ref positions)) = self.matches.get(row) {
                frame.render_widget(
                    self.row_line(&self.entries[entry], positions, is_selected),
                    area,
                );
            } else {
                let marker = if is_selected { "› " } else { "  " };
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(marker, Style::new().fg(self.theme.accent).bold()),
                        Span::styled(
                            "+ New note",
                            if is_selected {
                                Style::new().fg(self.theme.strong).bold()
                            } else {
                                Style::new().fg(self.theme.text)
                            },
                        ),
                    ])),
                    area,
                );
            }
        }

        let hints = Line::from(vec![
            Span::styled(
                " type to search · Ctrl+N new · ↑↓ move · enter open · esc ",
                Style::new().fg(self.theme.muted),
            ),
            Span::styled(
                if self.query.is_empty() {
                    "quit"
                } else {
                    "clear"
                },
                Style::new().fg(self.theme.muted),
            ),
        ]);
        frame.render_widget(Paragraph::new(hints), status);
    }

    fn row_line(&self, entry: &Entry, positions: &[usize], selected: bool) -> Line<'static> {
        let mut spans = vec![Span::styled(
            if selected { "› " } else { "  " },
            Style::new().fg(self.theme.accent).bold(),
        )];
        let name_start = entry.rel.rfind('/').map_or(0, |i| i + 1);
        for (i, (byte, c)) in entry.rel.char_indices().enumerate() {
            let mut style = if byte < name_start {
                Style::new().fg(self.theme.faint)
            } else if selected {
                Style::new().fg(self.theme.strong).bold()
            } else {
                Style::new().fg(self.theme.text)
            };
            if positions.contains(&i) {
                style = style.fg(self.theme.accent).bold();
            }
            spans.push(Span::styled(c.to_string(), style));
        }
        Line::from(spans)
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
    let mut contents =
        frontmatter::compose_lines(&Metadata::new(title), &[String::new()]).join("\n");
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    if let Err(error) = file.write_all(contents.as_bytes()) {
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
            let [description, spacer, input, target, error_row, _, help] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Length(1),
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
            frame.render_widget(
                Paragraph::new("Enter create · Ctrl+U clear · Esc back")
                    .style(Style::new().fg(theme.muted)),
                help,
            );
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

fn collect_markdown(root: &Path, dir: &Path, out: &mut Vec<Entry>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_markdown(root, &path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.push(Entry {
                rel,
                path,
                modified,
            });
        }
    }
    Ok(())
}

/// Case-insensitive subsequence match. Returns (score, matched char positions);
/// lower scores are better (earlier, more contiguous matches).
fn fuzzy_match(haystack: &str, needle: &str) -> Option<(i64, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    let hay: Vec<char> = haystack.chars().flat_map(char::to_lowercase).collect();
    let mut positions = Vec::new();
    let mut score = 0i64;
    let mut from = 0usize;
    for nc in needle.chars().flat_map(char::to_lowercase) {
        let found = (from..hay.len()).find(|&i| hay[i] == nc)?;
        if let Some(&prev) = positions.last() {
            score += (found - prev - 1) as i64;
        } else {
            score += found as i64;
        }
        positions.push(found);
        from = found + 1;
    }
    Some((score, positions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_subsequences_case_insensitively() {
        let (_, positions) = fuzzy_match("Daily/2026-08-19.md", "d19").unwrap();
        assert_eq!(positions.len(), 3);
        assert!(fuzzy_match("Home.md", "xyz").is_none());
    }

    #[test]
    fn contiguous_matches_score_better() {
        let contiguous = fuzzy_match("recipes.md", "rec").unwrap().0;
        let scattered = fuzzy_match("reference-notes.md", "rec").unwrap().0;
        assert!(contiguous < scattered);
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(fuzzy_match("anything.md", ""), Some((0, Vec::new())));
    }

    #[test]
    fn new_notes_are_named_safely_and_never_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let path = create_note(root.path(), "Project Brief").expect("create note");
        assert_eq!(path, root.path().join("project-brief.md"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "---\ncover: \"#cccc\"\ntitle: \"Project Brief\"\ndescription: \"\"\n---\n"
        );
        assert!(create_note(root.path(), "Project Brief.md").is_err());
        assert_eq!(note_stem("../../Secrets"), Ok("secrets".to_string()));
        assert!(note_stem("---").is_err());
    }

    #[test]
    fn an_empty_vault_selects_new_note_instead_of_a_default_document() {
        let root = tempfile::tempdir().unwrap();
        let picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();
        assert_eq!(picker.item_count(), 1);
        assert!(matches!(picker.choice_at(0), Some(Choice::Create)));
    }

    #[test]
    fn typing_filters_the_picker_and_escape_clears_the_query() {
        let mut picker = Picker {
            root: PathBuf::from("."),
            name: "notes".into(),
            entries: vec![
                Entry {
                    rel: "README.md".into(),
                    path: PathBuf::from("README.md"),
                    modified: SystemTime::UNIX_EPOCH,
                },
                Entry {
                    rel: "demo.md".into(),
                    path: PathBuf::from("demo.md"),
                    modified: SystemTime::UNIX_EPOCH,
                },
            ],
            matches: Vec::new(),
            query: String::new(),
            selected: 0,
            offset: 0,
            list_area: Rect::default(),
            theme: Theme::dark(),
        };
        picker.refilter();

        for ch in "demo".chars() {
            assert!(
                picker
                    .on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
                    .is_none()
            );
        }
        assert_eq!(picker.query, "demo");
        assert_eq!(picker.matches.len(), 1);
        assert_eq!(picker.entries[picker.matches[0].0].rel, "demo.md");

        assert!(
            picker
                .on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .is_none()
        );
        assert!(picker.query.is_empty());
        assert_eq!(picker.matches.len(), 2);
    }

    #[test]
    fn renders_workspace_vault_list() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vault");
        if !root.exists() {
            return; // workspace test vault not present; nothing to render
        }
        let mut picker = Picker::open(root, Theme::dark()).unwrap();
        picker.query = "2026".into();
        picker.refilter();

        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut rows = Vec::new();
        for y in 0..buffer.area.height {
            rows.push(
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>(),
            );
        }
        let frame = rows.join("\n");
        println!("{frame}");

        assert!(frame.contains("2026-08-19.md"));
        assert!(!frame.contains("Home.md"), "filtered-out notes should hide");
    }
}
