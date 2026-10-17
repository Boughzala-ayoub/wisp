//! The top-level window: Win32 messages, keyboard routing and frame pacing.
//!
//! Each window's state lives in a `RefCell` whose pointer is stored in the
//! window's user data. Win32 calls the window procedure re-entrantly (e.g.
//! `SetWindowPos` sends `WM_SIZE` while we're still handling a message), so
//! nested messages that find the state already borrowed go to `DefWindowProc`.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use libghostty_vt::terminal::SizeReportSize;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_F4, VK_PACKET, VK_PROCESSKEY};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::font::Font;
use crate::input::{KeyEvent, control_key_state};
use crate::render::{Renderer, Surface};
use crate::term::Pane;

const WM_APP_OUTPUT: u32 = WM_APP + 1;
const WM_APP_EXITED: u32 = WM_APP + 2;
const CLASS_NAME: PCWSTR = w!("WispWindow");

const FONT_FAMILY: &str = "Cascadia Code";
const FONT_SIZE_PT: f32 = 12.0;
const PADDING_PT: f32 = 2.0;
const INITIAL_COLUMNS: u16 = 100;
const INITIAL_ROWS: u16 = 30;

thread_local! {
    /// Every open window, so the message loop knows whose frames to pace.
    static WINDOWS: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

pub struct Window {
    hwnd: HWND,
    renderer: Renderer,
    surface: Surface,
    font: Font,
    pane: Pane,
    dpi: u32,
    focused: bool,
    /// Something changed since the last frame.
    dirty: bool,
    /// The swap chain signaled that it can take a frame, and we haven't used it yet.
    frame_ready: bool,
    /// A `WM_APP_OUTPUT` is already queued; the reader thread needn't post another.
    output_pending: Arc<AtomicBool>,
    /// Until the first frame is presented, for the startup-time log.
    first_frame: bool,
}

impl Window {
    /// Opens a window running `command_line` and shows it.
    pub fn open(command_line: &str) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
            let font = Font::new(FONT_FAMILY, FONT_SIZE_PT, dpi)?;
            let padding = padding_px(dpi);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: (INITIAL_COLUMNS as u32 * font.cell_width + 2 * padding) as i32,
                bottom: (INITIAL_ROWS as u32 * font.cell_height + 2 * padding) as i32,
            };
            AdjustWindowRectExForDpi(&mut rect, WS_OVERLAPPEDWINDOW, false, WS_EX_NOREDIRECTIONBITMAP, dpi)?;
            SetWindowPos(hwnd, None, 0, 0, rect.right - rect.left, rect.bottom - rect.top, SWP_NOMOVE | SWP_NOZORDER)?;

            let mut client = RECT::default();
            GetClientRect(hwnd, &mut client)?;
            let (width, height) = (client.right as u32, client.bottom as u32);
            let renderer = Renderer::new()?;
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
            let pane = Pane::spawn(command_line, size, wake, exited)?;

            style_title_bar(hwnd);
            let window = Window {
                hwnd,
                renderer,
                surface,
                font,
                pane,
                dpi,
                // WM_SETFOCUS sets this once the window is actually activated.
                focused: false,
                dirty: true,
                frame_ready: false,
                output_pending,
                first_frame: true,
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
        let padding = padding_px(self.dpi) as f32;
        let background = self.renderer.draw_pane(&mut self.pane, &mut self.font, (padding, padding), self.focused)?;
        self.renderer.render(&target, self.surface.width, self.surface.height, background)?;
        Ok(self.surface.present()?)
    }

    fn resize(&mut self, width: u32, height: u32) {
        if let Err(e) = self.surface.resize(width, height) {
            eprintln!("resize failed: {e}");
        }
        self.pane.resize(grid_size(&self.font, padding_px(self.dpi), width, height));
        self.dirty = true;
    }

    fn dpi_changed(&mut self, dpi: u32) {
        self.dpi = dpi;
        match Font::new(FONT_FAMILY, FONT_SIZE_PT, dpi) {
            Ok(font) => self.font = font,
            Err(e) => eprintln!("reloading font for {dpi} DPI failed: {e}"),
        }
        self.renderer.clear_glyphs();
        // The new size arrives with the WM_SIZE that follows.
    }

    fn output_arrived(&mut self) {
        self.output_pending.store(false, Ordering::Release);
        if self.pane.pump() {
            self.dirty = true;
        }
        if let Some(title) = self.pane.take_title() {
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

fn padding_px(dpi: u32) -> u32 {
    (PADDING_PT * dpi as f32 / 72.0).round() as u32
}

fn grid_size(font: &Font, padding: u32, width: u32, height: u32) -> SizeReportSize {
    let inner = |total: u32| total.saturating_sub(2 * padding);
    SizeReportSize {
        columns: (inner(width) / font.cell_width).max(1) as u16,
        rows: (inner(height) / font.cell_height).max(1) as u16,
        cell_width: font.cell_width,
        cell_height: font.cell_height,
    }
}

/// Dark title bar in the terminal's background color, so the frame blends in.
fn style_title_bar(hwnd: HWND) {
    let bg = crate::term::DEFAULT_BACKGROUND;
    let colorref = bg.r as u32 | (bg.g as u32) << 8 | (bg.b as u32) << 16;
    unsafe {
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&raw const dark).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, (&raw const colorref).cast(), 4);
        // The default 1px border is translucent and shows whatever is behind the window.
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, (&raw const colorref).cast(), 4);
    }
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
        WM_SETFOCUS | WM_KILLFOCUS => {
            window.focused = msg == WM_SETFOCUS;
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
