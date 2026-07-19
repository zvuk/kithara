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

use super::{chrome, deck};
use crate::render::{ControlAction, ReadValue, Reads, Skin, TrackRow, UiEvent, fonts, shaped_text};

const fn em_dash() -> &'static str {
    "\u{2014}"
}

const fn search_placeholder() -> &'static str {
    "Search title, artist, BPM, key\u{2026}"
}

pub(crate) fn view<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    skin: &Skin,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::TrackList(tracks)) = value else {
        return Space::new().into();
    };
    let palette = skin.palette;
    let query = read_text(reads, "library.query").unwrap_or("").to_owned();
    let query_normalized = query.trim().to_lowercase();
    let bpm = current_bpm(reads);
    let filtered: Vec<_> = tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| track_matches(track, &query_normalized, bpm))
        .collect();
    let count = format!("{} / {}", filtered.len(), tracks.len());
    let search = text_input(search_placeholder(), &query)
        .on_input(UiEvent::LibraryQuery)
        .font(fonts::sans(skin.text_input.font.weight))
        .size(skin.text_input.font.size)
        .padding([skin.text_input.padding_y, skin.text_input.padding_x])
        .style(chrome::text_input_style(skin))
        .width(Length::Fill);
    let search_bar = row![
        container(search)
            .height(Length::Fixed(skin.text_input.height))
            .width(Length::Fill)
            .align_y(Vertical::Center),
        container(
            shaped_text(count)
                .font(fonts::mono(skin.track_list.count_text.weight))
                .size(skin.track_list.count_text.size)
                .color(palette.muted),
        )
        .padding([
            skin.track_list.count_padding_y,
            skin.track_list.count_padding_x,
        ])
        .height(Length::Fixed(skin.text_input.height))
        .center_y(Length::Fill)
        .style(move |_| {
            ContainerStyle::default().background(Background::Color(palette.bg_panel))
        }),
    ]
    .spacing(skin.track_list.grid_gap)
    .align_y(Alignment::Center)
    .height(Length::Fixed(skin.text_input.height))
    .width(Length::Fill);

    let table_header = row![
        header_cell(skin, "#", skin.track_list.number_width),
        container(
            shaped_text("TITLE")
                .font(fonts::mono(skin.track_list.header_text.weight))
                .size(skin.track_list.header_text.size)
                .color(palette.muted),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(Vertical::Center),
        header_cell(skin, "ARTIST", skin.track_list.artist_width),
        header_cell(skin, "TIME", skin.track_list.time_width),
    ]
    .align_y(Alignment::Center)
    .height(Length::Fixed(skin.track_list.header_height))
    .width(Length::Fill);
    let table_header = container(table_header)
        .padding([
            skin.track_list.time_padding_y,
            skin.track_list.time_padding_x,
        ])
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)));

    let duration = read_scalar(reads, "deck.playback.duration_secs").unwrap_or(0.0);
    let rows = filtered
        .into_iter()
        .map(|(index, track)| track_row(skin, path, index, *track, duration));
    let rows = container(
        Column::with_children(rows)
            .spacing(skin.track_list.grid_gap)
            .width(Length::Fill),
    )
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.line_soft)));
    let table = scrollable(rows).width(Length::Fill).height(Length::Fill);

    container(
        column![search_bar, table_header, table]
            .spacing(skin.track_list.grid_gap)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.line_soft)))
    .into()
}

fn track_row(
    skin: &Skin,
    path: &str,
    index: usize,
    track: TrackRow<'_>,
    duration: f64,
) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    let time = if track.current && duration > 0.0 {
        deck::format_time(duration)
    } else {
        track.time.unwrap_or(em_dash()).to_owned()
    };
    let title = if track.title.is_empty() {
        em_dash()
    } else {
        track.title
    };
    let artist = track.artist.unwrap_or(em_dash());
    let mut title_children: Vec<Element<'static, UiEvent>> = Vec::new();
    if track.current {
        title_children.push(
            container(
                shaped_text("A")
                    .font(fonts::display(skin.track_list.chip_text.weight))
                    .size(skin.track_list.chip_text.size)
                    .color(palette.bg),
            )
            .center(skin.track_list.chip_size)
            .style({
                let border = skin.border(skin.track_list.chip_frame);
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
                .font(fonts::sans(skin.track_list.row_text.weight))
                .size(skin.track_list.row_text.size)
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
                shaped_text(format!("{:02}", index + 1))
                    .font(fonts::mono(skin.track_list.number_text.weight))
                    .size(skin.track_list.number_text.size)
                    .color(palette.muted),
            )
            .width(Length::Fixed(skin.track_list.number_width))
            .height(Length::Fill)
            .align_y(Vertical::Center),
            Row::with_children(title_children)
                .spacing(skin.track_list.title_gap)
                .align_y(Alignment::Center)
                .width(Length::Fill),
            container(
                shaped_text(artist.to_owned())
                    .font(fonts::sans(skin.track_list.time_text.weight))
                    .size(skin.track_list.time_text.size)
                    .color(palette.text_dim),
            )
            .width(Length::Fixed(skin.track_list.artist_width))
            .height(Length::Fill)
            .align_y(Vertical::Center),
            container(
                shaped_text(time)
                    .font(fonts::mono(skin.track_list.time_text.weight))
                    .size(skin.track_list.time_text.size)
                    .color(palette.text_dim),
            )
            .width(Length::Fixed(skin.track_list.time_width))
            .height(Length::Fill)
            .align_x(Horizontal::Right)
            .align_y(Vertical::Center),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fill),
    )
    .padding([
        skin.track_list.time_padding_y,
        skin.track_list.time_padding_x,
    ])
    .height(Length::Fixed(skin.track_list.row_height))
    .width(Length::Fill)
    .style(track_button_style(skin, track.selected))
    .on_press(UiEvent::Control {
        path: path.to_owned(),
        action: ControlAction::SelectIndex(index),
    })
    .into()
}

fn header_cell(skin: &Skin, label: &'static str, width: f32) -> Element<'static, UiEvent> {
    let palette = skin.palette;
    container(
        shaped_text(label)
            .font(fonts::mono(skin.track_list.header_text.weight))
            .size(skin.track_list.header_text.size)
            .color(palette.muted),
    )
    .width(Length::Fixed(width))
    .height(Length::Fill)
    .align_y(Vertical::Center)
    .into()
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
