//! The top-level window: tabs, splits, the custom title bar, input routing
//! and frame pacing.
//!
//! Each window's state lives in a `RefCell` whose pointer is stored in the
//! window's user data. Win32 calls the window procedure re-entrantly (e.g.
//! `SetWindowPos` sends `WM_SIZE` while we're still handling a message), so
//! nested messages that find the state already borrowed go to `DefWindowProc`.
//!
//! The title bar is ours: `WM_NCCALCSIZE` gives the caption's space to the
//! client area, `WM_NCHITTEST` tells Windows which parts drag the window and
//! where the caption buttons are (so Windows 11's snap layouts appear on the
//! maximize button), and the renderer draws tabs and buttons there.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use libghostty_vt::key::Mods;
use libghostty_vt::selection::gesture::Autoscroll;
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::{ScrollViewport, SizeReportSize};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, GetMonitorInfoW, LOGFONTW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_POINT, COMPOSITIONFORM, ImmGetContext, ImmReleaseContext,
    ImmSetCandidateWindow, ImmSetCompositionFontW, ImmSetCompositionWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
    VIRTUAL_KEY, VK_CAPITAL, VK_CONTROL, VK_DOWN, VK_F4, VK_LWIN, VK_MENU, VK_NUMLOCK, VK_PACKET, VK_PROCESSKEY,
    VK_RWIN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::bindings::{Action, Direction, Focus};
use crate::chrome::{Bar, Hit, Metrics};
use crate::clipboard;
use crate::config::{Backdrop, Config, RightClick};
use crate::font::Font;
use crate::input::{KeyEvent, control_key_state};
use crate::layout::{Axis, Divider, Node, PaneId, Rect, neighbor};
use crate::mouse::{Button, Layout, Mouse, Padding, Wheel};
use crate::render::{Renderer, Surface, Theme};
use crate::shells::{self, Shell};
use crate::term::Pane;

const WM_APP_OUTPUT: u32 = WM_APP + 1;
/// A pane's program exited; wParam is the pane id.
const WM_APP_EXITED: u32 = WM_APP + 2;
/// Shows config errors once the window is up, so startup isn't blocked on a dialog.
const WM_APP_CONFIG_ERRORS: u32 = WM_APP + 3;
/// Opens the new-tab menu. Menus and dialogs run their own message loop, so
/// they open from `window_proc` with our state released: the panes keep
/// receiving output and drawing behind them.
const WM_APP_SHELL_MENU: u32 = WM_APP + 4;
/// Asks about the paste stashed in `Window::pending_paste`, the same way.
const WM_APP_CONFIRM_PASTE: u32 = WM_APP + 5;
/// The new-tab menu's command for opening the config file.
const MENU_OPEN_CONFIG: usize = 1000;
const BLINK_TIMER: usize = 1;
/// Ghostty's cursor blink interval.
const BLINK_MS: u32 = 600;
/// While a selection is dragged past the top or bottom edge, scroll a line this often.
const AUTOSCROLL_TIMER: usize = 2;
const AUTOSCROLL_MS: u32 = 40;
const CLASS_NAME: PCWSTR = w!("WispWindow");

// Segoe Fluent Icons (Windows 11) and Segoe MDL2 Assets (Windows 10) share these code points.
const ICON_MINIMIZE: char = '\u{E921}';
const ICON_MAXIMIZE: char = '\u{E922}';
const ICON_RESTORE: char = '\u{E923}';
const ICON_CLOSE: char = '\u{E8BB}';
const ICON_ADD: char = '\u{E710}';
const ICON_CHEVRON_DOWN: char = '\u{E70D}';
/// Windows 11's red for a hovered close button.
const CLOSE_RED: RgbColor = RgbColor { r: 0xC4, g: 0x2B, b: 0x1C };
const WHITE: RgbColor = RgbColor { r: 255, g: 255, b: 255 };
const BLACK: RgbColor = RgbColor { r: 0, g: 0, b: 0 };

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

/// A tab: a tree of split panes, one of them focused.
struct Tab {
    tree: Node,
    focused: PaneId,
    /// The focused pane fills the tab (`toggle_split_zoom`).
    zoomed: bool,
}

pub struct Window {
    hwnd: HWND,
    config: Config,
    renderer: Renderer,
    surface: Surface,
    font: Font,
    /// Tab titles.
    ui_font: Font,
    /// Caption button and tab glyphs.
    icon_font: Font,
    /// The current font size; Ctrl+= and friends move it away from the config's.
    font_size: f32,
    /// Panes by id; closed panes leave a `None`.
    panes: Vec<Option<Pane>>,
    pane_titles: Vec<String>,
    /// Each pane's shell name, the tab title until the program sets its own.
    pane_names: Vec<String>,
    tabs: Vec<Tab>,
    active: usize,
    dpi: u32,
    focused: bool,
    /// Blink phase: a blinking cursor shows while this is true.
    blink_on: bool,
    blink_timer: bool,
    /// Something changed since the last frame.
    dirty: bool,
    /// The swap chain signaled that it can take a frame, and we haven't used it yet.
    frame_ready: bool,
    /// A `WM_APP_OUTPUT` is already queued; the reader threads needn't post another.
    output_pending: Arc<AtomicBool>,
    /// Until the first frame is presented, for the startup-time log.
    first_frame: bool,
    /// The last pointer position in client pixels.
    pointer: (f32, f32),
    /// The mouse pointer is hidden until it moves (`mouse-hide-while-typing`).
    pointer_hidden: bool,
    autoscroll_timer: bool,
    /// The pane a held mouse button belongs to, so drags stay in their pane.
    mouse_pane: Option<PaneId>,
    /// A split divider being dragged.
    dragging: Option<Divider>,
    /// The title bar part under the pointer, and the caption button held down.
    hover: Option<Hit>,
    pressed: Option<Hit>,
    /// Placement and style to restore when leaving fullscreen.
    fullscreen: Option<(WINDOWPLACEMENT, isize)>,
    window_title: String,
    /// The focused cursor's position in client pixels, where IME windows go.
    ime_spot: Option<(i32, i32)>,
    /// With `WISP_TRACE_LATENCY=1`: when the last keypress went out, and when
    /// its echo came back, until the frame showing it is presented.
    latency: Option<(Instant, Option<Instant>)>,
    /// A paste waiting on the user's confirmation, and the pane it's for.
    pending_paste: Option<(PaneId, String)>,
}

impl Window {
    /// Opens a window running the configured shell and shows it. `renderer`
    /// supplies the window's renderer when it's needed (main builds it ahead).
    pub fn open(
        config: Config,
        renderer: impl FnOnce() -> windows::core::Result<Renderer>,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
            // The title bar is drawn by us, inside the client area, over the backdrop.
            let _ = DwmExtendFrameIntoClientArea(
                hwnd,
                &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 },
            );

