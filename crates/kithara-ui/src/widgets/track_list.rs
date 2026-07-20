use iced::{
    Alignment, Background, Element, Length, Theme,
    alignment::{Horizontal, Vertical},
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, scrollable, text_input,
    },
};

use super::{Widget, chrome, deck};
use crate::render::{ControlAction, ReadValue, Reads, Skin, TrackRow, UiEvent, fonts, shaped_text};

const fn em_dash() -> &'static str {
    "\u{2014}"
}

const fn search_placeholder() -> &'static str {
    "Search title, artist, BPM, key\u{2026}"
}

#[derive(bon::Builder)]
pub(crate) struct TrackList<'path, 'value, 'data, 'reads, 'skin> {
    path: &'path str,
    value: Option<&'value ReadValue<'data>>,
    reads: &'reads dyn Reads,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for TrackList<'_, '_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::TrackList(tracks)) = self.value else {
            return Space::new().into();
        };
        let palette = self.skin.palette;
        let query = read_text(self.reads, "library.query")
            .unwrap_or("")
            .to_owned();
        let query_normalized = query.trim().to_lowercase();
        let bpm = current_bpm(self.reads);
        let filtered: Vec<_> = tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track_matches(track, &query_normalized, bpm))
            .collect();
        let count = format!("{} / {}", filtered.len(), tracks.len());
        let search = text_input(search_placeholder(), &query)
            .on_input(UiEvent::LibraryQuery)
            .font(fonts::sans(self.skin.text_input.font.weight))
            .size(self.skin.text_input.font.size)
            .padding([
                self.skin.text_input.padding_y,
                self.skin.text_input.padding_x,
            ])
            .style(chrome::text_input_style(self.skin))
            .width(Length::Fill);
        let search_bar = row![
            container(search)
                .height(Length::Fixed(self.skin.text_input.height))
                .width(Length::Fill)
                .align_y(Vertical::Center),
            container(
                shaped_text(count)
                    .font(fonts::mono(self.skin.track_list.count_text.weight))
                    .size(self.skin.track_list.count_text.size)
                    .color(palette.muted),
            )
            .padding([
                self.skin.track_list.count_padding_y,
                self.skin.track_list.count_padding_x,
            ])
            .height(Length::Fixed(self.skin.text_input.height))
            .center_y(Length::Fill)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(palette.bg_panel))
            }),
        ]
        .spacing(self.skin.track_list.grid_gap)
        .align_y(Alignment::Center)
        .height(Length::Fixed(self.skin.text_input.height))
        .width(Length::Fill);

        let table_header = row![
            HeaderCell::builder()
                .skin(self.skin)
                .label("#")
                .width(self.skin.track_list.number_width)
                .build()
                .view(),
            container(
                shaped_text("TITLE")
                    .font(fonts::mono(self.skin.track_list.header_text.weight))
                    .size(self.skin.track_list.header_text.size)
                    .color(palette.muted),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(Vertical::Center),
            HeaderCell::builder()
                .skin(self.skin)
                .label("ARTIST")
                .width(self.skin.track_list.artist_width)
                .build()
                .view(),
            HeaderCell::builder()
                .skin(self.skin)
                .label("TIME")
                .width(self.skin.track_list.time_width)
                .build()
                .view(),
        ]
        .align_y(Alignment::Center)
        .height(Length::Fixed(self.skin.track_list.header_height))
        .width(Length::Fill);
        let table_header = container(table_header)
            .padding([
                self.skin.track_list.time_padding_y,
                self.skin.track_list.time_padding_x,
            ])
            .align_y(Vertical::Center)
            .style(move |_| {
                ContainerStyle::default().background(Background::Color(palette.bg_panel))
            });

        let duration = read_scalar(self.reads, "deck.playback.duration_secs").unwrap_or(0.0);
        let rows = filtered.into_iter().map(|(index, track)| {
            TrackListRow::builder()
                .skin(self.skin)
                .path(self.path)
                .index(index)
                .track(*track)
                .duration(duration)
                .build()
                .view()
        });
        let rows = container(
            Column::with_children(rows)
                .spacing(self.skin.track_list.grid_gap)
                .width(Length::Fill),
        )
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.line_soft)));
        let table = scrollable(rows).width(Length::Fill).height(Length::Fill);

        container(
            column![search_bar, table_header, table]
                .spacing(self.skin.track_list.grid_gap)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.line_soft)))
        .into()
    }
}

#[derive(bon::Builder)]
struct TrackListRow<'path, 'data, 'skin> {
    skin: &'skin Skin,
    path: &'path str,
    index: usize,
    track: TrackRow<'data>,
    duration: f64,
}

