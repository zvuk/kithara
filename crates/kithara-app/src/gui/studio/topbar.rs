use iced::{
    Alignment, Border, Element, Length, Theme,
    font::Weight,
    widget::{
        Space, Svg, button, container,
        container::Style as ContainerStyle,
        row,
        svg::{self, Handle as SvgHandle},
        text,
    },
};

use super::{
    styles::{ghost_button_style, linear_background, mix_colors, vertical_divider},
    tokens::{Gap, StudioSize, StudioSpace, StudioType},
};
use crate::{
    gui::{dj::DjMsg, fonts, message::Message},
    theme::gui::GuiPalette,
};

pub(super) fn view_topbar(p: GuiPalette) -> Element<'static, Message> {
    let exit = button(
        text("Back to compact")
            .size(StudioType::BODY_MD)
            .font(fonts::mono(Weight::Medium))
            .color(p.text),
    )
    .padding(StudioSpace::BUTTON)
    .style(ghost_button_style(p))
    .on_press(Message::Dj(DjMsg::Toggle));

    container(
        row![brand_mark(p, "DJ STUDIO"), Space::new().width(Length::Fill), exit]
            .align_y(Alignment::Center)
            .spacing(Gap::SECTION_ROOMY),
    )
    .padding(StudioSpace::TOPBAR)
    .style(topbar_style(p))
    .into()
}

pub(crate) fn brand_mark(p: GuiPalette, sub: &'static str) -> Element<'static, Message> {
    let logo = Svg::new(SvgHandle::from_memory(
        include_bytes!("../../../assets/logo.svg") as &[u8],
    ))
    .width(Length::Fixed(StudioSize::BRAND_LOGO))
    .height(Length::Fixed(StudioSize::BRAND_LOGO))
    .style(move |_theme, _status| svg::Style {
        color: Some(p.accent),
    });

    row![
        logo,
        text("Kithara")
            .size(StudioType::BRAND)
            .font(fonts::display(Weight::Semibold))
            .color(p.text),
        vertical_divider(
            StudioSize::DIVIDER,
            StudioSize::BRAND_DIVIDER_HEIGHT,
            p.line
        ),
        text(sub)
            .size(StudioType::MONO_XS)
            .font(fonts::mono(Weight::Medium))
            .color(p.muted),
    ]
    .align_y(Alignment::Center)
    .spacing(Gap::CONTENT)
    .into()
}

fn topbar_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(linear_background(
                180.0,
                p.bg_panel.scale_alpha(0.7),
                mix_colors(p.bg_panel, p.bg_deep, 0.28).scale_alpha(0.5),
            ))
            .border(Border::default().width(1.0).color(p.line))
    }
}