            crate::startup_mark("window created");
            let dpi = GetDpiForWindow(hwnd);
            let font = Font::new(&config.font_families(), config.font_size, dpi, &config.font_features)?;
            crate::startup_mark("terminal font loaded");
            let padding = padding_px(&config, dpi);
            let title_height = Metrics::new(dpi).height;
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: (config.window_width as u32 * font.cell_width + padding.left + padding.right) as i32,
                bottom: (config.window_height as u32 * font.cell_height + padding.top + padding.bottom) as i32
                    + title_height,
            };
            AdjustWindowRectExForDpi(&mut rect, WS_OVERLAPPEDWINDOW, false, WS_EX_NOREDIRECTIONBITMAP, dpi)?;
            // Our caption lives inside the client area; take the standard one's height back.
            rect.top += GetSystemMetricsForDpi(SM_CYCAPTION, dpi);
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                rect.right - rect.left,
                rect.bottom - rect.top,
                // Not activating: WM_SETFOCUS would arrive before our state exists.
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )?;

            let mut client = RECT::default();
            GetClientRect(hwnd, &mut client)?;
            let (width, height) = (client.right as u32, client.bottom as u32);
            // The shell starts while the renderer (built on another thread) may still be coming.
            let output_pending = Arc::new(AtomicBool::new(false));
            let area = (width, (height as i32 - title_height).max(1) as u32);
            let command = config.command.clone().unwrap_or_else(shells::default_command);
            let first_size = grid_size(&font, padding, area.0, area.1);
            let first = start_pane(hwnd, &output_pending, 0, &command, None, first_size, &config)?;
            crate::startup_mark("shell started");
            let mut renderer = renderer()?;
            crate::startup_mark("Direct3D device and shaders ready");
            renderer.theme = Theme::from_config(&config);
            let surface = Surface::new(&renderer, hwnd, width, height)?;
            crate::startup_mark("swap chain and composition ready");
            apply_window_style(hwnd, &config);
            if !config.errors.is_empty() {
                let _ = PostMessageW(Some(hwnd), WM_APP_CONFIG_ERRORS, WPARAM(0), LPARAM(0));
            }

            let mut window = Window {
                hwnd,
                font_size: config.font_size,
                ui_font: ui_font(dpi),
                icon_font: icon_font(dpi),
                config,
                renderer,
                surface,
                font,
                panes: vec![Some(first)],
                pane_titles: vec![String::new()],
                pane_names: vec![shells::name_for(&command)],
                tabs: vec![Tab { tree: Node::Leaf(0), focused: 0, zoomed: false }],
                active: 0,
                dpi,
                // WM_SETFOCUS sets this once the window is actually activated.
                focused: false,
                blink_on: true,
                blink_timer: false,
                dirty: true,
                frame_ready: false,
                output_pending,
                first_frame: true,
                pointer: (0.0, 0.0),
                pointer_hidden: false,
                autoscroll_timer: false,
                mouse_pane: None,
                dragging: None,
                hover: None,
                pressed: None,
                fullscreen: None,
                window_title: String::new(),
                ime_spot: None,
                latency: None,
                pending_paste: None,
            };
            crate::startup_mark("UI fonts loaded");
            window.relayout();
            window.render(); // so the window never shows an empty frame
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(Box::new(RefCell::new(window))) as isize);
            WINDOWS.with_borrow_mut(|w| w.push(hwnd));
            let _ = ShowWindow(hwnd, SW_SHOW);
            // Windows doesn't activate windows launched from the background, so
            // WM_SETFOCUS may never come; ask instead of assuming.
            if let Some(cell) = state(hwnd) {
                cell.borrow_mut().focused = GetFocus() == hwnd;
            }
        }
        Ok(())
    }

    fn pane(&self, id: PaneId) -> &Pane {
        self.panes[id].as_ref().expect("panes in a tab are alive")
    }

    fn pane_mut(&mut self, id: PaneId) -> &mut Pane {
        self.panes[id].as_mut().expect("panes in a tab are alive")
    }

    fn focused_id(&self) -> PaneId {
        self.tabs[self.active].focused
    }

    /// Starts a shell in a new pane, in `cwd` if given: `shell`, or the
    /// configured `command`, or the default shell.
    fn spawn_pane(
        &mut self,
        cwd: Option<String>,
        shell: Option<Shell>,
    ) -> std::result::Result<PaneId, Box<dyn std::error::Error>> {
        let id = self.panes.iter().position(Option::is_none).unwrap_or(self.panes.len());
        let (command, name) = match shell {
            Some(shell) => (shell.command, shell.name),
            None => {
                let command = self.config.command.clone().unwrap_or_else(shells::default_command);
                let name = shells::name_for(&command);
                (command, name)
            }
        };
        // A first guess; relayout gives it its real size once it's in the tree.
        let size = grid_size(&self.font, padding_px(&self.config, self.dpi), self.surface.width, self.surface.height);
        let pane = start_pane(self.hwnd, &self.output_pending, id, &command, cwd, size, &self.config)?;
        if id == self.panes.len() {
            self.panes.push(Some(pane));
            self.pane_titles.push(String::new());
            self.pane_names.push(name);
        } else {
            self.panes[id] = Some(pane);
            self.pane_titles[id] = String::new();
            self.pane_names[id] = name;
        }
        Ok(id)
    }

    /// The focused pane's working directory, so new tabs and splits open there.
    fn inherited_cwd(&self) -> Option<String> {
        self.tabs.get(self.active).and_then(|t| self.pane(t.focused).working_directory())
    }

    fn new_tab(&mut self, shell: Option<Shell>) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let id = self.spawn_pane(self.inherited_cwd(), shell)?;
        let at = if self.tabs.is_empty() { 0 } else { self.active + 1 };
        self.tabs.insert(at, Tab { tree: Node::Leaf(id), focused: id, zoomed: false });
        self.active = at;
        self.relayout();
        Ok(())
    }

    fn new_split(&mut self, toward: Direction) {
        let cwd = self.inherited_cwd();
        let id = match self.spawn_pane(cwd, None) {
            Ok(id) => id,
            Err(e) => return eprintln!("starting a shell for the split failed: {e}"),
        };
        let tab = &mut self.tabs[self.active];
        tab.tree.split(tab.focused, id, toward);
        tab.focused = id;
        tab.zoomed = false;
        self.relayout();
    }

    /// Closes a pane and whatever it leaves empty. Returns true when the
    /// window has no tabs left and should close.
    fn close_pane(&mut self, id: PaneId) -> bool {
        let Some(t) = self.tabs.iter().position(|t| t.tree.leaves().contains(&id)) else { return self.tabs.is_empty() };
        let order = self.tabs[t].tree.leaves();
        let tree = std::mem::replace(&mut self.tabs[t].tree, Node::Leaf(id));
        match tree.remove(id) {
            Some(rest) => {
                let tab = &mut self.tabs[t];
                if tab.focused == id {
                    // Focus moves to the pane before it, or after it if it was first.
                    let i = order.iter().position(|&p| p == id).unwrap_or(0);
                    let remaining = rest.leaves();
                    tab.focused = order[..i]
                        .iter()
                        .rev()
                        .chain(&order[i + 1..])
                        .copied()
                        .find(|p| remaining.contains(p))
                        .unwrap();
                }
                tab.tree = rest;
                tab.zoomed = false;
            }
            None => {
                self.tabs.remove(t);
                if self.active >= t && self.active > 0 {
                    self.active -= 1;
                }
            }
        }
        // Dropping the pane closes its pseudoconsole and ends its program.
        self.panes[id] = None;
        self.relayout();
        self.tabs.is_empty()
    }

    fn close_tab(&mut self, index: usize) -> bool {
        for id in self.tabs[index].tree.leaves() {
            if self.close_pane(id) {
                return true;
            }
        }
        false
    }

    fn pane_count(&self) -> usize {
        self.tabs.iter().map(|t| t.tree.leaves().len()).sum()
    }

    fn title_height(&self) -> i32 {
        if self.fullscreen.is_some() { 0 } else { Metrics::new(self.dpi).height }
    }

    /// The space below the title bar that the active tab's panes share.
    fn terminal_area(&self) -> Rect {
        let top = self.title_height();
        Rect { x: 0, y: top, width: self.surface.width as i32, height: (self.surface.height as i32 - top).max(1) }
    }

    fn divider_px(&self) -> i32 {
        (self.dpi as i32 / 96).max(1)
    }

    /// Each visible pane of a tab with its rect.
    fn tab_layout(&self, tab: &Tab) -> Vec<(PaneId, Rect)> {
        if tab.zoomed {
            return vec![(tab.focused, self.terminal_area())];
        }
        tab.tree.layout(self.terminal_area(), self.divider_px())
    }

    fn visible_panes(&self) -> Vec<(PaneId, Rect)> {
        self.tab_layout(&self.tabs[self.active])
    }

    /// Gives every pane in every tab the grid size its rect fits.
    fn relayout(&mut self) {
        let padding = padding_px(&self.config, self.dpi);
        let sizes: Vec<(PaneId, Rect)> = self.tabs.iter().flat_map(|t| self.tab_layout(t)).collect();
        for (id, rect) in sizes {
            let size = grid_size(&self.font, padding, rect.width.max(1) as u32, rect.height.max(1) as u32);
            self.pane_mut(id).resize(size);
        }
        self.update_title();
        self.dirty = true;
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
        let visible = self.visible_panes();
        let focused = self.focused_id();
        let split = visible.len() > 1;
        let mut blinks = false;
        let mut ime_spot = None;
        for &(id, rect) in &visible {
            self.renderer.clip(rect);
            let origin = ((rect.x + padding.left as i32) as f32, (rect.y + padding.top as i32) as f32);
            let is_focused = self.focused && id == focused;
            let Window { renderer, font, panes, blink_on, config, .. } = self;
            let pane = panes[id].as_mut().expect("visible panes are alive");
            // A program changed this pane's background (OSC 11): paint it behind the cells.
            if let Ok(Some(bg)) = pane.terminal.bg_color()
                && bg != config.background
            {
                renderer.fill(rect, bg, config.background_opacity);
            }
            let frame = renderer.draw_pane(pane, font, origin, is_focused, *blink_on)?;
            blinks |= is_focused && frame.cursor_blinks;
            if id == focused {
                ime_spot = frame.cursor_at.map(|(x, y)| (x as i32, y as i32));
            }
            if split && id != focused {
                // Unfocused splits fade toward the background, as in Ghostty.
                let wash = 1.0 - self.config.unfocused_split_opacity;
                self.renderer.fill(rect, self.config.background, wash);
            }
        }
        self.renderer.clip(Rect { x: 0, y: 0, width: self.surface.width as i32, height: self.surface.height as i32 });
        if split {
            let dividers = self.tabs[self.active].tree.dividers(self.terminal_area(), self.divider_px());
            for divider in dividers {
                self.renderer.fill(divider.rect, self.config.foreground, 0.18);
            }
        }
        if self.fullscreen.is_none() {
            self.draw_title_bar();
        }
        let (w, h) = (self.surface.width, self.surface.height);
        self.renderer.render(&target, w, h, self.config.background, self.config.background_opacity)?;
        self.surface.present()?;
        if let Some((pressed, Some(echoed))) = self.latency {
            let ms = |t: Instant| t.duration_since(pressed).as_secs_f64() * 1000.0;
            eprintln!("latency key->echo {:.2} ms  key->present {:.2} ms", ms(echoed), ms(Instant::now()));
            self.latency = None;
        }
        self.run_blink_timer(blinks);
        if ime_spot.is_some() && ime_spot != self.ime_spot {
            self.ime_spot = ime_spot;
            self.place_ime();
        }
        Ok(())
    }

    /// Puts the IME's composition text and candidate list at the cursor, in
    /// the terminal's font, so Chinese, Japanese and Korean input happens in
    /// place rather than in a corner of the screen.
    fn place_ime(&self) {
        let Some((x, y)) = self.ime_spot else { return };
        unsafe {
            let context = ImmGetContext(self.hwnd);
            if context.is_invalid() {
                return;
            }
            let at = POINT { x, y };
            let composition = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: at, rcArea: RECT::default() };
            let _ = ImmSetCompositionWindow(context, &composition);
            let below = POINT { x, y: y + self.font.cell_height as i32 };
            let candidates =
                CANDIDATEFORM { dwIndex: 0, dwStyle: CFS_CANDIDATEPOS, ptCurrentPos: below, rcArea: RECT::default() };
            let _ = ImmSetCandidateWindow(context, &candidates);
            let mut font = LOGFONTW { lfHeight: -(self.font.em_px.round() as i32), ..Default::default() };
            let family: Vec<u16> = self.config.font_families()[0].encode_utf16().take(31).collect();
            font.lfFaceName[..family.len()].copy_from_slice(&family);
            let _ = ImmSetCompositionFontW(context, &font);
            let _ = ImmReleaseContext(self.hwnd, context);
        }
    }

    fn title_bar(&self) -> (Bar, Metrics) {
        let metrics = Metrics::new(self.dpi);
        (Bar::new(self.surface.width as i32, self.tabs.len(), &metrics), metrics)
    }

    /// Tabs on a darker strip, the active one in the terminal's own color so
    /// it reads as part of the terminal below, then the caption buttons.
    fn draw_title_bar(&mut self) {
        let (bar, m) = self.title_bar();
        let (bg, fg) = (self.config.background, self.config.foreground);
        let dark = is_dark(bg);
        let shade = if dark { 0.28 } else { 0.07 };
        let width = self.surface.width as i32;
        let r = &mut self.renderer;
        let active = bar.tabs.get(self.active).copied().unwrap_or_default();
        r.fill(Rect { x: 0, y: 0, width, height: m.tab_top }, BLACK, shade);
        r.fill(Rect { x: 0, y: m.tab_top, width: active.x, height: m.height - m.tab_top }, BLACK, shade);
        let after = active.x + active.width;
        r.fill(Rect { x: after, y: m.tab_top, width: width - after, height: m.height - m.tab_top }, BLACK, shade);

        let text_height = self.ui_font.cell_height as i32;
        for (i, tab) in bar.tabs.iter().enumerate() {
            let is_active = i == self.active;
            let hovered = matches!(self.hover, Some(Hit::Tab(t) | Hit::CloseTab(t)) if t == i);
            if hovered && !is_active {
                self.renderer.fill(*tab, fg, 0.06);
            }
            if !is_active && i + 1 != self.active && i + 1 < bar.tabs.len() {
                // A thin separator between neighbouring inactive tabs.
                let sep =
                    Rect { x: tab.x + tab.width - 1, y: tab.y + tab.height / 4, width: 1, height: tab.height / 2 };
                self.renderer.fill(sep, fg, 0.15);
            }
            let title = self.tab_title(i);
            let close = bar.closes[i];
            let text_x = (tab.x + m.margin * 2) as f32;
            let baseline = (tab.y + (tab.height - text_height) / 2) as f32 + self.ui_font.baseline as f32;
            let max_width = (close.x - tab.x - m.margin * 3) as f32;
            let alpha = if is_active { 1.0 } else { 0.65 };
            self.renderer.text(&mut self.ui_font, &title, text_x, baseline, max_width, fg, alpha);
            if is_active || hovered {
                if self.hover == Some(Hit::CloseTab(i)) {
                    self.renderer.fill(close, fg, 0.12);
                }
                self.renderer.icon(&mut self.icon_font, ICON_CLOSE, close, fg, 0.8);
            }
        }
        if self.hover == Some(Hit::NewTab) {
            self.renderer.fill(bar.new_tab, fg, 0.08);
        }
        self.renderer.icon(&mut self.icon_font, ICON_ADD, bar.new_tab, fg, 0.8);
        if self.hover == Some(Hit::NewTabMenu) {
            self.renderer.fill(bar.menu, fg, 0.08);
        }
        self.renderer.icon(&mut self.icon_font, ICON_CHEVRON_DOWN, bar.menu, fg, 0.6);

        let maximized = unsafe { IsZoomed(self.hwnd) }.as_bool();
        let buttons = [
            (Hit::Minimize, bar.minimize, ICON_MINIMIZE),
            (Hit::Maximize, bar.maximize, if maximized { ICON_RESTORE } else { ICON_MAXIMIZE }),
            (Hit::Close, bar.close, ICON_CLOSE),
        ];
        for (hit, rect, icon) in buttons {
            let (hovered, pressed) = (self.hover == Some(hit), self.pressed == Some(hit));
            let mut color = fg;
            if hit == Hit::Close && (hovered || pressed) {
                self.renderer.fill(rect, CLOSE_RED, if pressed { 0.8 } else { 1.0 });
                color = WHITE;
            } else if hovered || pressed {
                self.renderer.fill(rect, fg, if pressed { 0.16 } else { 0.08 });
            }
            self.renderer.icon(&mut self.icon_font, icon, rect, color, if self.focused { 1.0 } else { 0.5 });
        }
    }

    /// A tab's title: its focused pane's, with an executable path shortened to its name.
    fn tab_title(&self, index: usize) -> String {
        let id = self.tabs[index].focused;
        pretty_title(&self.pane_titles[id], &self.pane_names[id])
    }

    /// Where the new-tab menu opens, in screen coordinates: under the chevron.
    fn menu_anchor(&self) -> POINT {
        let (bar, _) = self.title_bar();
        let mut at = POINT { x: bar.menu.x, y: bar.menu.y + bar.menu.height };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut at);
        }
        at
    }

    /// Acts on the new-tab menu's command (see `show_shell_menu`).
    fn shell_chosen(&mut self, command: usize, shells: &[Shell]) {
        match command {
            0 => {}
            MENU_OPEN_CONFIG => open_config(self.hwnd),
            n => {
                if let Err(e) = self.new_tab(shells.get(n - 1).cloned()) {
                    eprintln!("opening a tab failed: {e}");
                }
            }
        }
        self.hover = None;
        self.dirty = true;
    }

    fn update_title(&mut self) {
        let title = match &self.config.title {
            Some(fixed) => fixed.clone(),
            None if self.tabs.is_empty() => "Wisp".into(),
            None => self.tab_title(self.active),
        };
        if title != self.window_title {
            unsafe {
                let _ = SetWindowTextW(self.hwnd, &HSTRING::from(&title));
            }
            self.window_title = title;
        }
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
        self.relayout();
    }

    /// Picks up the client size after a change made while our state was borrowed.
    fn sync_size(&mut self) {
        let mut client = RECT::default();
        if unsafe { GetClientRect(self.hwnd, &mut client) }.is_ok() {
            self.resize(client.right as u32, client.bottom as u32);
        }
    }

    fn dpi_changed(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.ui_font = ui_font(dpi);
        self.icon_font = icon_font(dpi);
        self.rebuild_font();
        // The new size arrives with the WM_SIZE that follows.
    }

    /// Reloads the font at the current size and DPI and refits the panes.
    fn rebuild_font(&mut self) {
        match Font::new(&self.config.font_families(), self.font_size, self.dpi, &self.config.font_features) {
            Ok(font) => self.font = font,
            Err(e) => eprintln!("loading the font failed: {e}"),
        }
        self.renderer.clear_glyphs();
        self.relayout();
    }

    /// Reads the config again and applies everything that can change live.
    /// Running shells keep running; `command` only affects new panes.
    fn reload_config(&mut self) {
        let config = Config::load();
        report(&config);
        let font_changed = config.font_families() != self.config.font_families()
            || config.font_size != self.config.font_size
            || config.font_features != self.config.font_features;
        if config.font_size != self.config.font_size {
            self.font_size = config.font_size;
        }
        for pane in self.panes.iter_mut().flatten() {
            pane.apply_config(&config);
        }
        self.renderer.theme = Theme::from_config(&config);
        apply_window_style(self.hwnd, &config);
        let errors = !config.errors.is_empty();
        self.config = config;
        if font_changed {
            self.rebuild_font()
        } else {
            self.relayout()
        }
        if errors {
            show_config_errors(self.hwnd, &self.config);
        }
    }

    fn output_arrived(&mut self) {
        self.output_pending.store(false, Ordering::Release);
        let visible: Vec<PaneId> = self.visible_panes().iter().map(|&(id, _)| id).collect();
        let focused = self.tabs.get(self.active).map(|t| t.focused);
        for (id, slot) in self.panes.iter_mut().enumerate() {
            let Some(pane) = slot else { continue };
            if pane.pump() && visible.contains(&id) {
                self.dirty = true;
                if Some(id) == focused
                    && let Some((_, echoed @ None)) = &mut self.latency
                {
                    *echoed = Some(Instant::now());
                }
            }
            if let Some(title) = pane.take_title() {
                self.pane_titles[id] = title;
                self.dirty = true; // tab titles are on screen
            }
        }
        self.update_title();
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
        if self.tabs.is_empty() {
            return true;
        }
        let id = self.focused_id();
        if down && !is_modifier(VIRTUAL_KEY(vk)) {
            // Like Ghostty: typing jumps back to the live screen and drops the selection.
            let clear = self.config.selection_clear_on_typing;
            let pane = self.pane_mut(id);
            pane.scroll(ScrollViewport::Bottom);
            if clear {
                pane.clear_selection();
            }
            if self.config.mouse_hide_while_typing && !self.pointer_hidden {
                self.pointer_hidden = true;
                unsafe { SetCursor(None) };
            }
            self.dirty = true;
        }
        if !self.pane(id).win32_input {
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
        if down && trace_latency() && !is_modifier(VIRTUAL_KEY(vk)) {
            self.latency = Some((Instant::now(), None));
        }
        self.pane(id).write(out.as_bytes());
        true
    }

    /// Characters that arrive without a key event of their own: IME results.
    fn char(&mut self, ch: u16) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let pane = self.pane(tab.focused);
        if pane.win32_input {
            let event = KeyEvent { vk: 0, scan: 0, ch, down: true, state: 0, repeat: 1 };
            let out = event.encode() + &KeyEvent { down: false, ..event }.encode();
            pane.write(out.as_bytes());
        } else if let Some(Ok(c)) = char::decode_utf16([ch]).next() {
            // ponytail: drops astral characters in this fallback; win32-input-mode carries them fine.
            pane.write(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }

    /// Runs a bound action. Returns false if it had nothing to act on.
    fn perform(&mut self, action: Action) -> bool {
        if self.tabs.is_empty() {
            return false;
        }
        let id = self.focused_id();
        let page = self.pane(id).terminal.rows().unwrap_or(1) as isize;
        self.dirty = true;
        match action {
            Action::CopyToClipboard => return self.copy_selection(),
            Action::PasteFromClipboard => return self.paste_clipboard(),
            Action::SelectAll => {
                let term = &self.pane(id).terminal;
                if let Ok(Some(all)) = term.select_all() {
                    let _ = term.set_selection(Some(&all));
                }
            }
            Action::ScrollPageUp => self.pane_mut(id).scroll(ScrollViewport::Delta(-page)),
            Action::ScrollPageDown => self.pane_mut(id).scroll(ScrollViewport::Delta(page)),
            Action::ScrollToTop => self.pane_mut(id).scroll(ScrollViewport::Top),
            Action::ScrollToBottom => self.pane_mut(id).scroll(ScrollViewport::Bottom),
            Action::IncreaseFontSize(by) => self.set_font_size(self.font_size + by),
            Action::DecreaseFontSize(by) => self.set_font_size(self.font_size - by),
            Action::ResetFontSize => self.set_font_size(self.config.font_size),
            Action::ReloadConfig => self.reload_config(),
            Action::OpenConfig => open_config(self.hwnd),
            Action::NewWindow => {
                if let Err(e) = Window::open(self.config.clone(), Renderer::new) {
                    eprintln!("opening a window failed: {e}");
                }
            }
            Action::NewTab => {
                if let Err(e) = self.new_tab(None) {
                    eprintln!("opening a tab failed: {e}");
                }
            }
            // Closing the last pane goes through WM_CLOSE so the window isn't
            // destroyed while this call still holds its state.
            Action::CloseSurface => {
                if self.close_pane(id) {
                    self.post_close();
                }
            }
            Action::CloseTab => {
                if self.close_tab(self.active) {
                    self.post_close();
                }
            }
            Action::CloseWindow => self.post_close(),
            Action::Quit => {
                for hwnd in WINDOWS.with_borrow(Vec::clone) {
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            Action::NextTab => self.select_tab((self.active + 1) % self.tabs.len()),
            Action::PreviousTab => self.select_tab((self.active + self.tabs.len() - 1) % self.tabs.len()),
            Action::LastTab => self.select_tab(self.tabs.len() - 1),
            Action::GotoTab(n) => self.select_tab(n.clamp(1, self.tabs.len()) - 1),
            Action::NewSplit(toward) => self.new_split(toward),
            Action::GotoSplit(focus) => {
                let tab = &self.tabs[self.active];
                let order = tab.tree.leaves();
                let i = order.iter().position(|&p| p == tab.focused).unwrap_or(0);
                let next = match focus {
                    Focus::Next => Some(order[(i + 1) % order.len()]),
                    Focus::Previous => Some(order[(i + order.len() - 1) % order.len()]),
                    Focus::Toward(dir) => neighbor(&self.visible_panes(), tab.focused, dir),
                };
                match next {
                    Some(p) => self.focus_pane(p),
                    None => return false,
                }
            }
            Action::ResizeSplit(toward, pixels) => {
                let (area, divider) = (self.terminal_area(), self.divider_px());
                let scaled = (pixels as u32 * self.dpi / 96) as i32;
                let tab = &mut self.tabs[self.active];
                if !tab.tree.resize(tab.focused, toward, scaled, area, divider) {
                    return false;
                }
                self.relayout();
            }
            Action::EqualizeSplits => {
                self.tabs[self.active].tree.equalize();
                self.relayout();
            }
            Action::ToggleSplitZoom => {
                let tab = &mut self.tabs[self.active];
                if tab.tree.leaves().len() < 2 {
                    return false;
                }
                tab.zoomed = !tab.zoomed;
                self.relayout();
            }
            Action::ToggleFullscreen => self.toggle_fullscreen(),
            Action::Ignore => {}
        }
        true
    }

    fn post_close(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    fn select_tab(&mut self, index: usize) {
        if index < self.tabs.len() && index != self.active {
            self.active = index;
            self.update_title();
            self.dirty = true;
        }
    }

    fn focus_pane(&mut self, id: PaneId) {
        let previous = self.focused_id();
        if previous == id {
            return;
        }
        self.pane(previous).focus_changed(false);
        self.tabs[self.active].focused = id;
        self.pane(id).focus_changed(self.focused);
        self.update_title();
        self.dirty = true;
    }

    fn set_font_size(&mut self, size: f32) {
        let size = size.clamp(4.0, 200.0);
        if size != self.font_size {
            self.font_size = size;
            self.rebuild_font();
        }
    }

    /// Fullscreen covers the monitor without borders or a title bar.
    fn toggle_fullscreen(&mut self) {
        let hwnd = self.hwnd;
        unsafe {
            match self.fullscreen.take() {
                Some((placement, style)) => {
                    SetWindowLongPtrW(hwnd, GWL_STYLE, style);
                    let _ = SetWindowPlacement(hwnd, &placement);
                    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED;
                    let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, flags);
                }
                None => {
                    let mut placement =
                        WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                    let _ = GetWindowPlacement(hwnd, &mut placement);
                    let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                    let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info);
                    self.fullscreen = Some((placement, style));
                    SetWindowLongPtrW(
                        hwnd,
                        GWL_STYLE,
                        (style & !(WS_OVERLAPPEDWINDOW.0 as isize)) | WS_POPUP.0 as isize,
                    );
                    let m = info.rcMonitor;
                    let flags = SWP_NOOWNERZORDER | SWP_FRAMECHANGED;
                    let _ =
                        SetWindowPos(hwnd, Some(HWND_TOP), m.left, m.top, m.right - m.left, m.bottom - m.top, flags);
                }
            }
        }
        // WM_SIZE came while we held our state and went unhandled.
        self.sync_size();
    }

    /// Copies the selection, if there is one. Returns whether there was.
    fn copy_selection(&mut self) -> bool {
        let Some(text) = self.pane(self.focused_id()).selection_text().filter(|t| !t.is_empty()) else { return false };
        clipboard::set_text(Some(self.hwnd), &text);
        true
    }

    /// Pastes the clipboard's text. Returns false if there was none.
    fn paste_clipboard(&mut self) -> bool {
        let Some(text) = clipboard::get_text(Some(self.hwnd)).filter(|t| !t.is_empty()) else { return false };
        let id = self.focused_id();
        if self.config.clipboard_paste_protection && self.pane(id).paste_needs_confirmation(&text) {
            self.pending_paste = Some((id, text));
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_APP_CONFIRM_PASTE, WPARAM(0), LPARAM(0));
            }
            return true;
        }
        self.paste_into(id, &text);
        true
    }

    /// Pastes into a pane, unless it closed in the meantime.
    fn paste_into(&mut self, id: PaneId, text: &str) {
        if let Some(pane) = self.panes.get_mut(id).and_then(Option::as_mut) {
            pane.scroll(ScrollViewport::Bottom);
            pane.paste(text);
        }
    }

    /// A pane's rect and the layout its mouse coordinates are relative to.
    fn pane_frame(&self, id: PaneId) -> Option<(Rect, Layout)> {
        let (_, rect) = self.visible_panes().into_iter().find(|&(p, _)| p == id)?;
        let layout = Layout {
            width: rect.width.max(1) as u32,
            height: rect.height.max(1) as u32,
            cell_width: self.font.cell_width,
            cell_height: self.font.cell_height,
            padding: padding_px(&self.config, self.dpi),
        };
        Some((rect, layout))
    }

    fn pane_at(&self, x: f32, y: f32) -> Option<PaneId> {
        self.visible_panes().into_iter().find(|(_, r)| r.contains(x as i32, y as i32)).map(|(id, _)| id)
    }

    /// A divider under the pointer, with a little slack to make it easy to grab.
    fn divider_at(&self, x: f32, y: f32) -> Option<Divider> {
        if self.tabs[self.active].zoomed {
            return None;
        }
        let slack = (3 * self.dpi / 96) as i32;
        self.tabs[self.active].tree.dividers(self.terminal_area(), self.divider_px()).into_iter().find(|d| {
            let r = d.rect;
            Rect { x: r.x - slack, y: r.y - slack, width: r.width + 2 * slack, height: r.height + 2 * slack }
                .contains(x as i32, y as i32)
        })
    }

    fn mouse_button(&mut self, button: Button, down: bool, x: f32, y: f32) {
        self.pointer = (x, y);
        if self.tabs.is_empty() {
            return;
        }
        if (y as i32) < self.title_height() {
            if down {
                self.title_bar_click(button, x, y);
            }
            return;
        }
        if button == Button::Left
            && down
            && let Some(divider) = self.divider_at(x, y)
        {
            self.dragging = Some(divider);
            unsafe { SetCapture(self.hwnd) };
            return;
        }
        if button == Button::Left && !down && self.dragging.take().is_some() {
            let _ = unsafe { ReleaseCapture() };
            return;
        }
        let target = if down { self.pane_at(x, y) } else { self.mouse_pane.or_else(|| self.pane_at(x, y)) };
        let Some(id) = target else { return };
        if down {
            self.focus_pane(id);
            self.mouse_pane = Some(id);
        }
        let Some((rect, layout)) = self.pane_frame(id) else { return };
        let (lx, ly) = (x - rect.x as f32, y - rect.y as f32);
        let mods = current_mods();
        // Right-click copies the selection or pastes, the way conhost and
        // Windows Terminal do, unless `right-click-action` says otherwise.
        if button == Button::Right && down && !Mouse::reports(&self.pane(id).terminal, mods) {
            match self.config.right_click_action {
                RightClick::CopyOrPaste => {
                    if self.copy_selection() {
                        self.pane(id).clear_selection();
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
            self.mouse_pane = None;
            self.dirty = true;
            return;
        }
        let ends_selection = button == Button::Left && !down && self.pane(id).mouse.is_selecting();
        unsafe {
            if down {
                SetCapture(self.hwnd);
            } else {
                let _ = ReleaseCapture();
            }
        }
        let mut out = Vec::new();
        let Pane { mouse, terminal, .. } = self.pane_mut(id);
        let result = if down {
            mouse.press(terminal, button, lx, ly, mods, layout, &mut out)
        } else {
            mouse.release(terminal, button, lx, ly, mods, layout, &mut out)
        };
        if let Err(e) = result {
            eprintln!("mouse button: {e}");
        }
        self.pane(id).write(&out);
        if !down {
            self.mouse_pane = None;
        }
        if ends_selection && self.config.copy_on_select {
            self.copy_selection();
        }
        self.run_autoscroll_timer();
        self.dirty = true;
    }

    fn title_bar_click(&mut self, button: Button, x: f32, y: f32) {
        let (bar, _) = self.title_bar();
        let hit = bar.hit(x as i32, y as i32);
        match (button, hit) {
            (Button::Left, Hit::Tab(i)) => self.select_tab(i),
            (Button::Left, Hit::CloseTab(i)) | (Button::Middle, Hit::Tab(i) | Hit::CloseTab(i)) => {
                if self.close_tab(i) {
                    self.post_close();
                }
            }
            (Button::Left, Hit::NewTab) => {
                if let Err(e) = self.new_tab(None) {
                    eprintln!("opening a tab failed: {e}");
                }
            }
            (Button::Left, Hit::NewTabMenu) => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_APP_SHELL_MENU, WPARAM(0), LPARAM(0));
            },
            _ => {}
        }
        self.dirty = true;
    }

    fn mouse_moved(&mut self, x: f32, y: f32) {
        self.pointer = (x, y);
        if self.pointer_hidden {
            self.pointer_hidden = false;
        }
        if self.tabs.is_empty() {
            return;
        }
        if let Some(divider) = &self.dragging {
            let ratio = match divider.axis {
                Axis::Horizontal => (x - divider.area.x as f32) / divider.area.width.max(1) as f32,
                Axis::Vertical => (y - divider.area.y as f32) / divider.area.height.max(1) as f32,
            };
            let path = divider.path.clone();
            self.tabs[self.active].tree.set_ratio(&path, ratio);
            self.relayout();
            return;
        }
        // Hover over tabs (the client part of the title bar).
        let hover = if (y as i32) < self.title_height() {
            unsafe {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
            }
            Some(self.title_bar().0.hit(x as i32, y as i32))
        } else {
            None
        };
        if hover != self.hover {
            self.hover = hover;
            self.dirty = true;
        }
        let Some(id) = self.mouse_pane.or_else(|| self.pane_at(x, y)) else { return };
        let Some((rect, layout)) = self.pane_frame(id) else { return };
        let mods = current_mods();
        let mut out = Vec::new();
        let Pane { mouse, terminal, .. } = self.pane_mut(id);
        let selecting = mouse.is_selecting();
        if let Err(e) = mouse.moved(terminal, x - rect.x as f32, y - rect.y as f32, mods, layout, &mut out) {
            eprintln!("mouse move: {e}");
        }
        self.pane(id).write(&out);
        if selecting {
            self.run_autoscroll_timer();
            self.dirty = true;
        }
    }

    /// `notches` is in wheel detents; positive turns away from the user (up).
    fn mouse_wheel(&mut self, notches: f32, x: f32, y: f32) {
        let Some(id) = self.pane_at(x, y) else { return };
        let Some((rect, layout)) = self.pane_frame(id) else { return };
        let mods = current_mods();
        let mut out = Vec::new();
        let lines = notches * wheel_scroll_lines();
        let Pane { mouse, terminal, .. } = self.pane_mut(id);
        match mouse.wheel(terminal, lines, x - rect.x as f32, y - rect.y as f32, mods, layout, &mut out) {
            Ok(Wheel::Arrows(presses)) => {
                let vk = if presses < 0 { VK_UP } else { VK_DOWN };
                for _ in 0..presses.unsigned_abs() {
                    self.send_key(id, vk);
                }
            }
            Ok(Wheel::Handled) => {}
            Err(e) => eprintln!("mouse wheel: {e}"),
        }
        self.pane(id).write(&out);
        self.dirty = true;
    }

    /// Sends one press and release of a key that types nothing (arrows and the like).
    fn send_key(&self, id: PaneId, vk: VIRTUAL_KEY) {
        let pane = self.pane(id);
        if !pane.win32_input {
            let seq: &[u8] = if vk == VK_UP { b"\x1b[A" } else { b"\x1b[B" };
            pane.write(seq);
            return;
        }
        use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};
        let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } as u16;
        let event = KeyEvent { vk: vk.0, scan, ch: 0, down: true, state: control_key_state(true), repeat: 1 };
        let out = event.encode() + &KeyEvent { down: false, ..event }.encode();
        pane.write(out.as_bytes());
    }

    /// Runs the autoscroll timer while a selection is dragged past an edge.
    fn run_autoscroll_timer(&mut self) {
        let wanted = self.mouse_pane.is_some_and(|id| {
            let pane = self.pane(id);
            pane.mouse.autoscroll(&pane.terminal) != Autoscroll::None
        });
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
        let Some(id) = self.mouse_pane else { return self.run_autoscroll_timer() };
        let Some((rect, layout)) = self.pane_frame(id) else { return };
        let (x, y) = (self.pointer.0 - rect.x as f32, self.pointer.1 - rect.y as f32);
        let Pane { mouse, terminal, .. } = self.pane_mut(id);
        if let Err(e) = mouse.autoscroll_tick(terminal, x, y, layout) {
            eprintln!("autoscroll: {e}");
        }
        self.run_autoscroll_timer();
        self.dirty = true;
    }

    /// The window-procedure answer to WM_NCHITTEST for a point in client pixels,
    /// once DefWindowProc has called it client area.
    fn hit_test(&self, x: i32, y: i32) -> u32 {
        if self.fullscreen.is_some() {
            return HTCLIENT;
        }
        let maximized = unsafe { IsZoomed(self.hwnd) }.as_bool();
        let border = resize_border(self.dpi);
        if !maximized && y < border {
            // The top edge resizes, like the other edges Windows still owns.
            let width = self.surface.width as i32;
            return if x < border * 2 {
                HTTOPLEFT
            } else if x >= width - border * 2 {
                HTTOPRIGHT
            } else {
                HTTOP
            };
        }
        if y >= self.title_height() {
            return HTCLIENT;
        }
        match self.title_bar().0.hit(x, y) {
            Hit::Caption => HTCAPTION,
            Hit::Minimize => HTMINBUTTON,
            Hit::Maximize => HTMAXBUTTON,
            Hit::Close => HTCLOSE,
            Hit::Tab(_) | Hit::CloseTab(_) | Hit::NewTab | Hit::NewTabMenu => HTCLIENT,
        }
    }

    /// Hover and clicks on the caption buttons arrive as non-client messages.
    fn caption_button(&mut self, msg: u32, hit: u32) -> bool {
        let button = match hit {
            HTMINBUTTON => Some(Hit::Minimize),
            HTMAXBUTTON => Some(Hit::Maximize),
            HTCLOSE => Some(Hit::Close),
            _ => None,
        };
        match msg {
            WM_NCMOUSEMOVE => {
                if button != self.hover {
                    self.hover = button;
                    self.dirty = true;
                }
                unsafe {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                }
                false
            }
            WM_NCLBUTTONDOWN if button.is_some() => {
                self.pressed = button;
                self.dirty = true;
                true // swallowed, or Windows draws its old-style buttons on top
            }
            WM_NCLBUTTONUP if button.is_some() => {
                if self.pressed == button {
                    unsafe {
                        match button {
                            Some(Hit::Minimize) => {
                                let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
                            }
                            Some(Hit::Maximize) => {
                                let maximized = IsZoomed(self.hwnd).as_bool();
                                let _ = ShowWindow(self.hwnd, if maximized { SW_RESTORE } else { SW_MAXIMIZE });
                            }
                            _ => self.post_close(),
                        }
                    }
                }
                self.pressed = None;
                self.dirty = true;
                true
            }
            _ => false,
        }
    }
}

