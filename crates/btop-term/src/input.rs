//! Platform-independent terminal input abstraction backed by `crossterm::event`.

use std::collections::VecDeque;
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode as CtKeyCode, KeyEventKind, KeyModifiers, MouseButton as CtMouseButton,
    MouseEventKind,
};

use crate::{Size, TermError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Delete,
    Tab,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyEvent {
    pub fn is_ctrl_c(self) -> bool {
        self.ctrl && matches!(self.code, KeyCode::Char('c' | 'C'))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub col: u16,
    pub row: u16,
    pub kind: MouseKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(Size),
}

/// Abstracts keyboard, mouse, and resize event polling for live terminals and tests.
pub trait InputSource {
    fn poll(&mut self, timeout: Duration) -> Result<Option<TermEvent>, TermError>;
}

/// Live terminal input source backed by `crossterm::event`.
#[derive(Debug, Default)]
pub struct CrosstermInput;

impl InputSource for CrosstermInput {
    fn poll(&mut self, timeout: Duration) -> Result<Option<TermEvent>, TermError> {
        if !event::poll(timeout).map_err(|e| TermError::Input(e.to_string()))? {
            return Ok(None);
        }
        let raw = event::read().map_err(|e| TermError::Input(e.to_string()))?;
        Ok(translate_crossterm_event(&raw))
    }
}

fn translate_crossterm_event(raw: &Event) -> Option<TermEvent> {
    match raw {
        Event::Key(k) => {
            if k.kind == KeyEventKind::Release {
                return None;
            }
            let code = match k.code {
                CtKeyCode::Char(c) => KeyCode::Char(c),
                CtKeyCode::Enter => KeyCode::Enter,
                CtKeyCode::Esc => KeyCode::Esc,
                CtKeyCode::Backspace => KeyCode::Backspace,
                CtKeyCode::Delete => KeyCode::Delete,
                CtKeyCode::Tab => KeyCode::Tab,
                CtKeyCode::Up => KeyCode::Up,
                CtKeyCode::Down => KeyCode::Down,
                CtKeyCode::Left => KeyCode::Left,
                CtKeyCode::Right => KeyCode::Right,
                CtKeyCode::PageUp => KeyCode::PageUp,
                CtKeyCode::PageDown => KeyCode::PageDown,
                CtKeyCode::Home => KeyCode::Home,
                CtKeyCode::End => KeyCode::End,
                CtKeyCode::F(n) => KeyCode::F(n),
                _ => return None,
            };
            Some(TermEvent::Key(KeyEvent {
                code,
                ctrl: k.modifiers.contains(KeyModifiers::CONTROL),
                alt: k.modifiers.contains(KeyModifiers::ALT),
                shift: k.modifiers.contains(KeyModifiers::SHIFT),
            }))
        }
        Event::Mouse(m) => {
            let kind = match m.kind {
                MouseEventKind::Down(CtMouseButton::Left) => MouseKind::Down(MouseButton::Left),
                MouseEventKind::Down(CtMouseButton::Right) => MouseKind::Down(MouseButton::Right),
                MouseEventKind::Down(CtMouseButton::Middle) => MouseKind::Down(MouseButton::Middle),
                MouseEventKind::Up(CtMouseButton::Left) => MouseKind::Up(MouseButton::Left),
                MouseEventKind::Up(CtMouseButton::Right) => MouseKind::Up(MouseButton::Right),
                MouseEventKind::Up(CtMouseButton::Middle) => MouseKind::Up(MouseButton::Middle),
                MouseEventKind::ScrollUp => MouseKind::ScrollUp,
                MouseEventKind::ScrollDown => MouseKind::ScrollDown,
                _ => return None,
            };
            Some(TermEvent::Mouse(MouseEvent {
                col: m.column,
                row: m.row,
                kind,
            }))
        }
        Event::Resize(width, height) => Some(TermEvent::Resize(Size {
            width: *width,
            height: *height,
        })),
        _ => None,
    }
}

/// Deterministic in-memory `InputSource` for unit and integration tests.
#[derive(Debug, Default)]
pub struct VecInputSource {
    events: VecDeque<TermEvent>,
}

impl VecInputSource {
    pub fn new(events: impl IntoIterator<Item = TermEvent>) -> Self {
        Self {
            events: events.into_iter().collect(),
        }
    }
}

impl InputSource for VecInputSource {
    fn poll(&mut self, _timeout: Duration) -> Result<Option<TermEvent>, TermError> {
        Ok(self.events.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent as CtKeyEvent, KeyEventState, MouseEvent as CtMouseEvent};

    use super::*;

    #[test]
    fn translates_ctrl_c_key_event() {
        let raw = Event::Key(CtKeyEvent {
            code: CtKeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        let translated = translate_crossterm_event(&raw).expect("ctrl+c");
        assert!(matches!(translated, TermEvent::Key(k) if k.is_ctrl_c()));
    }

    #[test]
    fn ignores_key_release_events() {
        let raw = Event::Key(CtKeyEvent {
            code: CtKeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        });
        assert!(translate_crossterm_event(&raw).is_none());
    }

    #[test]
    fn translates_mouse_click_and_scroll() {
        let click = Event::Mouse(CtMouseEvent {
            kind: MouseEventKind::Down(CtMouseButton::Left),
            column: 12,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            translate_crossterm_event(&click),
            Some(TermEvent::Mouse(MouseEvent {
                col: 12,
                row: 5,
                kind: MouseKind::Down(MouseButton::Left),
            }))
        );

        let wheel = Event::Mouse(CtMouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 3,
            row: 9,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            translate_crossterm_event(&wheel),
            Some(TermEvent::Mouse(MouseEvent {
                col: 3,
                row: 9,
                kind: MouseKind::ScrollDown,
            }))
        );
    }

    #[test]
    fn translates_resize_and_vec_source_drains() {
        assert_eq!(
            translate_crossterm_event(&Event::Resize(120, 40)),
            Some(TermEvent::Resize(Size {
                width: 120,
                height: 40,
            }))
        );

        let mut src = VecInputSource::new([TermEvent::Resize(Size {
            width: 80,
            height: 24,
        })]);
        assert_eq!(
            src.poll(Duration::from_millis(1)).expect("poll"),
            Some(TermEvent::Resize(Size {
                width: 80,
                height: 24,
            }))
        );
        assert_eq!(src.poll(Duration::from_millis(1)).expect("empty"), None);
    }
}
