//! A pane: one libghostty-vt terminal wired to one ConPTY child.
//!
//! The terminal types are `!Send`, so a pane lives on the UI thread. A reader
//! thread blocks on the pty's output pipe and forwards chunks over a channel;
//! the UI thread drains it in [`Pane::pump`] and feeds the parser.

use std::cell::Cell;
use std::fs::File;
use std::io::{Read, Write};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use libghostty_vt::fmt::Format;
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Options, PrimaryDeviceAttributes,
    ScrollViewport, SecondaryDeviceAttributes, SizeReportSize,
};
use libghostty_vt::{Terminal, focus, paste};

use crate::mouse::Mouse;
use crate::pty::Pty;

/// Ghostty's default colors, so Wisp looks the same out of the box.
pub const DEFAULT_BACKGROUND: RgbColor = RgbColor { r: 0x28, g: 0x2c, b: 0x34 };
pub const DEFAULT_FOREGROUND: RgbColor = RgbColor { r: 0xff, g: 0xff, b: 0xff };

const SCROLLBACK_LINES: usize = 10_000;

pub struct Pane {
    pub terminal: Terminal<'static, 'static>,
    pub render_state: RenderState<'static>,
    pub row_iter: RowIterator<'static>,
    pub cell_iter: CellIterator<'static>,
    pub mouse: Mouse,
    pty: Pty,
    output: Receiver<Vec<u8>>,
    /// conhost asked for keys in win32-input-mode (`ESC[?9001h`).
    pub win32_input: bool,
    size: Rc<Cell<SizeReportSize>>,
    title_changed: Rc<Cell<bool>>,
}

impl Pane {
    /// Starts `command_line` in a new pane. `wake` runs on the reader thread
    /// whenever output arrives, `exited` once the child process ends.
    pub fn spawn(
        command_line: &str,
        size: SizeReportSize,
        wake: impl Fn() + Send + 'static,
        exited: impl FnOnce() + Send + 'static,
    ) -> Result<Pane, Box<dyn std::error::Error>> {
        let cwd = std::env::var("USERPROFILE").ok();
        let (mut pty, mut output) = Pty::spawn(command_line, cwd.as_deref(), size.columns, size.rows)?;
        pty.on_exit(exited);

        let (tx, rx) = mpsc::channel();
        thread::Builder::new().name("pty-reader".into()).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n @ 1..) = output.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
                wake();
            }
        })?;

        let mut terminal =
            Terminal::new(Options { cols: size.columns, rows: size.rows, max_scrollback: SCROLLBACK_LINES })?;
        terminal.resize(size.columns, size.rows, size.cell_width, size.cell_height)?;
        terminal
            .set_default_bg_color(Some(DEFAULT_BACKGROUND))?
            .set_default_fg_color(Some(DEFAULT_FOREGROUND))?
            // Ghostty blinks the cursor unless a program asks it not to.
            .set_default_cursor_blink(Some(true))?;

        let size = Rc::new(Cell::new(size));
        let title_changed = Rc::new(Cell::new(false));
        let mut replies: File = pty.writer()?;
        // Replies to queries (cursor position, device attributes, modes). conhost
        // sends some of these at startup and holds output until they're answered.
        terminal
            .on_pty_write(move |_, data| {
                let _ = replies.write_all(data);
            })?
            .on_device_attributes(|_| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[
                            DeviceAttributeFeature::COLUMNS_132,
                            DeviceAttributeFeature::SELECTIVE_ERASE,
                            DeviceAttributeFeature::ANSI_COLOR,
                        ],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: Default::default(),
                })
            })?
            .on_xtversion(|_| Some(concat!("wisp ", env!("CARGO_PKG_VERSION"))))?
            .on_size({
                let size = size.clone();
                move |_| Some(size.get())
            })?
            .on_title_changed({
                let flag = title_changed.clone();
                move |_| flag.set(true)
            })?
            // OSC 52: programs (including ones inside ssh) can set the clipboard,
            // which Ghostty allows by default. Reading it back is never allowed.
            .on_clipboard_write(|_, write| {
                if let Some(text) = write.contents().find(|c| c.mime.starts_with("text/")) {
                    crate::clipboard::set_text(None, text.data);
                }
                Ok(())
            })?;

        Ok(Pane {
            terminal,
            render_state: RenderState::new()?,
            row_iter: RowIterator::new()?,
            cell_iter: CellIterator::new()?,
            mouse: Mouse::new()?,
            pty,
            output: rx,
            win32_input: false,
            size,
            title_changed,
        })
    }

    /// Feeds all output received so far into the terminal. Returns whether
    /// there was any.
    pub fn pump(&mut self) -> bool {
        let mut any = false;
        while let Ok(chunk) = self.output.try_recv() {
            if trace_pty() {
                eprintln!("pty> {}", chunk.escape_ascii());
            }
            // ponytail: misses the mode switch if a read splits the sequence; conhost
            // sends it in its first write, so that hasn't happened yet.
            if contains(&chunk, b"\x1b[?9001h") {
                self.win32_input = true;
            }
            if contains(&chunk, b"\x1b[?9001l") {
                self.win32_input = false;
            }
            self.terminal.vt_write(&chunk);
            any = true;
        }
        any
    }

    /// The new window title, if the program changed it since the last call.
    pub fn take_title(&mut self) -> Option<String> {
        if !self.title_changed.replace(false) {
            return None;
        }
        self.terminal.title().ok().map(str::to_owned)
    }

    pub fn write(&self, data: &[u8]) {
        if trace_pty() && !data.is_empty() {
            eprintln!("pty< {}", data.escape_ascii());
        }
        // A failed write means the child is gone; its exit is reported separately.
        let _ = self.pty.write(data);
    }

    pub fn resize(&mut self, size: SizeReportSize) {
        let old = self.size.replace(size);
        if (old.columns, old.rows, old.cell_width, old.cell_height)
            == (size.columns, size.rows, size.cell_width, size.cell_height)
        {
            return;
        }
        let _ = self.terminal.resize(size.columns, size.rows, size.cell_width, size.cell_height);
        let _ = self.pty.resize(size.columns, size.rows);
    }

    /// The selected text as plain text, the way Ghostty copies it: soft-wrapped
    /// lines rejoined and trailing spaces trimmed.
    pub fn selection_text(&self) -> Option<String> {
        let options = FormatOptions::new().with_emit_format(Format::Plain).with_unwrap(true).with_trim(true);
        let bytes = self.terminal.format_selection_alloc(None, options).ok()??;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn clear_selection(&self) {
        let _ = self.terminal.set_selection(None);
    }

    /// Whether pasting `text` could run commands before the user sees it: it
    /// has a line break and the program isn't using bracketed paste.
    pub fn paste_needs_confirmation(&self, text: &str) -> bool {
        !self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false) && !paste::is_safe(text)
    }

    pub fn paste(&self, text: &str) {
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        self.write(&encode_paste(text, bracketed));
    }

    pub fn scroll(&mut self, scroll: ScrollViewport) {
        self.terminal.scroll_viewport(scroll);
    }

    /// Reports focus changes to the program if it asked for them (mode 1004).
    pub fn focus_changed(&self, focused: bool) {
        if !self.terminal.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
            return;
        }
        let event = if focused { focus::Event::Gained } else { focus::Event::Lost };
        let mut buf = [0u8; 8];
        if let Ok(n) = event.encode(&mut buf) {
            self.write(&buf[..n]);
        }
    }
}

