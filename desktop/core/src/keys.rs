//! Нажатие клавиш, кнопок мыши и сочетаний на ПК по кнопкам макропада.
//! Кнопка удерживается на телефоне — клавиши удерживаются на ПК (подходит и для push-to-talk).

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Таблица клавиш: вариант => (подпись, код W3C как у физической клавиши, виртуальный код Windows, расширенная).
macro_rules! keys {
    ($($name:ident => ($label:expr, $code:expr, $vk:expr, $extended:expr),)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Key { $($name,)* }

        impl Key {
            pub const ALL: &[Key] = &[$(Key::$name,)*];

            pub fn label(self) -> &'static str {
                match self { $(Key::$name => $label,)* }
            }

            /// По имени физической клавиши (KeyA, Digit1, F13, ArrowUp…) — не зависит от раскладки.
            pub fn from_code(code: &str) -> Option<Key> {
                match code { $($code => Some(Key::$name),)* _ => None }
            }

            #[allow(dead_code)]
            fn vk(self) -> (u16, bool) {
                match self { $(Key::$name => ($vk, $extended),)* }
            }
        }
    };
}

keys! {
    A => ("A", "KeyA", 0x41, false),
    B => ("B", "KeyB", 0x42, false),
    C => ("C", "KeyC", 0x43, false),
    D => ("D", "KeyD", 0x44, false),
    E => ("E", "KeyE", 0x45, false),
    F => ("F", "KeyF", 0x46, false),
    G => ("G", "KeyG", 0x47, false),
    H => ("H", "KeyH", 0x48, false),
    I => ("I", "KeyI", 0x49, false),
    J => ("J", "KeyJ", 0x4A, false),
    K => ("K", "KeyK", 0x4B, false),
    L => ("L", "KeyL", 0x4C, false),
    M => ("M", "KeyM", 0x4D, false),
    N => ("N", "KeyN", 0x4E, false),
    O => ("O", "KeyO", 0x4F, false),
    P => ("P", "KeyP", 0x50, false),
    Q => ("Q", "KeyQ", 0x51, false),
    R => ("R", "KeyR", 0x52, false),
    S => ("S", "KeyS", 0x53, false),
    T => ("T", "KeyT", 0x54, false),
    U => ("U", "KeyU", 0x55, false),
    V => ("V", "KeyV", 0x56, false),
    W => ("W", "KeyW", 0x57, false),
    X => ("X", "KeyX", 0x58, false),
    Y => ("Y", "KeyY", 0x59, false),
    Z => ("Z", "KeyZ", 0x5A, false),
    D0 => ("0", "Digit0", 0x30, false),
    D1 => ("1", "Digit1", 0x31, false),
    D2 => ("2", "Digit2", 0x32, false),
    D3 => ("3", "Digit3", 0x33, false),
    D4 => ("4", "Digit4", 0x34, false),
    D5 => ("5", "Digit5", 0x35, false),
    D6 => ("6", "Digit6", 0x36, false),
    D7 => ("7", "Digit7", 0x37, false),
    D8 => ("8", "Digit8", 0x38, false),
    D9 => ("9", "Digit9", 0x39, false),
    F1 => ("F1", "F1", 0x70, false),
    F2 => ("F2", "F2", 0x71, false),
    F3 => ("F3", "F3", 0x72, false),
    F4 => ("F4", "F4", 0x73, false),
    F5 => ("F5", "F5", 0x74, false),
    F6 => ("F6", "F6", 0x75, false),
    F7 => ("F7", "F7", 0x76, false),
    F8 => ("F8", "F8", 0x77, false),
    F9 => ("F9", "F9", 0x78, false),
    F10 => ("F10", "F10", 0x79, false),
    F11 => ("F11", "F11", 0x7A, false),
    F12 => ("F12", "F12", 0x7B, false),
    F13 => ("F13", "F13", 0x7C, false),
    F14 => ("F14", "F14", 0x7D, false),
    F15 => ("F15", "F15", 0x7E, false),
    F16 => ("F16", "F16", 0x7F, false),
    F17 => ("F17", "F17", 0x80, false),
    F18 => ("F18", "F18", 0x81, false),
    F19 => ("F19", "F19", 0x82, false),
    F20 => ("F20", "F20", 0x83, false),
    F21 => ("F21", "F21", 0x84, false),
    F22 => ("F22", "F22", 0x85, false),
    F23 => ("F23", "F23", 0x86, false),
    F24 => ("F24", "F24", 0x87, false),
    Space => ("Пробел", "Space", 0x20, false),
    Enter => ("Enter", "Enter", 0x0D, false),
    Escape => ("Esc", "Escape", 0x1B, false),
    Tab => ("Tab", "Tab", 0x09, false),
    Backspace => ("Backspace", "Backspace", 0x08, false),
    Delete => ("Delete", "Delete", 0x2E, true),
    Insert => ("Insert", "Insert", 0x2D, true),
    Home => ("Home", "Home", 0x24, true),
    End => ("End", "End", 0x23, true),
    PageUp => ("Page Up", "PageUp", 0x21, true),
    PageDown => ("Page Down", "PageDown", 0x22, true),
    Left => ("←", "ArrowLeft", 0x25, true),
    Up => ("↑", "ArrowUp", 0x26, true),
    Right => ("→", "ArrowRight", 0x27, true),
    Down => ("↓", "ArrowDown", 0x28, true),
    PrintScreen => ("Print Screen", "PrintScreen", 0x2C, true),
    ScrollLock => ("Scroll Lock", "ScrollLock", 0x91, false),
    Pause => ("Pause", "Pause", 0x13, false),
    CapsLock => ("Caps Lock", "CapsLock", 0x14, false),
    NumLock => ("Num Lock", "NumLock", 0x90, true),
    Numpad0 => ("Num 0", "Numpad0", 0x60, false),
    Numpad1 => ("Num 1", "Numpad1", 0x61, false),
    Numpad2 => ("Num 2", "Numpad2", 0x62, false),
    Numpad3 => ("Num 3", "Numpad3", 0x63, false),
    Numpad4 => ("Num 4", "Numpad4", 0x64, false),
    Numpad5 => ("Num 5", "Numpad5", 0x65, false),
    Numpad6 => ("Num 6", "Numpad6", 0x66, false),
    Numpad7 => ("Num 7", "Numpad7", 0x67, false),
    Numpad8 => ("Num 8", "Numpad8", 0x68, false),
    Numpad9 => ("Num 9", "Numpad9", 0x69, false),
    NumpadMultiply => ("Num *", "NumpadMultiply", 0x6A, false),
    NumpadAdd => ("Num +", "NumpadAdd", 0x6B, false),
    NumpadSubtract => ("Num -", "NumpadSubtract", 0x6D, false),
    NumpadDecimal => ("Num .", "NumpadDecimal", 0x6E, false),
    NumpadDivide => ("Num /", "NumpadDivide", 0x6F, true),
    NumpadEnter => ("Num Enter", "NumpadEnter", 0x0D, true),
    Minus => ("-", "Minus", 0xBD, false),
    Equal => ("=", "Equal", 0xBB, false),
    BracketLeft => ("[", "BracketLeft", 0xDB, false),
    BracketRight => ("]", "BracketRight", 0xDD, false),
    Backslash => ("\\", "Backslash", 0xDC, false),
    Semicolon => (";", "Semicolon", 0xBA, false),
    Quote => ("'", "Quote", 0xDE, false),
    Comma => (",", "Comma", 0xBC, false),
    Period => (".", "Period", 0xBE, false),
    Slash => ("/", "Slash", 0xBF, false),
    Backquote => ("`", "Backquote", 0xC0, false),
    MediaPlayPause => ("Медиа: пуск/пауза", "MediaPlayPause", 0xB3, true),
    MediaNext => ("Медиа: следующий", "MediaTrackNext", 0xB0, true),
    MediaPrevious => ("Медиа: предыдущий", "MediaTrackPrevious", 0xB1, true),
    MediaStop => ("Медиа: стоп", "MediaStop", 0xB2, true),
    VolumeMute => ("Громкость: выкл", "AudioVolumeMute", 0xAD, true),
    VolumeDown => ("Громкость −", "AudioVolumeDown", 0xAE, true),
    VolumeUp => ("Громкость +", "AudioVolumeUp", 0xAF, true),
    MouseLeft => ("Мышь: левая", "MouseLeft", 0, false),
    MouseRight => ("Мышь: правая", "MouseRight", 0, false),
    MouseMiddle => ("Мышь: средняя", "MouseMiddle", 0, false),
    Mouse4 => ("Мышь 4 (назад)", "Mouse4", 0, false),
    Mouse5 => ("Мышь 5 (вперёд)", "Mouse5", 0, false),
    WheelUp => ("Колесо вверх", "WheelUp", 0, false),
    WheelDown => ("Колесо вниз", "WheelDown", 0, false),
}

