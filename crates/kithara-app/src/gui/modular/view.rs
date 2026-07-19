use iced::{
    Background, Element, Length,
    widget::{button, container, container::Style as ContainerStyle},
};
use kithara_ui::{
    render::{RenderPalette, UiEvent, fonts, shaped_text, tree},
    widgets::{module_chrome, secondary_button_style},
};

use super::{filter, reads::UiReads};
use crate::gui::{app::Kithara, message::Message};

struct Consts;

impl Consts {
    const BODY_TEXT_SIZE: f32 = 13.0;
    const ERROR_TEXT_SIZE: f32 = 12.0;
    const LOAD_BUTTON_PADDING_X: f32 = 10.0;
    const LOAD_BUTTON_PADDING_Y: f32 = 6.0;
    const LOAD_BUTTON_TEXT_SIZE: f32 = 12.0;
}

pub(crate) fn render(state: &Kithara) -> Element<'_, Message> {
    let palette = state.palette;
    let reads = UiReads::new(
        &state.ui_state,
        &state.library_query,
        &state.modular.preset,
        state.selected_track_index,
    );
    let body = state.modular.compiled.as_ref().map_or_else(
        || empty_state(state),
        |compiled| {
            filter::visible(&compiled.root, &state.modular.hidden, compiled).map_or_else(
                || hidden_state(palette),
                |root| tree::render(&root, compiled, &reads, palette).map(Message::Modular),
            )
        },
    );

    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg)))
        .into()
}

fn empty_state(state: &Kithara) -> Element<'_, Message> {
    let palette = state.palette;
    let content: Element<'_, Message> = state.modular.error.as_ref().map_or_else(
        || {
            button(
                container(
                    shaped_text("Load preset")
                        .font(fonts::SANS)
                        .size(Consts::LOAD_BUTTON_TEXT_SIZE),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill),
            )
            .padding([Consts::LOAD_BUTTON_PADDING_Y, Consts::LOAD_BUTTON_PADDING_X])
            .style(secondary_button_style(palette))
            .on_press(Message::Modular(UiEvent::SelectPreset(
                state.modular.preset.clone(),
            )))
            .into()
        },
        |error| {
            container(
                shaped_text(error.clone())
                    .font(fonts::SANS)
                    .size(Consts::ERROR_TEXT_SIZE)
                    .color(palette.danger),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
        },
    );
    module_chrome(content, palette)
}

fn hidden_state(palette: RenderPalette) -> Element<'static, Message> {
    let content = container(
        shaped_text("All modules are hidden")
            .font(fonts::SANS)
            .size(Consts::BODY_TEXT_SIZE)
            .color(palette.muted),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center_x(Length::Fill)
    .center_y(Length::Fill);
    module_chrome(content, palette)
}
