use std::fmt;

use iced::{
    Alignment, Background, Border, Element, Length, Padding, Pixels, Shadow, Theme,
    alignment::{Horizontal, Vertical},
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        overlay::menu,
        pick_list, row, scrollable,
        scrollable::{
            Direction as ScrollDirection, Rail, Scrollbar, Scroller, Style as ScrollableStyle,
        },
        text_input,
        text_input::Style as TextInputStyle,
    },
};
use num_traits::ToPrimitive;

use crate::{
    render::{
        ControlAction, Icon, ReadValue, Skin, TreeIcon, TreeRow, UiEvent, fonts, shaped_text,
    },
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Tree<'path, 'query, 'value, 'data, 'skin> {
    path: &'path str,
    query: &'query str,
    value: Option<&'value ReadValue<'data>>,
    icon: fn(TreeIcon) -> Icon,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for Tree<'_, '_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Tree(rows)) = self.value else {
            return Space::new().into();
        };
        let rows = Column::with_children(
            rows.iter()
                .copied()
                .enumerate()
                .map(|(index, row)| tree_row(self.path, index, row, self.icon, self.skin)),
        )
        .width(Length::Fill);
        let scrollbar = Scrollbar::new()
            .width(self.skin.tree.scrollbar_width)
            .margin(self.skin.tree.scrollbar_margin)
            .scroller_width(self.skin.tree.scrollbar_width);
        let tree = scrollable(rows)
            .direction(ScrollDirection::Vertical(scrollbar))
            .width(Length::Fill)
            .height(Length::Fill)
            .style({
                let background = self.skin.color(self.skin.tree.scrollbar_background);
                let scroller = self.skin.color(self.skin.tree.scroller_color);
                move |theme, status| ScrollableStyle {
                    vertical_rail: Rail {
                        background: Some(Background::Color(background)),
                        border: Border::default(),
                        scroller: Scroller {
                            background: Background::Color(scroller),
                            border: Border::default(),
                        },
                    },
                    ..scrollable::default(theme, status)
                }
            });
        let panel = container(tree)
            .padding(Padding {
                top: self.skin.tree.panel_padding_top,
                right: 0.0,
                bottom: self.skin.tree.panel_padding_bottom,
                left: 0.0,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .style({
                let background = self.skin.color(self.skin.tree.panel_background);
                move |_| ContainerStyle::default().background(Background::Color(background))
            });

        column![search_bar(self.query, self.skin), panel]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

#[derive(bon::Builder)]
pub(crate) struct ContextBar<'a, 'scope_value, 'value, 'data, 'skin> {
    path: &'a str,
    scope_items: Vec<&'a str>,
    scope_value: Option<&'scope_value ReadValue<'data>>,
    value: Option<&'value ReadValue<'data>>,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for ContextBar<'a, '_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Text(label)) = self.value else {
            return Space::new().into();
        };
        let content_height = self.skin.tree.context_height - self.skin.tree.context_divider_width;
        let icon = Icon::Zvuk.view(self.skin.tree.context_icon_size, self.skin.palette.text);
        let breadcrumb = shaped_text((*label).to_owned())
            .font(fonts::mono(self.skin.tree.context_text.weight))
            .size(self.skin.tree.context_text.size)
            .color(self.skin.palette.text_dim);
        let row = if self.scope_items.is_empty() {
            Row::new()
                .push(icon)
                .push(breadcrumb)
                .spacing(self.skin.tree.context_gap)
        } else {
            Row::new()
                .push(icon)
                .push(scope_picker(
                    self.path,
                    self.scope_items,
                    self.scope_value,
                    self.skin,
                ))
                .push(
                    shaped_text("\u{203a}")
                        .font(fonts::mono(self.skin.tree.scope_text.weight))
                        .size(self.skin.tree.scope_text.size)
                        .color(self.skin.color(self.skin.tree.scope_chevron_color)),
                )
                .push(breadcrumb)
                .spacing(self.skin.tree.scope_gap)
        }
        .align_y(Alignment::Center);
        let content = container(row)
            .padding([0.0, self.skin.tree.context_padding_x])
            .width(Length::Fill)
            .height(Length::Fixed(content_height))
            .align_y(Vertical::Center)
            .style({
                let background = self.skin.color(self.skin.tree.context_background);
                move |_| ContainerStyle::default().background(Background::Color(background))
            });
        let divider = container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(self.skin.tree.context_divider_width))
            .style({
                let color = self.skin.color(self.skin.tree.context_divider);
                move |_| ContainerStyle::default().background(Background::Color(color))
            });

        column![content, divider]
            .width(Length::Fill)
            .height(Length::Fixed(self.skin.tree.context_height))
            .into()
    }
}

