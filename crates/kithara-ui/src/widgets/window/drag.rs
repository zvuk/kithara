use iced::{
    Element, Length,
    widget::{Space, mouse_area},
};

use crate::{
    render::{UiEvent, WindowCommand},
    widgets::Widget,
};

/// The surface a window without system decorations is moved by. It paints
/// nothing: the row it sits in owns the background.
pub(crate) struct WindowDrag;

impl<'a> Widget<'a> for WindowDrag {
    fn view(self) -> Element<'a, UiEvent> {
        mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_press(UiEvent::Window(WindowCommand::Drag))
            .into()
    }
}
