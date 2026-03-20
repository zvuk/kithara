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

use super::{
    app::Kithara,
    icons::Icon,
    message::{Message, Tab},
};
use crate::theme::gui::GuiPalette;

const OUTER_PADDING: u16 = 18;
const SECTION_PADDING: u16 = 12;

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
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
            .style(panel_style(p))
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .spacing(12);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding::from(OUTER_PADDING))
        .style(root_style(p))
        .into()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped positive f32 -> u32"
)]
fn format_time(seconds: f32) -> String {
    let total = seconds.max(0.0).floor() as u32;
    let minutes = total / 60;
    let remaining = total % 60;
    format!("{minutes:02}:{remaining:02}")
}

fn view_header(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let logo = svg::Svg::new(svg::Handle::from_memory(
        include_bytes!("../../assets/logo.svg") as &[u8],
    ))
    .width(Length::Fixed(48.0))
    .height(Length::Fixed(48.0));

    let title_color = if state.playing { p.accent } else { p.text };

    let title = column![
        text("Kithara").size(28).color(title_color),
        text("Player").size(12).color(p.muted)
    ]
    .spacing(2);

    container(row![logo, title,].align_y(Alignment::Center).spacing(12))
        .width(Length::Fill)
        .padding(SECTION_PADDING)
        .style(panel_style(p))
        .into()
}

