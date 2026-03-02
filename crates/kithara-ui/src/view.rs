use std::path::Path;

use iced::{
    Alignment, Background, Border, Color, Element, Length, Padding, Theme,
    widget::{
        Space, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, scrollable, slider,
        slider::{
            Handle as SliderHandle, HandleShape as SliderHandleShape, Rail as SliderRail,
            Status as SliderStatus, Style as SliderStyle,
        },
        svg, text, vertical_slider,
    },
};

use crate::{
    icons::{GOLD, Icon, LIGHT, MUTED},
    message::{Message, Tab},
    state::Kithara,
    theme::{BG_DARK, BG_PANEL},
};

const OUTER_PADDING: u16 = 18;
const SECTION_PADDING: u16 = 12;
const TITLE_COLOR: Color = Color {
    r: 0.9,
    g: 0.9,
    b: 0.9,
    a: 1.0,
};

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let content = column![
        view_header(state),
        view_now_playing(state),
        view_seek(state),
        view_transport(state),
        view_volume(state),
        view_tabs(state),
        container(view_tab_content(state))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(SECTION_PADDING)
            .style(panel_style)
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .spacing(12);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding::from(OUTER_PADDING))
        .style(root_style)
        .into()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped positive f32 → u32"
)]
fn format_time(seconds: f32) -> String {
    let total = seconds.max(0.0).floor() as u32;
    let minutes = total / 60;
    let remaining = total % 60;
    format!("{minutes:02}:{remaining:02}")
}

fn view_header(state: &Kithara) -> Element<'_, Message> {
    let logo = svg::Svg::new(svg::Handle::from_memory(
        include_bytes!("../assets/logo.svg") as &[u8],
    ))
    .width(Length::Fixed(48.0))
    .height(Length::Fixed(48.0));

    let title_color = if state.playing { GOLD } else { TITLE_COLOR };

    let title = column![
        text("Kithara").size(28).color(title_color),
        text("Player").size(12).color(MUTED)
    ]
    .spacing(2);

    container(row![logo, title,].align_y(Alignment::Center).spacing(12))
        .width(Length::Fill)
        .padding(SECTION_PADDING)
        .style(panel_style)
        .into()
}

fn view_now_playing(state: &Kithara) -> Element<'_, Message> {
    let track_name = if state.track_name.trim().is_empty() {
        "No track loaded".to_string()
    } else {
        state.track_name.clone()
    };

    let subtitle = track_subtitle(state);

    container(
        column![
            row![
                Icon::MusicNote.gold(20.0),
                text(track_name).size(22).color(LIGHT)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            text(subtitle).size(14).color(MUTED)
        ]
        .spacing(6),
    )
    .width(Length::Fill)
    .padding(SECTION_PADDING)
    .style(section_style)
    .into()
}

