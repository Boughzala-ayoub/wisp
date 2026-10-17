//! Mouse input for one pane.
//!
//! When the program running in the terminal tracks the mouse (vim, htop,
//! Claude Code's TUI), clicks, motion and the wheel are reported to it.
//! Otherwise the mouse selects text, using libghostty's selection gesture
//! state machine: the same click, drag, double-click word and triple-click
//! line behavior as Ghostty. Holding Shift always selects.

use std::time::{Duration, Instant};

use libghostty_vt::key::Mods;
use libghostty_vt::mouse::{self, Action, Encoder, EncoderSize, Position};
use libghostty_vt::selection::gesture::{
    Autoscroll, AutoscrollTickEvent, DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent,
};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport};
use libghostty_vt::{Error, Terminal};

/// How the window's pixels map onto the pane's grid.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding: Padding,
}

/// Space between the window edges and the grid, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Padding {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// What a wheel turn should do, decided by the terminal's state.
#[derive(Debug, PartialEq, Eq)]
pub enum Wheel {
    /// Reported to the program, or scrolled through history; nothing left to do.
    Handled,
    /// The program is on the alternate screen without mouse tracking (a pager,
    /// say), so the wheel becomes this many presses of Up (negative) or Down.
    Arrows(i32),
}

pub struct Mouse {
    gesture: Gesture<'static>,
    press: PressEvent<'static>,
    release: ReleaseEvent<'static>,
    drag: DragEvent<'static>,
    tick: AutoscrollTickEvent<'static>,
    encoder: Encoder<'static>,
    event: mouse::Event<'static>,
    /// Gesture times are measured from here.
    epoch: Instant,
    /// The left button went down for a selection and hasn't come up yet.
    selecting: bool,
    /// Buttons currently down that were reported to the program.
    reported: u8,
    /// Wheel movement too small to scroll a whole line yet, in lines.
    wheel_remainder: f32,
}

impl Mouse {
    pub fn new() -> Result<Mouse, Error> {
        let mut press = PressEvent::new()?;
        // Ghostty's double-click window.
        press.set_repeat_interval(Duration::from_millis(500))?;
        Ok(Mouse {
            gesture: Gesture::new()?,
            press,
            release: ReleaseEvent::new()?,
            drag: DragEvent::new()?,
            tick: AutoscrollTickEvent::new()?,
            encoder: Encoder::new()?,
            event: mouse::Event::new()?,
            epoch: Instant::now(),
            selecting: false,
            reported: 0,
            wheel_remainder: 0.0,
        })
    }

    pub fn is_selecting(&self) -> bool {
        self.selecting
    }

    /// Whether this event goes to the program rather than to selection.
    pub fn reports(term: &Terminal, mods: Mods) -> bool {
        !mods.contains(Mods::SHIFT) && term.is_mouse_tracking().unwrap_or(false)
    }

    /// Handles a button press. Mouse reports for the program go into `out`.
    #[allow(clippy::too_many_arguments, reason = "an event's full context")]
    pub fn press(
        &mut self,
        term: &Terminal,
        button: Button,
        x: f32,
        y: f32,
        mods: Mods,
        layout: Layout,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        if Self::reports(term, mods) {
            self.reported |= bit(button);
            return self.report(term, Action::Press, Some(button), x, y, mods, layout, out);
        }
        if button != Button::Left {
            return Ok(());
        }
        let cell = term.grid_ref(Point::Viewport(viewport_cell(term, x, y, layout)?))?;
        let selection = self
            .press
            .set_repeat_distance(layout.cell_width as f64)?
            .set_time(self.epoch.elapsed())?
            .set_position(x as f64, y as f64)?
            .apply(&mut self.gesture, term, cell)?;
        term.set_selection(selection.as_ref())?;
        self.selecting = true;
        Ok(())
    }

    #[allow(clippy::too_many_arguments, reason = "an event's full context")]
    pub fn release(
        &mut self,
        term: &Terminal,
        button: Button,
        x: f32,
        y: f32,
        mods: Mods,
        layout: Layout,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        if button == Button::Left && self.selecting {
            self.selecting = false;
            let cell = viewport_cell(term, x, y, layout).and_then(|c| term.grid_ref(Point::Viewport(c))).ok();
            return self.release.apply(&mut self.gesture, term, cell);
        }
        // Release whatever was reported as pressed, even if Shift is down now.
        if self.reported & bit(button) != 0 {
            self.reported &= !bit(button);
            return self.report(term, Action::Release, Some(button), x, y, mods, layout, out);
        }
        Ok(())
    }

    pub fn moved(
        &mut self,
        term: &Terminal,
        x: f32,
        y: f32,
        mods: Mods,
        layout: Layout,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        if self.selecting {
            let cell = term.grid_ref(Point::Viewport(viewport_cell(term, x, y, layout)?))?;
            let selection = self
                .drag
                .set_rectangle(mods.contains(Mods::ALT))?
                .set_position(x as f64, y as f64)?
                .apply(&mut self.gesture, term, cell, geometry(term, layout)?)?;
            term.set_selection(selection.as_ref())?;
            return Ok(());
        }
        if Self::reports(term, mods) {
            let held = [Button::Left, Button::Middle, Button::Right].into_iter().find(|&b| self.reported & bit(b) != 0);
            return self.report(term, Action::Motion, held, x, y, mods, layout, out);
        }
        Ok(())
    }

