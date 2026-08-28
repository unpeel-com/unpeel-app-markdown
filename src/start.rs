//! Persistent bare-launch state shared in shape with `unpeel-design`.
//!
//! A command-line path is always explicit and bypasses this state. A bare
//! launch remembers only the user-chosen notes folder; notes themselves stay
//! ordinary Markdown files and are rescanned every time the picker opens.

use std::io;
use std::path::{Path, PathBuf};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::theme::Theme;

const STATE_VERSION: u64 = 1;

fn config_root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("UNPEEL_APP_CONFIG_HOME") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Some(path.join("unpeel-apps"));
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config").join("unpeel-apps"))
}

fn state_path(app_id: &str) -> Option<PathBuf> {
    Some(config_root()?.join(app_id).join("start.json"))
}

fn read_state_at(path: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read(path).ok()?;
    if raw.len() > 16 * 1024 {
        return None;
    }
    let state: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    if state.get("version")?.as_u64()? != STATE_VERSION {
        return None;
    }
    Some(state)
}

fn read_workspace_at(path: &Path) -> Option<PathBuf> {
    let state = read_state_at(path)?;
    let workspace = PathBuf::from(state.get("workspace")?.as_str()?);
    workspace.is_dir().then_some(workspace)
}

pub fn read_workspace(app_id: &str) -> Option<PathBuf> {
    read_workspace_at(&state_path(app_id)?)
}

fn read_autosave_at(path: &Path) -> bool {
    read_state_at(path)
        .and_then(|state| state.get("autosave").and_then(serde_json::Value::as_bool))
        .unwrap_or(true)
}

pub fn read_autosave(app_id: &str) -> bool {
    state_path(app_id).is_none_or(|path| read_autosave_at(&path))
}

fn write_state_at(path: &Path, workspace: Option<&Path>, autosave: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("start state has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "version": STATE_VERSION,
        "workspace": workspace.map(|path| path.to_string_lossy().into_owned()),
        "autosave": autosave,
    }))?;
    let temporary = parent.join(format!(".start.json.{}.tmp", std::process::id()));
    std::fs::write(&temporary, body)?;
    std::fs::rename(temporary, path)
}

fn write_workspace_at(path: &Path, workspace: &Path) -> io::Result<()> {
    write_state_at(path, Some(workspace), read_autosave_at(path))
}

pub fn write_workspace(app_id: &str, workspace: &Path) -> io::Result<()> {
    let Some(path) = state_path(app_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no home directory available for start state",
        ));
    };
    write_workspace_at(&path, workspace)
}

fn write_autosave_at(path: &Path, enabled: bool) -> io::Result<()> {
    let workspace = read_workspace_at(path);
    write_state_at(path, workspace.as_deref(), enabled)
}

pub fn write_autosave(app_id: &str, enabled: bool) -> io::Result<()> {
    let Some(path) = state_path(app_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no home directory available for start state",
        ));
    };
    write_autosave_at(&path, enabled)
}

pub fn resolve_folder_input(input: &str) -> io::Result<PathBuf> {
    let input = input.trim();
    if input.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "enter a folder path",
        ));
    }
    let expanded = if input == "~" {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?
    } else if let Some(rest) = input.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?
            .join(rest)
    } else {
        PathBuf::from(input)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    std::fs::create_dir_all(&absolute)?;
    let canonical = absolute.canonicalize()?;
    if !canonical.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a folder", canonical.display()),
        ));
    }
    Ok(canonical)
}

