//! The top-level window: Win32 messages, keyboard routing and frame pacing.
//!
//! Each window's state lives in a `RefCell` whose pointer is stored in the
//! window's user data. Win32 calls the window procedure re-entrantly (e.g.
//! `SetWindowPos` sends `WM_SIZE` while we're still handling a message), so
//! nested messages that find the state already borrowed go to `DefWindowProc`.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use libghostty_vt::key::Mods;
use libghostty_vt::selection::gesture::Autoscroll;
use libghostty_vt::terminal::{ScrollViewport, SizeReportSize};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT, ScreenToClient};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_CAPITAL, VK_CONTROL, VK_DOWN, VK_F4, VK_LWIN, VK_MENU,
    VK_NUMLOCK, VK_PACKET, VK_PROCESSKEY, VK_RWIN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::bindings::Action;
use crate::clipboard;
use crate::config::{Config, RightClick};
use crate::font::Font;
use crate::input::{KeyEvent, control_key_state};
use crate::mouse::{Button, Layout, Mouse, Padding, Wheel};
use crate::render::{Renderer, Surface, Theme};
use crate::term::Pane;

const WM_APP_OUTPUT: u32 = WM_APP + 1;
const WM_APP_EXITED: u32 = WM_APP + 2;
/// Shows config errors once the window is up, so startup isn't blocked on a dialog.
const WM_APP_CONFIG_ERRORS: u32 = WM_APP + 3;
const BLINK_TIMER: usize = 1;
/// Ghostty's cursor blink interval.
const BLINK_MS: u32 = 600;
/// While a selection is dragged past the top or bottom edge, scroll a line this often.
const AUTOSCROLL_TIMER: usize = 2;
const AUTOSCROLL_MS: u32 = 40;
const CLASS_NAME: PCWSTR = w!("WispWindow");