impl<'a> Widget<'a> for TrackListRow<'_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let palette = self.skin.palette;
        let time = if self.track.current && self.duration > 0.0 {
            deck::format_time(self.duration)
        } else {
            self.track.time.unwrap_or(em_dash()).to_owned()
        };
        let title = if self.track.title.is_empty() {
            em_dash()
        } else {
            self.track.title
        };
        let artist = self.track.artist.unwrap_or(em_dash());
        let mut title_children: Vec<Element<'a, UiEvent>> = Vec::new();
        if self.track.current {
            title_children.push(
                container(
                    shaped_text("A")
                        .font(fonts::display(self.skin.track_list.chip_text.weight))
                        .size(self.skin.track_list.chip_text.size)
                        .color(palette.bg),
                )
                .center(self.skin.track_list.chip_size)
                .style({
                    let border = self.skin.border(self.skin.track_list.chip_frame);
                    move |_| {
                        ContainerStyle::default()
                            .background(Background::Color(palette.accent))
                            .border(border)
                    }
                })
                .into(),
            );
        }
        title_children.push(
            container(
                shaped_text(title.to_owned())
                    .font(fonts::sans(self.skin.track_list.row_text.weight))
                    .size(self.skin.track_list.row_text.size)
                    .color(palette.text),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(Vertical::Center)
            .into(),
        );

        button(
            row![
                container(
                    shaped_text(format!("{:02}", self.index + 1))
                        .font(fonts::mono(self.skin.track_list.number_text.weight))
                        .size(self.skin.track_list.number_text.size)
                        .color(palette.muted),
                )
                .width(Length::Fixed(self.skin.track_list.number_width))
                .height(Length::Fill)
                .align_y(Vertical::Center),
                Row::with_children(title_children)
                    .spacing(self.skin.track_list.title_gap)
                    .align_y(Alignment::Center)
                    .width(Length::Fill),
                container(
                    shaped_text(artist.to_owned())
                        .font(fonts::sans(self.skin.track_list.time_text.weight))
                        .size(self.skin.track_list.time_text.size)
                        .color(palette.text_dim),
                )
                .width(Length::Fixed(self.skin.track_list.artist_width))
                .height(Length::Fill)
                .align_y(Vertical::Center),
                container(
                    shaped_text(time)
                        .font(fonts::mono(self.skin.track_list.time_text.weight))
                        .size(self.skin.track_list.time_text.size)
                        .color(palette.text_dim),
                )
                .width(Length::Fixed(self.skin.track_list.time_width))
                .height(Length::Fill)
                .align_x(Horizontal::Right)
                .align_y(Vertical::Center),
            ]
            .align_y(Alignment::Center)
            .width(Length::Fill),
        )
        .padding([
            self.skin.track_list.time_padding_y,
            self.skin.track_list.time_padding_x,
        ])
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

#[derive(bon::Builder)]
struct HeaderCell<'skin> {
    skin: &'skin Skin,
    label: &'static str,
    width: f32,
}

impl<'a> Widget<'a> for HeaderCell<'_> {
    fn view(self) -> Element<'a, UiEvent> {
        let palette = self.skin.palette;
        container(
            shaped_text(self.label)
                .font(fonts::mono(self.skin.track_list.header_text.weight))
                .size(self.skin.track_list.header_text.size)
                .color(palette.muted),
        )
        .width(Length::Fixed(self.width))
        .height(Length::Fill)
        .align_y(Vertical::Center)
        .into()
    }
}

fn track_matches(track: &TrackRow<'_>, query: &str, bpm: Option<f32>) -> bool {
    query.is_empty()
        || track.title.to_lowercase().contains(query)
        || track
            .artist
            .is_some_and(|artist| artist.to_lowercase().contains(query))
        || track
            .search
            .is_some_and(|search| search.to_lowercase().contains(query))
        || track.current && bpm.is_some_and(|value| format!("{value:.1}").contains(query))
}

fn current_bpm(reads: &dyn Reads) -> Option<f32> {
    match reads.get("deck.playback.waveform") {
        Some(ReadValue::Waveform(waveform)) => waveform.bpm,
        _ => None,
    }
}

fn read_text<'a>(reads: &'a dyn Reads, endpoint: &str) -> Option<&'a str> {
    match reads.get(endpoint) {
        Some(ReadValue::Text(value)) => Some(value),
        _ => None,
    }
}

fn read_scalar(reads: &dyn Reads, endpoint: &str) -> Option<f64> {
    match reads.get(endpoint) {
        Some(ReadValue::Scalar(value)) => Some(value),
        _ => None,
    }
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
            ButtonStatus::Hovered if !selected => palette.bg_inset,
            _ if selected => palette.bg_panel_2,
            ButtonStatus::Active | ButtonStatus::Hovered | ButtonStatus::Disabled => {
                palette.bg_inset
            }
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: palette.text,
            border,
            ..ButtonStyle::default()
        }
    }
}
