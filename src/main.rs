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
mod unpeel;

use std::path::PathBuf;

use app::App;
use backend::BackendCapture;
use picker::Picker;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    install::ensure_installed();
    let mut status = unpeel::StatusReporter::detect();
    status.idle();
    let path = resolve_path();
    ratatui::run(|terminal| {
        let _capture = BackendCapture::enable(terminal)?;
        if path.is_dir() {
            // Vault mode: searchable note list; quitting the editor returns here.
            let mut picker = Picker::open(path)?;
            while let Some(file) = picker.pick(terminal)? {
                status.set_status(&editing_status(&file));
                status.flush();
                App::open(file)?.run(terminal)?;
                status.set_status("browsing notes");
                status.flush();
            }
            Ok(())
        } else {
            status.set_status(&editing_status(&path));
            status.flush();
            App::open(path)?.run(terminal)
        }
    })?;
    Ok(())
}

/// Sidebar status line: which note is open. Editing is user-paced typing, so
/// this App never claims Busy — the status text is the live surface.
fn editing_status(path: &PathBuf) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    format!("editing {name}")
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