thread_local! {
    /// Every open window, so the message loop knows whose frames to pace.
    static WINDOWS: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

/// The modifier keys held right now.
fn current_mods() -> Mods {
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
    let mut mods = Mods::empty();
    mods.set(Mods::SHIFT, down(VK_SHIFT));
    mods.set(Mods::CTRL, down(VK_CONTROL));
    mods.set(Mods::ALT, down(VK_MENU));
    mods.set(Mods::SUPER, down(VK_LWIN) || down(VK_RWIN));
    mods
}

fn is_modifier(vk: VIRTUAL_KEY) -> bool {
    matches!(vk, VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN | VK_CAPITAL | VK_NUMLOCK)
        || (0xA0..=0xA5).contains(&vk.0) // left and right Shift, Ctrl, Alt
}

pub struct Window {
    hwnd: HWND,
    config: Config,
    renderer: Renderer,
    surface: Surface,
    font: Font,
    /// The current font size; Ctrl+= and friends move it away from the config's.
    font_size: f32,
    pane: Pane,
    dpi: u32,
    /// The mouse pointer is hidden until it moves (`mouse-hide-while-typing`).
    pointer_hidden: bool,
    focused: bool,
    /// Blink phase: a blinking cursor shows while this is true.
    blink_on: bool,
    blink_timer: bool,
    /// Something changed since the last frame.
    dirty: bool,
    /// The swap chain signaled that it can take a frame, and we haven't used it yet.
    frame_ready: bool,
    /// A `WM_APP_OUTPUT` is already queued; the reader thread needn't post another.
    output_pending: Arc<AtomicBool>,
    /// Until the first frame is presented, for the startup-time log.
    first_frame: bool,
    /// The last pointer position in client pixels, for autoscroll ticks.
    pointer: (f32, f32),
    autoscroll_timer: bool,
}

impl Window {
    /// Opens a window running the configured shell and shows it.
    pub fn open(config: Config) -> std::result::Result<(), Box<dyn std::error::Error>> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_IBEAM)?,
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            // Registering twice fails harmlessly when a second window opens.
            RegisterClassExW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                CLASS_NAME,
                w!("Wisp"),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(instance.into()),
                None,
            )?;

            let dpi = GetDpiForWindow(hwnd);
            let font = Font::new(&config.font_families(), config.font_size, dpi, &config.font_features)?;
            let padding = padding_px(&config, dpi);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: (config.window_width as u32 * font.cell_width + padding.left + padding.right) as i32,
                bottom: (config.window_height as u32 * font.cell_height + padding.top + padding.bottom) as i32,
            };
            AdjustWindowRectExForDpi(&mut rect, WS_OVERLAPPEDWINDOW, false, WS_EX_NOREDIRECTIONBITMAP, dpi)?;
            SetWindowPos(hwnd, None, 0, 0, rect.right - rect.left, rect.bottom - rect.top, SWP_NOMOVE | SWP_NOZORDER)?;

            let mut client = RECT::default();
            GetClientRect(hwnd, &mut client)?;
            let (width, height) = (client.right as u32, client.bottom as u32);
            let mut renderer = Renderer::new()?;
            renderer.theme = Theme::from_config(&config);
            let surface = Surface::new(&renderer, hwnd, width, height)?;
            let size = grid_size(&font, padding, width, height);

            let output_pending = Arc::new(AtomicBool::new(false));
            let target = hwnd.0 as usize; // HWND isn't Send; its value is just an id
            let pending = output_pending.clone();
            let wake = move || {
                if !pending.swap(true, Ordering::AcqRel) {
                    let _ = PostMessageW(Some(HWND(target as _)), WM_APP_OUTPUT, WPARAM(0), LPARAM(0));
                }
            };
            let exited = move || {
                let _ = PostMessageW(Some(HWND(target as _)), WM_APP_EXITED, WPARAM(0), LPARAM(0));
            };
            let command_line = config.command.clone().unwrap_or_else(default_shell);
            let pane = Pane::spawn(&command_line, size, &config, wake, exited)?;

            style_title_bar(hwnd, &config);
            if let Some(title) = &config.title {
                let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
            }
            if !config.errors.is_empty() {
                let _ = PostMessageW(Some(hwnd), WM_APP_CONFIG_ERRORS, WPARAM(0), LPARAM(0));
            }
            let window = Window {
                hwnd,
                font_size: config.font_size,
                config,
                renderer,
                surface,
                font,
                pane,
                dpi,
                pointer_hidden: false,
                // WM_SETFOCUS sets this once the window is actually activated.
                focused: false,
                blink_on: true,
                blink_timer: false,
                dirty: true,
                frame_ready: false,
                output_pending,
                first_frame: true,
                pointer: (0.0, 0.0),
                autoscroll_timer: false,
            };
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(Box::new(RefCell::new(window))) as isize);
            WINDOWS.with_borrow_mut(|w| w.push(hwnd));
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        Ok(())
    }

    fn render(&mut self) {
        if let Err(e) = self.try_render() {
            eprintln!("render failed: {e}");
        }
        self.dirty = false;
        self.frame_ready = false;
        if std::mem::take(&mut self.first_frame) {
            eprintln!("first frame presented {:.0} ms after process start", crate::ms_since_process_start());
        }
    }

    fn try_render(&mut self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let target = self.surface.target(&self.renderer)?;
        let padding = padding_px(&self.config, self.dpi);
        let origin = (padding.left as f32, padding.top as f32);
        let frame = self.renderer.draw_pane(&mut self.pane, &mut self.font, origin, self.focused, self.blink_on)?;
        self.renderer.render(&target, self.surface.width, self.surface.height, frame.background)?;
        self.surface.present()?;
        self.run_blink_timer(frame.cursor_blinks && self.focused);
        Ok(())
    }

    /// Starts or stops the blink timer; it only runs while there's a blinking cursor to show.
    fn run_blink_timer(&mut self, wanted: bool) {
        if wanted == self.blink_timer {
            return;
        }
        unsafe {
            if wanted {
                SetTimer(Some(self.hwnd), BLINK_TIMER, BLINK_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), BLINK_TIMER);
                self.blink_on = true;
            }
        }
        self.blink_timer = wanted;
    }

    /// Shows the cursor and restarts its blink period, e.g. after a keypress.
    fn reset_blink(&mut self) {
        if !self.blink_on {
            self.dirty = true;
        }
        self.blink_on = true;
        if self.blink_timer {
            unsafe { SetTimer(Some(self.hwnd), BLINK_TIMER, BLINK_MS, None) };
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if let Err(e) = self.surface.resize(width, height) {
            eprintln!("resize failed: {e}");
        }
        self.pane.resize(grid_size(&self.font, padding_px(&self.config, self.dpi), width, height));
        self.dirty = true;
    }

    fn dpi_changed(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.rebuild_font();
        // The new size arrives with the WM_SIZE that follows.
    }

    /// Reloads the font at the current size and DPI and refits the grid.
    fn rebuild_font(&mut self) {
        match Font::new(&self.config.font_families(), self.font_size, self.dpi, &self.config.font_features) {
            Ok(font) => self.font = font,
            Err(e) => eprintln!("loading the font failed: {e}"),
        }
        self.renderer.clear_glyphs();
        let (w, h) = (self.surface.width, self.surface.height);
        self.pane.resize(grid_size(&self.font, padding_px(&self.config, self.dpi), w, h));
        self.dirty = true;
    }

    /// Reads the config again and applies everything that can change live.
    /// The shell keeps running; `command` only affects new windows.
    fn reload_config(&mut self) {
        let config = Config::load();
        report(&config);
        let font_changed = config.font_families() != self.config.font_families()
            || config.font_size != self.config.font_size
            || config.font_features != self.config.font_features;
        if config.font_size != self.config.font_size {
            self.font_size = config.font_size;
        }
        self.pane.apply_config(&config);
        self.renderer.theme = Theme::from_config(&config);
        style_title_bar(self.hwnd, &config);
        if let Some(title) = &config.title {
            unsafe {
                let _ = SetWindowTextW(self.hwnd, &HSTRING::from(title));
            }
        }
        let errors = !config.errors.is_empty();
        self.config = config;
        if font_changed {
            self.rebuild_font();
        } else {
            let (w, h) = (self.surface.width, self.surface.height);
            self.resize(w, h); // padding may have changed
        }
        if errors {
            show_config_errors(self.hwnd, &self.config);
        }
        self.dirty = true;
    }

    fn output_arrived(&mut self) {
        self.output_pending.store(false, Ordering::Release);
        if self.pane.pump() {
            self.dirty = true;
        }
        if let Some(title) = self.pane.take_title()
            && self.config.title.is_none()
        {
            let title = if title.is_empty() { "Wisp".to_owned() } else { title };
            unsafe {
                let _ = SetWindowTextW(self.hwnd, &HSTRING::from(title));
            }
        }
    }

    /// Handles WM_(SYS)KEYDOWN/UP. Returns false to let `DefWindowProc` have it.
    fn key(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
        let vk = wparam.0 as u16;
        let down = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN);
        // Alt+F4 stays a window command, like in every other Windows app.
        if vk == VK_F4.0 && msg == WM_SYSKEYDOWN {
            return false;
        }
        // The IME is composing; its result arrives later as WM_CHAR.
        if vk == VK_PROCESSKEY.0 {
            return false;
        }
        if down && let Some(binding) = self.config.bindings.lookup(vk, current_mods()) {
            // A `performable:` binding whose action can't run (copy with nothing
            // selected) lets the key through to the program instead.
            if self.perform(binding.action) || !binding.performable {
                take_typed_chars(self.hwnd); // e.g. the ^V that Ctrl+Shift+V would type
                return true;
            }
        }
        if down && !is_modifier(VIRTUAL_KEY(vk)) {
            // Like Ghostty: typing jumps back to the live screen and drops the selection.
            self.pane.scroll(ScrollViewport::Bottom);
            if self.config.selection_clear_on_typing {
                self.pane.clear_selection();
            }
            if self.config.mouse_hide_while_typing && !self.pointer_hidden {
                self.pointer_hidden = true;
                unsafe { SetCursor(None) };
            }
            self.dirty = true;
        }
        if !self.pane.win32_input {
            // ponytail: without win32-input-mode only typed text gets through (via WM_CHAR).
            // conhost always asks for the mode, so a VT key encoder waits for a backend that doesn't.
            return msg == WM_SYSKEYUP || msg == WM_SYSKEYDOWN;
        }
        let scan = ((lparam.0 >> 16) & 0xff) as u16;
        let extended = (lparam.0 >> 24) & 1 == 1;
        let event = KeyEvent {
            // VK_PACKET carries text injected by SendInput; conhost wants no key code for it.
            vk: if vk == VK_PACKET.0 { 0 } else { vk },
            scan,
            ch: 0,
            down,
            state: control_key_state(extended),
            repeat: (lparam.0 & 0xffff).max(1) as u16,
        };
        self.reset_blink();
        let mut out = String::new();
        if down {
            // TranslateMessage has already queued the characters this key types.
            // Take them now so they travel inside this key event.
            for ch in take_typed_chars(self.hwnd) {
                out += &KeyEvent { ch, ..event }.encode();
            }
        }
        if out.is_empty() {
            out = event.encode();
        }
        self.pane.write(out.as_bytes());
        true
    }

    /// Runs a bound action. Returns false if it had nothing to act on.
    fn perform(&mut self, action: Action) -> bool {
        let page = self.pane.terminal.rows().unwrap_or(1) as isize;
        self.dirty = true;
        match action {
            Action::CopyToClipboard => return self.copy_selection(),
            Action::PasteFromClipboard => return self.paste_clipboard(),
            Action::SelectAll => {
                let term = &self.pane.terminal;
                if let Ok(Some(all)) = term.select_all() {
                    let _ = term.set_selection(Some(&all));
                }
            }
            Action::ScrollPageUp => self.pane.scroll(ScrollViewport::Delta(-page)),
            Action::ScrollPageDown => self.pane.scroll(ScrollViewport::Delta(page)),
            Action::ScrollToTop => self.pane.scroll(ScrollViewport::Top),
            Action::ScrollToBottom => self.pane.scroll(ScrollViewport::Bottom),
            Action::IncreaseFontSize(by) => self.set_font_size(self.font_size + by),
            Action::DecreaseFontSize(by) => self.set_font_size(self.font_size - by),
            Action::ResetFontSize => self.set_font_size(self.config.font_size),
            Action::ReloadConfig => self.reload_config(),
            Action::OpenConfig => open_config(self.hwnd),
            Action::NewWindow => {
                if let Err(e) = Window::open(self.config.clone()) {
                    eprintln!("opening a window failed: {e}");
                }
            }
            // Closing goes through WM_CLOSE so the window isn't destroyed while
            // this call still holds its state.
            Action::CloseSurface | Action::CloseTab | Action::CloseWindow => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            Action::Quit => {
                for hwnd in WINDOWS.with_borrow(Vec::clone) {
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            Action::Ignore => {}
            // ponytail: tabs, splits and fullscreen arrive in M5; until then their keys do nothing.
            other => eprintln!("{other:?} isn't implemented yet"),
        }
        true
    }

    fn set_font_size(&mut self, size: f32) {
        let size = size.clamp(4.0, 200.0);
        if size != self.font_size {
            self.font_size = size;
            self.rebuild_font();
        }
    }

    /// Copies the selection, if there is one. Returns whether there was.
    fn copy_selection(&mut self) -> bool {
        let Some(text) = self.pane.selection_text().filter(|t| !t.is_empty()) else { return false };
        clipboard::set_text(Some(self.hwnd), &text);
        true
    }

    /// Pastes the clipboard's text. Returns false if there was none.
    fn paste_clipboard(&mut self) -> bool {
        let Some(text) = clipboard::get_text(Some(self.hwnd)).filter(|t| !t.is_empty()) else { return false };
        let risky = self.config.clipboard_paste_protection && self.pane.paste_needs_confirmation(&text);
        if risky && !confirm_paste(self.hwnd, &text) {
            return true;
        }
        self.pane.scroll(ScrollViewport::Bottom);
        self.pane.paste(&text);
        true
    }

    fn layout(&self) -> Layout {
        Layout {
            width: self.surface.width,
            height: self.surface.height,
            cell_width: self.font.cell_width,
            cell_height: self.font.cell_height,
            padding: padding_px(&self.config, self.dpi),
        }
    }

    fn mouse_button(&mut self, button: Button, down: bool, x: f32, y: f32) {
        let (mods, layout) = (current_mods(), self.layout());
        self.pointer = (x, y);
        // Right-click copies the selection or pastes, the way conhost and
        // Windows Terminal do, unless `right-click-action` says otherwise.
        if button == Button::Right && down && !Mouse::reports(&self.pane.terminal, mods) {
            match self.config.right_click_action {
                RightClick::CopyOrPaste => {
                    if self.copy_selection() {
                        self.pane.clear_selection();
                    } else {
                        self.paste_clipboard();
                    }
                }
                RightClick::Copy => {
                    self.copy_selection();
                }
                RightClick::Paste => {
                    self.paste_clipboard();
                }
                RightClick::Ignore => {}
            }
            self.dirty = true;
            return;
        }
        let ends_selection = button == Button::Left && !down && self.pane.mouse.is_selecting();
        unsafe {
            if down {
                SetCapture(self.hwnd);
            } else {
                let _ = ReleaseCapture();
            }
        }
        let mut out = Vec::new();
        let Pane { mouse, terminal, .. } = &mut self.pane;
        let result = if down {
            mouse.press(terminal, button, x, y, mods, layout, &mut out)
        } else {
            mouse.release(terminal, button, x, y, mods, layout, &mut out)
        };
        if let Err(e) = result {
            eprintln!("mouse button: {e}");
        }
        self.pane.write(&out);
        if ends_selection && self.config.copy_on_select {
            self.copy_selection();
        }
        self.run_autoscroll_timer();
        self.dirty = true;
    }

    fn mouse_moved(&mut self, x: f32, y: f32) {
        self.pointer = (x, y);
        self.pointer_hidden = false;
        let (mods, layout) = (current_mods(), self.layout());
        let mut out = Vec::new();
        let Pane { mouse, terminal, .. } = &mut self.pane;
        let selecting = mouse.is_selecting();
        if let Err(e) = mouse.moved(terminal, x, y, mods, layout, &mut out) {
            eprintln!("mouse move: {e}");
        }
        self.pane.write(&out);
        if selecting {
            self.run_autoscroll_timer();
            self.dirty = true;
        }
    }

    /// `notches` is in wheel detents; positive turns away from the user (up).
    fn mouse_wheel(&mut self, notches: f32, x: f32, y: f32) {
        let (mods, layout) = (current_mods(), self.layout());
        let mut out = Vec::new();
        let lines = notches * wheel_scroll_lines();
        let Pane { mouse, terminal, .. } = &mut self.pane;
        match mouse.wheel(terminal, lines, x, y, mods, layout, &mut out) {
            Ok(Wheel::Arrows(presses)) => {
                let vk = if presses < 0 { VK_UP } else { VK_DOWN };
                for _ in 0..presses.unsigned_abs() {
                    self.send_key(vk);
                }
            }
            Ok(Wheel::Handled) => {}
            Err(e) => eprintln!("mouse wheel: {e}"),
        }
        self.pane.write(&out);
        self.dirty = true;
    }

    /// Sends one press and release of a key that types nothing (arrows and the like).
    fn send_key(&self, vk: VIRTUAL_KEY) {
        if !self.pane.win32_input {
            let seq: &[u8] = if vk == VK_UP { b"\x1b[A" } else { b"\x1b[B" };
            self.pane.write(seq);
            return;
        }
        use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};
        let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } as u16;
        let event = KeyEvent { vk: vk.0, scan, ch: 0, down: true, state: control_key_state(true), repeat: 1 };
        let out = event.encode() + &KeyEvent { down: false, ..event }.encode();
        self.pane.write(out.as_bytes());
    }

    /// Runs the autoscroll timer while a selection is dragged past an edge.
    fn run_autoscroll_timer(&mut self) {
        let wanted = self.pane.mouse.autoscroll(&self.pane.terminal) != Autoscroll::None;
        if wanted == self.autoscroll_timer {
            return;
        }
        unsafe {
            if wanted {
                SetTimer(Some(self.hwnd), AUTOSCROLL_TIMER, AUTOSCROLL_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), AUTOSCROLL_TIMER);
            }
        }
        self.autoscroll_timer = wanted;
    }

    fn autoscroll_tick(&mut self) {
        let ((x, y), layout) = (self.pointer, self.layout());
        let Pane { mouse, terminal, .. } = &mut self.pane;
        if let Err(e) = mouse.autoscroll_tick(terminal, x, y, layout) {
            eprintln!("autoscroll: {e}");
        }
        self.run_autoscroll_timer();
        self.dirty = true;
    }

    /// Characters that arrive without a key event of their own: IME results.
    fn char(&mut self, ch: u16) {
        if self.pane.win32_input {
            let event = KeyEvent { vk: 0, scan: 0, ch, down: true, state: 0, repeat: 1 };
            let out = event.encode() + &KeyEvent { down: false, ..event }.encode();
            self.pane.write(out.as_bytes());
        } else if let Some(Ok(c)) = char::decode_utf16([ch]).next() {
            // ponytail: drops astral characters in this fallback; win32-input-mode carries them fine.
            self.pane.write(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }
}

/// Removes the WM_CHAR messages TranslateMessage queued for the key being handled.
fn take_typed_chars(hwnd: HWND) -> Vec<u16> {
    let mut chars = Vec::new();
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, Some(hwnd), WM_CHAR, WM_DEADCHAR, PM_REMOVE).as_bool()
            || PeekMessageW(&mut msg, Some(hwnd), WM_SYSCHAR, WM_SYSDEADCHAR, PM_REMOVE).as_bool()
        {
            // A dead key types nothing yet; the next key carries the composed character.
            if matches!(msg.message, WM_CHAR | WM_SYSCHAR) {
                chars.push(msg.wParam.0 as u16);
            }
        }
    }
    chars
}

