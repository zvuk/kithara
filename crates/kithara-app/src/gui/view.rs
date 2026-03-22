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
        svg::{Handle as SvgHandle, Svg},
        text, vertical_slider,
    },
};

use super::{
    app::Kithara,
    icons::Icon,
    message::{Message, Tab},
};
use crate::theme::gui::GuiPalette;

// Layout
const OUTER_PADDING: f32 = 18.0;
const SECTION_PADDING: f32 = 12.0;
const SECTION_SPACING: f32 = 12.0;
const COMPACT_SPACING: f32 = 6.0;
const ELEMENT_SPACING: f32 = 10.0;
const SEEK_BAR_PADDING_Y: f32 = 4.0;
const SEEK_BAR_PADDING_X: f32 = 8.0;
const TRANSPORT_PADDING_Y: f32 = 2.0;
const TRANSPORT_PADDING_X: f32 = 8.0;
const TRANSPORT_BUTTON_SPACING: f32 = 20.0;
const TABS_PADDING_Y: f32 = 4.0;
const TAB_CONTENT_PADDING_Y: f32 = 8.0;
const TAB_CONTENT_PADDING_X: f32 = 8.0;
const PLAYLIST_ITEM_PADDING_Y: f32 = 8.0;
const PLAYLIST_ITEM_PADDING_X: f32 = 10.0;
const EQ_BAND_SPACING: f32 = 26.0;
const EQ_RESET_PADDING_Y: f32 = 4.0;
const EQ_RESET_PADDING_X: f32 = 8.0;
const EQ_SPACING: f32 = 8.0;
const DJ_SPACING: f32 = 10.0;
const SETTINGS_SPACING: f32 = 8.0;
const MAIN_BUTTON_PADDING: f32 = 14.0;

// Sizes
const LOGO_SIZE: f32 = 48.0;
const TITLE_FONT: f32 = 28.0;
const SUBTITLE_FONT: f32 = 12.0;
const TITLE_SPACING: f32 = 2.0;
const MUSIC_NOTE_SIZE: f32 = 20.0;
const TRACK_NAME_FONT: f32 = 22.0;
const CAPTION_FONT: f32 = 14.0;
const SMALL_FONT: f32 = 13.0;
const TRANSPORT_ICON_SIZE: f32 = 30.0;
const TRANSPORT_ICON_PADDING: f32 = 8.0;
const TOGGLE_ICON_SIZE: f32 = 22.0;
const TOGGLE_ICON_PADDING: f32 = 6.0;
const MAIN_TRANSPORT_ICON_SIZE: f32 = 35.0;
const TAB_ICON_SIZE: f32 = 18.0;
const UNDERLINE_HEIGHT: f32 = 2.0;
const VOLUME_ICON_SIZE: f32 = 20.0;
const EQ_SLIDER_HEIGHT: f32 = 190.0;
const EQ_LABEL_FONT: f32 = 14.0;
const EQ_RESET_FONT: f32 = 12.0;
const EQ_VALUE_FONT: f32 = 13.0;
const HEADING_FONT: f32 = 16.0;
const PLAYLIST_INDEX_FONT: f32 = 13.0;
const PLAYLIST_TRACK_FONT: f32 = 15.0;
const PLAYLIST_SPACING: f32 = 6.0;
const PLAYLIST_MAX_NAME_CHARS: usize = 40;
const DJ_LABEL_FONT: f32 = 13.0;
const DJ_SPACER_HEIGHT: f32 = 8.0;
const DJ_HINT_FONT: f32 = 13.0;
const SETTINGS_BODY_FONT: f32 = 13.0;
const EMPTY_PLAYLIST_FONT: f32 = 14.0;

// Slider
const SLIDER_RAIL_WIDTH: f32 = 4.0;
const SLIDER_RAIL_RADIUS: f32 = 4.0;
const SLIDER_HANDLE_RADIUS: f32 = 7.0;
const SLIDER_HANDLE_BORDER: f32 = 1.0;

// EQ range
const EQ_MIN_DB: f32 = -24.0;
const EQ_MAX_DB: f32 = 6.0;
const EQ_STEP: f32 = 0.5;
const EQ_ZERO_THRESHOLD: f32 = 0.05;

// Crossfade range
const CROSSFADE_MAX: f32 = 30.0;
const CROSSFADE_STEP: f32 = 0.5;

// Volume
const VOLUME_MUTE_THRESHOLD: f32 = 0.01;
const VOLUME_LOW_THRESHOLD: f32 = 0.5;
const VOLUME_STEP_SIZE: f32 = 0.01;
const VOLUME_PERCENT_SCALE: f32 = 100.0;