fn view_seek(state: &Kithara) -> Element<'_, Message> {
    let duration = state.duration.max(0.0);
    let slider_max = duration.max(1.0);
    let progress = if state.is_seeking {
        state.seek_position
    } else {
        state.position
    }
    .clamp(0.0, slider_max);

    let seek = slider(0.0..=slider_max, progress, Message::SeekChanged)
        .step(0.1)
        .on_release(Message::SeekReleased)
        .style(slider_style)
        .width(Length::Fill);

    container(
        row![
            text(format_time(progress)).size(13).color(MUTED),
            seek,
            text(format_time(duration)).size(13).color(MUTED)
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([4, 8])
    .into()
}

fn view_transport(state: &Kithara) -> Element<'_, Message> {
    let play_icon = if state.playing {
        Icon::Pause
    } else {
        Icon::Play
    };

    let transport_row = row![
        icon_button(Icon::SkipPrev, 30.0, LIGHT, 8, Message::Prev),
        main_transport_button(play_icon, Message::TogglePlayPause),
        icon_button(Icon::SkipNext, 30.0, LIGHT, 8, Message::Next),
    ]
    .spacing(20)
    .align_y(Alignment::Center);

    let shuffle_color = if state.shuffle_enabled { GOLD } else { MUTED };
    let repeat_color = if state.repeat_enabled { GOLD } else { MUTED };
    let repeat_icon = if state.repeat_enabled {
        Icon::RepeatOnce
    } else {
        Icon::Repeat
    };

    let toggles_row = row![
        icon_button(
            Icon::Shuffle,
            22.0,
            shuffle_color,
            6,
            Message::ToggleShuffle
        ),
        Space::new().width(Length::Fill),
        icon_button(repeat_icon, 22.0, repeat_color, 6, Message::ToggleRepeat),
    ]
    .width(Length::Fill)
    .align_y(Alignment::Center);

    container(
        column![transport_row, toggles_row]
            .spacing(10)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([2, 8])
    .into()
}

fn view_volume(state: &Kithara) -> Element<'_, Message> {
    let volume_icon = if state.volume <= 0.01 {
        Icon::VolumeMute
    } else if state.volume < 0.5 {
        Icon::VolumeLow
    } else {
        Icon::VolumeHigh
    };

    let volume_pct = format!("{:.0}%", state.volume.clamp(0.0, 1.0) * 100.0);

    let slider = slider(
        0.0..=1.0,
        state.volume.clamp(0.0, 1.0),
        Message::VolumeChanged,
    )
    .step(0.01)
    .style(slider_style)
    .width(Length::Fill);

    container(
        row![
            volume_icon.light(20.0),
            slider,
            text(volume_pct).size(13).color(MUTED)
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([2, 8])
    .into()
}

fn view_tabs(state: &Kithara) -> Element<'_, Message> {
    let tabs = row![
        tab_button(state, Tab::Playlist, Icon::Playlist, "Playlist"),
        tab_button(state, Tab::Dj, Icon::Dj, "DJ"),
        tab_button(state, Tab::Equalizer, Icon::Equalizer, "EQ"),
        tab_button(state, Tab::Settings, Icon::Settings, "Settings"),
    ]
    .spacing(6)
    .width(Length::Fill);

    container(tabs)
        .width(Length::Fill)
        .padding([4, 0])
        .style(section_style)
        .into()
}

fn view_tab_content(state: &Kithara) -> Element<'_, Message> {
    match state.active_tab {
        Tab::Playlist => view_playlist(state),
        Tab::Dj => view_dj(state),
        Tab::Equalizer => view_equalizer(state),
        Tab::Settings => view_settings(state),
    }
}

