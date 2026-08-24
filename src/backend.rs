//! Crossterm backend modes used by this app.

use std::io;

use ratatui::DefaultTerminal;
use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;

/// Enables mouse capture on the Ratatui Crossterm backend and restores it on drop.
pub struct BackendCapture;

impl BackendCapture {
    pub fn enable(terminal: &mut DefaultTerminal) -> io::Result<Self> {
        execute!(
            terminal.backend_mut(),
            EnableMouseCapture,
            EnableBracketedPaste,
            SetCursorStyle::BlinkingBar
        )?;
        let _ = execute!(
            terminal.backend_mut(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        );
        Ok(Self)
    }
}

impl Drop for BackendCapture {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            DisableBracketedPaste,
            SetCursorStyle::DefaultUserShape
        );
    }
}