/// Asks before pasting text that would run commands as it arrives. This is
/// Ghostty's clipboard-paste-protection, on by default there too.
fn confirm_paste(hwnd: HWND, text: &str) -> bool {
    let lines = text.lines().count();
    let preview: String =
        text.lines().take(5).map(|l| format!("  {}\n", l.chars().take(80).collect::<String>())).collect();
    let more = if lines > 5 { format!("  ...and {} more lines\n", lines - 5) } else { String::new() };
    let message = format!(
        "This text has {lines} lines. Pasting it may run commands before you can review them:\n\n{preview}{more}\nPaste anyway?"
    );
    let answer = unsafe {
        MessageBoxW(Some(hwnd), &HSTRING::from(message), w!("Paste"), MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2)
    };
    answer == IDOK
}

/// Lines to scroll per wheel notch, from the system mouse settings.
fn wheel_scroll_lines() -> f32 {
    let mut lines = 3u32;
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some((&raw mut lines).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    // u32::MAX means "a page at a time"; a generous fixed amount will do.
    if lines == u32::MAX { 10.0 } else { lines as f32 }
}

/// The configured padding (in points) in pixels at this DPI.
fn padding_px(config: &Config, dpi: u32) -> Padding {
    let px = |pt: f32| (pt * dpi as f32 / 72.0).round() as u32;
    Padding {
        left: px(config.window_padding_x.0),
        right: px(config.window_padding_x.1),
        top: px(config.window_padding_y.0),
        bottom: px(config.window_padding_y.1),
    }
}

fn grid_size(font: &Font, padding: Padding, width: u32, height: u32) -> SizeReportSize {
    SizeReportSize {
        columns: (width.saturating_sub(padding.left + padding.right) / font.cell_width).max(1) as u16,
        rows: (height.saturating_sub(padding.top + padding.bottom) / font.cell_height).max(1) as u16,
        cell_width: font.cell_width,
        cell_height: font.cell_height,
    }
}

/// PowerShell 7 if it's installed, otherwise the Windows PowerShell every
/// Windows install has.
fn default_shell() -> String {
    let on_path = |exe: &str| {
        std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(exe).is_file()))
    };
    let shell = if on_path("pwsh.exe") { "pwsh.exe" } else { "powershell.exe" };
    format!("{shell} -NoLogo")
}

