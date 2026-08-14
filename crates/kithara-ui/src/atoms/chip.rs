use iced::{
    Background, Element, Length, Theme,
    widget::{
        Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
    },
};

use crate::{
    module::ChipStyle,
    render::{
        ControlAction, ReadValue, Skin, UiEvent, fonts, shaped_text, typography::tracked_text,
    },
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Chip<'a, 'value, 'data, 'skin> {
    skin: &'skin Skin,
    label: &'a str,
    path: &'a str,
    style: ChipStyle,
    value: Option<&'value ReadValue<'data>>,
}

impl<'a> Widget<'a> for Chip<'a, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Bool(active)) = self.value else {
            return Space::new().into();
        };
        let content: Element<'a, UiEvent> = match self.style {
            ChipStyle::Deck => {
                let text = self.skin.chip.deck_text;
                shaped_text(self.label)
                    .font(fonts::mono(text.weight))
                    .size(text.size)
                    .into()
            }
            ChipStyle::Routing => {
                let text = self.skin.chip.routing_text;
                shaped_text(self.label)
                    .font(fonts::mono(text.weight))
                    .size(text.size)
                    .into()
            }
            ChipStyle::PivotFamily => tracked_text(self.label, self.skin.chip.pivot_family_text),
            ChipStyle::PivotMultiplier => {
                tracked_text(self.label, self.skin.chip.pivot_multiplier_text)
            }
        };
        let padding = match self.style {
            ChipStyle::Deck | ChipStyle::Routing => {
                [self.skin.chip.padding_y, self.skin.chip.padding_x]
            }
            ChipStyle::PivotFamily => [
                self.skin.chip.pivot_family_padding_y,
                self.skin.chip.pivot_family_padding_x,
            ],
            ChipStyle::PivotMultiplier => [
                self.skin.chip.pivot_multiplier_padding_y,
                self.skin.chip.pivot_multiplier_padding_x,
            ],
        };
        button(content)
            .padding(padding)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(chip_style(self.skin, *active, self.style))
            .on_press(UiEvent::Control {
                path: self.path.to_owned(),
                action: ControlAction::Activate,
            })
            .into()
    }
}

fn chip_style(
    skin: &Skin,
    active: bool,
    style: ChipStyle,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let palette = skin.palette;
    let mut border = match style {
        ChipStyle::Deck | ChipStyle::Routing => skin.border(if active {
            skin.chip.active_frame
        } else {
            skin.chip.inactive_frame
        }),
        ChipStyle::PivotFamily | ChipStyle::PivotMultiplier => skin.border(skin.chip.pivot_frame),
    };
    if active && matches!(style, ChipStyle::PivotFamily | ChipStyle::PivotMultiplier) {
        border.color = palette.accent;
    }
    move |_theme, _status| ButtonStyle {
        background: active.then_some(Background::Color(palette.accent)),
        text_color: if active {
            palette.bg_deep
        } else {
            palette.text_dim
        },
        border,
        ..ButtonStyle::default()
    }
}
