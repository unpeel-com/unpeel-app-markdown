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
    if let Some(home) = std::env::var_os("UNPEEL_HOME") {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".unpeel"))
}

/// Best-effort and silent: a failure to register must never affect the
/// standalone tool, and an absent `~/.unpeel` means no Unpeel — do nothing.
pub fn ensure_installed() {
    let Some(home) = unpeel_home().filter(|home| home.is_dir()) else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let manifest = APP_TOML.replace(
        "@LAUNCH_COMMAND@",
        &exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\""),
    );
    let dir = home.join("apps").join(APP_ID);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("app.toml");
    // Rewrite only on change so repeated launches do not churn watchers.
    if std::fs::read_to_string(&path).ok().as_deref() == Some(manifest.as_str()) {
        return;
    }
    let _ = std::fs::write(&path, manifest);
}
