//! Header layout: the language controls on the left, the backend indicator and the buttons on
//! the right edge. When both do not fit on one line (a narrow popup, a wide font, a long model
//! name), the right group moves to a second line instead of being pushed out of the panel.
//!
//! The decision uses the children's measured sizes, so it holds for every font the system
//! resolves; iced's own rows would clip the last controls instead.
//!
//! A status text ("Translating…") goes just left of the right group when there is room and is
//! left out otherwise, so it never moves the other controls while a translation runs.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse, overlay};
use iced::{Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

/// Positions of the groups inside the header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Arrangement {
    pub size: Size,
    pub left: Point,
    pub right: Point,
    /// Where the status text goes; `None` when it does not fit and is left out.
    pub status: Option<Point>,
    /// Both groups share one line.
    pub one_line: bool,
}

/// Places `left` at the start and `right` at the end of `available`, on one line when they fit
/// with `spacing` between them, otherwise `right` right-aligned on a second line. `status` is
/// placed left of `right` when it fits there and never changes where the groups go.
pub(super) fn arrange(
    available: f32,
    left: Size,
    right: Size,
    status: Size,
    spacing: f32,
) -> Arrangement {
    let needed = left.width + spacing + right.width;
    // An unbounded width (never the case inside the popup) behaves like a shrinking row.
    let width = if available.is_finite() {
        available
    } else {
        needed
    };
    let (size, left_at, right_at, line_top, line_height, room, one_line) = if needed <= width {
        let height = left.height.max(right.height);
        let right_at = Point::new(width - right.width, ((height - right.height) / 2.0).round());
        let left_at = Point::new(0.0, ((height - left.height) / 2.0).round());
        let room = right_at.x - left.width - spacing;
        (
            Size::new(width, height),
            left_at,
            right_at,
            0.0,
            height,
            room,
            true,
        )
    } else {
        let top = left.height + spacing;
        let right_at = Point::new((width - right.width).max(0.0), top);
        (
            Size::new(width.max(left.width).max(right.width), top + right.height),
            Point::ORIGIN,
            right_at,
            top,
            right.height,
            right_at.x,
            false,
        )
    };
    let status_at = (status.width > 0.0 && status.width + spacing <= room).then(|| {
        Point::new(
            right_at.x - spacing - status.width,
            line_top + ((line_height - status.height) / 2.0).round(),
        )
    });
    Arrangement {
        size,
        left: left_at,
        right: right_at,
        status: status_at,
        one_line,
    }
}

/// The header row. See the module documentation.
pub struct Header<'a, Message> {
    /// Left group, right group, status text.
    children: [Element<'a, Message>; 3],
    spacing: f32,
}

impl<'a, Message> Header<'a, Message> {
    pub fn new(
        left: impl Into<Element<'a, Message>>,
        right: impl Into<Element<'a, Message>>,
        status: impl Into<Element<'a, Message>>,
        spacing: f32,
    ) -> Self {
        Self {
            children: [left.into(), right.into(), status.into()],
            spacing,
        }
    }
}

