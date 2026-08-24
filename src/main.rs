mod app;
mod backend;
mod block;
mod clipboard;
mod format;
mod heading;
mod highlight;
mod install;
mod mouse;
mod picker;
mod slash;
mod theme;
mod unpeel;

use std::path::PathBuf;

use app::App;
use backend::BackendCapture;
use picker::Picker;
use theme::Theme;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    install::ensure_installed();
    let mut status = unpeel::StatusReporter::detect();
    status.idle();
    let path = resolve_path();
    let theme = Theme::detect();
    ratatui::run(|terminal| {
        let _capture = BackendCapture::enable(terminal)?;
        if path.is_dir() {
            // Vault mode: searchable note list; quitting the editor returns
            // here. The session title follows navigation: the open note
            // while editing, the vault folder while browsing.
            let vault_title = display_name(&path);
            status.set_title(&vault_title);
            let mut picker = Picker::open(path, theme)?;
            while let Some(file) = picker.pick(terminal)? {
                status.set_title(&display_name(&file));
                status.set_status(&editing_status(&file));
                status.flush();
                App::open(file, theme)?.run(terminal)?;
                status.set_title(&vault_title);
                status.set_status("browsing notes");
                status.flush();
            }
            Ok(())
        } else {
            status.set_title(&display_name(&path));
            status.set_status(&editing_status(&path));
            status.flush();
            App::open(path, theme)?.run(terminal)
        }
    })?;
    Ok(())
}

/// Sidebar status line: which note is open. Editing is user-paced typing, so
/// this App never claims Busy — the status text is the live surface.
fn editing_status(path: &PathBuf) -> String {
    format!("editing {}", display_name(path))
}

/// The session-title form of a path: the file or folder name. "." resolves
/// to the real directory name so a bare vault launch titles usefully.
fn display_name(path: &PathBuf) -> String {
    let resolved = if path.as_os_str() == "." {
        std::env::current_dir().unwrap_or_else(|_| path.clone())
    } else {
        path.clone()
    };
    resolved
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| resolved.display().to_string())
}

fn resolve_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    // No argument: open the current directory as a vault when it holds any
    // markdown, otherwise fall back to the bundled demo (development runs).
    let cwd = PathBuf::from(".");
    let has_markdown = std::fs::read_dir(&cwd).is_ok_and(|entries| {
        entries.filter_map(|entry| entry.ok()).any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "md")
        })
    });
    if has_markdown {
        return cwd;
    }
    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo.md");
    if bundled.exists() {
        return bundled;
    }
    cwd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_exists() {
        // Either the working directory as a vault or the bundled demo —
        // both must exist so a bare launch always opens something.
        let path = resolve_path();
        assert!(path.exists(), "expected a default document or vault");
    }
}