/// Logs config problems to stderr: errors, and keys Wisp doesn't support yet.
pub fn report(config: &Config) {
    for error in &config.errors {
        eprintln!("config error: {error}");
    }
    for key in &config.unsupported {
        eprintln!("config: not supported yet, ignored: {key}");
    }
}

fn show_config_errors(hwnd: HWND, config: &Config) {
    let file = config.path.as_ref().map_or("the command line".into(), |p| p.display().to_string());
    let list: String = config.errors.iter().take(12).map(|e| format!("\u{2022} {e}\n")).collect();
    let message = format!("Some settings in {file} couldn't be applied:\n\n{list}\nEverything else is in effect.");
    unsafe {
        MessageBoxW(Some(hwnd), &HSTRING::from(message), w!("Wisp configuration"), MB_OK | MB_ICONWARNING);
    }
}

/// Opens the config file in an editor, creating it with some pointers first.
fn open_config(hwnd: HWND) {
    let Some(path) = crate::config::default_path() else { return };
    if !path.exists() {
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let _ = std::fs::write(&path, CONFIG_TEMPLATE);
    }
    let file = HSTRING::from(path.as_os_str());
    unsafe {
        // `.ghostty` files have no editor associated by default; Notepad always works.
        let opened = ShellExecuteW(Some(hwnd), w!("open"), &file, None, None, SW_SHOWNORMAL);
        if opened.0 as usize <= 32 {
            ShellExecuteW(Some(hwnd), w!("open"), w!("notepad.exe"), &file, None, SW_SHOWNORMAL);
        }
    }
}