/// A left-out status text gets an empty node, which [`Header`] neither draws nor feeds.
fn is_shown(layout: Layout<'_>) -> bool {
    let bounds = layout.bounds();
    bounds.width > 0.0 && bounds.height > 0.0
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Header<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let available = limits.max().width;
        let child_limits = layout::Limits::new(Size::ZERO, Size::new(available, f32::INFINITY));
        let [left, right, status] = &mut self.children;
        let [left_tree, right_tree, status_tree] = &mut tree.children[..] else {
            return layout::Node::new(Size::ZERO);
        };
        let left = left
            .as_widget_mut()
            .layout(left_tree, renderer, &child_limits);
        let right = right
            .as_widget_mut()
            .layout(right_tree, renderer, &child_limits);
        let status = status
            .as_widget_mut()
            .layout(status_tree, renderer, &child_limits);
        let arrangement = arrange(
            available,
            left.size(),
            right.size(),
            status.size(),
            self.spacing,
        );
        let status = match arrangement.status {
            Some(at) => status.move_to(at),
            None => layout::Node::new(Size::ZERO),
        };
        layout::Node::with_children(
            arrangement.size,
            vec![
                left.move_to(arrangement.left),
                right.move_to(arrangement.right),
                status,
            ],
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| is_shown(*layout))
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| is_shown(*layout))
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| is_shown(*layout))
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Header<'a, Message>> for Element<'a, Message> {
    fn from(header: Header<'a, Message>) -> Self {
        Element::new(header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Size = Size::new(420.0, 32.0);
    const RIGHT: Size = Size::new(200.0, 28.0);
    const STATUS: Size = Size::new(70.0, 16.0);
    const NONE: Size = Size::ZERO;

    #[test]
    fn both_groups_share_a_line_when_they_fit() {
        let a = arrange(700.0, LEFT, RIGHT, NONE, 6.0);
        assert!(a.one_line);
        assert_eq!(a.size, Size::new(700.0, 32.0));
        assert_eq!(a.left, Point::new(0.0, 0.0));
        // Right-aligned and vertically centered.
        assert_eq!(a.right, Point::new(500.0, 2.0));
        assert_eq!(a.status, None, "an empty status takes no place");
        // Exactly enough room still fits.
        assert!(arrange(626.0, LEFT, RIGHT, NONE, 6.0).one_line);
    }

    #[test]
    fn right_group_moves_below_when_space_runs_out() {
        let a = arrange(625.0, LEFT, RIGHT, NONE, 6.0);
        assert!(!a.one_line);
        assert_eq!(a.size, Size::new(625.0, 32.0 + 6.0 + 28.0));
        assert_eq!(a.left, Point::ORIGIN);
        assert_eq!(a.right, Point::new(425.0, 38.0));
        // The right group never starts left of the panel.
        let a = arrange(150.0, LEFT, RIGHT, NONE, 6.0);
        assert_eq!(a.right.x, 0.0);
        assert!(a.size.width >= LEFT.width);
    }

    #[test]
    fn status_never_moves_the_groups() {
        for width in [480.0, 600.0, 625.0, 626.0, 690.0, 702.0, 703.0, 900.0] {
            let without = arrange(width, LEFT, RIGHT, NONE, 6.0);
            let with = arrange(width, LEFT, RIGHT, STATUS, 6.0);
            assert_eq!(
                (with.size, with.left, with.right, with.one_line),
                (without.size, without.left, without.right, without.one_line),
                "width {width}"
            );
        }
    }

    #[test]
    fn status_goes_left_of_the_right_group_when_there_is_room() {
        // One line with exactly enough room: 702 - 200 - 420 - 6 = 76 = 70 + 6.
        let a = arrange(702.0, LEFT, RIGHT, STATUS, 6.0);
        assert!(a.one_line);
        assert_eq!(a.status, Some(Point::new(502.0 - 6.0 - 70.0, 8.0)));
        // Not enough room on the shared line: left out instead of overlapping.
        let a = arrange(690.0, LEFT, RIGHT, STATUS, 6.0);
        assert!(a.one_line);
        assert_eq!(a.status, None);
        // On the second line there is room left of the right group.
        let a = arrange(480.0, LEFT, RIGHT, STATUS, 6.0);
        assert!(!a.one_line);
        assert_eq!(a.status, Some(Point::new(280.0 - 6.0 - 70.0, 38.0 + 6.0)));
        // A panel too narrow even for that leaves it out.
        let a = arrange(250.0, LEFT, RIGHT, STATUS, 6.0);
        assert_eq!(a.status, None);
    }

    #[test]
    fn unbounded_width_behaves_like_a_row() {
        let a = arrange(f32::INFINITY, LEFT, RIGHT, NONE, 6.0);
        assert!(a.one_line);
        assert_eq!(a.size.width, 626.0);
        assert_eq!(a.right.x, 426.0);
    }
}
