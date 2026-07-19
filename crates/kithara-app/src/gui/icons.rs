use iced::{
    Color, Element, Length,
    widget::svg::{self, Handle as SvgHandle, Svg},
};

/// SVG icon used by the modular UI.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Icon {
    Pause,
    Play,
    Settings,
    Volume,
}

fn icon_bytes(icon: Icon) -> &'static [u8] {
    match icon {
        Icon::Pause => include_bytes!("../../assets/icons/pause.svg"),
        Icon::Play => include_bytes!("../../assets/icons/play.svg"),
        Icon::Settings => include_bytes!("../../assets/icons/gear.svg"),
        Icon::Volume => include_bytes!("../../assets/icons/speaker-high.svg"),
    }
}

impl Icon {
    /// Render this icon as an SVG widget with the given size and color.
    pub(crate) fn view<'a, M: 'a>(self, size: f32, color: Color) -> Element<'a, M> {
        let handle = SvgHandle::from_memory(icon_bytes(self));
        Svg::new(handle)
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(move |_theme, _status| svg::Style { color: Some(color) })
            .into()
    }
}
