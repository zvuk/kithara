use iced::{
    Alignment, Background, Element, Font, Length, Theme,
    alignment::Horizontal,
    font::Weight,
    widget::{
        Column, Row, Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, scrollable, text_input,
    },
};
use kithara_ui::render::{
    ControlAction, ReadValue, RenderPalette, TrackRow, UiEvent, fonts, shaped_text,
};

use super::deck_controls;
use crate::gui::{
    app::Kithara,
    message::Message,
    tokens::{gap, track_list},
    widgets,
};

struct Consts;

impl Consts {
    const EM_DASH: &'static str = "\u{2014}";
    const SEARCH_PLACEHOLDER: &'static str = "Search title, artist, BPM, key\u{2026}";
}

pub(crate) fn render<'a>(
    state: &'a Kithara,
    path: &str,
    value: &Option<ReadValue<'_>>,
) -> Element<'a, Message> {
    let Some(ReadValue::TrackList(tracks)) = value else {
        return Space::new().into();
    };
    track_list(state, path, tracks)
}

fn track_list<'a>(state: &'a Kithara, path: &str, tracks: &[TrackRow<'_>]) -> Element<'a, Message> {
    let p = state.palette;
    let query = state.library_query.trim().to_lowercase();
    let filtered: Vec<_> = tracks
        .iter()
        .enumerate()
        .filter(|(index, track)| track_matches(state, *index, track, &query))
        .collect();
    let count = format!("{} / {}", filtered.len(), tracks.len());
    let search = text_input(Consts::SEARCH_PLACEHOLDER, state.library_query.as_str())
        .on_input(|query| Message::Modular(UiEvent::LibraryQuery(query)))
        .font(fonts::SANS)
        .size(track_list::INPUT_TEXT_SIZE)
        .padding([0.0, track_list::INPUT_PADDING_X])
        .style(widgets::text_input_style(p))
        .width(Length::Fill);
    let search_bar = row![
        container(search)
            .height(Length::Fixed(track_list::SEARCH_HEIGHT))
            .width(Length::Fill),
        container(
            shaped_text(count)
                .font(fonts::MONO)
                .size(track_list::COUNT_TEXT_SIZE)
                .color(p.muted),
        )
        .padding([0.0, track_list::COUNT_PADDING_X])
        .height(Length::Fixed(track_list::SEARCH_HEIGHT))
        .center_y(Length::Fill)
        .style(move |_| { ContainerStyle::default().background(Background::Color(p.bg_panel)) }),
    ]
    .spacing(gap::GRID)
    .height(Length::Fixed(track_list::SEARCH_HEIGHT))
    .width(Length::Fill);

    let table_header = row![
        header_cell(p, "#", track_list::NUMBER_WIDTH),
        shaped_text("TITLE")
            .font(fonts::MONO)
            .size(track_list::HEADER_TEXT_SIZE)
            .color(p.muted)
            .width(Length::Fill),
        header_cell(p, "ARTIST", track_list::ARTIST_WIDTH),
        header_cell(p, "TIME", track_list::TIME_WIDTH),
    ]
    .align_y(Alignment::Center)
    .height(Length::Fixed(track_list::HEADER_HEIGHT))
    .width(Length::Fill);
    let table_header = container(table_header)
        .padding([0.0, track_list::TIME_PADDING_X])
        .style(move |_| ContainerStyle::default().background(Background::Color(p.bg_panel)));

    let rows = filtered
        .into_iter()
        .map(|(index, track)| track_row(state, path, index, *track));
    let rows = container(
        Column::with_children(rows)
            .spacing(gap::GRID)
            .width(Length::Fill),
    )
    .style(move |_| ContainerStyle::default().background(Background::Color(p.line_soft)));
    let table = scrollable(rows).width(Length::Fill).height(Length::Fill);

    container(
        column![search_bar, table_header, table]
            .spacing(gap::GRID)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(p.line_soft)))
    .into()
}