fn view_now_playing(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let track_name = if state.track_name.trim().is_empty() {
        "No track loaded".to_string()
    } else {
        state.track_name.clone()
    };

    let subtitle = track_subtitle(state);

    container(
        column![
            row![
                Icon::MusicNote.view(20.0, p.accent),
                text(track_name).size(22).color(p.text)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            text(subtitle).size(14).color(p.muted)
        ]
        .spacing(6),
    )
    .width(Length::Fill)
    .padding(SECTION_PADDING)
    .style(section_style(p))
    .into()
}

fn view_seek(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
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
        .style(slider_style(p))
        .width(Length::Fill);

    container(
        row![
            text(format_time(progress)).size(13).color(p.muted),
            seek,
            text(format_time(duration)).size(13).color(p.muted)
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([4, 8])
    .into()
}

fn view_transport(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let play_icon = if state.playing {
        Icon::Pause
    } else {
        Icon::Play
    };

    let transport_row = row![
        icon_button(Icon::SkipPrev, 30.0, p.text, 8, Message::Prev),
        main_transport_button(play_icon, Message::TogglePlayPause, p),
        icon_button(Icon::SkipNext, 30.0, p.text, 8, Message::Next),
    ]
    .spacing(20)
    .align_y(Alignment::Center);

    let shuffle_color = if state.shuffle_enabled {
        p.accent
    } else {
        p.muted
    };
    let repeat_color = if state.repeat_enabled {
        p.accent
    } else {
        p.muted
    };
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
    let p = state.palette;
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
    .style(slider_style(p))
    .width(Length::Fill);

    container(
        row![
            volume_icon.view(20.0, p.text),
            slider,
            text(volume_pct).size(13).color(p.muted)
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([2, 8])
    .into()
}

fn view_tabs(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
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
        .style(section_style(p))
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
    let p = state.palette;
    if state.playlist.is_empty() {
        return container(text("No tracks in playlist").size(14).color(p.muted))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    let mut tracks = column![].spacing(6).width(Length::Fill);

    for index in 0..state.playlist.len() {
        let is_current = state.current_track_index == Some(index);
        let text_color = if is_current { p.accent } else { p.text };
        let index_color = if is_current { p.accent } else { p.muted };
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
        .style(move |_theme, status| playlist_item_style(p, is_current, status))
        .on_press(Message::SelectTrack(index));

        tracks = tracks.push(item);
    }

    scrollable(tracks).height(Length::Fill).into()
}

fn view_equalizer(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let band_count = state.eq_bands.len();
    let mut bands_row = row![].spacing(26).align_y(Alignment::End);

    for index in 0..band_count {
        let value = state.eq_bands[index].clamp(-24.0, 6.0);
        let value_color = if value.abs() < 0.05 {
            p.muted
        } else {
            p.accent
        };

        let label = eq_band_label(index, band_count);

        let band = column![
            text(format!("{value:+.1} dB")).size(13).color(value_color),
            vertical_slider(-24.0..=6.0, value, move |v| Message::EqBandChanged(
                index, v
            ))
            .step(0.5)
            .height(Length::Fixed(190.0))
            .style(slider_style(p)),
            text(label).size(14).color(p.text),
            button(text("Reset").size(12).color(p.muted))
                .padding([4, 8])
                .style(ghost_button_style(p))
                .on_press(Message::EqBandReset(index))
        ]
        .spacing(8)
        .align_x(Alignment::Center);

        bands_row = bands_row.push(band);
    }

    let title = format!("{band_count}-Band Equalizer");
    container(
        column![
            text(title).size(16).color(p.text),
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

/// Generate a short label for an EQ band by index.
fn eq_band_label(index: usize, total: usize) -> String {
    if total <= 3 {
        return match index {
            0 => "Low".to_string(),
            i if i == total - 1 => "High".to_string(),
            _ => "Mid".to_string(),
        };
    }
    format!("{}", index + 1)
}

fn view_dj(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let secs = state.crossfade.clamp(0.0, 30.0);

    let header = row![
        text("Crossfade").size(16).color(p.text),
        Space::new().width(Length::Fill),
        text(format!("{secs:.1}s")).size(16).color(p.accent),
    ]
    .align_y(Alignment::Center);

    let slider = slider(0.0..=30.0, secs, Message::CrossfadeChanged)
        .step(0.5)
        .style(slider_style(p));

    let labels = row![
        text("Track A").size(13).color(p.muted),
        Space::new().width(Length::Fill),
        text("Track B").size(13).color(p.muted),
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
                .color(p.muted)
        ]
        .spacing(10),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn view_settings(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    container(
        column![
            text("Settings").size(16).color(p.text),
            text("Playback and appearance options will be added here.")
                .size(13)
                .color(p.muted),
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
    let p = GuiPalette::from(crate::theme::Palette::default());
    button(icon.view(size, color))
        .padding(padding)
        .style(ghost_button_style(p))
        .on_press(message)
        .into()
}

fn main_transport_button(icon: Icon, message: Message, p: GuiPalette) -> Element<'static, Message> {
    button(icon.view(35.0, p.bg))
        .padding(14)
        .style(move |_theme, status| {
            let background = match status {
                ButtonStatus::Hovered => with_alpha(p.accent, 0.95),
                ButtonStatus::Pressed => with_alpha(p.accent, 0.75),
                ButtonStatus::Active => p.accent,
                ButtonStatus::Disabled => with_alpha(p.accent, 0.4),
            };

            ButtonStyle {
                background: Some(Background::Color(background)),
                text_color: p.text,
                border: Border::default()
                    .rounded(100.0)
                    .width(1.0)
                    .color(with_alpha(p.text, 0.2)),
                ..ButtonStyle::default()
            }
        })
        .on_press(message)
        .into()
}

fn tab_button(state: &Kithara, tab: Tab, icon: Icon, label: &str) -> Element<'static, Message> {
    let p = state.palette;
    let active = state.active_tab == tab;
    let icon_color = if active { p.accent } else { p.muted };
    let label_color = if active { p.text } else { p.muted };
    let underline_color = if active { p.accent } else { Color::TRANSPARENT };

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
        .style(move |_theme, status| tab_button_style(p, active, status))
        .on_press(Message::TabSelected(tab))
        .into()
}

// ---------------------------------------------------------------------------
// Style helpers — all take GuiPalette and return closures
// ---------------------------------------------------------------------------

fn root_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| ContainerStyle::default().background(p.bg).color(p.text)
}

fn panel_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(p.bg_panel)
            .color(p.text)
            .border(
                Border::default()
                    .rounded(12.0)
                    .width(1.0)
                    .color(with_alpha(p.accent, 0.25)),
            )
    }
}

fn section_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(with_alpha(p.bg_panel, 0.45))
            .color(p.text)
            .border(
                Border::default()
                    .rounded(10.0)
                    .width(1.0)
                    .color(with_alpha(p.muted, 0.2)),
            )
    }
}

fn ghost_button_style(p: GuiPalette) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Active => None,
            ButtonStatus::Hovered => Some(Background::Color(with_alpha(p.bg_panel, 0.85))),
            ButtonStatus::Pressed => Some(Background::Color(with_alpha(p.accent, 0.2))),
            ButtonStatus::Disabled => Some(Background::Color(with_alpha(p.bg_panel, 0.3))),
        };

        ButtonStyle {
            background,
            text_color: p.text,
            border: Border::default().rounded(8.0),
            ..ButtonStyle::default()
        }
    }
}

fn tab_button_style(p: GuiPalette, active: bool, status: ButtonStatus) -> ButtonStyle {
    let base_bg = if active {
        with_alpha(p.accent, 0.12)
    } else {
        Color::TRANSPARENT
    };

    let hover_bg = if active {
        with_alpha(p.accent, 0.2)
    } else {
        with_alpha(p.bg_panel, 0.8)
    };

    let pressed_bg = if active {
        with_alpha(p.accent, 0.28)
    } else {
        with_alpha(p.accent, 0.12)
    };

    let background = match status {
        ButtonStatus::Active => Some(Background::Color(base_bg)),
        ButtonStatus::Hovered => Some(Background::Color(hover_bg)),
        ButtonStatus::Pressed => Some(Background::Color(pressed_bg)),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(p.bg_panel, 0.2))),
    };

    ButtonStyle {
        background,
        text_color: p.text,
        border: Border::default().rounded(8.0),
        ..ButtonStyle::default()
    }
}

