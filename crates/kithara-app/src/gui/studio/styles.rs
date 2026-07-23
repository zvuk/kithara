use iced::{
    Color, Element, Length, Theme,
    widget::{Space, container, container::Style as ContainerStyle},
};

use crate::theme::gui::GuiPalette;

pub(super) fn vertical_divider<M: 'static>(
    width: f32,
    height: f32,
    color: Color,
) -> Element<'static, M> {
    container(Space::new())
        .width(Length::Fixed(width))
        .height(if height.is_finite() {
            Length::Fixed(height)
        } else {
            Length::Fill
        })
        .style(move |_theme: &Theme| ContainerStyle::default().background(color))
        .into()
}

pub(super) fn horizontal_divider<M: 'static>(height: f32, color: Color) -> Element<'static, M> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .style(move |_theme: &Theme| ContainerStyle::default().background(color))
        .into()
}

pub(super) fn shell_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| ContainerStyle::default().background(p.bg).color(p.text)
}