fn view_playlist(state: &Kithara) -> Element<'_, Message> {
    if state.playlist.is_empty() {
        return container(text("No tracks in playlist").size(14).color(MUTED))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    let mut tracks = column![].spacing(6).width(Length::Fill);

    for index in 0..state.playlist.len() {
        let is_current = state.current_track_index == Some(index);
        let text_color = if is_current { GOLD } else { LIGHT };
        let index_color = if is_current { GOLD } else { MUTED };
        let track_name = truncate_name(&state.playlist.track_name(index), 40);

        let item = button(
            row![
                text(format!("{:02}", index + 1))
                    .size(13)
                    .color(index_color),
                text(track_name).size(15).color(text_color),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([8, 10])
        .style(move |_theme, status| playlist_item_style(is_current, status))
        .on_press(Message::SelectTrack(index));

        tracks = tracks.push(item);
    }

    scrollable(tracks).height(Length::Fill).into()
}

fn view_equalizer(state: &Kithara) -> Element<'_, Message> {
    let labels = ["Low", "Mid", "High"];
    let mut bands_row = row![].spacing(26).align_y(Alignment::End);

    for (index, label) in labels.iter().enumerate() {
        let value = state.eq_bands[index].clamp(-24.0, 6.0);
        let value_color = if value.abs() < 0.05 { MUTED } else { GOLD };

        let band = column![
            text(format!("{value:+.1} dB")).size(13).color(value_color),
            vertical_slider(-24.0..=6.0, value, move |v| Message::EqBandChanged(
                index, v
            ))
            .step(0.5)
            .height(Length::Fixed(190.0))
            .style(slider_style),
            text(*label).size(14).color(LIGHT),
            button(text("Reset").size(12).color(MUTED))
                .padding([4, 8])
                .style(ghost_button_style)
                .on_press(Message::EqBandReset(index))
        ]
        .spacing(8)
        .align_x(Alignment::Center);

        bands_row = bands_row.push(band);
    }

    container(
        column![
            text("3-Band Equalizer").size(16).color(LIGHT),
            container(bands_row)
                .width(Length::Fill)
                .center_x(Length::Fill)
        ]
        .spacing(12),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn view_dj(state: &Kithara) -> Element<'_, Message> {
    let secs = state.crossfade.clamp(0.0, 30.0);

    let header = row![
        text("Crossfade").size(16).color(LIGHT),
        Space::new().width(Length::Fill),
        text(format!("{secs:.1}s")).size(16).color(GOLD),
    ]
    .align_y(Alignment::Center);

    let slider = slider(0.0..=30.0, secs, Message::CrossfadeChanged)
        .step(0.5)
        .style(slider_style);

    let labels = row![
        text("Track A").size(13).color(MUTED),
        Space::new().width(Length::Fill),
        text("Track B").size(13).color(MUTED),
    ]
    .align_y(Alignment::Center);

    container(
        column![
            header,
            slider,
            labels,
            Space::new().height(Length::Fixed(8.0)),
            text("Blend outgoing and incoming tracks with a smooth DJ transition.")
                .size(13)
                .color(MUTED)
        ]
        .spacing(10),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn view_settings(_state: &Kithara) -> Element<'_, Message> {
    container(
        column![
            text("Settings").size(16).color(LIGHT),
            text("Playback and appearance options will be added here.")
                .size(13)
                .color(MUTED),
        ]
        .spacing(8),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn truncate_name(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        name.to_string()
    } else {
        let truncated: String = name.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}\u{2026}")
    }
}

fn icon_button(
    icon: Icon,
    size: f32,
    color: Color,
    padding: u16,
    message: Message,
) -> Element<'static, Message> {
    button(icon.view(size, color))
        .padding(padding)
        .style(ghost_button_style)
        .on_press(message)
        .into()
}

fn main_transport_button(icon: Icon, message: Message) -> Element<'static, Message> {
    button(icon.view(35.0, BG_DARK))
        .padding(14)
        .style(|_theme, status| {
            let background = match status {
                ButtonStatus::Hovered => with_alpha(GOLD, 0.95),
                ButtonStatus::Pressed => with_alpha(GOLD, 0.75),
                ButtonStatus::Active => GOLD,
                ButtonStatus::Disabled => with_alpha(GOLD, 0.4),
            };

            ButtonStyle {
                background: Some(Background::Color(background)),
                text_color: LIGHT,
                border: Border::default()
                    .rounded(100.0)
                    .width(1.0)
                    .color(with_alpha(LIGHT, 0.2)),
                ..ButtonStyle::default()
            }
        })
        .on_press(message)
        .into()
}

fn tab_button(state: &Kithara, tab: Tab, icon: Icon, label: &str) -> Element<'static, Message> {
    let active = state.active_tab == tab;
    let icon_color = if active { GOLD } else { MUTED };
    let label_color = if active { LIGHT } else { MUTED };
    let underline_color = if active { GOLD } else { Color::TRANSPARENT };

    let content = column![
        row![
            icon.view(18.0, icon_color),
            text(label.to_string()).size(13).color(label_color)
        ]
        .spacing(6)
        .align_y(Alignment::Center),
        container(Space::new().height(Length::Fixed(2.0)).width(Length::Fill))
            .style(move |_theme| ContainerStyle::default().background(underline_color))
    ]
    .spacing(6)
    .align_x(Alignment::Center);

    button(content)
        .width(Length::FillPortion(1))
        .padding([8, 8])
        .style(move |_theme, status| tab_button_style(active, status))
        .on_press(Message::TabSelected(tab))
        .into()
}

fn root_style(_theme: &Theme) -> ContainerStyle {
    ContainerStyle::default()
        .background(BG_DARK)
        .color(TITLE_COLOR)
}

fn panel_style(_theme: &Theme) -> ContainerStyle {
    ContainerStyle::default()
        .background(BG_PANEL)
        .color(TITLE_COLOR)
        .border(
            Border::default()
                .rounded(12.0)
                .width(1.0)
                .color(with_alpha(GOLD, 0.25)),
        )
}

