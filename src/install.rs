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
"##;

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
    let manifest = APP_TOML.replace("@LAUNCH_COMMAND@", &toml_escaped(&launch_command()));
    let dir = home.join("apps").join(APP_ID);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("app.toml");
    // Rewrite only on change so repeated launches do not churn watchers.
    if std::fs::read_to_string(&path).ok().as_deref() == Some(manifest.as_str()) {
        return;
    }
    let temporary = dir.join(format!(".app.toml.{}.tmp", std::process::id()));
    if std::fs::write(&temporary, manifest).is_ok() {
        let _ = std::fs::rename(temporary, path);
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
        let manifest =
            std::fs::read_to_string(home.join("apps").join(APP_ID).join("app.toml")).unwrap();
        assert!(manifest.contains("command = \"unpeel-markdown\""));
        assert!(manifest.contains("command_aliases = [\"unpeel-markdown\"]"));
        assert!(manifest.contains("process_aliases = [\"unpeel-markdown\"]"));

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