/// Prepares pasted text for the pty: control characters neutralized, line
/// breaks sent as Enter, and bracketed paste markers added when asked for.
fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    // Windows text uses CRLF; one Enter per line, not two.
    let mut data = text.replace("\r\n", "\n").into_bytes();
    // The markers add 12 bytes; nothing else grows.
    let mut out = vec![0u8; data.len() + 12];
    let n = paste::encode(&mut data, bracketed, &mut out).unwrap_or(0);
    out.truncate(n);
    out
}

/// `WISP_TRACE_PTY=1` logs everything that crosses the pty to stderr.
fn trace_pty() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WISP_TRACE_PTY").is_some())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    fn screen_text(pane: &Pane) -> String {
        let mut f = Formatter::new(&pane.terminal, FormatterOptions::new().with_format(Format::Plain)).unwrap();
        // format_len reports an empty screen as InvalidValue rather than 0.
        let Ok(len) = f.format_len() else { return String::new() };
        let mut buf = vec![0; len];
        let n = f.format_buf(&mut buf).unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    #[test]
    fn shell_output_lands_on_the_terminal_screen() {
        let (wake_tx, wake_rx) = channel();
        let size = SizeReportSize { rows: 24, columns: 80, cell_width: 9, cell_height: 19 };
        let mut pane = Pane::spawn(
            "cmd.exe /k echo wisp-%COLORTERM%",
            size,
            move || {
                let _ = wake_tx.send(());
            },
            || {},
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !screen_text(&pane).contains("wisp-truecolor") {
            assert!(Instant::now() < deadline, "timed out; screen: {:?}", screen_text(&pane));
            let _ = wake_rx.recv_timeout(Duration::from_millis(100));
            pane.pump();
        }
        assert!(pane.win32_input, "conhost should request win32-input-mode");

        // Typing goes through the pty and echoes back onto the screen.
        pane.write(b"echo typed-%TERM_PROGRAM%\r");
        while !screen_text(&pane).contains("typed-wisp") {
            assert!(Instant::now() < deadline, "timed out; screen: {:?}", screen_text(&pane));
            let _ = wake_rx.recv_timeout(Duration::from_millis(100));
            pane.pump();
        }

        pane.resize(SizeReportSize { rows: 10, columns: 40, ..size });
        assert_eq!((pane.terminal.cols().unwrap(), pane.terminal.rows().unwrap()), (40, 10));
    }

    #[test]
    fn pastes_send_one_enter_per_line_and_respect_bracketed_mode() {
        assert_eq!(encode_paste("dir\r\nls\n", false), b"dir\rls\r");
        assert_eq!(encode_paste("a\x1bb", false), b"a b", "escape sequences can't be smuggled in");
        assert_eq!(encode_paste("x\ny", true), b"\x1b[200~x\ny\x1b[201~");
    }

    fn pane_with(text: &[u8]) -> Pane {
        let size = SizeReportSize { rows: 5, columns: 20, cell_width: 9, cell_height: 19 };
        let mut pane = Pane::spawn("cmd.exe /c exit", size, || {}, || {}).unwrap();
        pane.terminal.vt_write(b"\x1b[H\x1b[2J");
        pane.terminal.vt_write(text);
        pane
    }

    const LAYOUT: crate::mouse::Layout =
        crate::mouse::Layout { width: 180, height: 95, cell_width: 9, cell_height: 19, padding: 0 };

    #[test]
    fn selection_copies_unwrapped_trimmed_text() {
        use crate::mouse::Button;
        use libghostty_vt::key::Mods;
        // 25 characters wrap onto a second row of a 20-column terminal.
        let mut pane = pane_with(b"abcdefghijklmnopqrstuvwxy\r\nsecond line   ");
        let Pane { mouse, terminal, .. } = &mut pane;
        let mut out = Vec::new();
        // Drag from the first cell to the middle of the third row.
        mouse.press(terminal, Button::Left, 1.0, 5.0, Mods::empty(), LAYOUT, &mut out).unwrap();
        mouse.moved(terminal, 9.0 * 15.5, 19.0 * 2.5, Mods::empty(), LAYOUT, &mut out).unwrap();
        mouse.release(terminal, Button::Left, 9.0 * 15.5, 19.0 * 2.5, Mods::empty(), LAYOUT, &mut out).unwrap();
        assert!(out.is_empty(), "nothing is reported to a program that doesn't track the mouse");
        assert_eq!(pane.selection_text().as_deref(), Some("abcdefghijklmnopqrstuvwxy\nsecond line"));
        pane.clear_selection();
        assert_eq!(pane.selection_text(), None);
    }

    #[test]
    fn double_click_selects_a_word() {
        use crate::mouse::Button;
        use libghostty_vt::key::Mods;
        let mut pane = pane_with(b"echo hello-world done");
        let Pane { mouse, terminal, .. } = &mut pane;
        let mut out = Vec::new();
        let x = 9.0 * 7.5; // inside "hello-world"
        for _ in 0..2 {
            mouse.press(terminal, Button::Left, x, 5.0, Mods::empty(), LAYOUT, &mut out).unwrap();
            mouse.release(terminal, Button::Left, x, 5.0, Mods::empty(), LAYOUT, &mut out).unwrap();
        }
        assert_eq!(pane.selection_text().as_deref(), Some("hello-world"));
    }

    #[test]
    fn programs_that_track_the_mouse_get_reports() {
        use crate::mouse::Button;
        use libghostty_vt::key::Mods;
        // The program asks for button tracking with SGR encoding.
        let mut pane = pane_with(b"\x1b[?1000h\x1b[?1006h");
        let Pane { mouse, terminal, .. } = &mut pane;
        let mut out = Vec::new();
        mouse.press(terminal, Button::Left, 9.0 * 2.5, 19.0 * 1.5, Mods::empty(), LAYOUT, &mut out).unwrap();
        assert_eq!(out, b"\x1b[<0;3;2M", "left press at column 3, row 2");
        out.clear();
        // Shift bypasses tracking and selects instead.
        mouse.press(terminal, Button::Left, 9.0 * 2.5, 19.0 * 1.5, Mods::SHIFT, LAYOUT, &mut out).unwrap();
        assert!(out.is_empty());
        assert!(mouse.is_selecting());
    }
}
