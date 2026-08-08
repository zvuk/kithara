use iced::{
    Background, Element, Length, Padding,
    alignment::{Horizontal, Vertical},
    widget::{Space, column, container, container::Style as ContainerStyle, row},
};

use crate::{
    render::{IcedSkin, Icon, InputOwner, ReadValue, Skin, UiEvent, search_input, tree_rows},
    widgets::Widget,
};

#[derive(bon::Builder)]
pub(crate) struct Tree<'path, 'query, 'value, 'data, 'skin> {
    skin: &'skin Skin,
    path: &'path str,
    query: &'query str,
    value: Option<&'value ReadValue<'data>>,
    owner: InputOwner,
}

impl<'a, 'skin: 'a> Widget<'a> for Tree<'_, '_, '_, '_, 'skin> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::Tree(rows)) = self.value else {
            return Space::new().into();
        };
        let tree = tree_rows(self.path, rows, self.skin, self.owner);
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

        column![
            search_bar(self.path, self.query, self.skin, self.owner),
            panel
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

fn search_bar<'a>(
    path: &str,
    query: &str,
    skin: &'a Skin,
    owner: InputOwner,
) -> Element<'a, UiEvent> {
    let icon = container(Icon::Search.view(skin.tree.search_icon_size, skin.palette.muted.into()))
        .width(Length::Fixed(skin.tree.search_icon_width))
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .style({
            let background = skin.color(skin.tree.search_background);
            move |_| ContainerStyle::default().background(Background::Color(background))
        });
    let input = search_input(&format!("{path}/search"), query, skin, owner);

    container(row![icon, input].spacing(1).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fixed(skin.tree.search_height))
        .style({
            let divider = skin.color(skin.tree.search_divider);
            move |_| ContainerStyle::default().background(Background::Color(divider))
        })
        .into()
}
