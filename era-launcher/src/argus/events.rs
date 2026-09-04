//! Event handling for ARGUS terminal UI.
//!
//! Handles keyboard and mouse events, routing them to the focus manager
//! and app state. Ensures single-step navigation (no double advancement).

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

/// Represents a processed event that the main loop handles
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgusEvent {
    /// Tab pressed - move focus forward
    TabForward,
    /// Shift+Tab pressed - move focus backward
    TabBackward,
    /// Enter pressed - activate focused element
    Activate,
    /// Escape pressed
    Escape,
    /// Up arrow
    Up,
    /// Down arrow
    Down,
    /// Left arrow
    Left,
    /// Right arrow
    Right,
    /// Character input (for command prompt)
    Char(char),
    /// Backspace
    Backspace,
    /// Ctrl+L - focus command prompt
    FocusPrompt,
    /// Mouse click at position (row, col)
    MouseClick { row: u16, col: u16 },
    /// Mouse click on a specific focus target by id
    MouseClickTarget(String),
    /// Window resized to (cols, rows)
    Resize { cols: usize, rows: usize },
    /// Quit requested
    Quit,
}

/// Processes raw crossterm events and converts them to ArgusEvents.
///
/// This is the SINGLE canonical entry point for event processing.
/// Every physical key event results in exactly one ArgusEvent.
pub struct InputProcessor;

impl InputProcessor {
    /// Convert a raw crossterm Event into an ArgusEvent.
    /// Returns None for events that should be ignored.
    pub fn process_event(event: &Event) -> Option<ArgusEvent> {
        match event {
            Event::Key(KeyEvent {
                code, modifiers, ..
            }) => Self::process_key(code, modifiers),
            Event::Mouse(MouseEvent {
                kind, row, column, ..
            }) => Self::process_mouse(kind, *row, *column),
            Event::Resize(cols, rows) => Some(ArgusEvent::Resize {
                cols: *cols as usize,
                rows: *rows as usize,
            }),
            Event::FocusGained | Event::FocusLost | Event::Paste(_) => None,
        }
    }

    /// Process keyboard events — the canonical mapping.
    /// One key press → exactly one ArgusEvent (or None if irrelevant).
    fn process_key(code: &KeyCode, modifiers: &KeyModifiers) -> Option<ArgusEvent> {
        // Check for Ctrl+L first (focus command prompt)
        if modifiers.contains(KeyModifiers::CONTROL) && *code == KeyCode::Char('l') {
            return Some(ArgusEvent::FocusPrompt);
        }

        // Check for Ctrl+C / Ctrl+Q to quit
        if modifiers.contains(KeyModifiers::CONTROL)
            && (*code == KeyCode::Char('c') || *code == KeyCode::Char('q'))
        {
            return Some(ArgusEvent::Quit);
        }

        match code {
            KeyCode::Tab => {
                if modifiers.contains(KeyModifiers::SHIFT) {
                    Some(ArgusEvent::TabBackward)
                } else {
                    Some(ArgusEvent::TabForward)
                }
            }
            KeyCode::Enter => Some(ArgusEvent::Activate),
            KeyCode::Esc => Some(ArgusEvent::Escape),
            KeyCode::Up => Some(ArgusEvent::Up),
            KeyCode::Down => Some(ArgusEvent::Down),
            KeyCode::Left => Some(ArgusEvent::Left),
            KeyCode::Right => Some(ArgusEvent::Right),
            KeyCode::Backspace => Some(ArgusEvent::Backspace),
            KeyCode::Char(c) => {
                if modifiers.contains(KeyModifiers::CONTROL) {
                    None
                } else {
                    Some(ArgusEvent::Char(*c))
                }
            }
            _ => None,
        }
    }

    /// Process mouse events.
    fn process_mouse(kind: &MouseEventKind, row: u16, col: u16) -> Option<ArgusEvent> {
        match kind {
            MouseEventKind::Down(button) => {
                let _ = button; // We handle all button types the same for now
                Some(ArgusEvent::MouseClick { row, col })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyModifiers};

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn test_tab_forward() {
        let event = key_event(KeyCode::Tab, KeyModifiers::NONE);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::TabForward));
    }

    #[test]
    fn test_tab_backward() {
        let event = key_event(KeyCode::Tab, KeyModifiers::SHIFT);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::TabBackward));
    }

    #[test]
    fn test_enter() {
        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::Activate));
    }

    #[test]
    fn test_escape() {
        let event = key_event(KeyCode::Esc, KeyModifiers::NONE);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::Escape));
    }

    #[test]
    fn test_arrow_keys() {
        assert_eq!(
            InputProcessor::process_event(&key_event(KeyCode::Up, KeyModifiers::NONE)),
            Some(ArgusEvent::Up)
        );
        assert_eq!(
            InputProcessor::process_event(&key_event(KeyCode::Down, KeyModifiers::NONE)),
            Some(ArgusEvent::Down)
        );
        assert_eq!(
            InputProcessor::process_event(&key_event(KeyCode::Left, KeyModifiers::NONE)),
            Some(ArgusEvent::Left)
        );
        assert_eq!(
            InputProcessor::process_event(&key_event(KeyCode::Right, KeyModifiers::NONE)),
            Some(ArgusEvent::Right)
        );
    }

    #[test]
    fn test_ctrl_l_focuses_prompt() {
        let event = key_event(KeyCode::Char('l'), KeyModifiers::CONTROL);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::FocusPrompt));
    }

    #[test]
    fn test_ctrl_c_quits() {
        let event = key_event(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::Quit));
    }

    #[test]
    fn test_ctrl_q_quits() {
        let event = key_event(KeyCode::Char('q'), KeyModifiers::CONTROL);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::Quit));
    }

    #[test]
    fn test_char_input() {
        let event = key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::Char('a')));
    }

    #[test]
    fn test_char_with_shift_treated_as_char() {
        // Shift+A should be treated as a char event (not a modifier combo for our purposes)
        // crossterm reports shifted characters as Char('A')
        let event = key_event(KeyCode::Char('A'), KeyModifiers::SHIFT);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::Char('A')));
    }

    #[test]
    fn test_backspace() {
        let event = key_event(KeyCode::Backspace, KeyModifiers::NONE);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, Some(ArgusEvent::Backspace));
    }

    #[test]
    fn test_ctrl_char_ignored() {
        // Ctrl+A should be ignored (not a char or command we handle)
        let event = key_event(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resize_event() {
        let event = Event::Resize(120, 40);
        let result = InputProcessor::process_event(&event);
        assert_eq!(
            result,
            Some(ArgusEvent::Resize {
                cols: 120,
                rows: 40
            })
        );
    }

    #[test]
    fn test_one_key_one_event() {
        // Regression test: ensure no double advancement
        // A single Tab press must produce exactly one TabForward event
        let event = key_event(KeyCode::Tab, KeyModifiers::NONE);
        let results: Vec<_> = (0..10)
            .map(|_| InputProcessor::process_event(&event))
            .collect();
        // Each call should return exactly one Some(TabForward)
        let tab_count = results
            .iter()
            .filter(|r| **r == Some(ArgusEvent::TabForward))
            .count();
        assert_eq!(tab_count, 10);
        // No unexpected events
        assert!(results.iter().all(|r| *r == Some(ArgusEvent::TabForward)));
    }

    #[test]
    fn test_f1_ignored() {
        let event = key_event(KeyCode::F(1), KeyModifiers::NONE);
        let result = InputProcessor::process_event(&event);
        assert_eq!(result, None);
    }
}
