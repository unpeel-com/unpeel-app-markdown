//! Terminal-aware colors. Backgrounds stay terminal-native; foregrounds and
//! interaction surfaces switch palettes so both light and dark terminals keep
//! enough contrast.

use ratatui::style::Color;
use terminal_colorsaurus::{QueryOptions, ThemeMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub strong: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    pub cursor_line: Color,
    pub cursor: Color,
    pub cursor_text: Color,
    pub selected: Color,
    pub selected_text: Color,
}

impl Theme {
    pub fn detect() -> Self {
        if let Some(theme) = theme_override() {
            return theme;
        }
        // Prefer the shell-provided palette hint. Besides being instant, this
        // avoids an OSC query reading from the same input stream as the picker
        // and swallowing search keystrokes typed during startup.
        if let Some(theme) = colorfgbg_theme() {
            return theme;
        }
        match terminal_colorsaurus::theme_mode(QueryOptions::default()) {
            Ok(ThemeMode::Light) => Self::light(),
            Ok(ThemeMode::Dark) => Self::dark(),
            Err(_) => Self::dark(),
        }
    }

    pub const fn dark() -> Self {
        Self {
            strong: Color::Rgb(228, 228, 231),
            text: Color::Rgb(174, 174, 184),
            muted: Color::Rgb(139, 139, 151),
            faint: Color::Rgb(105, 105, 117),
            accent: Color::Rgb(250, 204, 21),
            cursor_line: Color::Rgb(28, 28, 28),
            cursor: Color::Rgb(174, 174, 184),
            cursor_text: Color::Rgb(24, 24, 27),
            selected: Color::Rgb(174, 174, 184),
            selected_text: Color::Rgb(24, 24, 27),
        }
    }

    pub const fn light() -> Self {
        Self {
            strong: Color::Rgb(24, 24, 27),
            text: Color::Rgb(63, 63, 70),
            muted: Color::Rgb(82, 82, 91),
            faint: Color::Rgb(113, 113, 122),
            accent: Color::Rgb(161, 98, 7),
            cursor_line: Color::Rgb(244, 244, 245),
            cursor: Color::Rgb(63, 63, 70),
            cursor_text: Color::Rgb(250, 250, 250),
            selected: Color::Rgb(63, 63, 70),
            selected_text: Color::Rgb(250, 250, 250),
        }
    }
}

fn theme_override() -> Option<Theme> {
    let value = std::env::var("UNPEEL_THEME").ok()?;
    theme_from_override(&value)
}

fn theme_from_override(value: &str) -> Option<Theme> {
    match value.trim().to_ascii_lowercase().as_str() {
        "light" => Some(Theme::light()),
        "dark" => Some(Theme::dark()),
        _ => None,
    }
}

fn colorfgbg_theme() -> Option<Theme> {
    let value = std::env::var("COLORFGBG").ok()?;
    theme_from_colorfgbg(&value)
}

fn theme_from_colorfgbg(value: &str) -> Option<Theme> {
    let background = value.rsplit(';').next()?.parse::<u8>().ok()?;
    let [r, g, b] = ansi_rgb(background);
    let perceived_lightness =
        (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1_000;
    if perceived_lightness >= 140 {
        Some(Theme::light())
    } else {
        Some(Theme::dark())
    }
}

fn ansi_rgb(index: u8) -> [u8; 3] {
    const ANSI: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    if index < 16 {
        return ANSI[usize::from(index)];
    }
    if index >= 232 {
        let gray = 8 + (index - 232) * 10;
        return [gray, gray, gray];
    }
    let cube = index - 16;
    let levels = [0, 95, 135, 175, 215, 255];
    [
        levels[usize::from(cube / 36)],
        levels[usize::from((cube % 36) / 6)],
        levels[usize::from(cube % 6)],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark_palettes_are_distinct() {
        assert_ne!(Theme::light(), Theme::dark());
        assert_eq!(Theme::light().strong, Color::Rgb(24, 24, 27));
        assert_eq!(Theme::dark().strong, Color::Rgb(228, 228, 231));
    }

    #[test]
    fn explicit_theme_values_are_case_insensitive() {
        assert_eq!(theme_from_override(" LIGHT "), Some(Theme::light()));
        assert_eq!(theme_from_override("dark"), Some(Theme::dark()));
        assert_eq!(theme_from_override("system"), None);
    }

    #[test]
    fn colorfgbg_fallback_handles_basic_and_256_color_backgrounds() {
        assert_eq!(theme_from_colorfgbg("15;0"), Some(Theme::dark()));
        assert_eq!(theme_from_colorfgbg("0;15"), Some(Theme::light()));
        assert_eq!(theme_from_colorfgbg("0;16"), Some(Theme::dark()));
        assert_eq!(theme_from_colorfgbg("0;231"), Some(Theme::light()));
        assert_eq!(theme_from_colorfgbg("unknown"), None);
    }
}
