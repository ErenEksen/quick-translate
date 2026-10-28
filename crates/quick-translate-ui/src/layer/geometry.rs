//! Panel placement and resize math in logical surface coordinates (design D3).
//!
//! Each overlay covers one output, so its surface-local logical coordinates are the output's
//! logical coordinates and fractional scale needs no special handling.

use iced::mouse::Interaction;
use iced::{Point, Rectangle, Size, Vector};

use crate::shell::{MIN_SIZE, POINTER_OFFSET};

/// Width of the resize grip along the panel's edges, inside the panel (logical px).
pub const GRIP: f32 = 6.0;
/// Extent of the corner grips along each edge (logical px).
pub const CORNER: f32 = 16.0;

/// An edge or corner being dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Edge {
    fn moves_left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    fn moves_right(self) -> bool {
        matches!(self, Self::Right | Self::TopRight | Self::BottomRight)
    }

    fn moves_top(self) -> bool {
        matches!(self, Self::Top | Self::TopLeft | Self::TopRight)
    }

    fn moves_bottom(self) -> bool {
        matches!(self, Self::Bottom | Self::BottomLeft | Self::BottomRight)
    }

    pub fn interaction(self) -> Interaction {
        match self {
            Self::Left | Self::Right => Interaction::ResizingHorizontally,
            Self::Top | Self::Bottom => Interaction::ResizingVertically,
            Self::TopLeft | Self::BottomRight => Interaction::ResizingDiagonallyDown,
            Self::TopRight | Self::BottomLeft => Interaction::ResizingDiagonallyUp,
        }
    }
}

/// The popup size that fits on `surface`: the requested size, at least [`MIN_SIZE`], but never
/// larger than the surface.
pub fn fit(size: Size, surface: Size) -> Size {
    let clamp = |want: f32, min: f32, max: f32| {
        let want = if want.is_finite() { want } else { min };
        want.max(min).min(max.max(0.0))
    };
    Size::new(
        clamp(size.width, MIN_SIZE.width, surface.width),
        clamp(size.height, MIN_SIZE.height, surface.height),
    )
}

/// The panel with its top-left corner [`POINTER_OFFSET`] below and right of the pointer,
/// shifted up and left as needed to stay fully on the surface.
pub fn at_pointer(pointer: Point, size: Size, surface: Size) -> Rectangle {
    let size = fit(size, surface);
    let x = (pointer.x + POINTER_OFFSET)
        .min(surface.width - size.width)
        .max(0.0);
    let y = (pointer.y + POINTER_OFFSET)
        .min(surface.height - size.height)
        .max(0.0);
    Rectangle::new(Point::new(x, y), size)
}

/// The panel centered on the surface.
pub fn centered(size: Size, surface: Size) -> Rectangle {
    let size = fit(size, surface);
    let x = ((surface.width - size.width) / 2.0).max(0.0);
    let y = ((surface.height - size.height) / 2.0).max(0.0);
    Rectangle::new(Point::new(x, y), size)
}

/// The edge or corner grip under `point`, if any. Grips lie just inside the panel border.
pub fn hit_edge(panel: Rectangle, point: Point) -> Option<Edge> {
    if !panel.contains(point) {
        return None;
    }
    let left = point.x - panel.x < GRIP;
    let right = panel.x + panel.width - point.x <= GRIP;
    let top = point.y - panel.y < GRIP;
    let bottom = panel.y + panel.height - point.y <= GRIP;
    let near_left = point.x - panel.x < CORNER;
    let near_right = panel.x + panel.width - point.x <= CORNER;
    let near_top = point.y - panel.y < CORNER;
    let near_bottom = panel.y + panel.height - point.y <= CORNER;
    match () {
        () if (top && near_left) || (left && near_top) => Some(Edge::TopLeft),
        () if (top && near_right) || (right && near_top) => Some(Edge::TopRight),
        () if (bottom && near_left) || (left && near_bottom) => Some(Edge::BottomLeft),
        () if (bottom && near_right) || (right && near_bottom) => Some(Edge::BottomRight),
        () if top => Some(Edge::Top),
        () if bottom => Some(Edge::Bottom),
        () if left => Some(Edge::Left),
        () if right => Some(Edge::Right),
        () => None,
    }
}