const CONFIG_TEMPLATE: &str = "\
# Wisp reads Ghostty's config format: https://ghostty.org/docs/config
# Save, then press Ctrl+Shift+, to reload. Lines starting with # are comments.

# font-family = Cascadia Code
# font-size = 12
# theme = light:Catppuccin Latte,dark:Catppuccin Mocha
# cursor-style = bar
# window-padding-x = 4
# keybind = performable:ctrl+c=copy_to_clipboard
";

/// A dark or light title bar in the terminal's background color, so the
/// frame and the terminal read as one surface.
fn style_title_bar(hwnd: HWND, config: &Config) {
    let bg = config.background;
    let colorref = bg.r as u32 | (bg.g as u32) << 8 | (bg.b as u32) << 16;
    let dark_background = (bg.r as u32 * 299 + bg.g as u32 * 587 + bg.b as u32 * 114) / 1000 < 128;
    unsafe {
        let dark = BOOL(dark_background as i32);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&raw const dark).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, (&raw const colorref).cast(), 4);
        // The default 1px border is translucent and shows whatever is behind the window.
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, (&raw const colorref).cast(), 4);
    }
}

/// The client-area position packed into a mouse message's lParam.
fn client_point(lparam: LPARAM) -> (f32, f32) {
    ((lparam.0 & 0xffff) as i16 as f32, ((lparam.0 >> 16) & 0xffff) as i16 as f32)
}