fn playlist_item_style(p: GuiPalette, current: bool, status: ButtonStatus) -> ButtonStyle {
    let active_bg = if current {
        with_alpha(p.accent, 0.18)
    } else {
        Color::TRANSPARENT
    };

    let hover_bg = if current {
        with_alpha(p.accent, 0.24)
    } else {
        with_alpha(p.bg_panel, 0.75)
    };

    let pressed_bg = if current {
        with_alpha(p.accent, 0.32)
    } else {
        with_alpha(p.accent, 0.14)
    };

    let background = match status {
        ButtonStatus::Active => Some(Background::Color(active_bg)),
        ButtonStatus::Hovered => Some(Background::Color(hover_bg)),
        ButtonStatus::Pressed => Some(Background::Color(pressed_bg)),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(p.bg_panel, 0.25))),
    };

    ButtonStyle {
        background,
        text_color: p.text,
        border: Border::default().rounded(8.0),
        ..ButtonStyle::default()
    }
}

fn slider_style(p: GuiPalette) -> impl Fn(&Theme, SliderStatus) -> SliderStyle {
    move |_theme, status| {
        let active = match status {
            SliderStatus::Active => p.accent,
            SliderStatus::Hovered => with_alpha(p.accent, 0.95),
            SliderStatus::Dragged => with_alpha(p.accent, 0.85),
        };

        SliderStyle {
            rail: SliderRail {
                backgrounds: (
                    Background::Color(active),
                    Background::Color(with_alpha(p.muted, 0.35)),
                ),
                width: 4.0,
                border: Border::default().rounded(4.0),
            },
            handle: SliderHandle {
                shape: SliderHandleShape::Circle { radius: 7.0 },
                background: Background::Color(p.text),
                border_width: 1.0,
                border_color: with_alpha(p.bg, 0.65),
            },
        }
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
