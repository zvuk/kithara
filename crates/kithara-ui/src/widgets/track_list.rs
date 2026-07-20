use iced::{
    Alignment, Background, Element, Length, Theme,
    alignment::{Horizontal, Vertical},
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, scrollable,
    },
};

use super::Widget;
use crate::{
    module::TrackColumn,
    render::{ControlAction, ReadValue, Reads, Skin, TrackRow, UiEvent, fonts, shaped_text},
};

const fn em_dash() -> &'static str {
    "\u{2014}"
}

#[derive(bon::Builder)]
pub(crate) struct TrackList<'path, 'columns, 'state, 'value, 'data, 'reads, 'skin> {
    path: &'path str,
    columns: &'columns [TrackColumn],
    columns_state: Option<&'state str>,
    value: Option<&'value ReadValue<'data>>,
    reads: &'reads dyn Reads,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for TrackList<'_, '_, '_, '_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::TrackList(tracks)) = self.value else {
            return Space::new().into();
        };
        let columns: Vec<_> = self
            .columns
            .iter()
            .copied()
            .filter(|column| column_visible(self.reads, self.columns_state, *column))
            .collect();
        let header = Row::with_children(
            columns
                .iter()
                .copied()
                .map(|column| header_cell(column, self.skin)),
        )
        .align_y(Alignment::Center)
        .height(Length::Fixed(self.skin.track_list.header_height))
        .width(Length::Fill);
        let header_background = self.skin.palette.bg_panel;
        let header = container(header)
            .height(Length::Fixed(self.skin.track_list.header_height))
            .width(Length::Fill)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(header_background))
            });
        let rows = tracks.iter().enumerate().map(|(index, track)| {
            TrackListRow::builder()
                .skin(self.skin)
                .path(self.path)
                .index(index)
                .track(*track)
                .columns(&columns)
                .build()
                .view()
        });
        let line_soft = self.skin.palette.line_soft;
        let rows = container(
            Column::with_children(rows)
                .spacing(self.skin.track_list.grid_gap)
                .width(Length::Fill),
        )
        .style(move |_| ContainerStyle::default().background(Background::Color(line_soft)));
        let table = scrollable(rows).width(Length::Fill).height(Length::Fill);
        let footer = footer(tracks.len(), self.skin);

        let line_soft = self.skin.palette.line_soft;
        container(
            column![header, table, footer]
                .spacing(self.skin.track_list.grid_gap)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(line_soft)))
        .into()
    }
}

#[derive(bon::Builder)]
struct TrackListRow<'path, 'columns, 'data, 'skin> {
    skin: &'skin Skin,
    path: &'path str,
    index: usize,
    track: TrackRow<'data>,
    columns: &'columns [TrackColumn],
}

impl<'a> Widget<'a> for TrackListRow<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let cells = self
            .columns
            .iter()
            .copied()
            .map(|column| row_cell(column, self.index, self.track, self.skin));
        button(
            Row::with_children(cells)
                .align_y(Alignment::Center)
                .height(Length::Fill)
                .width(Length::Fill),
        )
        .padding(0)
        .height(Length::Fixed(self.skin.track_list.row_height))
        .width(Length::Fill)
        .style(track_button_style(self.skin, self.track.selected))
        .on_press(UiEvent::Control {
            path: self.path.to_owned(),
            action: ControlAction::SelectIndex(self.index),
        })
        .into()
    }
}

fn header_cell(column: TrackColumn, skin: &Skin) -> Element<'static, UiEvent> {
    let label = column_label(column, skin).to_owned();
    let horizontal = if column == TrackColumn::Index {
        Horizontal::Right
    } else {
        Horizontal::Left
    };
    container(
        shaped_text(label)
            .font(fonts::mono(skin.track_list.header_text.weight))
            .size(skin.track_list.header_text.size)
            .color(skin.palette.muted),
    )
    .padding([0.0, skin.track_list.cell_padding_x])
    .width(column_width(column, skin))
    .height(Length::Fill)
    .align_x(horizontal)
    .align_y(Vertical::Center)
    .into()
}

