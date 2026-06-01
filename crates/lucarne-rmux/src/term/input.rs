//! input — unified terminal input.

use serde::{Deserialize, Serialize};

/// One unit of input destined for a monitored pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TermInput {
    /// Bulk text / paste / IME commit → raw bytes to the pane.
    Text { text: String },
    /// A printable key with modifiers → bytes to the pane.
    Key { code: String, mods: KeyMods },
    /// A named control key (Enter / Ctrl-C / arrows…) → a tmux key token.
    Control { key: ControlKey },
    /// Viewport changed — a hint only; never force-resizes the PTY.
    ResizeHint { cols: u16, rows: u16 },
}

/// Keyboard modifier flags for [`TermInput::Key`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

/// Named control keys for mirror input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKey {
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    /// Ctrl-<char>, e.g. `CtrlChar('c')` for Ctrl-C.
    CtrlChar(char),
}

/// Maps a [`ControlKey`] to the rmux/tmux `send-keys` key token.
pub fn control_key_token(key: &ControlKey) -> String {
    match key {
        ControlKey::Enter => "Enter".to_string(),
        ControlKey::Tab => "Tab".to_string(),
        ControlKey::Backspace => "BSpace".to_string(),
        ControlKey::Escape => "Escape".to_string(),
        ControlKey::Up => "Up".to_string(),
        ControlKey::Down => "Down".to_string(),
        ControlKey::Left => "Left".to_string(),
        ControlKey::Right => "Right".to_string(),
        ControlKey::Home => "Home".to_string(),
        ControlKey::End => "End".to_string(),
        ControlKey::PageUp => "PageUp".to_string(),
        ControlKey::PageDown => "PageDown".to_string(),
        ControlKey::Delete => "DC".to_string(),
        ControlKey::CtrlChar(c) => format!("C-{}", c.to_ascii_lowercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_tokens_are_tmux_names() {
        assert_eq!(control_key_token(&ControlKey::Enter), "Enter");
        assert_eq!(control_key_token(&ControlKey::Backspace), "BSpace");
        assert_eq!(control_key_token(&ControlKey::Delete), "DC");
        assert_eq!(control_key_token(&ControlKey::CtrlChar('C')), "C-c");
    }

    #[test]
    fn term_input_round_trips_tagged() {
        let ev = TermInput::Control {
            key: ControlKey::CtrlChar('c'),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: TermInput = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);

        let txt = TermInput::Text {
            text: "你好".to_string(),
        };
        let back: TermInput = serde_json::from_str(&serde_json::to_string(&txt).unwrap()).unwrap();
        assert_eq!(back, txt);
    }
}
