//! The Unpeel hook-in: when Unpeel is on this machine, register as an
//! installed Unpeel App by writing one manifest. Declaring is the entire
//! integration — Unpeel then recognizes a hand-typed `unpeel-markdown` in
//! any hosted terminal and brands the session row with the tint below.

use std::path::PathBuf;

pub const APP_ID: &str = "unpeel.app.markdown";

const APP_TOML: &str = r##"# Installed by unpeel-markdown; safe to delete (it reinstalls on next run).
manifest_version = 1
id = "unpeel.app.markdown"
name = "Markdown"
version = "@APP_VERSION@"
command = "@LAUNCH_COMMAND@"
description = "Terminal markdown editor with live block styling, headings picker, slash commands, and mouse support; open a file or a folder of notes"

# Runtime detection: Unpeel recognizes a hand-typed `unpeel-markdown` in any
# hosted terminal and brands the session row with the display tint below.
# Data-only — built-in runtime aliases always win over these.
[detection]
command_aliases = ["unpeel-markdown"]
process_aliases = ["unpeel-markdown"]

[display]
tint = "#3B82F6"

[views]
terminal = true
media_types = ["text/markdown"]

[agent]
skill = "skill.md"
"##;

const SKILL_MD: &str = r#"# Markdown — agent skill

Markdown (`unpeel-markdown <file|folder>`) is a terminal markdown editor.
Notes are plain markdown files on disk — edit them with your ordinary file
tools; there is no hidden state or build step.

## Live context

Inside Unpeel the editor publishes what the user is looking at on its pane.
Call the unified `unpeel` MCP's `sessions` tool with `{"action":"current"}`
(or `apps` `{"action":"context"}`) — a neighboring Markdown pane's entry
carries `app_context`:

    {"app": "unpeel.app.markdown",
     "context": {"file": "/abs/note.md",
                 "folder": "/abs/vault",
                 "cursor_line": 12,
                 "selection_lines": [4, 9],
                 "dirty": false},
     "updated_at": <unix ms>}

`file` is the note open in the editor (`null` while the user is browsing a
notes folder — `folder` is set instead, and the cursor fields are absent).
`cursor_line` and `selection_lines` are 1-based; `selection_lines` is
`null` when nothing is selected. When the user says "this line", "the
selected part", or "here", read that span from the file.

## Editing rules

- The editor does NOT watch for external changes. While `dirty` is true
  the user has unsaved edits and their next save would overwrite yours —
  do not edit the open file then; work on other notes or ask the user to
  save first.
- Even when clean, an edit to the open file appears only after the user
  reopens the note — tell them to reopen it when you change it.
- Keep edits minimal and local; it is the user's document.
"#;

fn unpeel_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("UNPEEL_HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".unpeel"))
}

/// Prefer the stable binary name when it is available on PATH so manifests
/// survive upgrades; development builds fall back to the running executable.
fn launch_command() -> String {
    const NAME: &str = "unpeel-markdown";
    let on_path = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(NAME).is_file()));
    if on_path {
        return NAME.to_string();
    }
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| NAME.to_string())
}

fn toml_escaped(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Best-effort and silent: a failure to register must never affect the
/// standalone tool, and an absent `~/.unpeel` means no Unpeel — do nothing.
pub fn ensure_installed() {
    let Some(home) = unpeel_home().filter(|home| home.is_dir()) else {
        return;
    };
    let manifest = APP_TOML
        .replace("@APP_VERSION@", env!("CARGO_PKG_VERSION"))
        .replace("@LAUNCH_COMMAND@", &toml_escaped(&launch_command()));
    let dir = home.join("apps").join(APP_ID);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, content) in [("app.toml", manifest.as_str()), ("skill.md", SKILL_MD)] {
        let path = dir.join(name);
        // Rewrite only on change so repeated launches do not churn watchers.
        if std::fs::read_to_string(&path).ok().as_deref() == Some(content) {
            continue;
        }
        let temporary = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        if std::fs::write(&temporary, content).is_ok() {
            let _ = std::fs::rename(temporary, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn manifest_declares_both_unpeel_runtime_detection_surfaces() {
        assert!(APP_TOML.contains("command_aliases = [\"unpeel-markdown\"]"));
        assert!(APP_TOML.contains("process_aliases = [\"unpeel-markdown\"]"));
        assert!(APP_TOML.contains("id = \"unpeel.app.markdown\""));
        assert!(APP_TOML.contains("version = \"@APP_VERSION@\""));
    }

    #[test]
    fn manifest_declares_the_skill_and_the_skill_documents_app_context() {
        assert!(APP_TOML.contains("skill = \"skill.md\""));
        assert!(SKILL_MD.contains("app_context"));
        assert!(SKILL_MD.contains("selection_lines"));
        assert!(SKILL_MD.contains("dirty"));
    }

    #[test]
    fn launch_commands_are_toml_escaped() {
        assert_eq!(
            toml_escaped(r#"C:\\Apps\"Markdown\""#),
            r#"C:\\\\Apps\\\"Markdown\\\""#
        );
    }

    #[test]
    fn installed_manifest_prefers_the_detectable_path_command() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("unpeel-home");
        let bin = temp.path().join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("unpeel-markdown"), "").unwrap();
        let previous_home = std::env::var_os("UNPEEL_HOME");
        let previous_path = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("UNPEEL_HOME", &home);
            std::env::set_var("PATH", &bin);
        }
        ensure_installed();
        let dir = home.join("apps").join(APP_ID);
        let manifest = std::fs::read_to_string(dir.join("app.toml")).unwrap();
        assert!(manifest.contains("command = \"unpeel-markdown\""));
        assert!(manifest.contains(concat!("version = \"", env!("CARGO_PKG_VERSION"), "\"")));
        assert!(manifest.contains("command_aliases = [\"unpeel-markdown\"]"));
        assert!(manifest.contains("process_aliases = [\"unpeel-markdown\"]"));
        assert!(dir.join("skill.md").is_file());

        match previous_home {
            Some(value) => unsafe { std::env::set_var("UNPEEL_HOME", value) },
            None => unsafe { std::env::remove_var("UNPEEL_HOME") },
        }
        match previous_path {
            Some(value) => unsafe { std::env::set_var("PATH", value) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}