fn row_cell(
    column: TrackColumn,
    index: usize,
    track: TrackRow<'_>,
    skin: &Skin,
) -> Element<'static, UiEvent> {
    match column {
        TrackColumn::Index => text_cell(
            format!("{:02}", index + 1),
            column,
            skin.track_list.index_text,
            fonts::mono,
            skin.palette.muted,
            Horizontal::Right,
            skin,
        ),
        TrackColumn::Deck => deck_cell(track, skin),
        TrackColumn::Title => text_cell(
            value_or_dash(track.title),
            column,
            skin.track_list.title_text,
            fonts::display,
            skin.palette.text,
            Horizontal::Left,
            skin,
        ),
        TrackColumn::Artist => text_cell(
            optional_or_dash(track.artist),
            column,
            skin.track_list.artist_text,
            fonts::sans,
            skin.palette.text_dim,
            Horizontal::Left,
            skin,
        ),
        TrackColumn::Bpm => bpm_cell(track.bpm, skin),
        TrackColumn::Key => text_cell(
            optional_or_dash(track.key),
            column,
            skin.track_list.key_text,
            fonts::mono,
            skin.palette.accent,
            Horizontal::Left,
            skin,
        ),
        TrackColumn::Time => text_cell(
            optional_or_dash(track.time),
            column,
            skin.track_list.time_text,
            fonts::mono,
            skin.palette.text_dim,
            Horizontal::Right,
            skin,
        ),
        TrackColumn::Energy => energy_cell(track.energy, skin),
        TrackColumn::Transition => text_cell(
            track
                .transition
                .map_or_else(|| em_dash().to_owned(), str::to_uppercase),
            column,
            skin.track_list.transition_text,
            fonts::mono,
            skin.palette.muted,
            Horizontal::Left,
            skin,
        ),
    }
}

fn text_cell(
    value: String,
    column: TrackColumn,
    font: crate::skin::FontSkin,
    family: fn(crate::skin::FontWeight) -> iced::Font,
    color: iced::Color,
    horizontal: Horizontal,
    skin: &Skin,
) -> Element<'static, UiEvent> {
    container(
        shaped_text(value)
            .font(family(font.weight))
            .size(font.size)
            .color(color),
    )
    .padding([0.0, skin.track_list.cell_padding_x])
    .width(column_width(column, skin))
    .height(Length::Fill)
    .align_x(horizontal)
    .align_y(Vertical::Center)
    .into()
}

fn deck_cell(track: TrackRow<'_>, skin: &Skin) -> Element<'static, UiEvent> {
    let value = optional_or_dash(track.deck);
    let active = track.current && track.deck.is_some();
    let chip = container(
        shaped_text(value)
            .font(fonts::mono(skin.track_list.deck_text.weight))
            .size(skin.track_list.deck_text.size)
            .color(if active {
                skin.palette.bg_deep
            } else {
                skin.palette.text_dim
            }),
    )
    .width(Length::Fixed(skin.track_list.deck_chip_width))
    .height(Length::Fixed(skin.track_list.deck_chip_height))
    .align_x(Horizontal::Center)
    .align_y(Vertical::Center)
    .style({
        let border = skin.border(skin.track_list.deck_chip_frame);
        let background = active.then_some(Background::Color(skin.palette.accent));
        move |_| ContainerStyle {
            background,
            border,
            ..ContainerStyle::default()
        }
    });
    container(chip)
        .width(Length::Fixed(skin.track_list.deck_width))
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
}

fn bpm_cell(value: Option<&str>, skin: &Skin) -> Element<'static, UiEvent> {
    let badge = container(
        shaped_text(optional_or_dash(value))
            .font(fonts::mono(skin.track_list.bpm_text.weight))
            .size(skin.track_list.bpm_text.size)
            .color(skin.palette.text),
    )
    .padding([0.0, skin.track_list.bpm_badge_padding_x])
    .height(Length::Fixed(skin.track_list.bpm_badge_height))
    .align_y(Vertical::Center)
    .style({
        let border = skin.border(skin.track_list.bpm_badge_frame);
        let background = skin.color(skin.track_list.bpm_badge_background);
        move |_| {
            ContainerStyle::default()
                .background(Background::Color(background))
                .border(border)
        }
    });
    container(badge)
        .padding([0.0, skin.track_list.cell_padding_x])
        .width(Length::Fixed(skin.track_list.bpm_width))
        .height(Length::Fill)
        .align_y(Vertical::Center)
        .into()
}

