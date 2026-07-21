use iced::{Element, Length, alignment::Vertical, widget::container};

use crate::{
    module::TextStyle,
    render::{ReadValue, Skin, UiEvent, typography::styled_text},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Text<'value, 'data, 'skin> {
    style: TextStyle,
    value: Option<&'value ReadValue<'data>>,
    label: Option<&'data str>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Text<'_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let value = match self.value {
            Some(ReadValue::Text(value)) => Some(*value),
            _ => self.label,
        };
        let Some(value) = value else {
            return iced::widget::Space::new().into();
        };
        let role = match self.style {
            TextStyle::Brand => self.skin.text.brand,
            TextStyle::DeckLetter => self.skin.text.deck_letter,
            TextStyle::TrackTitle => self.skin.text.track_title,
            TextStyle::Body => self.skin.text.body,
            TextStyle::Telemetry => self.skin.text.telemetry,
            TextStyle::MicroLabel => self.skin.text.micro_label,
            TextStyle::Section => self.skin.text.section,
            TextStyle::VisFooter | TextStyle::VisMeta => self.skin.vis.meta,
            TextStyle::VisTitle => self.skin.vis.title,
        };
        let content = if self.style == TextStyle::MicroLabel {
            value.to_uppercase()
        } else {
            value.to_owned()
        };
        let padding_x = match self.style {
            TextStyle::VisFooter => self.skin.vis.footer_padding_x,
            TextStyle::VisMeta => self.skin.vis.index_padding_x,
            TextStyle::VisTitle => self.skin.vis.name_padding_x,
            _ => 0.0,
        };
        container(styled_text(content, role, self.skin))
            .padding([0.0, padding_x])
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(Vertical::Center)
            .into()
    }
}