fn state(hwnd: HWND) -> Option<&'static RefCell<Window>> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<Window>;
    // SAFETY: set in `open` from a leaked Box and cleared before it's freed in WM_NCDESTROY.
    unsafe { ptr.as_ref() }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let default = || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    if msg == WM_NCDESTROY {
        let ptr = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) } as *mut RefCell<Window>;
        if !ptr.is_null() {
            drop(unsafe { Box::from_raw(ptr) });
        }
        WINDOWS.with_borrow_mut(|w| w.retain(|&h| h != hwnd));
        if WINDOWS.with_borrow(Vec::is_empty) {
            unsafe { PostQuitMessage(0) };
        }
        return default();
    }
    let Some(cell) = state(hwnd) else { return default() };
    let Ok(mut window) = cell.try_borrow_mut() else { return default() };
    match msg {
        WM_APP_OUTPUT => window.output_arrived(),
        WM_APP_CONFIG_ERRORS => show_config_errors(hwnd, &window.config),
        WM_SETTINGCHANGE => {
            // Windows switched between light and dark mode: a `light:...,dark:...`
            // theme follows it.
            let setting = lparam.0 as *const u16;
            if !setting.is_null() && unsafe { PCWSTR(setting).to_string() }.is_ok_and(|s| s == "ImmersiveColorSet") {
                window.reload_config();
            }
            drop(window);
            return default();
        }
        WM_APP_EXITED => {
            drop(window);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            if !window.key(msg, wparam, lparam) {
                drop(window);
                return default();
            }
        }
        WM_CHAR => window.char(wparam.0 as u16),
        WM_SIZE => {
            if wparam.0 as u32 != SIZE_MINIMIZED {
                let (width, height) = ((lparam.0 & 0xffff) as u32, ((lparam.0 >> 16) & 0xffff) as u32);
                window.resize(width, height);
                // Draw now: during a drag-resize the modal loop owns the thread.
                window.render();
            }
        }
        WM_DPICHANGED => {
            window.dpi_changed((wparam.0 & 0xffff) as u32);
            drop(window);
            let r = unsafe { &*(lparam.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        WM_TIMER if wparam.0 == BLINK_TIMER => {
            window.blink_on = !window.blink_on;
            window.dirty = true;
        }
        WM_TIMER if wparam.0 == AUTOSCROLL_TIMER => window.autoscroll_tick(),
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP => {
            let button = match msg {
                WM_LBUTTONDOWN | WM_LBUTTONUP => Button::Left,
                WM_MBUTTONDOWN | WM_MBUTTONUP => Button::Middle,
                _ => Button::Right,
            };
            let down = matches!(msg, WM_LBUTTONDOWN | WM_MBUTTONDOWN | WM_RBUTTONDOWN);
            let (x, y) = client_point(lparam);
            window.mouse_button(button, down, x, y);
        }
        WM_MOUSEMOVE => {
            let (x, y) = client_point(lparam);
            window.mouse_moved(x, y);
        }
        WM_MOUSEWHEEL => {
            // Wheel messages carry screen coordinates, unlike the other mouse messages.
            let mut p = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
            let _ = unsafe { ScreenToClient(hwnd, &mut p) };
            let notches = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / WHEEL_DELTA as f32;
            window.mouse_wheel(notches, p.x as f32, p.y as f32);
        }
        WM_CAPTURECHANGED if window.pane.mouse.is_selecting() => {
            // Lost the mouse mid-drag (Alt+Tab, say): finish the selection where it is.
            let (x, y) = window.pointer;
            window.mouse_button(Button::Left, false, x, y);
        }
        WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
            // An arrow when clicks go to the program, an I-beam when they select
            // text, nothing while typing hides it.
            let shape = if Mouse::reports(&window.pane.terminal, current_mods()) { IDC_ARROW } else { IDC_IBEAM };
            unsafe {
                let cursor = if window.pointer_hidden { None } else { LoadCursorW(None, shape).ok() };
                let _ = SetCursor(cursor);
            }
            return LRESULT(1);
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            window.focused = msg == WM_SETFOCUS;
            window.reset_blink();
            window.pane.focus_changed(window.focused);
            window.dirty = true;
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut ps) };
            window.render();
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        _ => {
            drop(window);
            return default();
        }
    }
    LRESULT(0)
}