impl Key {
    pub fn is_mouse(self) -> bool {
        matches!(
            self,
            Key::MouseLeft
                | Key::MouseRight
                | Key::MouseMiddle
                | Key::Mouse4
                | Key::Mouse5
                | Key::WheelUp
                | Key::WheelDown
        )
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Сочетание: модификаторы и, возможно, одна клавиша (только модификаторы тоже допустимы).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyCombo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    pub key: Option<Key>,
}

impl KeyCombo {
    pub fn is_empty(&self) -> bool {
        !(self.ctrl || self.shift || self.alt || self.win || self.key.is_some())
    }
}

impl std::fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts: Vec<&str> = Vec::new();
        for (on, name) in [(self.ctrl, "Ctrl"), (self.shift, "Shift"), (self.alt, "Alt"), (self.win, "Win")] {
            if on {
                parts.push(name);
            }
        }
        if let Some(key) = self.key {
            parts.push(key.label());
        }
        f.write_str(&parts.join("+"))
    }
}

/// Нажимает (`down`) или отпускает сочетание.
pub fn send(combo: &KeyCombo, down: bool) -> Result<()> {
    platform::send(combo, down)
}

#[cfg(windows)]
mod platform {
    use super::Key;
    use super::KeyCombo;
    use anyhow::{Result, bail};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
        KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MOUSE_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
        MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MapVirtualKeyW, SendInput, VIRTUAL_KEY,
    };

    const XBUTTON1: u32 = 1;
    const XBUTTON2: u32 = 2;
    const WHEEL_DELTA: i32 = 120;

    /// Событие мыши для нажатия/отпускания; у колеса есть только «нажатие» (один щелчок).
    fn mouse_input(key: Key, up: bool) -> Option<INPUT> {
        let (flags, data) = match (key, up) {
            (Key::MouseLeft, false) => (MOUSEEVENTF_LEFTDOWN, 0),
            (Key::MouseLeft, true) => (MOUSEEVENTF_LEFTUP, 0),
            (Key::MouseRight, false) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (Key::MouseRight, true) => (MOUSEEVENTF_RIGHTUP, 0),
            (Key::MouseMiddle, false) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (Key::MouseMiddle, true) => (MOUSEEVENTF_MIDDLEUP, 0),
            (Key::Mouse4, false) => (MOUSEEVENTF_XDOWN, XBUTTON1),
            (Key::Mouse4, true) => (MOUSEEVENTF_XUP, XBUTTON1),
            (Key::Mouse5, false) => (MOUSEEVENTF_XDOWN, XBUTTON2),
            (Key::Mouse5, true) => (MOUSEEVENTF_XUP, XBUTTON2),
            (Key::WheelUp, false) => (MOUSEEVENTF_WHEEL, WHEEL_DELTA as u32),
            (Key::WheelDown, false) => (MOUSEEVENTF_WHEEL, (-WHEEL_DELTA) as u32),
            _ => return None,
        };
        Some(INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: data,
                    dwFlags: MOUSE_EVENT_FLAGS(flags.0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
    }

    const VK_LCONTROL: u16 = 0xA2;
    const VK_LSHIFT: u16 = 0xA0;
    const VK_LMENU: u16 = 0xA4;
    const VK_LWIN: u16 = 0x5B;

    fn input(vk: u16, extended: bool, up: bool) -> INPUT {
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        // Скан-код вместе с виртуальным кодом: его читают игры с raw input.
        let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    pub fn send(combo: &KeyCombo, down: bool) -> Result<()> {
        let mut keys: Vec<(u16, bool)> = Vec::new();
        let mouse = combo.key.filter(|k| k.is_mouse());
        for (on, vk, ext) in [
            (combo.ctrl, VK_LCONTROL, false),
            (combo.shift, VK_LSHIFT, false),
            (combo.alt, VK_LMENU, false),
            (combo.win, VK_LWIN, true),
        ] {
            if on {
                keys.push((vk, ext));
            }
        }
        if let Some(key) = combo.key.filter(|k| !k.is_mouse()) {
            keys.push(key.vk());
        }
        // Нажатие: модификаторы, затем клавиша или кнопка мыши; отпускание — в обратном порядке.
        let mut inputs: Vec<INPUT> = keys.iter().map(|&(vk, ext)| input(vk, ext, !down)).collect();
        if let Some(event) = mouse.and_then(|m| mouse_input(m, !down)) {
            inputs.push(event);
        }
        if !down {
            inputs.reverse();
        }
        if inputs.is_empty() {
            return Ok(());
        }
        let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            bail!("Windows не приняла нажатие (окно запущено от администратора?)");
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::KeyCombo;
    use anyhow::{Result, bail};

    pub fn send(_combo: &KeyCombo, _down: bool) -> Result<()> {
        bail!("нажатие клавиш пока работает только в Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_map_to_keys() {
        assert_eq!(Key::from_code("KeyM"), Some(Key::M));
        assert_eq!(Key::from_code("F13"), Some(Key::F13));
        assert_eq!(Key::from_code("ArrowUp"), Some(Key::Up));
        assert_eq!(Key::from_code("ControlLeft"), None);
        assert_eq!(Key::from_code("Mouse4"), Some(Key::Mouse4));
        assert!(Key::Mouse4.is_mouse() && !Key::F13.is_mouse());
    }

    #[test]
    fn combo_display_and_serde() {
        let combo = KeyCombo { ctrl: true, shift: true, key: Some(Key::M), ..Default::default() };
        assert_eq!(combo.to_string(), "Ctrl+Shift+M");
        let json = serde_json::to_string(&combo).unwrap();
        assert_eq!(serde_json::from_str::<KeyCombo>(&json).unwrap(), combo);
        assert!(KeyCombo::default().is_empty());
        let mouse = KeyCombo { ctrl: true, alt: true, key: Some(Key::Mouse4), ..Default::default() };
        assert_eq!(mouse.to_string(), "Ctrl+Alt+Мышь 4 (назад)");
    }

    #[test]
    fn windows_codes() {
        assert_eq!(Key::A.vk(), (0x41, false));
        assert_eq!(Key::F24.vk(), (0x87, false));
        assert_eq!(Key::Up.vk(), (0x26, true));
    }
}