fn section_style(_theme: &Theme) -> ContainerStyle {
    ContainerStyle::default()
        .background(with_alpha(BG_PANEL, 0.45))
        .color(TITLE_COLOR)
        .border(
            Border::default()
                .rounded(10.0)
                .width(1.0)
                .color(with_alpha(MUTED, 0.2)),
        )
}

fn ghost_button_style(_theme: &Theme, status: ButtonStatus) -> ButtonStyle {
    let background = match status {
        ButtonStatus::Active => None,
        ButtonStatus::Hovered => Some(Background::Color(with_alpha(BG_PANEL, 0.85))),
        ButtonStatus::Pressed => Some(Background::Color(with_alpha(GOLD, 0.2))),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(BG_PANEL, 0.3))),
    };

    ButtonStyle {
        background,
        text_color: LIGHT,
        border: Border::default().rounded(8.0),
        ..ButtonStyle::default()
    }
}

fn tab_button_style(active: bool, status: ButtonStatus) -> ButtonStyle {
    let base_bg = if active {
        with_alpha(GOLD, 0.12)
    } else {
        Color::TRANSPARENT
    };

    let hover_bg = if active {
        with_alpha(GOLD, 0.2)
    } else {
        with_alpha(BG_PANEL, 0.8)
    };

    let pressed_bg = if active {
        with_alpha(GOLD, 0.28)
    } else {
        with_alpha(GOLD, 0.12)
    };

    let background = match status {
        ButtonStatus::Active => Some(Background::Color(base_bg)),
        ButtonStatus::Hovered => Some(Background::Color(hover_bg)),
        ButtonStatus::Pressed => Some(Background::Color(pressed_bg)),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(BG_PANEL, 0.2))),
    };

    ButtonStyle {
        background,
        text_color: LIGHT,
        border: Border::default().rounded(8.0),
        ..ButtonStyle::default()
    }
}

fn playlist_item_style(current: bool, status: ButtonStatus) -> ButtonStyle {
    let active_bg = if current {
        with_alpha(GOLD, 0.18)
    } else {
        Color::TRANSPARENT
    };

    let hover_bg = if current {
        with_alpha(GOLD, 0.24)
    } else {
        with_alpha(BG_PANEL, 0.75)
    };

    let pressed_bg = if current {
        with_alpha(GOLD, 0.32)
    } else {
        with_alpha(GOLD, 0.14)
    };

    let background = match status {
        ButtonStatus::Active => Some(Background::Color(active_bg)),
        ButtonStatus::Hovered => Some(Background::Color(hover_bg)),
        ButtonStatus::Pressed => Some(Background::Color(pressed_bg)),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(BG_PANEL, 0.25))),
    };

    ButtonStyle {
        background,
        text_color: LIGHT,
        border: Border::default().rounded(8.0),
        ..ButtonStyle::default()
    }
}

fn slider_style(_theme: &Theme, status: SliderStatus) -> SliderStyle {
    let active = match status {
        SliderStatus::Active => GOLD,
        SliderStatus::Hovered => with_alpha(GOLD, 0.95),
        SliderStatus::Dragged => with_alpha(GOLD, 0.85),
    };

    SliderStyle {
        rail: SliderRail {
            backgrounds: (
                Background::Color(active),
                Background::Color(with_alpha(MUTED, 0.35)),
            ),
            width: 4.0,
            border: Border::default().rounded(4.0),
        },
        handle: SliderHandle {
            shape: SliderHandleShape::Circle { radius: 7.0 },
            background: Background::Color(LIGHT),
            border_width: 1.0,
            border_color: with_alpha(BG_DARK, 0.65),
        },
    }
}

fn track_subtitle(state: &Kithara) -> String {
    let Some(index) = state.current_track_index else {
        return "Artist / Album unavailable".to_string();
    };
    let Some(path) = state.playlist.track_path(index) else {
        return "Artist / Album unavailable".to_string();
    };

    let path = Path::new(path);
    let album = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|p| p.to_str());
    let artist = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|p| p.to_str());

    match (artist, album) {
        (Some(artist), Some(album)) if !artist.is_empty() && !album.is_empty() => {
            format!("{artist} / {album}")
        }
        (None, Some(album)) if !album.is_empty() => album.to_string(),
        _ => "Artist / Album unavailable".to_string(),
    }
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}