// Seek
const SEEK_STEP: f32 = 0.1;

// Border
const BORDER_RADIUS: f32 = 12.0;
const BORDER_RADIUS_SECTION: f32 = 10.0;
const BORDER_RADIUS_BUTTON: f32 = 8.0;
const BORDER_WIDTH: f32 = 1.0;
const BORDER_RADIUS_CIRCLE: f32 = 100.0;

// Alpha values
const ALPHA_ACCENT_BORDER: f32 = 0.25;
const ALPHA_SECTION_BG: f32 = 0.45;
const ALPHA_SECTION_BORDER: f32 = 0.2;
const ALPHA_GHOST_HOVER: f32 = 0.85;
const ALPHA_GHOST_PRESSED: f32 = 0.2;
const ALPHA_GHOST_DISABLED: f32 = 0.3;
const ALPHA_MAIN_HOVER: f32 = 0.95;
const ALPHA_MAIN_PRESSED: f32 = 0.75;
const ALPHA_MAIN_DISABLED: f32 = 0.4;
const ALPHA_MAIN_BORDER: f32 = 0.2;
const ALPHA_TAB_ACTIVE_BASE: f32 = 0.12;
const ALPHA_TAB_ACTIVE_HOVER: f32 = 0.2;
const ALPHA_TAB_ACTIVE_PRESSED: f32 = 0.28;
const ALPHA_TAB_INACTIVE_HOVER: f32 = 0.8;
const ALPHA_TAB_INACTIVE_PRESSED: f32 = 0.12;
const ALPHA_TAB_DISABLED: f32 = 0.2;
const ALPHA_PLAYLIST_CURRENT: f32 = 0.18;
const ALPHA_PLAYLIST_CURRENT_HOVER: f32 = 0.24;
const ALPHA_PLAYLIST_CURRENT_PRESSED: f32 = 0.32;
const ALPHA_PLAYLIST_INACTIVE_HOVER: f32 = 0.75;
const ALPHA_PLAYLIST_INACTIVE_PRESSED: f32 = 0.14;
const ALPHA_PLAYLIST_DISABLED: f32 = 0.25;
const ALPHA_SLIDER_ACTIVE_RAIL: f32 = 0.95;
const ALPHA_SLIDER_DRAGGED_RAIL: f32 = 0.85;
const ALPHA_SLIDER_INACTIVE_RAIL: f32 = 0.35;
const ALPHA_SLIDER_HANDLE_BORDER: f32 = 0.65;

// Time
const SECONDS_PER_MINUTE: u32 = 60;

// EQ band count threshold for simple labels
const EQ_SIMPLE_LABEL_THRESHOLD: usize = 3;

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
    .spacing(SECTION_SPACING);

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
    let minutes = total / SECONDS_PER_MINUTE;
    let remaining = total % SECONDS_PER_MINUTE;
    format!("{minutes:02}:{remaining:02}")
}

fn view_header(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let logo = Svg::new(SvgHandle::from_memory(
        include_bytes!("../../assets/logo.svg") as &[u8],
    ))
    .width(Length::Fixed(LOGO_SIZE))
    .height(Length::Fixed(LOGO_SIZE));

    let title_color = if state.playing { p.accent } else { p.text };

    let title = column![
        text("Kithara").size(TITLE_FONT).color(title_color),
        text("Player").size(SUBTITLE_FONT).color(p.muted)
    ]
    .spacing(TITLE_SPACING);

    container(
        row![logo, title,]
            .align_y(Alignment::Center)
            .spacing(SECTION_SPACING),
    )
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
                Icon::MusicNote.view(MUSIC_NOTE_SIZE, p.accent),
                text(track_name).size(TRACK_NAME_FONT).color(p.text)
            ]
            .spacing(ELEMENT_SPACING)
            .align_y(Alignment::Center),
            text(subtitle).size(CAPTION_FONT).color(p.muted)
        ]
        .spacing(COMPACT_SPACING),
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
        .step(SEEK_STEP)
        .on_release(Message::SeekReleased)
        .style(slider_style(p))
        .width(Length::Fill);

    container(
        row![
            text(format_time(progress)).size(SMALL_FONT).color(p.muted),
            seek,
            text(format_time(duration)).size(SMALL_FONT).color(p.muted)
        ]
        .spacing(ELEMENT_SPACING)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([SEEK_BAR_PADDING_Y, SEEK_BAR_PADDING_X])
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
        icon_button(
            Icon::SkipPrev,
            TRANSPORT_ICON_SIZE,
            p.text,
            TRANSPORT_ICON_PADDING,
            Message::Prev
        ),
        main_transport_button(play_icon, Message::TogglePlayPause, p),
        icon_button(
            Icon::SkipNext,
            TRANSPORT_ICON_SIZE,
            p.text,
            TRANSPORT_ICON_PADDING,
            Message::Next
        ),
    ]
    .spacing(TRANSPORT_BUTTON_SPACING)
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
            TOGGLE_ICON_SIZE,
            shuffle_color,
            TOGGLE_ICON_PADDING,
            Message::ToggleShuffle
        ),
        Space::new().width(Length::Fill),
        icon_button(
            repeat_icon,
            TOGGLE_ICON_SIZE,
            repeat_color,
            TOGGLE_ICON_PADDING,
            Message::ToggleRepeat
        ),
    ]
    .width(Length::Fill)
    .align_y(Alignment::Center);

    container(
        column![transport_row, toggles_row]
            .spacing(ELEMENT_SPACING)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([TRANSPORT_PADDING_Y, TRANSPORT_PADDING_X])
    .into()
}

