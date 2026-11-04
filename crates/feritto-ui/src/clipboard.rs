//! Wayland clipboard for feritto's own text fields.
//!
//! The Wayland GUI runs without iced's clipboard (see this crate's Cargo.toml), so text widgets
//! would see an empty, write-only clipboard. `Bridge` wraps a window's content and hands every
//! widget inside it a clipboard backed by the platform service instead. The widgets keep their
//! own behaviour: paste inserts at the cursor or replaces the selection, and copy and cut write
//! the selected text.
use feritto_core::{ClipboardOwner, Offer};
use feritto_platform::Platform;
use iced::{
    Element, Event, Length, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget,
        clipboard::Kind,
        layout, mouse, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
};
use std::sync::Arc;

pub struct Bridge<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    platform: Platform,
}

impl<'a, Message, Theme, Renderer> Bridge<'a, Message, Theme, Renderer> {
    pub fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        platform: Platform,
    ) -> Self {
        Self {
            content: content.into(),
            platform,
        }
    }
}

/// Reads and writes the standard clipboard through the platform service. PRIMARY is not
/// exposed to the GUI, matching a clipboard that offers no primary selection.
struct ServiceClipboard<'a> {
    platform: &'a Platform,
}

impl Clipboard for ServiceClipboard<'_> {
    fn read(&self, kind: Kind) -> Option<String> {
        if kind != Kind::Standard {
            return None;
        }
        self.platform
            .clipboard_text()
            .inspect_err(|error| tracing::debug!(%error, "clipboard read for a text field failed"))
            .ok()
    }

    fn write(&mut self, kind: Kind, contents: String) {
        if kind != Kind::Standard || contents.is_empty() {
            return;
        }
        if let Err(error) = self.platform.publish(Arc::new(Offer::text(contents))) {
            tracing::warn!(%error, "could not copy text from a text field");
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Bridge<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let mut clipboard = ServiceClipboard {
            platform: &self.platform,
        };
        self.content.as_widget_mut().update(
            tree,
            event,
            layout,
            cursor,
            renderer,
            &mut clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    /// feritto's windows have no overlays with text fields; any overlay keeps the runtime's
    /// clipboard.
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

impl<'a, Message, Theme, Renderer> From<Bridge<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(bridge: Bridge<'a, Message, Theme, Renderer>) -> Self {
        Element::new(bridge)
    }
}
