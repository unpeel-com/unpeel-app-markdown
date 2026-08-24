//! Vault picker: when the editor is pointed at a folder instead of a file,
//! this screen lists every markdown file in it with a fuzzy search on top.
//! Enter opens the note; quitting the editor returns here.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame};

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
}

impl Picker {
    pub fn open(root: PathBuf) -> io::Result<Self> {
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
                        return Ok(choice.into_option());
                    }
                }
                Event::Mouse(mouse) => {
                    if let Some(choice) = self.on_mouse(mouse) {
                        return Ok(choice.into_option());
                    }
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
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
        self.offset = self.offset.min(self.selected);
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
                if let Some(&(entry, _)) = self.matches.get(self.selected) {
                    return Some(Choice::Open(self.entries[entry].path.clone()));
                }
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
                    if let Some(&(entry, _)) = self.matches.get(row) {
                        self.selected = row;
                        return Some(Choice::Open(self.entries[entry].path.clone()));
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    fn page(&self) -> usize {
        (self.list_area.height as usize).max(1)
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [main, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let title = format!(" {} — {} notes ", self.name, self.entries.len());
        let block = Block::bordered().title(title.dark_gray()).dark_gray();
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
            Span::styled("search ", Style::new().dark_gray()),
            Span::styled(&self.query, Style::new().bold()),
        ]);
        frame.render_widget(Paragraph::new(prompt), search);
        frame.set_cursor_position(Position {
            x: search.x + 7 + self.query.chars().count() as u16,
            y: search.y,
        });
        frame.render_widget(
            Paragraph::new("─".repeat(divider.width as usize).dark_gray()),
            divider,
        );

        let height = list.height as usize;
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if height > 0 && self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }

        if self.matches.is_empty() {
            let message = if self.entries.is_empty() {
                "no markdown files in this folder"
            } else {
                "no matches"
            };
            frame.render_widget(Paragraph::new(message.dark_gray().italic()), list);
        }
        for (row, &(entry, ref positions)) in self
            .matches
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(height)
        {
            let area = Rect {
                y: list.y + (row - self.offset) as u16,
                height: 1,
                ..list
            };
            let is_selected = row == self.selected;
            frame.render_widget(
                self.row_line(&self.entries[entry], positions, is_selected),
                area,
            );
        }

        let hints = Line::from(vec![
            Span::styled(
                " type to search · ↑↓ move · enter open · esc ",
                Style::new().dark_gray(),
            ),
            Span::styled(
                if self.query.is_empty() {
                    "quit"
                } else {
                    "clear"
                },
                Style::new().dark_gray(),
            ),
        ]);
        frame.render_widget(Paragraph::new(hints), status);
    }

    fn row_line(&self, entry: &Entry, positions: &[usize], selected: bool) -> Line<'static> {
        let mut spans = vec![Span::styled(
            if selected { "› " } else { "  " },
            Style::new().yellow().bold(),
        )];
        let name_start = entry.rel.rfind('/').map_or(0, |i| i + 1);
        for (i, (byte, c)) in entry.rel.char_indices().enumerate() {
            let mut style = if byte < name_start {
                Style::new().dark_gray()
            } else if selected {
                Style::new().bold()
            } else {
                Style::new()
            };
            if positions.contains(&i) {
                style = style.yellow().bold();
            }
            spans.push(Span::styled(c.to_string(), style));
        }
        Line::from(spans)
    }
}

enum Choice {
    Open(PathBuf),
    Quit,
}

impl Choice {
    fn into_option(self) -> Option<PathBuf> {
        match self {
            Choice::Open(path) => Some(path),
            Choice::Quit => None,
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
    fn renders_workspace_vault_list() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vault");
        if !root.exists() {
            return; // workspace test vault not present; nothing to render
        }
        let mut picker = Picker::open(root).unwrap();
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