fn view_volume(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let volume_icon = if state.volume <= VOLUME_MUTE_THRESHOLD {
        Icon::VolumeMute
    } else if state.volume < VOLUME_LOW_THRESHOLD {
        Icon::VolumeLow
    } else {
        Icon::VolumeHigh
    };

    let volume_pct = format!(
        "{:.0}%",
        state.volume.clamp(0.0, 1.0) * VOLUME_PERCENT_SCALE
    );

    let slider = slider(
        0.0..=1.0,
        state.volume.clamp(0.0, 1.0),
        Message::VolumeChanged,
    )
    .step(VOLUME_STEP_SIZE)
    .style(slider_style(p))
    .width(Length::Fill);

    container(
        row![
            volume_icon.view(VOLUME_ICON_SIZE, p.text),
            slider,
            text(volume_pct).size(SMALL_FONT).color(p.muted)
        ]
        .spacing(ELEMENT_SPACING)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([TRANSPORT_PADDING_Y, TRANSPORT_PADDING_X])
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
    .spacing(COMPACT_SPACING)
    .width(Length::Fill);

    container(tabs)
        .width(Length::Fill)
        .padding([TABS_PADDING_Y, 0.0])
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
        return container(
            text("No tracks in playlist")
                .size(EMPTY_PLAYLIST_FONT)
                .color(p.muted),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into();
    }

    let mut tracks = column![].spacing(PLAYLIST_SPACING).width(Length::Fill);

    for index in 0..state.playlist.len() {
        let is_current = state.current_track_index == Some(index);
        let text_color = if is_current { p.accent } else { p.text };
        let index_color = if is_current { p.accent } else { p.muted };
        let track_name = truncate_name(&state.playlist.track_name(index), PLAYLIST_MAX_NAME_CHARS);

        let item = button(
            row![
                text(format!("{:02}", index + 1))
                    .size(PLAYLIST_INDEX_FONT)
                    .color(index_color),
                text(track_name).size(PLAYLIST_TRACK_FONT).color(text_color),
            ]
            .spacing(ELEMENT_SPACING)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([PLAYLIST_ITEM_PADDING_Y, PLAYLIST_ITEM_PADDING_X])
        .style(move |_theme, status| playlist_item_style(p, is_current, status))
        .on_press(Message::SelectTrack(index));

        tracks = tracks.push(item);
    }

    scrollable(tracks).height(Length::Fill).into()
}

fn view_equalizer(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let band_count = state.eq_bands.len();
    let mut bands_row = row![].spacing(EQ_BAND_SPACING).align_y(Alignment::End);

    for index in 0..band_count {
        let value = state.eq_bands[index].clamp(EQ_MIN_DB, EQ_MAX_DB);
        let value_color = if value.abs() < EQ_ZERO_THRESHOLD {
            p.muted
        } else {
            p.accent
        };

        let label = eq_band_label(index, band_count);

        let band = column![
            text(format!("{value:+.1} dB"))
                .size(EQ_VALUE_FONT)
                .color(value_color),
            vertical_slider(EQ_MIN_DB..=EQ_MAX_DB, value, move |v| {
                Message::EqBandChanged(index, v)
            })
            .step(EQ_STEP)
            .height(Length::Fixed(EQ_SLIDER_HEIGHT))
            .style(slider_style(p)),
            text(label).size(EQ_LABEL_FONT).color(p.text),
            button(text("Reset").size(EQ_RESET_FONT).color(p.muted))
                .padding([EQ_RESET_PADDING_Y, EQ_RESET_PADDING_X])
                .style(ghost_button_style(p))
                .on_press(Message::EqBandReset(index))
        ]
        .spacing(EQ_SPACING)
        .align_x(Alignment::Center);

        bands_row = bands_row.push(band);
    }

    let title = format!("{band_count}-Band Equalizer");
    container(
        column![
            text(title).size(HEADING_FONT).color(p.text),
            container(bands_row)
                .width(Length::Fill)
                .center_x(Length::Fill)
        ]
        .spacing(SECTION_SPACING),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Generate a short label for an EQ band by index.
fn eq_band_label(index: usize, total: usize) -> String {
    if total <= EQ_SIMPLE_LABEL_THRESHOLD {
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
    let secs = state.crossfade.clamp(0.0, CROSSFADE_MAX);

    let header = row![
        text("Crossfade").size(HEADING_FONT).color(p.text),
        Space::new().width(Length::Fill),
        text(format!("{secs:.1}s"))
            .size(HEADING_FONT)
            .color(p.accent),
    ]
    .align_y(Alignment::Center);

    let slider = slider(0.0..=CROSSFADE_MAX, secs, Message::CrossfadeChanged)
        .step(CROSSFADE_STEP)
        .style(slider_style(p));

    let labels = row![
        text("Track A").size(DJ_LABEL_FONT).color(p.muted),
        Space::new().width(Length::Fill),
        text("Track B").size(DJ_LABEL_FONT).color(p.muted),
    ]
    .align_y(Alignment::Center);

    container(
        column![
            header,
            slider,
            labels,
            Space::new().height(Length::Fixed(DJ_SPACER_HEIGHT)),
            text("Blend outgoing and incoming tracks with a smooth DJ transition.")
                .size(DJ_HINT_FONT)
                .color(p.muted)
        ]
        .spacing(DJ_SPACING),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn view_settings(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    container(
        column![
            text("Settings").size(HEADING_FONT).color(p.text),
            text("Playback and appearance options will be added here.")
                .size(SETTINGS_BODY_FONT)
                .color(p.muted),
        ]
        .spacing(SETTINGS_SPACING),
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
    padding: f32,
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
    button(icon.view(MAIN_TRANSPORT_ICON_SIZE, p.bg))
        .padding(MAIN_BUTTON_PADDING)
        .style(move |_theme, status| {
            let background = match status {
                ButtonStatus::Hovered => with_alpha(p.accent, ALPHA_MAIN_HOVER),
                ButtonStatus::Pressed => with_alpha(p.accent, ALPHA_MAIN_PRESSED),
                ButtonStatus::Active => p.accent,
                ButtonStatus::Disabled => with_alpha(p.accent, ALPHA_MAIN_DISABLED),
            };

            ButtonStyle {
                background: Some(Background::Color(background)),
                text_color: p.text,
                border: Border::default()
                    .rounded(BORDER_RADIUS_CIRCLE)
                    .width(BORDER_WIDTH)
                    .color(with_alpha(p.text, ALPHA_MAIN_BORDER)),
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
            icon.view(TAB_ICON_SIZE, icon_color),
            text(label.to_string()).size(SMALL_FONT).color(label_color)
        ]
        .spacing(COMPACT_SPACING)
        .align_y(Alignment::Center),
        container(
            Space::new()
                .height(Length::Fixed(UNDERLINE_HEIGHT))
                .width(Length::Fill)
        )
        .style(move |_theme| ContainerStyle::default().background(underline_color))
    ]
    .spacing(COMPACT_SPACING)
    .align_x(Alignment::Center);

    button(content)
        .width(Length::FillPortion(1))
        .padding([TAB_CONTENT_PADDING_Y, TAB_CONTENT_PADDING_X])
        .style(move |_theme, status| tab_button_style(p, active, status))
        .on_press(Message::TabSelected(tab))
        .into()
}

// Style helpers — all take GuiPalette and return closures

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
                    .rounded(BORDER_RADIUS)
                    .width(BORDER_WIDTH)
                    .color(with_alpha(p.accent, ALPHA_ACCENT_BORDER)),
            )
    }
}

fn section_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(with_alpha(p.bg_panel, ALPHA_SECTION_BG))
            .color(p.text)
            .border(
                Border::default()
                    .rounded(BORDER_RADIUS_SECTION)
                    .width(BORDER_WIDTH)
                    .color(with_alpha(p.muted, ALPHA_SECTION_BORDER)),
            )
    }
}

fn ghost_button_style(p: GuiPalette) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Active => None,
            ButtonStatus::Hovered => {
                Some(Background::Color(with_alpha(p.bg_panel, ALPHA_GHOST_HOVER)))
            }
            ButtonStatus::Pressed => {
                Some(Background::Color(with_alpha(p.accent, ALPHA_GHOST_PRESSED)))
            }
            ButtonStatus::Disabled => Some(Background::Color(with_alpha(
                p.bg_panel,
                ALPHA_GHOST_DISABLED,
            ))),
        };

        ButtonStyle {
            background,
            text_color: p.text,
            border: Border::default().rounded(BORDER_RADIUS_BUTTON),
            ..ButtonStyle::default()
        }
    }
}

fn tab_button_style(p: GuiPalette, active: bool, status: ButtonStatus) -> ButtonStyle {
    let base_bg = if active {
        with_alpha(p.accent, ALPHA_TAB_ACTIVE_BASE)
    } else {
        Color::TRANSPARENT
    };

    let hover_bg = if active {
        with_alpha(p.accent, ALPHA_TAB_ACTIVE_HOVER)
    } else {
        with_alpha(p.bg_panel, ALPHA_TAB_INACTIVE_HOVER)
    };

    let pressed_bg = if active {
        with_alpha(p.accent, ALPHA_TAB_ACTIVE_PRESSED)
    } else {
        with_alpha(p.accent, ALPHA_TAB_INACTIVE_PRESSED)
    };

    let background = match status {
        ButtonStatus::Active => Some(Background::Color(base_bg)),
        ButtonStatus::Hovered => Some(Background::Color(hover_bg)),
        ButtonStatus::Pressed => Some(Background::Color(pressed_bg)),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(
            p.bg_panel,
            ALPHA_TAB_DISABLED,
        ))),
    };

    ButtonStyle {
        background,
        text_color: p.text,
        border: Border::default().rounded(BORDER_RADIUS_BUTTON),
        ..ButtonStyle::default()
    }
}

