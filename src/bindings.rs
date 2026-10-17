//! Key bindings in Ghostty's `keybind = trigger=action` syntax.
//!
//! Triggers name a key the way Ghostty does: a character (`ctrl+shift+c`,
//! `ctrl+comma`) matches whichever key types it on the current keyboard
//! layout, while digits and named keys (`alt+1`, `shift+page_up`) match the
//! physical key, so they work on AZERTY and other layouts too.

use libghostty_vt::key::Mods;
use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_CHAR, MapVirtualKeyW};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Which split to move focus to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Previous,
    Next,
    Toward(Direction),
}

/// Ghostty's action names, as far as Wisp implements them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    CopyToClipboard,
    PasteFromClipboard,
    SelectAll,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    IncreaseFontSize(f32),
    DecreaseFontSize(f32),
    ResetFontSize,
    ReloadConfig,
    OpenConfig,
    NewWindow,
    NewTab,
    CloseSurface,
    CloseTab,
    CloseWindow,
    Quit,
    NextTab,
    PreviousTab,
    LastTab,
    /// 1-based, as in Ghostty.
    GotoTab(usize),
    NewSplit(Direction),
    GotoSplit(Focus),
    ResizeSplit(Direction, u16),
    EqualizeSplits,
    ToggleSplitZoom,
    ToggleFullscreen,
    /// Swallow the key and do nothing.
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Whichever key types this (lowercase) character, unshifted.
    Char(char),
    /// A virtual-key code: named keys and the digit row.
    Vk(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub key: Key,
    pub mods: Mods,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Binding {
    pub trigger: Trigger,
    pub action: Action,
    /// Only take the key if the action did something (Ghostty's `performable:`),
    /// so `performable:ctrl+c=copy_to_clipboard` still sends Ctrl+C with nothing selected.
    pub performable: bool,
}

#[derive(Clone, Debug)]
pub struct Bindings {
    list: Vec<Binding>,
}

/// Ghostty's default bindings on Linux and Windows, with two Windows changes:
/// fullscreen is F11 and Alt+Enter rather than Ctrl+Enter (PSReadLine uses
/// Ctrl+Enter), and Shift+Home/End don't scroll (PSReadLine selects with them).
const DEFAULTS: &[(&str, &str)] = &[
    ("ctrl+shift+c", "copy_to_clipboard"),
    ("ctrl+shift+v", "paste_from_clipboard"),
    ("ctrl+insert", "copy_to_clipboard"),
    ("shift+insert", "paste_from_clipboard"),
    ("ctrl+shift+a", "select_all"),
    ("ctrl+shift+comma", "reload_config"),
    ("ctrl+comma", "open_config"),
    ("ctrl+equal", "increase_font_size:1"),
    ("ctrl+plus", "increase_font_size:1"),
    ("ctrl+minus", "decrease_font_size:1"),
    ("ctrl+0", "reset_font_size"),
    ("shift+page_up", "scroll_page_up"),
    ("shift+page_down", "scroll_page_down"),
    ("ctrl+shift+n", "new_window"),
    ("ctrl+shift+w", "close_surface"),
    ("ctrl+shift+q", "quit"),
    ("ctrl+shift+t", "new_tab"),
    ("ctrl+shift+left", "previous_tab"),
    ("ctrl+shift+right", "next_tab"),
    ("ctrl+page_up", "previous_tab"),
    ("ctrl+page_down", "next_tab"),
    ("ctrl+tab", "next_tab"),
    ("ctrl+shift+tab", "previous_tab"),
    ("alt+1", "goto_tab:1"),
    ("alt+2", "goto_tab:2"),
    ("alt+3", "goto_tab:3"),
    ("alt+4", "goto_tab:4"),
    ("alt+5", "goto_tab:5"),
    ("alt+6", "goto_tab:6"),
    ("alt+7", "goto_tab:7"),
    ("alt+8", "goto_tab:8"),
    ("alt+9", "last_tab"),
    ("ctrl+shift+o", "new_split:right"),
    ("ctrl+shift+e", "new_split:down"),
    ("ctrl+super+bracket_left", "goto_split:previous"),
    ("ctrl+super+bracket_right", "goto_split:next"),
    ("ctrl+alt+up", "goto_split:up"),
    ("ctrl+alt+down", "goto_split:down"),
    ("ctrl+alt+left", "goto_split:left"),
    ("ctrl+alt+right", "goto_split:right"),
    ("ctrl+super+shift+up", "resize_split:up,10"),
    ("ctrl+super+shift+down", "resize_split:down,10"),
    ("ctrl+super+shift+left", "resize_split:left,10"),
    ("ctrl+super+shift+right", "resize_split:right,10"),
    ("ctrl+shift+enter", "toggle_split_zoom"),
    ("f11", "toggle_fullscreen"),
    ("alt+enter", "toggle_fullscreen"),
];

impl Default for Bindings {
    fn default() -> Bindings {
        let mut bindings = Bindings { list: Vec::new() };
        for (trigger, action) in DEFAULTS {
            bindings.parse_line(&format!("{trigger}={action}")).expect("default bindings parse");
        }
        bindings
    }
}

impl Bindings {
    /// Applies one `keybind` value: `clear`, `trigger=action` or `trigger=unbind`.
    pub fn parse_line(&mut self, value: &str) -> Result<(), String> {
        if value == "clear" {
            self.list.clear();
            return Ok(());
        }
        let (trigger, action) = value.rsplit_once('=').ok_or("expected trigger=action")?;
        // `=` itself can be the key (`ctrl+==...`); rsplit keeps it in the trigger.
        let mut trigger = trigger.trim();
        let mut performable = false;
        // Ghostty's prefixes. Wisp bindings are always local and consume the key.
        while let Some((prefix, rest)) = trigger.split_once(':') {
            match prefix {
                "performable" => performable = true,
                "global" | "all" | "unconsumed" => {}
                "physical" => {} // Wisp matches named keys physically already
                _ => break,
            }
            trigger = rest;
        }
        if trigger.contains('>') {
            return Err(format!("key sequences like {trigger:?} aren't supported yet"));
        }
        let trigger = parse_trigger(trigger)?;
        self.list.retain(|b| b.trigger != trigger);
        if action.trim() != "unbind" {
            self.list.push(Binding { trigger, action: parse_action(action.trim())?, performable });
        }
        Ok(())
    }

    /// The binding for a key press, if any. `vk` is the virtual-key code.
    pub fn lookup(&self, vk: u16, mods: Mods) -> Option<Binding> {
        let mods = mods & (Mods::SHIFT | Mods::CTRL | Mods::ALT | Mods::SUPER);
        let typed = typed_char(vk);
        self.list.iter().copied().find(|b| {
            b.trigger.mods == mods
                && match b.trigger.key {
                    Key::Vk(code) => code == vk,
                    Key::Char(c) => typed == Some(c),
                }
        })
    }
}

/// The character a key types on the current layout without modifiers, lowercased.
fn typed_char(vk: u16) -> Option<char> {
    let mapped = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_CHAR) };
    // The high bit marks a dead key; it still names the character.
    char::from_u32(mapped & 0x7fff).filter(|c| *c != '\0').map(|c| c.to_ascii_lowercase())
}

