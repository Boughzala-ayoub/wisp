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

use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Options, PrimaryDeviceAttributes,
    SecondaryDeviceAttributes, SizeReportSize,
};
use libghostty_vt::{Terminal, focus};

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
            })?;

        Ok(Pane {
            terminal,
            render_state: RenderState::new()?,
            row_iter: RowIterator::new()?,
            cell_iter: CellIterator::new()?,
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

    /// Reports focus changes to the program if it asked for them (mode 1004).
    pub fn focus_changed(&self, focused: bool) {
        if !self.terminal.mode(libghostty_vt::terminal::Mode::FOCUS_EVENT).unwrap_or(false) {
            return;
        }
        let event = if focused { focus::Event::Gained } else { focus::Event::Lost };
        let mut buf = [0u8; 8];
        if let Ok(n) = event.encode(&mut buf) {
            self.write(&buf[..n]);
        }
    }
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
}