fn playlist_item_style(p: GuiPalette, current: bool, status: ButtonStatus) -> ButtonStyle {
    let active_bg = if current {
        with_alpha(p.accent, ALPHA_PLAYLIST_CURRENT)
    } else {
        Color::TRANSPARENT
    };

    let hover_bg = if current {
        with_alpha(p.accent, ALPHA_PLAYLIST_CURRENT_HOVER)
    } else {
        with_alpha(p.bg_panel, ALPHA_PLAYLIST_INACTIVE_HOVER)
    };

    let pressed_bg = if current {
        with_alpha(p.accent, ALPHA_PLAYLIST_CURRENT_PRESSED)
    } else {
        with_alpha(p.accent, ALPHA_PLAYLIST_INACTIVE_PRESSED)
    };

    let background = match status {
        ButtonStatus::Active => Some(Background::Color(active_bg)),
        ButtonStatus::Hovered => Some(Background::Color(hover_bg)),
        ButtonStatus::Pressed => Some(Background::Color(pressed_bg)),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(
            p.bg_panel,
            ALPHA_PLAYLIST_DISABLED,
        ))),
    };

    ButtonStyle {
        background,
        text_color: p.text,
        border: Border::default().rounded(BORDER_RADIUS_BUTTON),
        ..ButtonStyle::default()
    }
}

fn slider_style(p: GuiPalette) -> impl Fn(&Theme, SliderStatus) -> SliderStyle {
    move |_theme, status| {
        let active = match status {
            SliderStatus::Active => p.accent,
            SliderStatus::Hovered => with_alpha(p.accent, ALPHA_SLIDER_ACTIVE_RAIL),
            SliderStatus::Dragged => with_alpha(p.accent, ALPHA_SLIDER_DRAGGED_RAIL),
        };

        SliderStyle {
            rail: SliderRail {
                backgrounds: (
                    Background::Color(active),
                    Background::Color(with_alpha(p.muted, ALPHA_SLIDER_INACTIVE_RAIL)),
                ),
                width: SLIDER_RAIL_WIDTH,
                border: Border::default().rounded(SLIDER_RAIL_RADIUS),
            },
            handle: SliderHandle {
                shape: SliderHandleShape::Circle {
                    radius: SLIDER_HANDLE_RADIUS,
                },
                background: Background::Color(p.text),
                border_width: SLIDER_HANDLE_BORDER,
                border_color: with_alpha(p.bg, ALPHA_SLIDER_HANDLE_BORDER),
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