fn parse_trigger(text: &str) -> Result<Trigger, String> {
    let mut mods = Mods::empty();
    let mut parts: Vec<&str> = text.split('+').collect();
    // `ctrl++` splits into ["ctrl", "", ""]: the key is a plus.
    if text.ends_with("++") {
        parts.truncate(parts.len() - 2);
        parts.push("plus");
    }
    let key = parts.pop().filter(|k| !k.is_empty()).ok_or_else(|| format!("no key in trigger {text:?}"))?;
    for m in parts {
        mods |= match m.to_ascii_lowercase().as_str() {
            "shift" => Mods::SHIFT,
            "ctrl" | "control" => Mods::CTRL,
            "alt" | "opt" | "option" => Mods::ALT,
            "super" | "cmd" | "command" => Mods::SUPER,
            other => return Err(format!("unknown modifier {other:?}")),
        };
    }
    Ok(Trigger { key: parse_key(key)?, mods })
}

fn parse_key(name: &str) -> Result<Key, String> {
    let lower = name.to_ascii_lowercase();
    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(match c {
            '0'..='9' => Key::Vk(c as u16), // the digit row, physically
            _ => Key::Char(c),
        });
    }
    let vk = |code: u16| Ok(Key::Vk(code));
    let ch = |c: char| Ok(Key::Char(c));
    match lower.as_str() {
        "plus" => ch('+'),
        "minus" => ch('-'),
        "equal" => ch('='),
        "comma" => ch(','),
        "period" => ch('.'),
        "slash" => ch('/'),
        "backslash" => ch('\\'),
        "semicolon" => ch(';'),
        "quote" | "apostrophe" => ch('\''),
        "grave" | "backquote" | "grave_accent" => ch('`'),
        "bracket_left" | "left_bracket" => ch('['),
        "bracket_right" | "right_bracket" => ch(']'),
        "space" => vk(0x20),
        "enter" | "return" => vk(0x0D),
        "tab" => vk(0x09),
        "escape" => vk(0x1B),
        "backspace" => vk(0x08),
        "delete" => vk(0x2E),
        "insert" => vk(0x2D),
        "home" => vk(0x24),
        "end" => vk(0x23),
        "page_up" => vk(0x21),
        "page_down" => vk(0x22),
        "up" | "arrow_up" => vk(0x26),
        "down" | "arrow_down" => vk(0x28),
        "left" | "arrow_left" => vk(0x25),
        "right" | "arrow_right" => vk(0x27),
        _ => {
            if let Some(n) =
                lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()).filter(|n| (1..=24).contains(n))
            {
                return vk(0x70 + n - 1);
            }
            if let Some(d) = lower.strip_prefix("digit_").filter(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit()) {
                return vk(d.as_bytes()[0] as u16);
            }
            if let Some(c) = lower.strip_prefix("key_").filter(|c| c.len() == 1) {
                return ch(c.chars().next().unwrap());
            }
            Err(format!("unknown key {name:?}"))
        }
    }
}