/// Starts `command` for pane `id` of window `hwnd`. Its reader
/// thread wakes the window through `pending`; its exit posts WM_APP_EXITED.
fn start_pane(
    hwnd: HWND,
    pending: &Arc<AtomicBool>,
    id: PaneId,
    command: &str,
    cwd: Option<String>,
    size: SizeReportSize,
    config: &Config,
) -> std::result::Result<Pane, Box<dyn std::error::Error>> {
    let target = hwnd.0 as usize; // HWND isn't Send; its value is just an id
    let pending = pending.clone();
    let wake = move || {
        if !pending.swap(true, Ordering::AcqRel) {
            let _ = unsafe { PostMessageW(Some(HWND(target as _)), WM_APP_OUTPUT, WPARAM(0), LPARAM(0)) };
        }
    };
    let exited = move || {
        let _ = unsafe { PostMessageW(Some(HWND(target as _)), WM_APP_EXITED, WPARAM(id), LPARAM(0)) };
    };
    Pane::spawn(command, cwd.as_deref(), size, config, wake, exited)
}

/// `WISP_TRACE_LATENCY=1` logs, per keypress, how long until the echo arrives
/// and until the frame showing it is presented.
fn trace_latency() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WISP_TRACE_LATENCY").is_some())
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

/// Lists the installed shells, then a Settings entry, and returns the chosen
/// command: a shell's index plus one, `MENU_OPEN_CONFIG`, or 0 if dismissed.
fn show_shell_menu(hwnd: HWND, shells: &[Shell], at: POINT) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        for (i, shell) in shells.iter().enumerate() {
            let _ = AppendMenuW(menu, MF_STRING, i + 1, &HSTRING::from(&shell.name));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN_CONFIG, w!("Settings (config file)\tCtrl+,"));
        let flags = TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN;
        let chosen = TrackPopupMenu(menu, flags, at.x, at.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        chosen.0 as usize
    }
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

