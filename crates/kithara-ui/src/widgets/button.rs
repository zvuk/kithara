use iced::{
    Alignment, Background, Element, Length, Theme,
    widget::{
        button,
        button::{Status as ButtonStatus, Style as IcedButtonStyle},
        container, row,
    },
};

use crate::{
    module::ButtonStyle,
    render::{ControlAction, Icon, ReadValue, Skin, UiEvent, fonts, shaped_text},
    skin::FontSkin,
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct ControlButton<'a, 'value, 'data, 'skin> {
    path: &'a str,
    label: &'a str,
    icon: Option<Icon>,
    active_label: Option<&'a str>,
    style: ButtonStyle,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for ControlButton<'a, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let active = matches!(self.value, Some(ReadValue::Bool(true)));
        let label = if active {
            self.active_label.unwrap_or(self.label)
        } else {
            self.label
        };
        let highlighted = is_primary(self.style) || active;
        let font = if highlighted {
            self.skin.button.primary_text
        } else {
            self.skin.button.text
        };
        let palette = self.skin.palette;
        let content: Element<'a, UiEvent> = match self.style {
            ButtonStyle::MicroPrimary => {
                let icon = if active { Icon::Pause } else { Icon::Play };
                icon.view(self.skin.button.micro_icon_size, palette.bg)
            }
            ButtonStyle::Transport | ButtonStyle::TransportPrimary => {
                transport_content(self.icon, label, font, highlighted, self.skin)
            }
            ButtonStyle::Default => self.icon.map_or_else(
                || text_content(label, font),
                |icon| {
                    let color = if highlighted {
                        palette.bg
                    } else {
                        palette.text
                    };
                    icon_label(icon, label, font, color, self.skin)
                },
            ),
        };
        let height = if self.style == ButtonStyle::MicroPrimary {
            self.skin.button.micro_size
        } else {
            self.skin.button.height
        };
        let centered = container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        let control = button(centered)
            .height(Length::Fixed(height))
            .padding([self.skin.button.padding_y, self.skin.button.padding_x])
            .style(control_button_style(self.skin, self.style, active))
            .on_press(UiEvent::Control {
                path: self.path.to_owned(),
                action: ControlAction::Activate,
            });
        match self.style {
            ButtonStyle::Transport => control
                .width(Length::FillPortion(self.skin.button.transport_fill))
                .into(),
            ButtonStyle::TransportPrimary => control
                .width(Length::FillPortion(self.skin.button.primary_fill))
                .into(),
            ButtonStyle::MicroPrimary => control
                .width(Length::Fixed(self.skin.button.micro_size))
                .into(),
            ButtonStyle::Default => control.width(Length::Shrink).into(),
        }
    }
}

fn transport_content<'a>(
    icon: Option<Icon>,
    label: &'a str,
    font: FontSkin,
    highlighted: bool,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let icon = icon.or(match label {
        "PREV" => Some(Icon::SkipBack),
        "NEXT" => Some(Icon::SkipForward),
        _ => None,
    });
    icon.map_or_else(
        || text_content(label, font),
        |icon| {
            if matches!(icon, Icon::ZoomIn | Icon::ZoomOut) {
                icon.view(skin.button.zoom_icon_size, skin.palette.text_dim)
            } else {
                let color = if highlighted {
                    skin.palette.bg
                } else {
                    skin.palette.text
                };
                icon_label(icon, label, font, color, skin)
            }
        },
    )
}

fn text_content<'a>(label: &'a str, font: FontSkin) -> Element<'a, UiEvent> {
    shaped_text(label)
        .font(fonts::mono(font.weight))
        .size(font.size)
        .into()
}

fn icon_label<'a>(
    icon: Icon,
    label: &'a str,
    font: FontSkin,
    color: iced::Color,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    row![
        icon.view(skin.button.icon_size, color),
        shaped_text(label)
            .font(fonts::mono(font.weight))
            .size(font.size),
    ]
    .spacing(skin.button.icon_gap)
    .align_y(Alignment::Center)
    .into()
}

fn is_primary(style: ButtonStyle) -> bool {
    matches!(
        style,
        ButtonStyle::TransportPrimary | ButtonStyle::MicroPrimary
    )
}

fn control_button_style(
    skin: &Skin,
    style: ButtonStyle,
    active: bool,
) -> impl Fn(&Theme, ButtonStatus) -> IcedButtonStyle + 'static {
    let palette = skin.palette;
    let highlighted = is_primary(style) || active;
    let mut border = skin.border(if is_primary(style) {
        skin.button.primary_frame
    } else {
        skin.button.frame
    });
    if style == ButtonStyle::Transport {
        border.color = palette.line_inner;
    }
    move |_theme, status| {
        let background = if highlighted {
            match status {
                ButtonStatus::Hovered => palette.accent_strong,
                ButtonStatus::Pressed => palette.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => palette.accent,
            }
        } else {
            match status {
                ButtonStatus::Hovered => palette.bg_panel_2,
                ButtonStatus::Pressed => palette.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_panel,
            }
        };
        IcedButtonStyle {
            background: Some(Background::Color(background)),
            text_color: if highlighted {
                palette.bg
            } else {
                palette.text
            },
            border,
            ..IcedButtonStyle::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, ids::SourceUri};

    #[kithara::test]
    fn active_transport_keeps_line_inner_separator() {
        let origin = SourceUri("button.kskin.ron".to_owned());
        let skin = Skin::resolve(builtin::skin_doc().clone(), &origin).unwrap();
        let style = control_button_style(&skin, ButtonStyle::Transport, true)(
            &Theme::Dark,
            ButtonStatus::Active,
        );

        assert_eq!(
            style.background,
            Some(Background::Color(skin.palette.accent))
        );
        assert_eq!(style.border.color, skin.palette.line_inner);
        assert_eq!(style.border.width, 1.0);
    }
}