fn energy_cell(value: Option<u8>, skin: &Skin) -> Element<'static, UiEvent> {
    let value = value.map(|value| value.min(100));
    let ratio = value.map_or(0.0, |value| f32::from(value) / 100.0);
    let filled = skin.track_list.energy_bar_width * ratio;
    let empty = skin.track_list.energy_bar_width - filled;
    let accent = skin.palette.accent;
    let fill = container(Space::new())
        .width(Length::Fixed(filled))
        .height(Length::Fixed(skin.track_list.energy_bar_height))
        .style(move |_| ContainerStyle::default().background(Background::Color(accent)));
    let remainder = container(Space::new())
        .width(Length::Fixed(empty))
        .height(Length::Fixed(skin.track_list.energy_bar_height))
        .style({
            let background = skin.color(skin.track_list.energy_bar_background);
            move |_| ContainerStyle::default().background(Background::Color(background))
        });
    let bar = row![fill, remainder]
        .width(Length::Fixed(skin.track_list.energy_bar_width))
        .height(Length::Fixed(skin.track_list.energy_bar_height));
    let label = value.map_or_else(|| em_dash().to_owned(), |value| value.to_string());
    container(
        row![
            bar,
            shaped_text(label)
                .font(fonts::mono(skin.track_list.energy_text.weight))
                .size(skin.track_list.energy_text.size)
                .color(skin.palette.accent),
        ]
        .spacing(skin.track_list.energy_bar_gap)
        .align_y(Alignment::Center),
    )
    .padding([0.0, skin.track_list.cell_padding_x])
    .width(Length::Fixed(skin.track_list.energy_width))
    .height(Length::Fill)
    .align_y(Vertical::Center)
    .into()
}

fn footer(count: usize, skin: &Skin) -> Element<'static, UiEvent> {
    let left = format!(
        "{} \u{00b7} {count} {}",
        skin.track_list.labels.footer_component, skin.track_list.labels.footer_tracks
    );
    let right = skin.track_list.labels.footer_usage.clone();
    let font = skin.track_list.footer_text;
    let content = row![
        shaped_text(left)
            .font(fonts::mono(font.weight))
            .size(font.size)
            .color(skin.palette.muted),
        Space::new().width(Length::Fill),
        shaped_text(right)
            .font(fonts::mono(font.weight))
            .size(font.size)
            .color(skin.palette.muted),
    ]
    .align_y(Alignment::Center);
    let background = skin.palette.bg_footer;
    container(content)
        .padding([0.0, skin.track_list.footer_padding_x])
        .width(Length::Fill)
        .height(Length::Fixed(skin.track_list.footer_height))
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(background)))
        .into()
}

fn column_visible(reads: &dyn Reads, prefix: Option<&str>, column: TrackColumn) -> bool {
    let Some(prefix) = prefix else {
        return true;
    };
    let endpoint = format!("{prefix}.{}", column.endpoint_name());
    !matches!(reads.get(&endpoint), Some(ReadValue::Bool(false)))
}

fn column_label(column: TrackColumn, skin: &Skin) -> &str {
    let labels = &skin.track_list.labels;
    match column {
        TrackColumn::Index => &labels.index,
        TrackColumn::Deck => &labels.deck,
        TrackColumn::Title => &labels.title,
        TrackColumn::Artist => &labels.artist,
        TrackColumn::Bpm => &labels.bpm,
        TrackColumn::Key => &labels.key,
        TrackColumn::Time => &labels.time,
        TrackColumn::Energy => &labels.energy,
        TrackColumn::Transition => &labels.transition,
    }
}

fn column_width(column: TrackColumn, skin: &Skin) -> Length {
    let width = match column {
        TrackColumn::Index => skin.track_list.index_width,
        TrackColumn::Deck => skin.track_list.deck_width,
        TrackColumn::Title => return Length::Fill,
        TrackColumn::Artist => skin.track_list.artist_width,
        TrackColumn::Bpm => skin.track_list.bpm_width,
        TrackColumn::Key => skin.track_list.key_width,
        TrackColumn::Time => skin.track_list.time_width,
        TrackColumn::Energy => skin.track_list.energy_width,
        TrackColumn::Transition => skin.track_list.transition_width,
    };
    Length::Fixed(width)
}

fn value_or_dash(value: &str) -> String {
    if value.is_empty() {
        em_dash().to_owned()
    } else {
        value.to_owned()
    }
}

fn optional_or_dash(value: Option<&str>) -> String {
    value.map_or_else(|| em_dash().to_owned(), value_or_dash)
}

fn track_button_style(
    skin: &Skin,
    selected: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let palette = skin.palette;
    let border = skin.border(skin.track_list.row_frame);
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Pressed => palette.accent_soft,
            _ if selected => palette.bg_select,
            ButtonStatus::Hovered => palette.bg_panel_2,
            ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_inset,
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: palette.text,
            border,
            ..ButtonStyle::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    struct ColumnReads(Option<bool>);

    impl Reads for ColumnReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            (endpoint == "columns.title")
                .then_some(self.0)
                .flatten()
                .map(ReadValue::Bool)
        }
    }

    #[kithara::test]
    fn absent_column_endpoint_is_visible() {
        assert!(column_visible(
            &ColumnReads(None),
            Some("columns"),
            TrackColumn::Title
        ));
    }

    #[kithara::test]
    fn false_column_endpoint_is_hidden() {
        assert!(!column_visible(
            &ColumnReads(Some(false)),
            Some("columns"),
            TrackColumn::Title
        ));
    }
}