fn ui_font(dpi: u32) -> Font {
    let families = ["Segoe UI Variable Text".to_owned(), "Segoe UI".to_owned()];
    Font::new(&families, 9.0, dpi, &[]).expect("Segoe UI ships with Windows")
}

fn icon_font(dpi: u32) -> Font {
    let families = ["Segoe Fluent Icons".to_owned(), "Segoe MDL2 Assets".to_owned()];
    // 10 px glyphs at 100%, the size Windows' own caption buttons use.
    Font::new(&families, 7.5, dpi, &[]).expect("an icon font ships with Windows")
}

/// The thickness of the resize border Windows would draw, for the top edge we draw ourselves.
fn resize_border(dpi: u32) -> i32 {
    unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
}

fn is_dark(c: RgbColor) -> bool {
    (c.r as u32 * 299 + c.g as u32 * 587 + c.b as u32 * 114) / 1000 < 128
}

/// The program's own title, unless it's empty or just the path of the
/// executable (conhost's default), in which case the shell's name reads better.
fn pretty_title(title: &str, shell_name: &str) -> String {
    let is_exe_path = title.to_ascii_lowercase().ends_with(".exe") && (title.contains('\\') || !title.contains(' '));
    if title.is_empty() || is_exe_path { shell_name.to_owned() } else { title.to_owned() }
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
# background-opacity = 0.9
# background-blur = true
# keybind = performable:ctrl+c=copy_to_clipboard
";

/// The window frame in the terminal's colors, and the backdrop that shows
/// through a translucent background: acrylic blur, Mica, or nothing.
fn apply_window_style(hwnd: HWND, config: &Config) {
    let bg = config.background;
    let colorref = bg.r as u32 | (bg.g as u32) << 8 | (bg.b as u32) << 16;
    let backdrop = if config.background_opacity >= 1.0 {
        DWMSBT_NONE
    } else {
        match config.backdrop {
            Backdrop::Mica => DWMSBT_MAINWINDOW,
            Backdrop::Acrylic => DWMSBT_TRANSIENTWINDOW,
            Backdrop::None => DWMSBT_NONE,
            Backdrop::Auto if config.background_blur => DWMSBT_TRANSIENTWINDOW,
            Backdrop::Auto => DWMSBT_NONE,
        }
    };
    unsafe {
        let dark = BOOL(is_dark(bg) as i32);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&raw const dark).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, (&raw const colorref).cast(), 4);
        // The default 1px border is translucent and shows whatever is behind the window.
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, (&raw const colorref).cast(), 4);
        // Windows 10 doesn't know this attribute; the background just stays solid there.
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, (&raw const backdrop).cast(), 4);
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

