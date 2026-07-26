use iced::{
    Element, Length,
    mouse::Interaction,
    widget::{Space, mouse_area},
};

use crate::{
    render::{UiEvent, WindowCommand, WindowEdge},
    widgets::Widget,
};

pub(crate) struct WindowSurface {
    command: WindowCommand,
    width: Length,
    height: Length,
    interaction: Option<Interaction>,
}

impl WindowSurface {
    pub(crate) const fn drag() -> Self {
        Self {
            command: WindowCommand::Drag,
            width: Length::Fill,
            height: Length::Fill,
            interaction: None,
        }
    }

    pub(crate) const fn resize(edge: WindowEdge, width: Length, height: Length) -> Self {
        Self {
            command: WindowCommand::Resize(edge),
            width,
            height,
            interaction: Some(resize_interaction(edge)),
        }
    }
}

impl<'a> Widget<'a> for WindowSurface {
    fn view(self) -> Element<'a, UiEvent> {
        let area = mouse_area(Space::new().width(self.width).height(self.height))
            .on_press(UiEvent::Window(self.command));
        match self.interaction {
            Some(interaction) => area.interaction(interaction),
            None => area,
        }
        .into()
    }
}

const fn resize_interaction(edge: WindowEdge) -> Interaction {
    match edge {
        WindowEdge::North | WindowEdge::South => Interaction::ResizingVertically,
        WindowEdge::East | WindowEdge::West => Interaction::ResizingHorizontally,
        WindowEdge::NorthWest | WindowEdge::SouthEast => Interaction::ResizingDiagonallyDown,
        WindowEdge::NorthEast | WindowEdge::SouthWest => Interaction::ResizingDiagonallyUp,
    }
}