fn user_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home.filter(|home| !home.as_os_str().is_empty()) {
        if path == home {
            return "~".to_string();
        }
        if let Ok(relative) = path.strip_prefix(home) {
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

fn default_folder_input() -> String {
    let folder = std::env::current_dir()
        .map(|path| path.join("docs"))
        .unwrap_or_else(|_| PathBuf::from("docs"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    user_path(&folder, home.as_deref())
}

/// First-run folder chooser. The project's `docs` folder is prefilled so it
/// can be accepted with Enter, while still allowing a different path.
pub fn choose_workspace(
    terminal: &mut DefaultTerminal,
    theme: Theme,
) -> io::Result<Option<PathBuf>> {
    let mut value = default_folder_input();
    let mut error: Option<String> = None;
    let hint = "Press Enter to use this folder, or edit the path";
    loop {
        terminal.draw(|frame| {
            let width = frame.area().width.saturating_sub(4).clamp(20, 78);
            let height = 11.min(frame.area().height);
            let area = ratatui::layout::Rect::new(
                frame.area().width.saturating_sub(width) / 2,
                frame.area().height.saturating_sub(height) / 2,
                width,
                height,
            );
            let block = Block::bordered()
                .title(Span::styled(
                    " UNPEEL MARKDOWN ",
                    Style::new().fg(theme.accent).bold(),
                ))
                .border_style(Style::new().fg(theme.faint));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let [
                heading,
                description,
                spacer,
                input,
                example_row,
                error_row,
                _,
            ] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .areas(inner);
            frame.render_widget(
                Paragraph::new("Choose your notes folder")
                    .style(Style::new().fg(theme.strong).bold()),
                heading,
            );
            frame.render_widget(
                Paragraph::new("Notes will stay as ordinary Markdown files in this folder.")
                    .style(Style::new().fg(theme.muted)),
                description,
            );
            let prefix = "Folder  ";
            let available = input.width.saturating_sub(prefix.len() as u16).max(1) as usize;
            let chars: Vec<char> = value.chars().collect();
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
            frame.render_widget(
                Paragraph::new(hint).style(Style::new().fg(theme.faint)),
                example_row,
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
                    KeyCode::Enter => match resolve_folder_input(&value) {
                        Ok(path) => return Ok(Some(path)),
                        Err(failure) => error = Some(failure.to_string()),
                    },
                    KeyCode::Backspace => {
                        value.pop();
                        error = None;
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.clear();
                        error = None;
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                            && value.chars().count() < 512 =>
                    {
                        value.push(character);
                        error = None;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                let pasted: String = text
                    .chars()
                    .filter(|character| !matches!(character, '\n' | '\r' | '\0'))
                    .take(512usize.saturating_sub(value.chars().count()))
                    .collect();
                value.push_str(&pasted);
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
    fn state_round_trip_keeps_only_an_existing_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("notes");
        std::fs::create_dir(&workspace).unwrap();
        let state = temp.path().join("state/start.json");
        write_workspace_at(&state, &workspace).unwrap();
        assert_eq!(read_workspace_at(&state), Some(workspace.clone()));
        std::fs::remove_dir(workspace).unwrap();
        assert_eq!(read_workspace_at(&state), None);
    }

    #[test]
    fn autosave_defaults_on_and_persists_without_losing_the_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("notes");
        std::fs::create_dir(&workspace).unwrap();
        let state = temp.path().join("state/start.json");

        assert!(read_autosave_at(&state));
        write_workspace_at(&state, &workspace).unwrap();
        write_autosave_at(&state, false).unwrap();
        assert!(!read_autosave_at(&state));
        assert_eq!(read_workspace_at(&state), Some(workspace.clone()));

        write_workspace_at(&state, &workspace).unwrap();
        assert!(!read_autosave_at(&state));
    }

    #[test]
    fn corrupt_and_future_state_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("start.json");
        std::fs::write(&state, b"not json").unwrap();
        assert_eq!(read_workspace_at(&state), None);
        std::fs::write(
            &state,
            format!(
                r#"{{"version":99,"workspace":{}}}"#,
                serde_json::to_string(&temp.path()).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(read_workspace_at(&state), None);
    }

    #[test]
    fn paths_inside_home_use_a_tilde_prefix() {
        let home = Path::new("/Users/alice");
        assert_eq!(
            user_path(Path::new("/Users/alice/Dev/project/docs"), Some(home)),
            "~/Dev/project/docs"
        );
        assert_eq!(user_path(home, Some(home)), "~");
        assert_eq!(
            user_path(Path::new("/opt/project/docs"), Some(home)),
            "/opt/project/docs"
        );
    }
}