fn parse_action(text: &str) -> Result<Action, String> {
    let (name, param) = text.split_once(':').unwrap_or((text, ""));
    let direction = |p: &str| match p {
        "left" => Ok(Direction::Left),
        "right" | "auto" => Ok(Direction::Right),
        "up" => Ok(Direction::Up),
        "down" => Ok(Direction::Down),
        _ => Err(format!("unknown direction {p:?}")),
    };
    let amount = |p: &str, default: f32| {
        if p.is_empty() { Ok(default) } else { p.parse().map_err(|_| format!("{p:?} is not a number")) }
    };
    Ok(match name {
        // Ghostty's copy formats (plain, mixed, vt, html) all copy plain text in Wisp.
        "copy_to_clipboard" => Action::CopyToClipboard,
        "paste_from_clipboard" => Action::PasteFromClipboard,
        "select_all" => Action::SelectAll,
        "scroll_page_up" => Action::ScrollPageUp,
        "scroll_page_down" => Action::ScrollPageDown,
        "scroll_to_top" => Action::ScrollToTop,
        "scroll_to_bottom" => Action::ScrollToBottom,
        "increase_font_size" => Action::IncreaseFontSize(amount(param, 1.0)?),
        "decrease_font_size" => Action::DecreaseFontSize(amount(param, 1.0)?),
        "reset_font_size" => Action::ResetFontSize,
        "reload_config" => Action::ReloadConfig,
        "open_config" => Action::OpenConfig,
        "new_window" => Action::NewWindow,
        "new_tab" => Action::NewTab,
        "close_surface" => Action::CloseSurface,
        "close_tab" => Action::CloseTab,
        "close_window" => Action::CloseWindow,
        "quit" => Action::Quit,
        "next_tab" => Action::NextTab,
        "previous_tab" => Action::PreviousTab,
        "last_tab" => Action::LastTab,
        "goto_tab" => Action::GotoTab(param.parse().map_err(|_| format!("goto_tab needs a number, got {param:?}"))?),
        "new_split" => Action::NewSplit(direction(param)?),
        "goto_split" => Action::GotoSplit(match param {
            "previous" => Focus::Previous,
            "next" => Focus::Next,
            other => Focus::Toward(direction(other)?),
        }),
        "resize_split" => {
            let (dir, by) = param.split_once(',').unwrap_or((param, "10"));
            Action::ResizeSplit(direction(dir)?, by.trim().parse().map_err(|_| format!("{by:?} is not a number"))?)
        }
        "equalize_splits" => Action::EqualizeSplits,
        "toggle_split_zoom" => Action::ToggleSplitZoom,
        "toggle_fullscreen" => Action::ToggleFullscreen,
        "ignore" => Action::Ignore,
        _ => return Err(format!("unknown or unsupported action {text:?}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanW;

    /// The virtual key that types `c` on this machine's layout.
    fn vk_for(c: char) -> u16 {
        (unsafe { VkKeyScanW(c as u16) } & 0xff) as u16
    }

    #[test]
    fn defaults_cover_copy_paste_and_tabs() {
        let b = Bindings::default();
        let ctrl_shift = Mods::CTRL | Mods::SHIFT;
        assert_eq!(b.lookup(vk_for('c'), ctrl_shift).unwrap().action, Action::CopyToClipboard);
        assert_eq!(b.lookup(0x2D, Mods::SHIFT).unwrap().action, Action::PasteFromClipboard);
        assert_eq!(b.lookup(vk_for(','), ctrl_shift).unwrap().action, Action::ReloadConfig);
        assert_eq!(b.lookup(0x33, Mods::ALT).unwrap().action, Action::GotoTab(3), "alt + the 3 key, on any layout");
        assert_eq!(b.lookup(0x7A, Mods::empty()).unwrap().action, Action::ToggleFullscreen);
        assert!(b.lookup(vk_for('c'), Mods::CTRL).is_none(), "plain Ctrl+C belongs to the shell");
    }

    #[test]
    fn config_lines_add_replace_and_remove() {
        let mut b = Bindings::default();
        b.parse_line("performable:ctrl+c=copy_to_clipboard").unwrap();
        let copy = b.lookup(vk_for('c'), Mods::CTRL).unwrap();
        assert!(copy.performable);
        b.parse_line("ctrl+shift+t=new_split:down").unwrap();
        assert_eq!(b.lookup(vk_for('t'), Mods::CTRL | Mods::SHIFT).unwrap().action, Action::NewSplit(Direction::Down));
        b.parse_line("f11=unbind").unwrap();
        assert!(b.lookup(0x7A, Mods::empty()).is_none());
        b.parse_line("clear").unwrap();
        assert!(b.lookup(vk_for('c'), Mods::CTRL | Mods::SHIFT).is_none());
    }

    #[test]
    fn mac_style_and_odd_triggers_parse() {
        let mut b = Bindings::default();
        b.parse_line("super+c=copy_to_clipboard").unwrap();
        b.parse_line("ctrl++=increase_font_size:2").unwrap();
        b.parse_line("global:ctrl+grave=ignore").unwrap();
        assert!(b.parse_line("ctrl+a>n=new_tab").is_err(), "sequences aren't supported");
        assert!(b.parse_line("ctrl+x=launch_rockets").is_err());
        assert!(b.parse_line("hyper+x=new_tab").is_err());
    }
}
