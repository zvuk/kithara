use iced::{
    Element, Length,
    mouse::Interaction,
    widget::{Space, mouse_area},
};

use crate::{
    render::{UiEvent, WindowCommand, WindowEdge},
    widgets::Widget,
};

/// One edge or corner of a window that draws its own chrome. It paints
/// nothing; it only turns a press into the drag the host executes.
pub(crate) struct ResizeEdge {
    edge: WindowEdge,
    width: Length,
    height: Length,
}

impl ResizeEdge {
    pub(crate) const fn new(edge: WindowEdge, width: Length, height: Length) -> Self {
        Self {
            edge,
            width,
            height,
        }
    }
}

impl<'a> Widget<'a> for ResizeEdge {
    fn view(self) -> Element<'a, UiEvent> {
        mouse_area(Space::new().width(self.width).height(self.height))
            .interaction(interaction(self.edge))
            .on_press(UiEvent::Window(WindowCommand::Resize(self.edge)))
            .into()
    }
}

const fn interaction(edge: WindowEdge) -> Interaction {
    match edge {
        WindowEdge::North | WindowEdge::South => Interaction::ResizingVertically,
        WindowEdge::East | WindowEdge::West => Interaction::ResizingHorizontally,
        WindowEdge::NorthWest | WindowEdge::SouthEast => Interaction::ResizingDiagonallyDown,
        WindowEdge::NorthEast | WindowEdge::SouthWest => Interaction::ResizingDiagonallyUp,
    }
}
