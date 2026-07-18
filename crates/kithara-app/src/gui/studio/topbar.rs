use iced::{
    Alignment, Element, Length,
    font::Weight,
    widget::{
        Space, Svg, button, row,
        svg::{self, Handle as SvgHandle},
        text,
    },
};

use super::{
    module::Module,
    styles::{ghost_button_style, vertical_divider},
    tokens::{studio_size, studio_space, studio_type},
};
use crate::{
    gui::{dj::DjMsg, fonts, message::Message, tokens::gap},
    theme::gui::GuiPalette,
};

pub(super) fn view_topbar(p: GuiPalette) -> Element<'static, Message> {
    let exit = button(
        text("Back to compact")
            .size(studio_type::BODY_MD)
            .font(fonts::mono(Weight::Medium)),
    )
    .padding(studio_space::BUTTON)
    .style(ghost_button_style(p))
    .on_press(Message::Dj(DjMsg::Toggle));

    Module::new().bg(p.bg_panel).pad(studio_space::TOPBAR).wrap(
        row![
            brand_mark(p, "DJ STUDIO"),
            Space::new().width(Length::Fill),
            exit
        ]
        .align_y(Alignment::Center)
        .spacing(gap::SECTION_ROOMY),
    )
}

pub(crate) fn brand_mark(p: GuiPalette, sub: &'static str) -> Element<'static, Message> {
    let logo = Svg::new(SvgHandle::from_memory(
        include_bytes!("../../../assets/logo.svg") as &[u8],
    ))
    .width(Length::Fixed(studio_size::BRAND_LOGO))
    .height(Length::Fixed(studio_size::BRAND_LOGO))
    .style(move |_theme, _status| svg::Style {
        color: Some(p.accent),
    });

    // Spaced letters emulate the design-system 0.3em brand tracking; iced
    // has no native letter-spacing.
    row![
        logo,
        text("K I T H A R A")
            .size(studio_type::BRAND)
            .font(fonts::display(Weight::Bold))
            .color(p.text),
        vertical_divider(
            studio_size::DIVIDER,
            studio_size::BRAND_DIVIDER_HEIGHT,
            p.line
        ),
        text(sub)
            .size(studio_type::MONO_XS)
            .font(fonts::mono(Weight::Medium))
            .color(p.muted),
    ]
    .align_y(Alignment::Center)
    .spacing(gap::CONTENT)
    .into()
}
