//! Two children side by side with a draggable vertical divider.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse, overlay};
use iced::{Border, Element, Event, Length, Point, Rectangle, Shadow, Size, Theme, Vector};

/// Width of the divider's hit area in logical pixels.
const HANDLE_WIDTH: f32 = 10.0;

/// Lays out `left` and `right` horizontally, giving `ratio` of the width (minus the divider)
/// to `left`. Dragging the divider publishes `on_drag` with the new, unclamped ratio and
/// `on_release` when the button is let go.
pub struct Split<'a, Message> {
    children: [Element<'a, Message>; 2],
    ratio: f32,
    focused: bool,
    on_drag: Box<dyn Fn(f32) -> Message + 'a>,
    on_release: Option<Message>,
}

impl<'a, Message> Split<'a, Message> {
    pub fn new(
        left: impl Into<Element<'a, Message>>,
        right: impl Into<Element<'a, Message>>,
        ratio: f32,
        on_drag: impl Fn(f32) -> Message + 'a,
    ) -> Self {
        Self {
            children: [left.into(), right.into()],
            ratio,
            focused: false,
            on_drag: Box::new(on_drag),
            on_release: None,
        }
    }

    /// Message published when a drag ends.
    #[must_use]
    pub fn on_release(mut self, message: Message) -> Self {
        self.on_release = Some(message);
        self
    }

    /// Highlights the divider as keyboard-focused.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
}

#[derive(Debug, Default)]
struct State {
    dragging: bool,
}

fn handle_bounds(layout: Layout<'_>) -> Rectangle {
    let bounds = layout.bounds();
    let left = layout.children().next().map_or(bounds, |l| l.bounds());
    Rectangle {
        x: left.x + left.width,
        y: bounds.y,
        width: HANDLE_WIDTH,
        height: bounds.height,
    }
}

fn ratio_at(bounds: Rectangle, x: f32) -> f32 {
    let available = (bounds.width - HANDLE_WIDTH).max(1.0);
    (x - bounds.x - HANDLE_WIDTH / 2.0) / available
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for Split<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        let available = (size.width - HANDLE_WIDTH).max(0.0);
        let left_width = (available * self.ratio.clamp(0.0, 1.0)).round();
        let widths = [left_width, available - left_width];
        let offsets = [0.0, left_width + HANDLE_WIDTH];

        let nodes = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(widths.into_iter().zip(offsets))
            .map(|((child, state), (width, x))| {
                let limits = layout::Limits::new(Size::ZERO, Size::new(width, size.height));
                child
                    .as_widget_mut()
                    .layout(state, renderer, &limits)
                    .move_to(Point::new(x, 0.0))
            })
            .collect();

        layout::Node::with_children(size, nodes)
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
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(handle_bounds(layout)) =>
            {
                state.dragging = true;
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
                shell.publish((self.on_drag)(ratio_at(layout.bounds(), position.x)));
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.dragging => {
                state.dragging = false;
                if let Some(message) = self.on_release.clone() {
                    shell.publish(message);
                }
                shell.capture_event();
                return;
            }
            _ => {}
        }

        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
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
        let state = tree.state.downcast_ref::<State>();
        if state.dragging || cursor.is_over(handle_bounds(layout)) {
            return mouse::Interaction::ResizingHorizontally;
        }
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
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
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }

        let state = tree.state.downcast_ref::<State>();
        let handle = handle_bounds(layout);
        let palette = theme.extended_palette();
        let active = state.dragging || self.focused || cursor.is_over(handle);
        let (width, color) = if active {
            (3.0, palette.primary.base.color)
        } else {
            (1.0, palette.background.strong.color)
        };
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: handle.center_x() - width / 2.0,
                    y: handle.y + 4.0,
                    width,
                    height: (handle.height - 8.0).max(0.0),
                },
                border: Border::default().rounded(width / 2.0),
                shadow: Shadow::default(),
                snap: true,
            },
            color,
        );
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

impl<'a, Message: Clone + 'a> From<Split<'a, Message>> for Element<'a, Message> {
    fn from(split: Split<'a, Message>) -> Self {
        Element::new(split)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_tracks_cursor_inside_bounds() {
        let bounds = Rectangle {
            x: 100.0,
            y: 0.0,
            width: 410.0,
            height: 200.0,
        };
        let middle = ratio_at(bounds, 100.0 + HANDLE_WIDTH / 2.0 + 200.0);
        assert!((middle - 0.5).abs() < 1e-6);
        assert!(ratio_at(bounds, 0.0) < 0.0);
    }
}
