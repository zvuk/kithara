use iced::{
    Alignment, Background, Element, Font, Length, Theme,
    alignment::{Horizontal, Vertical},
    font::Weight,
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, scrollable, text_input,
    },
};

use super::{chrome, deck};
use crate::{
    registry::{ControlKindDesc, ValueKind},
    render::{
        ControlAction, ReadValue, Reads, RenderPalette, TrackRow, UiEvent, fonts, shaped_text,
    },
    size::{Dim, SizeSpec},
};

struct Consts;

impl Consts {
    const ARTIST_WIDTH: f32 = 170.0;
    const CHIP_SIZE: f32 = 18.0;
    const CHIP_TEXT_SIZE: f32 = 14.0;
    const COUNT_PADDING_X: f32 = 11.0;
    const COUNT_TEXT_SIZE: f32 = 9.0;
    const EM_DASH: &'static str = "\u{2014}";
    const GRID_GAP: f32 = 1.0;
    const HEADER_HEIGHT: f32 = 24.0;
    const HEADER_TEXT_SIZE: f32 = 8.0;
    const INPUT_PADDING_X: f32 = 12.0;
    const INPUT_TEXT_SIZE: f32 = 12.0;
    const MIN_HEIGHT: f32 = 210.0;
    const MIN_WIDTH: f32 = 600.0;
    const NUMBER_TEXT_SIZE: f32 = 10.0;
    const NUMBER_WIDTH: f32 = 44.0;
    const ROW_HEIGHT: f32 = 30.0;
    const ROW_TEXT_SIZE: f32 = 13.0;
    const SEARCH_HEIGHT: f32 = 30.0;
    const SEARCH_PLACEHOLDER: &'static str = "Search title, artist, BPM, key\u{2026}";
    const TIME_PADDING_X: f32 = 12.0;
    const TIME_TEXT_SIZE: f32 = 12.0;
    const TIME_WIDTH: f32 = 70.0;
    const TITLE_GAP: f32 = 8.0;
}

pub(crate) fn desc() -> ControlKindDesc {
    ControlKindDesc::new(Some(ValueKind::TrackList), None).with_size(SizeSpec::new(
        Dim::Range {
            min: Consts::MIN_WIDTH,
            max: None,
        },
        Dim::Range {
            min: Consts::MIN_HEIGHT,
            max: None,
        },
    ))
}

pub(crate) fn view<'a>(
    path: &str,
    value: Option<&ReadValue<'_>>,
    reads: &dyn Reads,
    palette: RenderPalette,
) -> Element<'a, UiEvent> {
    let Some(ReadValue::TrackList(tracks)) = value else {
        return Space::new().into();
    };
    let query = read_text(reads, "library.query").unwrap_or("").to_owned();
    let query_normalized = query.trim().to_lowercase();
    let bpm = current_bpm(reads);
    let filtered: Vec<_> = tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| track_matches(track, &query_normalized, bpm))
        .collect();
    let count = format!("{} / {}", filtered.len(), tracks.len());
    let search = text_input(Consts::SEARCH_PLACEHOLDER, &query)
        .on_input(UiEvent::LibraryQuery)
        .font(fonts::SANS)
        .size(Consts::INPUT_TEXT_SIZE)
        .padding([0.0, Consts::INPUT_PADDING_X])
        .style(chrome::text_input_style(palette))
        .width(Length::Fill);
    let search_bar = row![
        container(search)
            .height(Length::Fixed(Consts::SEARCH_HEIGHT))
            .width(Length::Fill)
            .align_y(Vertical::Center),
        container(
            shaped_text(count)
                .font(fonts::MONO)
                .size(Consts::COUNT_TEXT_SIZE)
                .color(palette.muted),
        )
        .padding([0.0, Consts::COUNT_PADDING_X])
        .height(Length::Fixed(Consts::SEARCH_HEIGHT))
        .center_y(Length::Fill)
        .style(move |_| {
            ContainerStyle::default().background(Background::Color(palette.bg_panel))
        }),
    ]
    .spacing(Consts::GRID_GAP)
    .align_y(Alignment::Center)
    .height(Length::Fixed(Consts::SEARCH_HEIGHT))
    .width(Length::Fill);

    let table_header = row![
        header_cell(palette, "#", Consts::NUMBER_WIDTH),
        container(
            shaped_text("TITLE")
                .font(fonts::MONO)
                .size(Consts::HEADER_TEXT_SIZE)
                .color(palette.muted),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(Vertical::Center),
        header_cell(palette, "ARTIST", Consts::ARTIST_WIDTH),
        header_cell(palette, "TIME", Consts::TIME_WIDTH),
    ]
    .align_y(Alignment::Center)
    .height(Length::Fixed(Consts::HEADER_HEIGHT))
    .width(Length::Fill);
    let table_header = container(table_header)
        .padding([0.0, Consts::TIME_PADDING_X])
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(palette.bg_panel)));

    let duration = read_scalar(reads, "deck.playback.duration_secs").unwrap_or(0.0);
    let rows = filtered
        .into_iter()
        .map(|(index, track)| track_row(palette, path, index, *track, duration));
    let rows = container(
        Column::with_children(rows)
            .spacing(Consts::GRID_GAP)
            .width(Length::Fill),
    )
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.line_soft)));
    let table = scrollable(rows).width(Length::Fill).height(Length::Fill);

    container(
        column![search_bar, table_header, table]
            .spacing(Consts::GRID_GAP)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(palette.line_soft)))
    .into()
}