#[derive(Clone, Copy, PartialEq)]
struct ScopeOption<'a> {
    index: usize,
    label: &'a str,
}

impl fmt::Display for ScopeOption<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label)
    }
}

fn scope_picker<'a>(
    path: &'a str,
    items: Vec<&'a str>,
    value: Option<&ReadValue<'_>>,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let options: Vec<_> = items
        .into_iter()
        .enumerate()
        .map(|(index, label)| ScopeOption { index, label })
        .collect();
    let last = options.len().saturating_sub(1);
    let selected = match value {
        Some(ReadValue::Scalar(value)) => {
            let index = value.round().to_usize().map_or(0, |index| index.min(last));
            options.get(index).copied()
        }
        _ => None,
    };
    let padding_y = ((skin.tree.scope_item_height - skin.tree.scope_text.size) / 2.0).max(0.0);
    pick_list(options, selected, move |option| UiEvent::Control {
        path: path.to_owned(),
        action: ControlAction::SelectIndex(option.index),
    })
    .padding(Padding {
        top: padding_y,
        right: skin.tree.scope_padding_x,
        bottom: padding_y,
        left: skin.tree.scope_padding_x,
    })
    .text_size(skin.tree.scope_text.size)
    .text_line_height(Pixels(skin.tree.scope_text.size))
    .font(fonts::mono(skin.tree.scope_text.weight))
    .handle(pick_list::Handle::Arrow {
        size: Some(Pixels(skin.tree.scope_chevron_size)),
    })
    .style({
        let background = skin.color(skin.tree.scope_background);
        let text = skin.color(skin.tree.scope_text_color);
        let chevron = skin.color(skin.tree.scope_chevron_color);
        let border = skin.border(skin.tree.scope_frame);
        move |_theme: &Theme, _status| pick_list::Style {
            text_color: text,
            placeholder_color: text,
            handle_color: chevron,
            background: Background::Color(background),
            border,
        }
    })
    .menu_style({
        let background = skin.color(skin.tree.scope_menu_background);
        let text = skin.color(skin.tree.scope_menu_text);
        let selected_background = skin.color(skin.tree.scope_selected_background);
        let selected_text = skin.color(skin.tree.scope_selected_text);
        let border = skin.border(skin.tree.scope_menu_frame);
        move |_theme: &Theme| menu::Style {
            background: Background::Color(background),
            border,
            text_color: text,
            selected_text_color: selected_text,
            selected_background: Background::Color(selected_background),
            shadow: Shadow::default(),
        }
    })
    .into()
}