/// Runs until the last window closes, drawing each window at most once per
/// display refresh and only when something changed.
pub fn run_message_loop() {
    let mut msg = MSG::default();
    loop {
        // Wait for input, output, or a dirty window's swap chain becoming ready.
        let waiting: Vec<(HWND, HANDLE)> = WINDOWS.with_borrow(|windows| {
            windows
                .iter()
                .filter_map(|&hwnd| {
                    let window = state(hwnd)?.try_borrow().ok()?;
                    (window.dirty && !window.frame_ready).then_some((hwnd, window.surface.waitable))
                })
                .collect()
        });
        let handles: Vec<HANDLE> = waiting.iter().map(|&(_, h)| h).collect();
        let woke = unsafe { MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        // A wait that returns because of a swap chain consumes its signal, so remember it.
        if let Some(&(hwnd, _)) = waiting.get(woke.0.wrapping_sub(WAIT_OBJECT_0.0) as usize)
            && let Some(cell) = state(hwnd)
        {
            cell.borrow_mut().frame_ready = true;
        }

        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        let windows = WINDOWS.with_borrow(Vec::clone);
        for hwnd in windows {
            let Some(cell) = state(hwnd) else { continue };
            let mut window = cell.borrow_mut();
            // A WM_APP_OUTPUT that arrived while the window was busy (in a modal
            // dialog, say) went to DefWindowProc; the flag still says output waits.
            if window.output_pending.load(Ordering::Acquire) {
                window.output_arrived();
            }
            if !window.dirty {
                continue;
            }
            if !window.frame_ready {
                window.frame_ready = unsafe { WaitForSingleObject(window.surface.waitable, 0) } == WAIT_OBJECT_0;
            }
            if window.frame_ready {
                window.render();
            }
        }
    }
}
