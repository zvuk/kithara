use iced::{
    Alignment, Element, Length,
    font::Weight,
    widget::{
        Svg, row,
        svg::{self, Handle as SvgHandle},
        text,
    },
};

use super::{
    module::Module,
    tokens::{studio_size, studio_space, studio_type},
};
use crate::{
    gui::{fonts, tokens::gap},
    theme::gui::GuiPalette,
};

pub(super) fn view_topbar<M: 'static>(p: GuiPalette) -> Element<'static, M> {
    Module::new()
        .bg(p.bg_panel)
        .pad(studio_space::TOPBAR)
        .wrap(row![brand_mark(p)].align_y(Alignment::Center))
}

fn brand_mark<M: 'static>(p: GuiPalette) -> Element<'static, M> {
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
    ]
    .align_y(Alignment::Center)
    .spacing(gap::CONTENT)
    .into()
}