/// Gives the caption's space to the client area while keeping the side and
/// bottom borders Windows draws and resizes with. Needs no window state, so
/// it works during creation and while the state is borrowed.
unsafe fn calc_client_area(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    if wparam.0 == 0 || style & WS_THICKFRAME.0 as isize == 0 {
        // Fullscreen (no frame) and the size-only form keep the default.
        return unsafe { DefWindowProcW(hwnd, WM_NCCALCSIZE, wparam, lparam) };
    }
    let params = unsafe { &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS) };
    let top = params.rgrc[0].top;
    unsafe { DefWindowProcW(hwnd, WM_NCCALCSIZE, wparam, lparam) };
    params.rgrc[0].top = top;
    // A maximized window hangs over the screen edge by its border; keep the title bar on screen.
    if unsafe { IsZoomed(hwnd) }.as_bool() {
        params.rgrc[0].top += resize_border(unsafe { GetDpiForWindow(hwnd) });
    }
    LRESULT(0)
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let default = || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    if msg == WM_NCCALCSIZE {
        return unsafe { calc_client_area(hwnd, wparam, lparam) };
    }
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
        // Menus and dialogs run with our state released (see WM_APP_SHELL_MENU).
        // The window may close while one is open, so look it up again after.
        WM_APP_CONFIG_ERRORS => {
            let config = window.config.clone();
            drop(window);
            show_config_errors(hwnd, &config);
        }
        WM_APP_SHELL_MENU => {
            let at = window.menu_anchor();
            drop(window);
            let shells = shells::detect();
            let command = show_shell_menu(hwnd, &shells, at);
            if let Some(mut window) = state(hwnd).and_then(|cell| cell.try_borrow_mut().ok()) {
                window.shell_chosen(command, &shells);
            }
        }
        WM_APP_CONFIRM_PASTE => {
            if let Some((id, text)) = window.pending_paste.take() {
                drop(window);
                if confirm_paste(hwnd, &text)
                    && let Some(mut window) = state(hwnd).and_then(|cell| cell.try_borrow_mut().ok())
                {
                    window.paste_into(id, &text);
                }
            }
        }
        WM_APP_EXITED => {
            if window.close_pane(wparam.0) {
                drop(window);
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
        WM_CLOSE => {
            let panes = window.pane_count();
            let confirm = window.config.confirm_close_surface;
            drop(window); // the dialog's message loop needs our state free
            if confirm && panes > 1 {
                let message = format!("Close this window and the {panes} shells running in it?");
                let answer = unsafe {
                    MessageBoxW(Some(hwnd), &HSTRING::from(message), w!("Wisp"), MB_OKCANCEL | MB_ICONQUESTION)
                };
                if answer != IDOK {
                    return LRESULT(0);
                }
            }
            return default();
        }
        WM_NCHITTEST => {
            drop(window);
            let hit = default();
            if hit.0 as u32 != HTCLIENT {
                return hit;
            }
            let mut p = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
            let _ = unsafe { ScreenToClient(hwnd, &mut p) };
            let Ok(window) = cell.try_borrow() else { return hit };
            return LRESULT(window.hit_test(p.x, p.y) as isize);
        }
        WM_NCMOUSEMOVE | WM_NCLBUTTONDOWN | WM_NCLBUTTONUP => {
            if window.caption_button(msg, wparam.0 as u32) {
                return LRESULT(0);
            }
            drop(window);
            return default();
        }
        WM_NCMOUSELEAVE | WM_MOUSELEAVE => {
            window.hover = None;
            window.pressed = None;
            window.dirty = true;
        }
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
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            if !window.key(msg, wparam, lparam) {
                drop(window);
                return default();
            }
        }
        WM_CHAR => window.char(wparam.0 as u16),
        WM_IME_STARTCOMPOSITION => {
            window.place_ime();
            drop(window);
            return default();
        }
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
        WM_CAPTURECHANGED => {
            // Lost the mouse mid-drag (Alt+Tab, say): finish what was in progress.
            window.dragging = None;
            if let Some(id) = window.mouse_pane
                && window.pane(id).mouse.is_selecting()
            {
                let (x, y) = window.pointer;
                window.mouse_button(Button::Left, false, x, y);
            }
        }
        WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
            // Resize arrows over dividers, an arrow when clicks go to the program
            // or over the title bar, an I-beam when they select text, nothing
            // while typing hides it.
            let (x, y) = window.pointer;
            let shape = if (y as i32) < window.title_height() {
                IDC_ARROW
            } else if let Some(divider) = window.divider_at(x, y) {
                if divider.axis == Axis::Horizontal { IDC_SIZEWE } else { IDC_SIZENS }
            } else if window.pane_at(x, y).is_some_and(|id| Mouse::reports(&window.pane(id).terminal, current_mods())) {
                IDC_ARROW
            } else {
                IDC_IBEAM
            };
            unsafe {
                let cursor = if window.pointer_hidden { None } else { LoadCursorW(None, shape).ok() };
                let _ = SetCursor(cursor);
            }
            return LRESULT(1);
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            window.focused = msg == WM_SETFOCUS;
            window.reset_blink();
            if !window.tabs.is_empty() {
                let id = window.focused_id();
                window.pane(id).focus_changed(window.focused);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conhost_default_titles_give_way_to_the_shell_name() {
        let path = "C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
        assert_eq!(pretty_title(path, "Windows PowerShell"), "Windows PowerShell");
        assert_eq!(pretty_title("cmd.exe", "Command Prompt"), "Command Prompt");
        assert_eq!(pretty_title("", "Ubuntu"), "Ubuntu");
        assert_eq!(pretty_title("vim notes.txt", "Ubuntu"), "vim notes.txt");
    }
}