fn track_row<'a>(
    state: &'a Kithara,
    path: &str,
    index: usize,
    track: TrackRow<'_>,
) -> Element<'a, Message> {
    let p = state.palette;
    let current = state.ui_state.current_track_index == Some(index);
    let selected = state.selected_track_index == Some(index);
    let time = if current && state.ui_state.duration > 0.0 {
        deck_controls::format_time(state.ui_state.duration)
    } else {
        track.time.unwrap_or(Consts::EM_DASH).to_owned()
    };
    let title = if track.title.is_empty() {
        Consts::EM_DASH
    } else {
        track.title
    };
    let artist = track.artist.unwrap_or(Consts::EM_DASH);
    let mut title_children: Vec<Element<'a, Message>> = Vec::new();
    if current {
        title_children.push(
            container(
                shaped_text("A")
                    .font(fonts::display(Weight::Bold))
                    .size(track_list::CHIP_TEXT_SIZE)
                    .color(p.bg),
            )
            .center(track_list::CHIP_SIZE)
            .style(move |_| ContainerStyle::default().background(Background::Color(p.accent)))
            .into(),
        );
    }
    title_children.push(
        shaped_text(title.to_owned())
            .font(Font {
                weight: Weight::Medium,
                ..fonts::SANS
            })
            .size(track_list::ROW_TEXT_SIZE)
            .color(p.text)
            .width(Length::Fill)
            .into(),
    );

    button(
        row![
            container(
                shaped_text(format!("{:02}", index + 1))
                    .font(fonts::MONO)
                    .size(track_list::NUMBER_TEXT_SIZE)
                    .color(p.muted),
            )
            .width(Length::Fixed(track_list::NUMBER_WIDTH)),
            Row::with_children(title_children)
                .spacing(track_list::TITLE_GAP)
                .align_y(Alignment::Center)
                .width(Length::Fill),
            container(
                shaped_text(artist.to_owned())
                    .font(fonts::SANS)
                    .size(track_list::TIME_TEXT_SIZE)
                    .color(p.text_dim),
            )
            .width(Length::Fixed(track_list::ARTIST_WIDTH)),
            container(
                shaped_text(time)
                    .font(fonts::MONO)
                    .size(track_list::TIME_TEXT_SIZE)
                    .color(p.text_dim),
            )
            .width(Length::Fixed(track_list::TIME_WIDTH))
            .align_x(Horizontal::Right),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fill),
    )
    .padding([0.0, track_list::TIME_PADDING_X])
    .height(Length::Fixed(track_list::ROW_HEIGHT))
    .width(Length::Fill)
    .style(track_button_style(p, selected))
    .on_press(Message::Modular(UiEvent::Control {
        path: path.to_owned(),
        action: ControlAction::SelectIndex(index),
    }))
    .into()
}

fn header_cell(p: RenderPalette, label: &'static str, width: f32) -> Element<'static, Message> {
    container(
        shaped_text(label)
            .font(fonts::MONO)
            .size(track_list::HEADER_TEXT_SIZE)
            .color(p.muted),
    )
    .width(Length::Fixed(width))
    .into()
}

fn track_matches(state: &Kithara, index: usize, track: &TrackRow<'_>, query: &str) -> bool {
    if query.is_empty()
        || track.title.to_lowercase().contains(query)
        || track
            .artist
            .is_some_and(|artist| artist.to_lowercase().contains(query))
        || state
            .ui_state
            .tracks
            .get(index)
            .and_then(|track| track.url.as_deref())
            .is_some_and(|url| url.to_lowercase().contains(query))
    {
        return true;
    }
    state.ui_state.current_track_index == Some(index)
        && state
            .ui_state
            .analysis
            .as_ref()
            .and_then(|analysis| analysis.beat())
            .is_some_and(|grid| format!("{:.1}", grid.bpm()).contains(query))
}

fn track_button_style(
    p: RenderPalette,
    selected: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Pressed => p.accent_soft,
            ButtonStatus::Hovered if !selected => p.bg_inset,
            _ if selected => p.bg_panel_2,
            ButtonStatus::Active | ButtonStatus::Hovered | ButtonStatus::Disabled => p.bg_inset,
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: p.text,
            ..ButtonStyle::default()
        }
    }
}