    /// While dragging a selection past the top or bottom edge, which way to scroll.
    pub fn autoscroll(&self, term: &Terminal) -> Autoscroll {
        if !self.selecting {
            return Autoscroll::None;
        }
        self.gesture.autoscroll(term).unwrap_or(Autoscroll::None)
    }

    /// One step of scrolling while a selection is dragged past an edge.
    pub fn autoscroll_tick(&mut self, term: &mut Terminal, x: f32, y: f32, layout: Layout) -> Result<(), Error> {
        let delta = match self.autoscroll(term) {
            Autoscroll::Up => -1,
            Autoscroll::Down => 1,
            _ => return Ok(()),
        };
        term.scroll_viewport(ScrollViewport::Delta(delta));
        let cell = viewport_cell(term, x, y, layout)?;
        let selection = self.tick.set_position(x as f64, y as f64)?.apply(
            &mut self.gesture,
            term,
            cell,
            geometry(term, layout)?,
        )?;
        term.set_selection(selection.as_ref())?;
        Ok(())
    }

    /// Handles wheel movement of `lines` (positive scrolls up, into history).
    #[allow(clippy::too_many_arguments, reason = "an event's full context")]
    pub fn wheel(
        &mut self,
        term: &mut Terminal,
        lines: f32,
        x: f32,
        y: f32,
        mods: Mods,
        layout: Layout,
        out: &mut Vec<u8>,
    ) -> Result<Wheel, Error> {
        let total = self.wheel_remainder + lines;
        let whole = total.trunc() as i32;
        self.wheel_remainder = total - whole as f32;
        if whole == 0 {
            return Ok(Wheel::Handled);
        }
        if Self::reports(term, mods) {
            let button = if whole > 0 { mouse::Button::Four } else { mouse::Button::Five };
            for _ in 0..whole.unsigned_abs() {
                self.event.set_button(Some(button));
                for action in [Action::Press, Action::Release] {
                    self.prepare(term, action, x, y, mods, layout);
                    self.encoder.encode_to_vec(&self.event, out)?;
                }
            }
            return Ok(Wheel::Handled);
        }
        let alternate = term.active_screen().is_ok_and(|s| s == libghostty_vt::screen::Screen::Alternate);
        if alternate && term.mode(Mode::ALT_SCROLL).unwrap_or(false) {
            return Ok(Wheel::Arrows(-whole));
        }
        term.scroll_viewport(ScrollViewport::Delta(-whole as isize));
        Ok(Wheel::Handled)
    }

    #[allow(clippy::too_many_arguments, reason = "an event's full context")]
    fn report(
        &mut self,
        term: &Terminal,
        action: Action,
        button: Option<Button>,
        x: f32,
        y: f32,
        mods: Mods,
        layout: Layout,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        self.event.set_button(button.map(|b| match b {
            Button::Left => mouse::Button::Left,
            Button::Middle => mouse::Button::Middle,
            Button::Right => mouse::Button::Right,
        }));
        self.prepare(term, action, x, y, mods, layout);
        self.encoder.encode_to_vec(&self.event, out)
    }

    fn prepare(&mut self, term: &Terminal, action: Action, x: f32, y: f32, mods: Mods, layout: Layout) {
        self.event.set_action(action).set_mods(mods).set_position(Position { x, y });
        self.encoder
            .set_options_from_terminal(term)
            .set_size(EncoderSize {
                screen_width: layout.width,
                screen_height: layout.height,
                cell_width: layout.cell_width,
                cell_height: layout.cell_height,
                padding_top: layout.padding.top,
                padding_bottom: layout.padding.bottom,
                padding_left: layout.padding.left,
                padding_right: layout.padding.right,
            })
            .set_any_button_pressed(self.reported != 0)
            // Only report motion when the pointer enters a new cell.
            .set_track_last_cell(true);
    }
}

fn bit(button: Button) -> u8 {
    match button {
        Button::Left => 1,
        Button::Middle => 2,
        Button::Right => 4,
    }
}

/// The viewport cell under a window position, clamped to the grid.
fn viewport_cell(term: &Terminal, x: f32, y: f32, layout: Layout) -> Result<PointCoordinate, Error> {
    let cell = |pos: f32, start: u32, size: u32, count: u32| {
        (((pos - start as f32) / size as f32).floor().max(0.0) as u32).min(count.saturating_sub(1))
    };
    Ok(PointCoordinate {
        x: cell(x, layout.padding.left, layout.cell_width, term.cols()? as u32) as u16,
        y: cell(y, layout.padding.top, layout.cell_height, term.rows()? as u32),
    })
}

fn geometry(term: &Terminal, layout: Layout) -> Result<Geometry, Error> {
    Ok(Geometry {
        columns: term.cols()? as u32,
        cell_width: layout.cell_width,
        padding_left: layout.padding.left,
        screen_height: layout.height,
    })
}