fn search_bar(query: &str, skin: &Skin) -> Element<'static, UiEvent> {
    let icon = container(Icon::Search.view(skin.tree.search_icon_size, skin.palette.muted))
        .width(Length::Fixed(skin.tree.search_icon_width))
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .style({
            let background = skin.color(skin.tree.search_background);
            move |_| ContainerStyle::default().background(Background::Color(background))
        });
    let padding_y = ((skin.tree.search_height - skin.tree.search_text.size) / 2.0).max(0.0);
    let input = text_input(&skin.tree.search_placeholder, query)
        .on_input(UiEvent::LibraryQuery)
        .padding(Padding {
            top: padding_y,
            right: skin.tree.search_padding_x,
            bottom: padding_y,
            left: skin.tree.search_padding_x,
        })
        .font(fonts::sans(skin.tree.search_text.weight))
        .size(skin.tree.search_text.size)
        .line_height(Pixels(skin.tree.search_text.size))
        .width(Length::Fill)
        .style({
            let background = skin.color(skin.tree.search_background);
            let palette = skin.palette;
            move |_theme: &Theme, _status| TextInputStyle {
                background: Background::Color(background),
                border: Border::default(),
                icon: palette.muted,
                placeholder: palette.muted,
                value: palette.text,
                selection: palette.accent_soft,
            }
        });

    container(row![icon, input].spacing(1).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fixed(skin.tree.search_height))
        .style({
            let divider = skin.color(skin.tree.search_divider);
            move |_| ContainerStyle::default().background(Background::Color(divider))
        })
        .into()
}

fn tree_row(
    path: &str,
    index: usize,
    row: TreeRow<'_>,
    icon: fn(TreeIcon) -> Icon,
    skin: &Skin,
) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    let color = if row.selected {
        palette.text
    } else if row.muted {
        palette.muted
    } else {
        palette.text_dim
    };
    let marker = container(Space::new())
        .width(Length::Fixed(skin.tree.marker_width))
        .height(Length::Fill)
        .style(move |_| {
            ContainerStyle::default().background(Background::Color(if row.selected {
                palette.accent
            } else {
                iced::Color::TRANSPARENT
            }))
        });
    let chevron = row.expanded.map_or(
        "",
        |expanded| {
            if expanded { "\u{2228}" } else { "\u{203a}" }
        },
    );
    let count = row
        .count
        .map_or_else(String::new, |value| value.to_string());
    let indent = skin
        .tree
        .indent_step
        .mul_add(f32::from(row.depth), skin.tree.indent_base);
    let content = container(
        row![
            container(
                shaped_text(chevron)
                    .font(fonts::MONO)
                    .size(skin.tree.chevron_size)
                    .color(palette.muted),
            )
            .width(Length::Fixed(skin.tree.chevron_width))
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center),
            container(icon(row.icon).view(skin.tree.icon_size, color))
                .width(Length::Fixed(skin.tree.icon_size))
                .height(Length::Fill)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center),
            container(
                shaped_text(row.label.to_owned())
                    .font(fonts::sans(skin.tree.label_text.weight))
                    .size(skin.tree.label_text.size)
                    .color(color),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(Vertical::Center),
            shaped_text(count)
                .font(fonts::mono(skin.tree.count_text.weight))
                .size(skin.tree.count_text.size)
                .color(palette.muted),
        ]
        .spacing(skin.tree.content_gap)
        .align_y(Alignment::Center),
    )
    .padding(Padding {
        top: 0.0,
        right: skin.tree.row_padding_right,
        bottom: 0.0,
        left: indent,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .align_y(Vertical::Center);

    button(row![marker, content].height(Length::Fill))
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fixed(skin.tree.row_height))
        .style(tree_row_style(skin, row.selected))
        .on_press(UiEvent::Control {
            path: path.to_owned(),
            action: ControlAction::SelectIndex(index),
        })
        .into()
}

fn tree_row_style(
    skin: &Skin,
    selected: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let palette = skin.palette;
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Pressed => Some(Background::Color(palette.accent_soft)),
            _ if selected => Some(Background::Color(palette.bg_select)),
            ButtonStatus::Hovered => Some(Background::Color(palette.bg_panel_2)),
            ButtonStatus::Active | ButtonStatus::Disabled => None,
        };
        ButtonStyle {
            background,
            text_color: palette.text,
            border: Border::default(),
            ..ButtonStyle::default()
        }
    }
}
