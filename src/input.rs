//! Keyboard input for conhost's win32-input-mode.
//!
//! Behind ConPTY every program is a console program, and conhost asks the
//! terminal (`ESC[?9001h`) to send keys as `ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`:
//! one sequence per key event, holding exactly what a console
//! `KEY_EVENT_RECORD` holds. conhost turns those back into real key events, so
//! programs see Shift+Enter, Ctrl+Space, AltGr and dead keys exactly as they
//! would in a classic console window. Plain VT can't express most of that.

use windows::Win32::System::Console::{
    CAPSLOCK_ON, ENHANCED_KEY, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, NUMLOCK_ON, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED,
    SCROLLLOCK_ON, SHIFT_PRESSED,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CAPITAL, VK_LCONTROL, VK_LMENU, VK_NUMLOCK, VK_RCONTROL, VK_RMENU, VK_SCROLL, VK_SHIFT,
};

/// One key press or release, as conhost wants to hear about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub vk: u16,
    pub scan: u16,
    /// The UTF-16 code unit the key typed, or 0.
    pub ch: u16,
    pub down: bool,
    /// Console `dwControlKeyState` flags.
    pub state: u32,
    pub repeat: u16,
}

impl KeyEvent {
    pub fn encode(&self) -> String {
        let KeyEvent { vk, scan, ch, down, state, repeat } = *self;
        format!("\x1b[{vk};{scan};{ch};{};{state};{repeat}_", down as u8)
    }
}

/// The console's `dwControlKeyState` for the keyboard right now. `extended`
/// comes from bit 24 of the key message's lParam.
pub fn control_key_state(extended: bool) -> u32 {
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
    let toggled = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } & 1 != 0;
    let flags = [
        (down(VK_RMENU), RIGHT_ALT_PRESSED),
        (down(VK_LMENU), LEFT_ALT_PRESSED),
        (down(VK_RCONTROL), RIGHT_CTRL_PRESSED),
        (down(VK_LCONTROL), LEFT_CTRL_PRESSED),
        (down(VK_SHIFT), SHIFT_PRESSED),
        (toggled(VK_NUMLOCK), NUMLOCK_ON),
        (toggled(VK_SCROLL), SCROLLLOCK_ON),
        (toggled(VK_CAPITAL), CAPSLOCK_ON),
        (extended, ENHANCED_KEY),
    ];
    flags.iter().filter(|(on, _)| *on).fold(0, |acc, (_, flag)| acc | flag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Pane;
    use libghostty_vt::terminal::SizeReportSize;
    use std::time::{Duration, Instant};
    use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};

    #[test]
    fn encodes_shift_enter() {
        let e = KeyEvent { vk: 13, scan: 28, ch: 13, down: true, state: SHIFT_PRESSED, repeat: 1 };
        assert_eq!(e.encode(), "\x1b[13;28;13;1;16;1_");
    }

    /// Typing through win32-input-mode reaches a real console program.
    #[test]
    fn conhost_understands_our_keys() {
        let size = SizeReportSize { rows: 24, columns: 80, cell_width: 9, cell_height: 19 };
        let mut pane = Pane::spawn("cmd.exe /k", size, || {}, || {}).unwrap();
        let screen = |pane: &Pane| {
            use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
            let mut f = Formatter::new(&pane.terminal, FormatterOptions::new().with_format(Format::Plain)).unwrap();
            let Ok(len) = f.format_len() else { return String::new() };
            let mut buf = vec![0; len];
            let n = f.format_buf(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pane.win32_input {
            assert!(Instant::now() < deadline, "conhost never asked for win32-input-mode");
            std::thread::sleep(Duration::from_millis(20));
            pane.pump();
        }

        let mut keys = String::new();
        for ch in "echo w32ok\r".chars() {
            let vk = match ch {
                'a'..='z' => ch.to_ascii_uppercase() as u16,
                _ => ch as u16, // space, digits and CR share their VK codes
            };
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
            for down in [true, false] {
                keys += &KeyEvent { vk, scan, ch: ch as u16, down, state: 0, repeat: 1 }.encode();
            }
        }
        pane.write(keys.as_bytes());
        // "w32ok" shows up twice: once as the typed command, once as its output.
        while screen(&pane).matches("w32ok").count() < 2 {
            assert!(Instant::now() < deadline, "screen: {:?}", screen(&pane));
            std::thread::sleep(Duration::from_millis(20));
            pane.pump();
        }
    }
}
