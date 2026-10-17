//! Split layout: a binary tree of panes, as in Ghostty. Pure geometry, so it
//! can be tested without a window.

use crate::bindings::Direction;

pub type PaneId = usize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    fn center(&self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

/// Which way a split divides its space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Side by side, with a vertical divider.
    Horizontal,
    /// Stacked, with a horizontal divider.
    Vertical,
}

impl Axis {
    fn of(direction: Direction) -> Axis {
        match direction {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// The first child's share of the space, between 0 and 1.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// A divider between two children, for drawing and dragging.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    pub rect: Rect,
    pub axis: Axis,
    /// The space the split divides, to turn a drag into a ratio.
    pub area: Rect,
    /// Which split this is: false means "first child", true "second", from the root.
    pub path: Vec<bool>,
}

impl Node {
    /// The panes in reading order: left before right, top before bottom.
    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    /// Splits `target` in two, putting `new` on the `toward` side. Returns
    /// false if `target` isn't in the tree.
    pub fn split(&mut self, target: PaneId, new: PaneId, toward: Direction) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let (first, second) = match toward {
                    Direction::Right | Direction::Down => (target, new),
                    Direction::Left | Direction::Up => (new, target),
                };
                *self = Node::Split {
                    axis: Axis::of(toward),
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(first)),
                    second: Box::new(Node::Leaf(second)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => first.split(target, new, toward) || second.split(target, new, toward),
        }
    }

    /// The tree without `target`, its sibling taking the freed space. `None`
    /// if `target` was the only pane.
    pub fn remove(self, target: PaneId) -> Option<Node> {
        match self {
            Node::Leaf(id) if id == target => None,
            leaf @ Node::Leaf(_) => Some(leaf),
            Node::Split { axis, ratio, first, second } => match (first.remove(target), second.remove(target)) {
                (Some(a), Some(b)) => Some(Node::Split { axis, ratio, first: Box::new(a), second: Box::new(b) }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// Divides `area` among the panes, leaving `divider` pixels between them.
    pub fn layout(&self, area: Rect, divider: i32) -> Vec<(PaneId, Rect)> {
        let mut panes = Vec::new();
        self.walk(area, divider, &mut Vec::new(), &mut panes, &mut Vec::new());
        panes
    }

    pub fn dividers(&self, area: Rect, divider: i32) -> Vec<Divider> {
        let mut dividers = Vec::new();
        self.walk(area, divider, &mut Vec::new(), &mut Vec::new(), &mut dividers);
        dividers
    }

    fn walk(
        &self,
        area: Rect,
        divider: i32,
        path: &mut Vec<bool>,
        panes: &mut Vec<(PaneId, Rect)>,
        dividers: &mut Vec<Divider>,
    ) {
        match self {
            Node::Leaf(id) => panes.push((*id, area)),
            Node::Split { axis, ratio, first, second } => {
                let (a, line, b) = cut(area, *axis, *ratio, divider);
                dividers.push(Divider { rect: line, axis: *axis, area, path: path.clone() });
                path.push(false);
                first.walk(a, divider, path, panes, dividers);
                path.pop();
                path.push(true);
                second.walk(b, divider, path, panes, dividers);
                path.pop();
            }
        }
    }

    /// Sets the ratio of the split at `path`, keeping both sides visible.
    pub fn set_ratio(&mut self, path: &[bool], ratio: f32) {
        match (self, path.split_first()) {
            (Node::Split { ratio: r, .. }, None) => *r = ratio.clamp(0.05, 0.95),
            (Node::Split { first, second, .. }, Some((&go_second, rest))) => {
                if go_second {
                    second.set_ratio(rest, ratio)
                } else {
                    first.set_ratio(rest, ratio)
                }
            }
            _ => {}
        }
    }

    /// Moves the divider nearest `target` along `toward` by `pixels`, the
    /// way Ghostty's `resize_split` does. Returns whether a split moved.
    pub fn resize(&mut self, target: PaneId, toward: Direction, pixels: i32, area: Rect, divider: i32) -> bool {
        let Node::Split { axis, ratio, first, second } = self else { return false };
        let (a, _, b) = cut(area, *axis, *ratio, divider);
        let in_first = first.leaves().contains(&target);
        // A deeper split around the target gets the first chance to move.
        let moved = if in_first {
            first.resize(target, toward, pixels, a, divider)
        } else if second.leaves().contains(&target) {
            second.resize(target, toward, pixels, b, divider)
        } else {
            return false;
        };
        if moved || *axis != Axis::of(toward) {
            return moved;
        }
        let span = match axis {
            Axis::Horizontal => area.width - divider,
            Axis::Vertical => area.height - divider,
        }
        .max(1) as f32;
        let sign = if matches!(toward, Direction::Right | Direction::Down) { 1.0 } else { -1.0 };
        *ratio = (*ratio + sign * pixels as f32 / span).clamp(0.05, 0.95);
        true
    }

    /// Gives every pane an equal share along each axis.
    pub fn equalize(&mut self) {
        if let Node::Split { axis, ratio, first, second } = self {
            let (a, b) = (first.weight(*axis), second.weight(*axis));
            *ratio = a as f32 / (a + b) as f32;
            first.equalize();
            second.equalize();
        }
    }

    /// How many panes sit side by side along `axis`.
    fn weight(&self, along: Axis) -> usize {
        match self {
            Node::Split { axis, first, second, .. } if *axis == along => first.weight(along) + second.weight(along),
            _ => 1,
        }
    }
}

/// Splits `area` into the first child's rect, the divider and the second child's.
fn cut(area: Rect, axis: Axis, ratio: f32, divider: i32) -> (Rect, Rect, Rect) {
    match axis {
        Axis::Horizontal => {
            let first = ((area.width - divider) as f32 * ratio).round() as i32;
            let second = area.width - divider - first;
            (
                Rect { width: first, ..area },
                Rect { x: area.x + first, width: divider, ..area },
                Rect { x: area.x + first + divider, width: second, ..area },
            )
        }
        Axis::Vertical => {
            let first = ((area.height - divider) as f32 * ratio).round() as i32;
            let second = area.height - divider - first;
            (
                Rect { height: first, ..area },
                Rect { y: area.y + first, height: divider, ..area },
                Rect { y: area.y + first + divider, height: second, ..area },
            )
        }
    }
}

/// The pane next to `from` in `toward`: of the panes on that side that line
/// up with it, the nearest one.
pub fn neighbor(panes: &[(PaneId, Rect)], from: PaneId, toward: Direction) -> Option<PaneId> {
    let (_, origin) = panes.iter().find(|(id, _)| *id == from)?;
    let (ox, oy) = origin.center();
    panes
        .iter()
        .filter(|(id, _)| *id != from)
        .filter_map(|(id, r)| {
            let (beyond, overlaps) = match toward {
                Direction::Left => (r.x + r.width <= origin.x, overlap(r.y, r.height, origin.y, origin.height)),
                Direction::Right => (r.x >= origin.x + origin.width, overlap(r.y, r.height, origin.y, origin.height)),
                Direction::Up => (r.y + r.height <= origin.y, overlap(r.x, r.width, origin.x, origin.width)),
                Direction::Down => (r.y >= origin.y + origin.height, overlap(r.x, r.width, origin.x, origin.width)),
            };
            let (cx, cy) = r.center();
            (beyond && overlaps).then_some((*id, (cx - ox).abs() + (cy - oy).abs()))
        })
        .min_by_key(|&(_, distance)| distance)
        .map(|(id, _)| id)
}

fn overlap(a: i32, a_len: i32, b: i32, b_len: i32) -> bool {
    a < b + b_len && b < a + a_len
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0, y: 0, width: 1001, height: 601 };

    fn rect_of(panes: &[(PaneId, Rect)], id: PaneId) -> Rect {
        panes.iter().find(|(p, _)| *p == id).unwrap().1
    }

    #[test]
    fn splits_divide_the_area_with_a_divider_between() {
        let mut tree = Node::Leaf(0);
        assert!(tree.split(0, 1, Direction::Right));
        assert!(tree.split(1, 2, Direction::Down));
        let panes = tree.layout(AREA, 1);
        assert_eq!(tree.leaves(), [0, 1, 2]);
        assert_eq!(rect_of(&panes, 0), Rect { x: 0, y: 0, width: 500, height: 601 });
        assert_eq!(rect_of(&panes, 1), Rect { x: 501, y: 0, width: 500, height: 300 });
        assert_eq!(rect_of(&panes, 2), Rect { x: 501, y: 301, width: 500, height: 300 });
        assert_eq!(tree.dividers(AREA, 1).len(), 2);
    }

    #[test]
    fn splitting_left_or_up_puts_the_new_pane_first() {
        let mut tree = Node::Leaf(0);
        tree.split(0, 1, Direction::Left);
        assert_eq!(tree.leaves(), [1, 0]);
    }

    #[test]
    fn removing_a_pane_gives_its_space_to_the_sibling() {
        let mut tree = Node::Leaf(0);
        tree.split(0, 1, Direction::Right);
        tree.split(1, 2, Direction::Down);
        let tree = tree.remove(1).unwrap();
        assert_eq!(tree.leaves(), [0, 2]);
        assert_eq!(rect_of(&tree.layout(AREA, 1), 2).height, 601, "2 now fills the right half");
        assert_eq!(Node::Leaf(0).remove(0), None);
    }

    #[test]
    fn neighbors_follow_the_geometry() {
        // +---+---+
        // | 0 | 1 |
        // |   +---+
        // |   | 2 |
        // +---+---+
        let mut tree = Node::Leaf(0);
        tree.split(0, 1, Direction::Right);
        tree.split(1, 2, Direction::Down);
        let panes = tree.layout(AREA, 1);
        assert_eq!(neighbor(&panes, 0, Direction::Right), Some(1), "the nearer of 1 and 2");
        assert_eq!(neighbor(&panes, 2, Direction::Up), Some(1));
        assert_eq!(neighbor(&panes, 2, Direction::Left), Some(0));
        assert_eq!(neighbor(&panes, 0, Direction::Left), None);
    }

    #[test]
    fn resize_moves_the_nearest_matching_divider() {
        let mut tree = Node::Leaf(0);
        tree.split(0, 1, Direction::Right);
        tree.split(1, 2, Direction::Down);
        // Pane 2 resized right moves the vertical divider (the root's), not the horizontal one.
        assert!(tree.resize(2, Direction::Right, 100, AREA, 1));
        assert_eq!(rect_of(&tree.layout(AREA, 1), 0).width, 600);
        assert!(tree.resize(2, Direction::Up, 50, AREA, 1));
        assert_eq!(rect_of(&tree.layout(AREA, 1), 1).height, 250);
    }

    #[test]
    fn equalize_gives_three_columns_equal_width() {
        let mut tree = Node::Leaf(0);
        tree.split(0, 1, Direction::Right);
        tree.split(1, 2, Direction::Right);
        tree.equalize();
        let widths: Vec<i32> = tree.layout(Rect { width: 902, ..AREA }, 1).iter().map(|(_, r)| r.width).collect();
        assert_eq!(widths, [300, 300, 300]);
    }

    #[test]
    fn ratios_stay_in_bounds() {
        let mut tree = Node::Leaf(0);
        tree.split(0, 1, Direction::Right);
        tree.set_ratio(&[], 2.0);
        assert!(rect_of(&tree.layout(AREA, 1), 1).width > 0, "the second pane never disappears");
    }
}