/// The panel after dragging `edge` by `delta` from `start`. The opposite edges stay fixed, the
/// size never drops below [`MIN_SIZE`] and the panel never leaves the surface.
pub fn resize(start: Rectangle, edge: Edge, delta: Vector, surface: Size) -> Rectangle {
    let min_w = MIN_SIZE.width.min(surface.width);
    let min_h = MIN_SIZE.height.min(surface.height);
    let (mut left, mut top) = (start.x, start.y);
    let (mut right, mut bottom) = (start.x + start.width, start.y + start.height);
    if edge.moves_left() {
        left = (left + delta.x).clamp(0.0, (right - min_w).max(0.0));
    }
    if edge.moves_right() {
        right = (right + delta.x).clamp(left + min_w, surface.width.max(left + min_w));
    }
    if edge.moves_top() {
        top = (top + delta.y).clamp(0.0, (bottom - min_h).max(0.0));
    }
    if edge.moves_bottom() {
        bottom = (bottom + delta.y).clamp(top + min_h, surface.height.max(top + min_h));
    }
    Rectangle::new(Point::new(left, top), Size::new(right - left, bottom - top))
}

/// Keeps a panel on a surface that changed size (output mode or scale change).
pub fn keep_on(panel: Rectangle, surface: Size) -> Rectangle {
    let size = fit(panel.size(), surface);
    let x = panel.x.min(surface.width - size.width).max(0.0);
    let y = panel.y.min(surface.height - size.height).max(0.0);
    Rectangle::new(Point::new(x, y), size)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FHD: Size = Size::new(1920.0, 1080.0);
    const PANEL: Size = Size::new(720.0, 320.0);

    #[test]
    fn pointer_in_the_middle_puts_the_corner_next_to_it() {
        let rect = at_pointer(Point::new(960.0, 540.0), PANEL, FHD);
        assert_eq!(rect, Rectangle::new(Point::new(972.0, 552.0), PANEL));
    }

    #[test]
    fn pointer_near_bottom_right_shifts_up_and_left() {
        let rect = at_pointer(Point::new(1900.0, 1060.0), PANEL, FHD);
        assert_eq!(rect.position(), Point::new(1200.0, 760.0));
        assert_eq!(rect.x + rect.width, FHD.width);
        assert_eq!(rect.y + rect.height, FHD.height);
    }

    #[test]
    fn pointer_near_right_edge_only_shifts_left() {
        let rect = at_pointer(Point::new(1910.0, 100.0), PANEL, FHD);
        assert_eq!(rect.position(), Point::new(1200.0, 112.0));
    }

    #[test]
    fn fractional_second_output_uses_its_logical_size() {
        // A 2880x1620 output at scale 1.5 is a 1920x1080 logical surface; the pointer
        // position is surface-local.
        let logical = Size::new(2880.0 / 1.5, 1620.0 / 1.5);
        assert_eq!(logical, FHD);
        let rect = at_pointer(Point::new(580.0, 300.0), PANEL, logical);
        assert_eq!(rect.position(), Point::new(592.0, 312.0));
        let corner = at_pointer(Point::new(1919.5, 1079.5), PANEL, logical);
        assert_eq!(corner.position(), Point::new(1200.0, 760.0));
    }

    #[test]
    fn panel_larger_than_output_is_shrunk_and_pinned() {
        let small = Size::new(640.0, 360.0);
        let rect = at_pointer(Point::new(10.0, 10.0), Size::new(900.0, 400.0), small);
        assert_eq!(rect, Rectangle::new(Point::ORIGIN, small));
    }

    #[test]
    fn persisted_size_below_minimum_is_raised() {
        let rect = at_pointer(Point::ORIGIN, Size::new(10.0, 10.0), FHD);
        assert_eq!(rect.size(), MIN_SIZE);
        assert_eq!(
            fit(Size::new(f32::NAN, 300.0), FHD),
            Size::new(MIN_SIZE.width, 300.0)
        );
    }

    #[test]
    fn centered_fallback() {
        let rect = centered(PANEL, FHD);
        assert_eq!(rect.position(), Point::new(600.0, 380.0));
    }

    #[test]
    fn bottom_right_drag_keeps_top_left_fixed() {
        let start = Rectangle::new(Point::new(100.0, 100.0), PANEL);
        let rect = resize(start, Edge::BottomRight, Vector::new(180.0, 80.0), FHD);
        assert_eq!(
            rect,
            Rectangle::new(Point::new(100.0, 100.0), Size::new(900.0, 400.0))
        );
    }

    #[test]
    fn shrinking_stops_at_minimum() {
        let start = Rectangle::new(Point::new(100.0, 100.0), PANEL);
        let rect = resize(start, Edge::Right, Vector::new(-1000.0, 0.0), FHD);
        assert_eq!(rect.width, MIN_SIZE.width);
        assert_eq!(rect.x, 100.0);
        let rect = resize(start, Edge::TopLeft, Vector::new(1000.0, 1000.0), FHD);
        assert_eq!(rect.size(), MIN_SIZE);
        assert_eq!(rect.x + rect.width, 820.0, "right edge stays fixed");
        assert_eq!(rect.y + rect.height, 420.0, "bottom edge stays fixed");
    }

    #[test]
    fn growing_stops_at_the_output_edge() {
        let start = Rectangle::new(Point::new(1000.0, 600.0), PANEL);
        let rect = resize(start, Edge::BottomRight, Vector::new(5000.0, 5000.0), FHD);
        assert_eq!(rect.x + rect.width, FHD.width);
        assert_eq!(rect.y + rect.height, FHD.height);
        let rect = resize(start, Edge::TopLeft, Vector::new(-5000.0, -5000.0), FHD);
        assert_eq!(rect.position(), Point::ORIGIN);
    }

    #[test]
    fn grips() {
        let panel = Rectangle::new(Point::new(100.0, 100.0), PANEL);
        assert_eq!(hit_edge(panel, Point::new(50.0, 50.0)), None);
        assert_eq!(hit_edge(panel, Point::new(400.0, 250.0)), None);
        assert_eq!(
            hit_edge(panel, Point::new(101.0, 101.0)),
            Some(Edge::TopLeft)
        );
        assert_eq!(
            hit_edge(panel, Point::new(819.0, 419.0)),
            Some(Edge::BottomRight)
        );
        assert_eq!(
            hit_edge(panel, Point::new(810.0, 102.0)),
            Some(Edge::TopRight)
        );
        assert_eq!(
            hit_edge(panel, Point::new(102.0, 410.0)),
            Some(Edge::BottomLeft)
        );
        assert_eq!(hit_edge(panel, Point::new(400.0, 102.0)), Some(Edge::Top));
        assert_eq!(
            hit_edge(panel, Point::new(400.0, 418.0)),
            Some(Edge::Bottom)
        );
        assert_eq!(hit_edge(panel, Point::new(102.0, 250.0)), Some(Edge::Left));
        assert_eq!(hit_edge(panel, Point::new(818.0, 250.0)), Some(Edge::Right));
    }

    #[test]
    fn output_shrinking_keeps_the_panel_visible() {
        let panel = Rectangle::new(Point::new(1200.0, 760.0), PANEL);
        let rect = keep_on(panel, Size::new(1280.0, 720.0));
        assert_eq!(rect, Rectangle::new(Point::new(560.0, 400.0), PANEL));
    }
}