fn track_row(
    palette: RenderPalette,
    path: &str,
    index: usize,
    track: TrackRow<'_>,
    duration: f64,
) -> Element<'static, UiEvent> {
    let time = if track.current && duration > 0.0 {
        deck::format_time(duration)
    } else {
        track.time.unwrap_or(Consts::EM_DASH).to_owned()
    };
    let title = if track.title.is_empty() {
        Consts::EM_DASH
    } else {
        track.title
    };
    let artist = track.artist.unwrap_or(Consts::EM_DASH);
    let mut title_children: Vec<Element<'static, UiEvent>> = Vec::new();
    if track.current {
        title_children.push(
            container(
                shaped_text("A")
                    .font(fonts::display(Weight::Bold))
                    .size(Consts::CHIP_TEXT_SIZE)
                    .color(palette.bg),
            )
            .center(Consts::CHIP_SIZE)
            .style(move |_| ContainerStyle::default().background(Background::Color(palette.accent)))
            .into(),
        );
    }
    title_children.push(
        container(
            shaped_text(title.to_owned())
                .font(Font {
                    weight: Weight::Medium,
                    ..fonts::SANS
                })
                .size(Consts::ROW_TEXT_SIZE)
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
                    .font(fonts::MONO)
                    .size(Consts::NUMBER_TEXT_SIZE)
                    .color(palette.muted),
            )
            .width(Length::Fixed(Consts::NUMBER_WIDTH))
            .height(Length::Fill)
            .align_y(Vertical::Center),
            Row::with_children(title_children)
                .spacing(Consts::TITLE_GAP)
                .align_y(Alignment::Center)
                .width(Length::Fill),
            container(
                shaped_text(artist.to_owned())
                    .font(fonts::SANS)
                    .size(Consts::TIME_TEXT_SIZE)
                    .color(palette.text_dim),
            )
            .width(Length::Fixed(Consts::ARTIST_WIDTH))
            .height(Length::Fill)
            .align_y(Vertical::Center),
            container(
                shaped_text(time)
                    .font(fonts::MONO)
                    .size(Consts::TIME_TEXT_SIZE)
                    .color(palette.text_dim),
            )
            .width(Length::Fixed(Consts::TIME_WIDTH))
            .height(Length::Fill)
            .align_x(Horizontal::Right)
            .align_y(Vertical::Center),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fill),
    )
    .padding([0.0, Consts::TIME_PADDING_X])
    .height(Length::Fixed(Consts::ROW_HEIGHT))
    .width(Length::Fill)
    .style(track_button_style(palette, track.selected))
    .on_press(UiEvent::Control {
        path: path.to_owned(),
        action: ControlAction::SelectIndex(index),
    })
    .into()
}

fn header_cell(
    palette: RenderPalette,
    label: &'static str,
    width: f32,
) -> Element<'static, UiEvent> {
    container(
        shaped_text(label)
            .font(fonts::MONO)
            .size(Consts::HEADER_TEXT_SIZE)
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
    palette: RenderPalette,
    selected: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
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
            ..ButtonStyle::default()
        }
    }
}
