use iced::{
    Alignment, Background, Border, Color, Element, Length, Theme,
    widget::{
        button,
        button::{Status as ButtonStatus, Style as IcedButtonStyle},
        container, row,
    },
};

use crate::{
    layout::FrameSides,
    module::ButtonStyle,
    render::{ControlAction, Icon, ReadValue, Skin, UiEvent, fonts, shaped_text},
    skin::FontSkin,
    widgets::{Widget, frame_overlay},
};

#[derive(bon::Builder)]
pub(crate) struct ControlButton<'a, 'value, 'data, 'skin> {
    skin: &'skin Skin,
    label: &'a str,
    path: &'a str,
    style: ButtonStyle,
    active_label: Option<&'a str>,
    frame: Option<FrameSides>,
    icon: Option<Icon>,
    value: Option<&'value ReadValue<'data>>,
}

impl<'a> Widget<'a> for ControlButton<'a, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let active = matches!(self.value, Some(ReadValue::Bool(true)));
        let label = if active {
            self.active_label.unwrap_or(self.label)
        } else {
            self.label
        };
        let highlighted = active;
        let font = if is_primary(self.style) || active {
            self.skin.button.primary_text
        } else if self.style == ButtonStyle::VisNav {
            self.skin.vis.nav_text
        } else {
            self.skin.button.text
        };
        let palette = self.skin.palette;
        let content: Element<'a, UiEvent> = match self.style {
            ButtonStyle::MicroPrimary => {
                let icon = if active { Icon::Pause } else { Icon::Play };
                let color = if active { palette.bg } else { palette.text_dim };
                icon.view(self.skin.button.micro_icon_size, color)
            }
            ButtonStyle::Transport | ButtonStyle::TransportPrimary => {
                transport_content(self.icon, label, font, highlighted, self.skin)
            }
            ButtonStyle::Default | ButtonStyle::VisNav => self.icon.map_or_else(
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
        let centered = container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        let padding = if self.style == ButtonStyle::VisNav {
            [self.skin.vis.nav_padding_y, self.skin.vis.nav_padding_x]
        } else {
            [self.skin.button.padding_y, self.skin.button.padding_x]
        };
        let control = button(centered)
            .height(Length::Fill)
            .padding(padding)
            .style(control_button_style(self.skin, self.style, active))
            .on_press(UiEvent::Control {
                path: self.path.to_owned(),
                action: ControlAction::Activate,
            });
        match self.style {
            ButtonStyle::Transport | ButtonStyle::TransportPrimary => {
                let fill = if is_primary(self.style) {
                    self.skin.button.primary_fill
                } else {
                    self.skin.button.transport_fill
                };
                frame_overlay(
                    control.width(Length::FillPortion(fill)).into(),
                    self.frame.unwrap_or(self.skin.button.transport_sides),
                    (Length::Fill, Length::Fill),
                    self.skin.color(self.skin.divider.color),
                    self.skin.divider.width,
                )
            }
            ButtonStyle::MicroPrimary => control
                .width(Length::Fixed(self.skin.button.micro_size))
                .into(),
            ButtonStyle::VisNav => control
                .width(Length::Fixed(self.skin.vis.nav_cell_size))
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
    let Some(icon) = icon else {
        return text_content(label, font);
    };
    if label.is_empty() {
        let color = if highlighted {
            skin.palette.bg
        } else {
            skin.palette.text_dim
        };
        return icon.view(skin.button.transport_icon_size, color);
    }
    let color = if highlighted {
        skin.palette.bg
    } else {
        skin.palette.text
    };
    icon_label(icon, label, font, color, skin)
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
    color: Color,
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

const fn is_primary(style: ButtonStyle) -> bool {
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
    let highlighted = active;
    let is_transport = matches!(
        style,
        ButtonStyle::Transport | ButtonStyle::TransportPrimary
    );
    let border = if is_transport {
        Border::default()
    } else {
        skin.border(if style == ButtonStyle::VisNav {
            skin.vis.nav_frame
        } else if is_primary(style) {
            skin.button.primary_frame
        } else {
            skin.button.frame
        })
    };
    let vis_background = skin.color(skin.vis.nav_background);
    let vis_text = skin.color(skin.vis.nav_text_color);
    move |_theme, status| {
        let background = if style == ButtonStyle::VisNav {
            match status {
                ButtonStatus::Hovered => palette.bg_select,
                ButtonStatus::Pressed => palette.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => vis_background,
            }
        } else if highlighted {
            match status {
                ButtonStatus::Hovered => palette.accent_strong,
                ButtonStatus::Pressed => palette.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => palette.accent,
            }
        } else if is_transport {
            match status {
                ButtonStatus::Hovered => palette.bg_panel_2,
                ButtonStatus::Pressed => palette.accent_soft,
                ButtonStatus::Active | ButtonStatus::Disabled => Color::TRANSPARENT,
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
            text_color: if style == ButtonStyle::VisNav {
                vis_text
            } else if highlighted {
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

    fn dark_skin() -> Skin {
        let origin = SourceUri("button.kskin.ron".to_owned());
        Skin::resolve(builtin::skin_doc().clone(), builtin::text_doc(), &origin).unwrap()
    }

    #[kithara::test]
    fn transport_cells_fill_with_accent_only_while_active() {
        let skin = dark_skin();
        let style = |button, active| {
            control_button_style(&skin, button, active)(&Theme::Dark, ButtonStatus::Active)
        };

        for button in [ButtonStyle::Transport, ButtonStyle::TransportPrimary] {
            assert_eq!(
                style(button, false).background,
                Some(Background::Color(Color::TRANSPARENT)),
                "{button:?} must let the wave through while it is off",
            );
            assert_eq!(style(button, false).text_color, skin.palette.text);
            assert_eq!(
                style(button, true).background,
                Some(Background::Color(skin.palette.accent))
            );
            assert_eq!(style(button, true).text_color, skin.palette.bg);
        }
    }

    #[kithara::test]
    fn transport_cells_draw_no_border_of_their_own() {
        let skin = dark_skin();
        let border = |button| {
            control_button_style(&skin, button, false)(&Theme::Dark, ButtonStatus::Active).border
        };

        assert_eq!(border(ButtonStyle::Transport).width, 0.0);
        assert_eq!(border(ButtonStyle::TransportPrimary).width, 0.0);
        assert_eq!(
            border(ButtonStyle::Default).width,
            skin.button.frame.border_width
        );
    }
}
