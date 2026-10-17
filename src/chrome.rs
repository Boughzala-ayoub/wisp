//! The custom title bar: tabs, a new-tab button and the caption buttons
//! (minimize, maximize, close), in the space Windows would give its own
//! caption, the way Windows Terminal does it. This lays them out and hit-tests
//! them; the window draws them.

use crate::layout::Rect;

/// Title bar sizes, scaled from their 96 DPI values.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub height: i32,
    /// Windows 11's caption buttons are 46 px wide at 100%.
    pub button_width: i32,
    pub tab_min: i32,
    pub tab_max: i32,
    /// Empty space at the top of the bar, above the tabs.
    pub tab_top: i32,
    pub margin: i32,
    pub new_tab_width: i32,
    /// The chevron next to the new-tab button that lists the shells.
    pub menu_width: i32,
    pub close_size: i32,
    /// Caption kept free between the tabs and the buttons, to drag the window by.
    pub drag_space: i32,
}

impl Metrics {
    pub fn new(dpi: u32) -> Metrics {
        let px = |v: f32| (v * dpi as f32 / 96.0).round() as i32;
        Metrics {
            height: px(36.0),
            button_width: px(46.0),
            tab_min: px(64.0),
            tab_max: px(220.0),
            tab_top: px(6.0),
            margin: px(6.0),
            new_tab_width: px(34.0),
            menu_width: px(24.0),
            close_size: px(20.0),
            drag_space: px(48.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Empty title bar: drags the window, double-click maximizes.
    Caption,
    Minimize,
    Maximize,
    Close,
    Tab(usize),
    CloseTab(usize),
    NewTab,
    /// Opens the list of shells to start a tab with.
    NewTabMenu,
}

#[derive(Clone, Debug)]
pub struct Bar {
    pub tabs: Vec<Rect>,
    /// Each tab's close button.
    pub closes: Vec<Rect>,
    pub new_tab: Rect,
    pub menu: Rect,
    pub minimize: Rect,
    pub maximize: Rect,
    pub close: Rect,
}

impl Bar {
    /// Lays out `tabs` tabs in a title bar `width` pixels wide. Tabs share the
    /// space up to their maximum width and shrink to their minimum.
    pub fn new(width: i32, tabs: usize, m: &Metrics) -> Bar {
        let button =
            |i: i32| Rect { x: width - (3 - i) * m.button_width, y: 0, width: m.button_width, height: m.height };
        let room = (width - 3 * m.button_width - m.margin - m.new_tab_width - m.menu_width - m.drag_space).max(0);
        // ponytail: past the minimum width, extra tabs run under the buttons; a scrolling strip if anyone opens that many.
        let tab_width = (room / tabs.max(1) as i32).clamp(m.tab_min, m.tab_max);
        let tab_rects: Vec<Rect> = (0..tabs as i32)
            .map(|i| Rect { x: m.margin + i * tab_width, y: m.tab_top, width: tab_width, height: m.height - m.tab_top })
            .collect();
        let closes = tab_rects
            .iter()
            .map(|t| Rect {
                x: t.x + t.width - m.close_size - m.margin,
                y: t.y + (t.height - m.close_size) / 2,
                width: m.close_size,
                height: m.close_size,
            })
            .collect();
        let after_tabs = m.margin + tabs as i32 * tab_width;
        Bar {
            tabs: tab_rects,
            closes,
            new_tab: Rect { x: after_tabs, y: m.tab_top, width: m.new_tab_width, height: m.height - m.tab_top },
            menu: Rect {
                x: after_tabs + m.new_tab_width,
                y: m.tab_top,
                width: m.menu_width,
                height: m.height - m.tab_top,
            },
            minimize: button(0),
            maximize: button(1),
            close: button(2),
        }
    }

    pub fn hit(&self, x: i32, y: i32) -> Hit {
        if self.close.contains(x, y) {
            return Hit::Close;
        }
        if self.maximize.contains(x, y) {
            return Hit::Maximize;
        }
        if self.minimize.contains(x, y) {
            return Hit::Minimize;
        }
        if let Some(i) = self.closes.iter().position(|r| r.contains(x, y)) {
            return Hit::CloseTab(i);
        }
        if let Some(i) = self.tabs.iter().position(|r| r.contains(x, y)) {
            return Hit::Tab(i);
        }
        if self.new_tab.contains(x, y) {
            return Hit::NewTab;
        }
        if self.menu.contains(x, y) {
            return Hit::NewTabMenu;
        }
        Hit::Caption
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_fill_the_space_then_shrink() {
        let m = Metrics::new(96);
        let one = Bar::new(1000, 1, &m);
        assert_eq!(one.tabs[0].width, m.tab_max);
        let many = Bar::new(1000, 8, &m);
        assert!(many.tabs[0].width < m.tab_max && many.tabs[0].width >= m.tab_min);
        let last = many.tabs.last().unwrap();
        assert!(many.new_tab.x >= last.x + last.width);
        assert_eq!(many.menu.x, many.new_tab.x + many.new_tab.width);
        assert!(many.menu.x + many.menu.width + m.drag_space <= many.minimize.x, "room left to drag the window");
    }

    #[test]
    fn hits_land_on_the_right_parts() {
        let m = Metrics::new(96);
        let bar = Bar::new(1000, 2, &m);
        assert_eq!(bar.hit(999, 5), Hit::Close);
        assert_eq!(bar.hit(1000 - 46 - 10, 5), Hit::Maximize);
        assert_eq!(bar.hit(bar.tabs[1].x + 10, 20), Hit::Tab(1));
        let close = bar.closes[0];
        assert_eq!(bar.hit(close.x + 2, close.y + 2), Hit::CloseTab(0));
        assert_eq!(bar.hit(bar.new_tab.x + 5, 20), Hit::NewTab);
        assert_eq!(bar.hit(bar.menu.x + 5, 20), Hit::NewTabMenu);
        assert_eq!(bar.hit(bar.tabs[0].x + 10, 2), Hit::Caption, "the strip above the tabs drags the window");
    }

    #[test]
    fn sizes_scale_with_dpi() {
        assert_eq!(Metrics::new(144).height, Metrics::new(96).height * 3 / 2);
    }
}
