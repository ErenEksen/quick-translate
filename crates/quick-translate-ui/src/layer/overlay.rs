//! The full-output overlay widget: reports the pointer, hosts the panel at a fixed rectangle,
//! resizes it from its edges and corners, and detects clicks outside it.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, mouse, overlay};
use iced::{Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

use super::geometry::{self, Edge};

/// How the pointer position was learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    /// `wl_pointer.enter` (the toolkit stores its position before dispatching).
    Entered,
    Moved,
    Pressed,
}

pub struct Overlay<'a, Message> {
    panel: Option<(Element<'a, Message>, Rectangle)>,
    on_pointer: Option<Box<dyn Fn(PointerKind, Point) -> Message + 'a>>,
    on_outside_press: Option<Message>,
    on_resize: Option<Box<dyn Fn(Rectangle) -> Message + 'a>>,
    on_resize_end: Option<Box<dyn Fn(Rectangle) -> Message + 'a>>,
}

impl<'a, Message> Overlay<'a, Message> {
    /// An empty overlay (no panel yet).
    pub fn new() -> Self {
        Self {
            panel: None,
            on_pointer: None,
            on_outside_press: None,
            on_resize: None,
            on_resize_end: None,
        }
    }

    /// Hosts `content` at `rect` (surface-local logical px).
    #[must_use]
    pub fn panel(mut self, content: impl Into<Element<'a, Message>>, rect: Rectangle) -> Self {
        self.panel = Some((content.into(), rect));
        self
    }

    /// Reports every pointer enter, motion and press with its position.
    #[must_use]
    pub fn on_pointer(mut self, f: impl Fn(PointerKind, Point) -> Message + 'a) -> Self {
        self.on_pointer = Some(Box::new(f));
        self
    }

    /// Published for a press outside the panel.
    #[must_use]
    pub fn on_outside_press(mut self, message: Message) -> Self {
        self.on_outside_press = Some(message);
        self
    }

    /// Published while an edge or corner is dragged, with the new panel rectangle.
    #[must_use]
    pub fn on_resize(mut self, f: impl Fn(Rectangle) -> Message + 'a) -> Self {
        self.on_resize = Some(Box::new(f));
        self
    }

    /// Published when a resize drag ends, with the final rectangle.
    #[must_use]
    pub fn on_resize_end(mut self, f: impl Fn(Rectangle) -> Message + 'a) -> Self {
        self.on_resize_end = Some(Box::new(f));
        self
    }

    fn rect(&self) -> Option<Rectangle> {
        self.panel.as_ref().map(|(_, rect)| *rect)
    }

    fn children_mut(&mut self) -> &mut [Element<'a, Message>] {
        match &mut self.panel {
            Some((element, _)) => std::slice::from_mut(element),
            None => &mut [],
        }
    }
}

impl<Message> Default for Overlay<'_, Message> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    edge: Edge,
    origin: Point,
    start: Rectangle,
    current: Rectangle,
}

#[derive(Debug, Default)]
struct State {
    drag: Option<Drag>,
}

fn position(cursor: mouse::Cursor) -> Option<Point> {
    match cursor {
        mouse::Cursor::Available(p) | mouse::Cursor::Levitating(p) => Some(p),
        mouse::Cursor::Unavailable => None,
    }
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for Overlay<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.panel
            .iter()
            .map(|(element, _)| Tree::new(element))
            .collect()
    }

    fn diff(&self, tree: &mut Tree) {
        match &self.panel {
            Some((element, _)) => tree.diff_children(std::slice::from_ref(element)),
            None => tree.children.clear(),
        }
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
        let size = limits.max();
        let children = match (&mut self.panel, tree.children.first_mut()) {
            (Some((element, rect)), Some(state)) => {
                let limits = layout::Limits::new(rect.size(), rect.size());
                vec![
                    element
                        .as_widget_mut()
                        .layout(state, renderer, &limits)
                        .move_to(rect.position()),
                ]
            }
            _ => Vec::new(),
        };
        layout::Node::with_children(size, children)
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
            if let (Some((element, _)), Some(state), Some(layout)) = (
                &mut self.panel,
                tree.children.first_mut(),
                layout.children().next(),
            ) {
                element
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
        let point = position(cursor);
        if let (Some(on_pointer), Some(point)) = (&self.on_pointer, point) {
            let kind = match event {
                Event::Mouse(mouse::Event::CursorEntered) => Some(PointerKind::Entered),
                Event::Mouse(mouse::Event::CursorMoved { .. }) => Some(PointerKind::Moved),
                Event::Mouse(mouse::Event::ButtonPressed(_)) => Some(PointerKind::Pressed),
                _ => None,
            };
            if let Some(kind) = kind {
                shell.publish(on_pointer(kind, point));
            }
        }

        let surface = layout.bounds().size();
        let state = tree.state.downcast_mut::<State>();
        if let Some(drag) = &mut state.drag {
            match event {
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    let rect =
                        geometry::resize(drag.start, drag.edge, *position - drag.origin, surface);
                    if rect != drag.current {
                        drag.current = rect;
                        if let Some(on_resize) = &self.on_resize {
                            shell.publish(on_resize(rect));
                        }
                    }
                    shell.capture_event();
                    return;
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                | Event::Mouse(mouse::Event::CursorLeft) => {
                    let rect = drag.current;
                    state.drag = None;
                    if let Some(on_resize_end) = &self.on_resize_end {
                        shell.publish(on_resize_end(rect));
                    }
                    shell.capture_event();
                    return;
                }
                _ => {}
            }
        }

        if let (Some(rect), Some(point), Event::Mouse(mouse::Event::ButtonPressed(button))) =
            (self.rect(), point, event)
        {
            if *button == mouse::Button::Left
                && let Some(edge) = geometry::hit_edge(rect, point)
            {
                tracing::debug!(?edge, "panel resize started");
                state.drag = Some(Drag {
                    edge,
                    origin: point,
                    start: rect,
                    current: rect,
                });
                shell.capture_event();
                return;
            }
            if !rect.contains(point) {
                if let Some(message) = self.on_outside_press.clone() {
                    shell.publish(message);
                }
                shell.capture_event();
                return;
            }
        }

        if let (Some((element, _)), Some(state), Some(layout)) = (
            &mut self.panel,
            tree.children.first_mut(),
            layout.children().next(),
        ) {
            element.as_widget_mut().update(
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
        if let Some(drag) = state.drag {
            return drag.edge.interaction();
        }
        let Some((element, rect)) = &self.panel else {
            return mouse::Interaction::None;
        };
        if let Some(edge) = position(cursor).and_then(|p| geometry::hit_edge(*rect, p)) {
            return edge.interaction();
        }
        match (tree.children.first(), layout.children().next()) {
            (Some(state), Some(layout)) => element
                .as_widget()
                .mouse_interaction(state, layout, cursor, viewport, renderer),
            _ => mouse::Interaction::None,
        }
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
        if let (Some((element, _)), Some(state), Some(layout)) =
            (&self.panel, tree.children.first(), layout.children().next())
        {
            element
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
            self.children_mut(),
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: Clone + 'a> From<Overlay<'a, Message>> for Element<'a, Message> {
    fn from(overlay: Overlay<'a, Message>) -> Self {
        Element::new(overlay)
    }
}
