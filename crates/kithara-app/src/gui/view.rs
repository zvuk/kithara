use std::path::Path;

use iced::{
    Alignment, Background, Border, Color, Degrees, Element, Gradient, Length, Padding, Theme,
    font::Weight,
    gradient::Linear,
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
use kithara_queue::RepeatMode;
use num_traits::cast::ToPrimitive;

use super::{
    app::Kithara,
    dj::DjMsg,
    fonts,
    icons::Icon,
    message::{Message, Tab},
    tokens::Gap,
    widgets,
};
use crate::{theme::gui::GuiPalette, track::TrackRow};

const ALBUM_PLACEHOLDER_SVG: &[u8] = include_bytes!("../../assets/album-placeholder.svg");

struct Consts;
impl Consts {
    const ALPHA_GHOST_DISABLED: f32 = 0.3;
    const ALPHA_GHOST_HOVER: f32 = 0.85;
    const ALPHA_GHOST_PRESSED: f32 = 0.2;
    const ALPHA_PLAYLIST_DISABLED: f32 = 0.25;
    const ALPHA_PLAYLIST_INACTIVE_HOVER: f32 = 0.75;
    const ALPHA_PLAYLIST_INACTIVE_PRESSED: f32 = 0.14;
    const ALPHA_PLAYLIST_SELECTED: f32 = 0.18;
    const ALPHA_PLAYLIST_SELECTED_HOVER: f32 = 0.24;
    const ALPHA_PLAYLIST_SELECTED_PRESSED: f32 = 0.32;
    const ALPHA_SECTION_BG: f32 = 0.45;
    const ALPHA_SLIDER_ACTIVE_RAIL: f32 = 0.95;
    const ALPHA_SLIDER_DRAGGED_RAIL: f32 = 0.85;
    const ALPHA_SLIDER_HANDLE_BORDER: f32 = 0.65;
    const ALPHA_SLIDER_INACTIVE_RAIL: f32 = 0.35;
    const ALPHA_SPEED_FILL: f32 = 0.4;

    const ALPHA_TAB_ACTIVE_BASE: f32 = 0.12;
    const ALPHA_TAB_ACTIVE_HOVER: f32 = 0.2;
    const ALPHA_TAB_ACTIVE_PRESSED: f32 = 0.28;
    const ALPHA_TAB_DISABLED: f32 = 0.2;
    const ALPHA_TAB_INACTIVE_HOVER: f32 = 0.8;
    const ALPHA_TAB_INACTIVE_PRESSED: f32 = 0.12;
    const ALPHA_TAB_STRIP_FILL: f32 = 0.5;
    const BLINK_DIVISOR: u8 = 5;
    const BLINK_PERIOD: u64 = 2;
    const BORDER_RADIUS: f32 = 12.0;
    const BORDER_RADIUS_BUTTON: f32 = 8.0;
    const BORDER_RADIUS_CIRCLE: f32 = 100.0;
    const BORDER_RADIUS_PILL: f32 = 4.0;
    const BORDER_RADIUS_SECTION: f32 = 10.0;
    const BORDER_WIDTH: f32 = 1.0;
    /// Crossfade slider upper bound — mirrors the iOS reference (0…8s).
    const CROSSFADE_MAX: f32 = 8.0;
    const CROSSFADE_STEP: f32 = 0.5;
    const DJ_LAUNCH_GAP: f32 = 7.0;
    const DJ_LAUNCH_ICON: f32 = 14.0;
    const DJ_LAUNCH_PADDING_X: f32 = 10.0;
    const DJ_LAUNCH_PADDING_Y: f32 = 7.0;
    const EMPTY_PLAYLIST_FONT: f32 = 14.0;
    /// Minimum gap between bands. Anything beyond it gets soaked up by
    /// `Space::Fill` spacers so the row reads as "evenly spread".
    const EQ_BAND_MIN_GAP: f32 = 2.0;
    /// Width of a single EQ band column — kept stable so the slider
    /// rails stay readable. The window is allowed to grow; flex
    /// spacers between bands absorb the extra width instead of
    /// stretching the bands themselves.
    const EQ_BAND_WIDTH: f32 = 28.0;
    const EQ_LABEL_FONT: f32 = 11.0;
    const EQ_MAX_DB: f32 = 6.0;
    const EQ_MIN_DB: f32 = -24.0;
    const EQ_RESET_FONT: f32 = 12.0;
    const EQ_RESET_PADDING_X: f32 = 8.0;

    const EQ_RESET_PADDING_Y: f32 = 4.0;
    const EQ_SIMPLE_LABEL_THRESHOLD: usize = 3;

    const EQ_STEP: f32 = 0.5;
    const EQ_VALUE_FONT: f32 = 9.0;

    const EQ_ZERO_THRESHOLD: f32 = 0.05;
    const HEADING_FONT: f32 = 16.0;
    const NOW_BITRATE_DOT_SIZE: f32 = 4.0;
    const NOW_BITRATE_FONT: f32 = 10.0;
    const NOW_COVER_SIZE: f32 = 88.0;
    const NOW_GRADIENT_END_ALPHA: f32 = 0.9;
    const NOW_GRADIENT_START_ALPHA: f32 = 0.8;

    const OUTER_PADDING: f32 = 18.0;
    const PLAYLIST_INDEX_FONT: f32 = 13.0;
    const PLAYLIST_ITEM_PADDING_X: f32 = 10.0;
    const PLAYLIST_ITEM_PADDING_Y: f32 = 8.0;

    const PLAYLIST_MAX_NAME_CHARS: usize = 40;

    const PILL_PADDING_X: f32 = 8.0;
    const PILL_PADDING_Y: f32 = 4.0;
    const PLAYLIST_TRACK_FONT: f32 = 15.0;
    const SECONDS_PER_MINUTE: u32 = 60;

    const SECTION_PADDING: f32 = 12.0;
    const SEEK_STEP: f32 = 0.1;
    const SETTINGS_BODY_FONT: f32 = 13.0;
    const SETTINGS_LABEL_FONT: f32 = 10.0;
    const SLIDER_HANDLE_BORDER: f32 = 1.0;
    const SLIDER_HANDLE_RADIUS: f32 = 7.0;
    const SLIDER_RAIL_RADIUS: f32 = 4.0;
    const SLIDER_RAIL_WIDTH: f32 = 4.0;
    const SMALL_FONT: f32 = 13.0;
    const SPEED_PADDING_X: f32 = 12.0;
    const SPEED_PADDING_Y: f32 = 8.0;
    const SUBTITLE_FONT: f32 = 12.0;
    const TABS_PADDING_Y: f32 = 6.0;
    const TAB_CONTENT_PADDING_X: f32 = 8.0;
    const TAB_CONTENT_PADDING_Y: f32 = 8.0;
    const TAB_ICON_SIZE: f32 = 18.0;
    const TOGGLE_ICON_PADDING: f32 = 6.0;
    const TRACK_NAME_FONT: f32 = 20.0;
    const TRANSPORT_BTN_BASE: f32 = 36.0;
    const TRANSPORT_BTN_LG: f32 = 44.0;
    const TRANSPORT_GAP: f32 = 14.0;
    const TRANSPORT_PRIMARY_ICON: f32 = 22.0;
    const TRANSPORT_TOGGLE_ICON: f32 = 18.0;
    const ALPHA_TRANSPORT_HOVER: f32 = 0.6;
    const ALPHA_TRANSPORT_PRESSED: f32 = 0.45;

    const VOLUME_ICON_SIZE: f32 = 20.0;
    const VOLUME_LOW_THRESHOLD: f32 = 0.5;

    const VOLUME_MUTE_THRESHOLD: f32 = 0.01;
    const VOLUME_PERCENT_SCALE: f32 = 100.0;
    const VOLUME_STEP_SIZE: f32 = 0.01;
}

pub(crate) fn view(state: &Kithara, _window: iced::window::Id) -> Element<'_, Message> {
    if state.dj.open {
        return super::studio::view_dj_studio(state);
    }

    let p = state.palette;
    let content = column![
        view_header(state),
        super::url_bar::view(state),
        view_now_playing(state),
        view_seek(state),
        view_transport(state),
        view_speed(state),
        view_volume(state),
        view_tabs(state),
        container(view_tab_content(state))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Consts::SECTION_PADDING)
            .style(panel_style(p))
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .spacing(Gap::SECTION);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding::from(Consts::OUTER_PADDING))
        .style(root_style(p))
        .into()
}

pub(crate) fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0).floor().to_u32().unwrap_or(u32::MAX);
    let minutes = total / Consts::SECONDS_PER_MINUTE;
    let remaining = total % Consts::SECONDS_PER_MINUTE;
    format!("{minutes:02}:{remaining:02}")
}

fn view_header(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;

    container(
        row![
            super::studio::brand_mark(p, "PLAYER"),
            Space::new().width(Length::Fill),
            dj_studio_button(p),
        ]
        .align_y(Alignment::Center)
        .spacing(Gap::SECTION),
    )
    .width(Length::Fill)
    .into()
}

/// Compact-header entry point to the DJ Studio. The studio has no tab of
/// its own on this layout, so this button is the only way in.
fn dj_studio_button(p: GuiPalette) -> Element<'static, Message> {
    button(
        row![
            Icon::Disc.view(Consts::DJ_LAUNCH_ICON, p.accent),
            text("DJ Studio").size(Consts::SMALL_FONT).color(p.text),
        ]
        .spacing(Consts::DJ_LAUNCH_GAP)
        .align_y(Alignment::Center),
    )
    .padding([Consts::DJ_LAUNCH_PADDING_Y, Consts::DJ_LAUNCH_PADDING_X])
    .style(super::studio::ghost_button_style(p))
    .on_press(Message::Dj(DjMsg::Toggle))
    .into()
}

fn view_now_playing(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let track_name = if state.ui_state.track_name.trim().is_empty() {
        "No track loaded".to_string()
    } else {
        state.ui_state.track_name.clone()
    };

    let subtitle = track_subtitle(state);

    let cover = Svg::new(SvgHandle::from_memory(ALBUM_PLACEHOLDER_SVG))
        .width(Length::Fixed(Consts::NOW_COVER_SIZE))
        .height(Length::Fixed(Consts::NOW_COVER_SIZE));

    let mut meta = column![
        text(track_name)
            .size(Consts::TRACK_NAME_FONT)
            .font(fonts::display(Weight::Semibold))
            .color(p.text),
        text(subtitle)
            .size(Consts::SMALL_FONT)
            .font(fonts::SANS)
            .color(p.text_dim),
    ]
    .spacing(Gap::INLINE_TIGHT)
    .width(Length::Fill);

    if !state.ui_state.variant_label.is_empty() {
        let dot = container(Space::new())
            .width(Length::Fixed(Consts::NOW_BITRATE_DOT_SIZE))
            .height(Length::Fixed(Consts::NOW_BITRATE_DOT_SIZE))
            .style(dot_style(p));
        meta = meta.push(
            row![
                dot,
                text(state.ui_state.variant_label.clone())
                    .size(Consts::NOW_BITRATE_FONT)
                    .font(fonts::MONO)
                    .color(p.muted),
            ]
            .spacing(Gap::INLINE_WIDE)
            .align_y(Alignment::Center),
        );
    }

    container(
        row![cover, meta]
            .spacing(Gap::SECTION_ROOMY)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(Gap::SECTION_ROOMY)
    .style(now_card_style(p))
    .into()
}

fn view_seek(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let duration = state.ui_state.duration.max(0.0);
    let slider_max = duration.max(1.0);
    let progress = if state.ui_state.is_seeking {
        state.ui_state.seek_position
    } else {
        state.ui_state.position
    }
    .clamp(0.0, slider_max);

    let seek = slider(0.0..=slider_max, progress, Message::SeekChanged)
        .step(Consts::SEEK_STEP)
        .on_release(Message::SeekReleased)
        .style(slider_style(p))
        .width(Length::Fill);

    container(
        row![
            text(format_time(progress))
                .size(Consts::SMALL_FONT)
                .font(fonts::MONO)
                .color(p.muted),
            seek,
            text(format_time(duration))
                .size(Consts::SMALL_FONT)
                .font(fonts::MONO)
                .color(p.muted)
        ]
        .spacing(Gap::CONTENT)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .into()
}

fn view_transport(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;

    let shuffle_active = state.ui_state.shuffle_enabled;
    // `RepeatMode` is non_exhaustive; anything other than `Off` reads as an
    // active loop. `One` gets the distinct single-repeat glyph.
    let repeat_active = !matches!(state.ui_state.repeat_mode, RepeatMode::Off);
    let repeat_icon = match state.ui_state.repeat_mode {
        RepeatMode::One => Icon::RepeatOnce,
        _ => Icon::Repeat,
    };

    let transport_row = row![
        transport_square_button(
            Icon::Shuffle,
            Consts::TRANSPORT_TOGGLE_ICON,
            Consts::TRANSPORT_BTN_BASE,
            p,
            shuffle_active,
            Message::ToggleShuffle,
        ),
        transport_square_button(
            Icon::SkipPrev,
            Consts::TRANSPORT_PRIMARY_ICON,
            Consts::TRANSPORT_BTN_LG,
            p,
            false,
            Message::Prev,
        ),
        widgets::play_button(state.ui_state.playing, p, Message::TogglePlayPause),
        transport_square_button(
            Icon::SkipNext,
            Consts::TRANSPORT_PRIMARY_ICON,
            Consts::TRANSPORT_BTN_LG,
            p,
            false,
            Message::Next,
        ),
        transport_square_button(
            repeat_icon,
            Consts::TRANSPORT_TOGGLE_ICON,
            Consts::TRANSPORT_BTN_BASE,
            p,
            repeat_active,
            Message::ToggleRepeat,
        ),
    ]
    .spacing(Consts::TRANSPORT_GAP)
    .align_y(Alignment::Center);

    container(transport_row)
        .width(Length::Fill)
        .center_x(Length::Fill)
        .into()
}

fn view_speed(state: &Kithara) -> Element<'_, Message> {
    // A dead-band around 1.0x keeps the RESET pill from flickering while
    // dragging through unity.
    const RESET_DEADBAND: f32 = 0.06;
    let p = state.palette;
    let rate = state.ui_state.selected_rate;

    let mut controls = row![
        text("SPEED")
            .size(Consts::SUBTITLE_FONT)
            .font(fonts::mono(Weight::Medium))
            .color(p.text_dim),
        widgets::speed_slider(rate, p),
        text(format!("{rate:.2}\u{00d7}"))
            .size(Consts::SMALL_FONT)
            .font(fonts::mono(Weight::Medium))
            .color(p.accent),
    ]
    .spacing(Gap::CONTENT)
    .align_y(Alignment::Center);

    if (rate - 1.0).abs() > RESET_DEADBAND {
        controls = controls.push(
            button(
                text("RESET")
                    .size(Consts::EQ_RESET_FONT)
                    .font(fonts::mono(Weight::Medium))
                    .color(p.muted),
            )
            .padding([Consts::EQ_RESET_PADDING_Y, Consts::EQ_RESET_PADDING_X])
            .style(ghost_button_style(p))
            .on_press(Message::PlayRateChanged(1.0)),
        );
    }

    container(controls)
        .width(Length::Fill)
        .padding([Consts::SPEED_PADDING_Y, Consts::SPEED_PADDING_X])
        .style(speed_control_style(p))
        .into()
}

fn view_volume(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let volume_icon = if state.ui_state.volume <= Consts::VOLUME_MUTE_THRESHOLD {
        Icon::VolumeMute
    } else if state.ui_state.volume < Consts::VOLUME_LOW_THRESHOLD {
        Icon::VolumeLow
    } else {
        Icon::VolumeHigh
    };

    let volume_pct = format!(
        "{:.0}%",
        state.ui_state.volume.clamp(0.0, 1.0) * Consts::VOLUME_PERCENT_SCALE
    );

    let slider = slider(
        0.0..=1.0,
        state.ui_state.volume.clamp(0.0, 1.0),
        Message::VolumeChanged,
    )
    .step(Consts::VOLUME_STEP_SIZE)
    .style(slider_style(p))
    .width(Length::Fill);

    container(
        row![
            icon_button(
                volume_icon,
                Consts::VOLUME_ICON_SIZE,
                if state.ui_state.volume <= Consts::VOLUME_MUTE_THRESHOLD {
                    p.muted
                } else {
                    p.accent
                },
                Consts::TOGGLE_ICON_PADDING,
                Message::ToggleMute
            ),
            slider,
            text(volume_pct)
                .size(Consts::SMALL_FONT)
                .font(fonts::MONO)
                .color(p.muted)
        ]
        .spacing(Gap::CONTENT)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .into()
}

fn view_tabs(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let tabs = row![
        tab_button(state, Tab::Playlist, Icon::Playlist, "Playlist"),
        tab_button(state, Tab::Equalizer, Icon::Equalizer, "EQ"),
        tab_button(state, Tab::Settings, Icon::Settings, "Settings"),
    ]
    .spacing(Gap::INLINE)
    .width(Length::Fill);

    container(tabs)
        .width(Length::Fill)
        .padding(Consts::TABS_PADDING_Y)
        .style(tab_strip_style(p))
        .into()
}

fn view_tab_content(state: &Kithara) -> Element<'_, Message> {
    match state.active_tab {
        Tab::Playlist => view_playlist(state),
        Tab::Equalizer => view_equalizer(state),
        Tab::Settings => view_settings(state),
    }
}

fn view_playlist(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    if state.ui_state.tracks.is_empty() {
        return container(
            text("No tracks in playlist")
                .size(Consts::EMPTY_PLAYLIST_FONT)
                .color(p.muted),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into();
    }

    let mut tracks = column![].spacing(Gap::INLINE).width(Length::Fill);

    for (index, entry) in state.ui_state.tracks.iter().enumerate() {
        let is_current = state.ui_state.current_track_index == Some(index);
        let is_selected = state.selected_track_index == Some(index);
        let blink_on = u64::from(state.blink_counter / Consts::BLINK_DIVISOR)
            .is_multiple_of(Consts::BLINK_PERIOD);
        let row = TrackRow::classify(&entry.status, is_current);
        let text_color = match &row {
            TrackRow::Failed => p.danger,
            TrackRow::SlowCurrent => {
                if blink_on {
                    p.warning
                } else {
                    p.muted
                }
            }
            TrackRow::Slow => p.warning,
            TrackRow::Current => p.accent,
            TrackRow::Normal => p.text,
        };
        let index_color = match &row {
            TrackRow::Failed => p.danger,
            TrackRow::SlowCurrent => {
                if blink_on {
                    p.warning
                } else {
                    p.muted
                }
            }
            TrackRow::Slow => p.warning,
            TrackRow::Current => p.accent,
            TrackRow::Normal => p.muted,
        };
        let track_name = truncate_name(&entry.name, Consts::PLAYLIST_MAX_NAME_CHARS);

        let item = button(
            row![
                text(format!("{:02}", index + 1))
                    .size(Consts::PLAYLIST_INDEX_FONT)
                    .font(fonts::MONO)
                    .color(index_color),
                text(track_name)
                    .size(Consts::PLAYLIST_TRACK_FONT)
                    .font(fonts::display(Weight::Medium))
                    .color(text_color),
            ]
            .spacing(Gap::CONTENT)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([
            Consts::PLAYLIST_ITEM_PADDING_Y,
            Consts::PLAYLIST_ITEM_PADDING_X,
        ])
        .style(move |_theme, status| playlist_item_style(p, is_selected, status))
        .on_press(Message::SelectTrack(index));

        tracks = tracks.push(item);
    }

    scrollable(tracks).height(Length::Fill).into()
}

fn view_equalizer(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let band_count = state.ui_state.eq_bands.len();
    let mut bands_row = row![].spacing(Consts::EQ_BAND_MIN_GAP).width(Length::Fill);

    for index in 0..band_count {
        let value = state.ui_state.eq_bands[index].clamp(Consts::EQ_MIN_DB, Consts::EQ_MAX_DB);
        let value_color = if value.abs() < Consts::EQ_ZERO_THRESHOLD {
            p.muted
        } else {
            p.accent
        };

        let label = eq_band_label(index, band_count);

        let band = column![
            text(format!("{value:+.0}"))
                .size(Consts::EQ_VALUE_FONT)
                .font(fonts::mono(Weight::Medium))
                .color(value_color),
            container(
                vertical_slider(Consts::EQ_MIN_DB..=Consts::EQ_MAX_DB, value, move |v| {
                    Message::EqBandChanged(index, v)
                })
                .step(Consts::EQ_STEP)
                .width(Consts::SLIDER_RAIL_WIDTH)
                .height(Length::Fill)
                .style(slider_style(p)),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill),
            text(label)
                .size(Consts::EQ_LABEL_FONT)
                .font(fonts::mono(Weight::Medium))
                .color(p.text),
        ]
        .spacing(Gap::INLINE)
        .align_x(Alignment::Center)
        .width(Length::Fixed(Consts::EQ_BAND_WIDTH))
        .height(Length::Fill);

        if index > 0 {
            bands_row = bands_row.push(Space::new().width(Length::Fill));
        }
        bands_row = bands_row.push(band);
    }

    let title = format!("{band_count}-Band Equalizer");
    container(
        column![
            row![
                text(title)
                    .size(Consts::HEADING_FONT)
                    .font(fonts::display(Weight::Semibold))
                    .color(p.text),
                Space::new().width(Length::Fill),
                button(
                    text("Reset All")
                        .size(Consts::EQ_RESET_FONT)
                        .font(fonts::mono(Weight::Medium))
                        .color(p.muted)
                )
                .padding([Consts::EQ_RESET_PADDING_Y, Consts::EQ_RESET_PADDING_X])
                .style(ghost_button_style(p))
                .on_press(Message::EqResetAll)
            ]
            .align_y(Alignment::Center),
            container(bands_row)
                .width(Length::Fill)
                .height(Length::Fill)
        ]
        .spacing(Gap::SECTION)
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Generate a short label for an EQ band by index. Mirrors the iOS
/// reference: a log-spaced frequency between 30 Hz and 18 kHz is
/// rendered as `60`, `200`, `1k`, `12k`, etc.; tiny EQs (≤3 bands) get
/// the friendlier `Low`/`Mid`/`High` triplet.
pub(crate) fn eq_band_label(index: usize, total: usize) -> String {
    /// Lower bound of the log-spaced label range, in Hz.
    const MIN_FREQ_HZ: f32 = 30.0;
    /// Upper bound of the log-spaced label range, in Hz.
    const MAX_FREQ_HZ: f32 = 18_000.0;
    /// Threshold above which the label switches to "Nk" notation.
    const KILO_THRESHOLD_HZ: f32 = 1_000.0;
    /// Hz per kHz, expressed as `f32` so the cast lives in one place.
    const HZ_PER_KHZ: f32 = 1_000.0;

    if total <= Consts::EQ_SIMPLE_LABEL_THRESHOLD {
        return match index {
            0 => "Low".to_string(),
            i if i == total - 1 => "High".to_string(),
            _ => "Mid".to_string(),
        };
    }
    if total < 2 {
        return format!("{}", index + 1);
    }
    let exponent = index.to_f32().unwrap_or(0.0) / (total - 1).to_f32().unwrap_or(1.0);
    let freq = MIN_FREQ_HZ * (MAX_FREQ_HZ / MIN_FREQ_HZ).powf(exponent);
    if freq >= KILO_THRESHOLD_HZ {
        format!("{:.0}k", freq / HZ_PER_KHZ)
    } else {
        format!("{freq:.0}")
    }
}

fn view_settings(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let mut col = column![
        text("Settings")
            .size(Consts::HEADING_FONT)
            .font(fonts::display(Weight::Semibold))
            .color(p.text),
    ]
    .spacing(Gap::INLINE_WIDE)
    .width(Length::Fill);

    col = col.push(
        text("QUALITY")
            .size(Consts::SETTINGS_LABEL_FONT)
            .font(fonts::MONO)
            .color(p.muted),
    );
    let mut quality_row = row![].spacing(Gap::INLINE);
    quality_row = quality_row.push(pill_button(
        "Auto",
        state.ui_state.abr_mode_is_auto,
        p,
        Message::SetAbrMode(None),
    ));
    for (idx, label) in &state.ui_state.abr_variants {
        let active = state.ui_state.selected_variant == Some(*idx);
        quality_row = quality_row.push(pill_button(
            label,
            active,
            p,
            Message::SetAbrMode(Some(*idx)),
        ));
    }
    col = col.push(quality_row);

    let secs = state.ui_state.crossfade.clamp(0.0, Consts::CROSSFADE_MAX);
    col = col.push(Space::new().height(Length::Fixed(Gap::SECTION)));
    col = col.push(
        row![
            text("CROSSFADE")
                .size(Consts::SETTINGS_LABEL_FONT)
                .font(fonts::MONO)
                .color(p.muted),
            Space::new().width(Length::Fill),
            text(format!("{secs:.1}s"))
                .size(Consts::SETTINGS_BODY_FONT)
                .font(fonts::mono(Weight::Medium))
                .color(p.accent),
        ]
        .align_y(Alignment::Center),
    );

    let cf_slider = slider(0.0..=Consts::CROSSFADE_MAX, secs, Message::CrossfadeChanged)
        .step(Consts::CROSSFADE_STEP)
        .width(Length::Fill)
        .style(slider_style(p));
    col = col.push(cf_slider);

    container(col)
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

/// Secondary / toggle transport button: a fixed-size rounded square. `active`
/// drives the toggle highlight (accent fill) for shuffle and repeat; prev/next
/// pass `false`. Hover paints a faint panel fill so the target reads as live.
fn transport_square_button(
    icon: Icon,
    icon_size: f32,
    box_size: f32,
    p: GuiPalette,
    active: bool,
    message: Message,
) -> Element<'static, Message> {
    let icon_color = if active { p.accent } else { p.text_dim };
    button(
        container(icon.view(icon_size, icon_color))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(box_size))
    .height(Length::Fixed(box_size))
    .padding(0)
    .style(move |_theme, status| transport_square_style(p, active, status))
    .on_press(message)
    .into()
}

fn transport_square_style(p: GuiPalette, active: bool, status: ButtonStatus) -> ButtonStyle {
    let background = if active {
        match status {
            ButtonStatus::Pressed => with_alpha(p.accent, Consts::ALPHA_TAB_ACTIVE_PRESSED),
            _ => p.accent_soft,
        }
    } else {
        match status {
            ButtonStatus::Hovered => with_alpha(p.bg_panel_2, Consts::ALPHA_TRANSPORT_HOVER),
            ButtonStatus::Pressed => with_alpha(p.bg_panel_2, Consts::ALPHA_TRANSPORT_PRESSED),
            ButtonStatus::Active | ButtonStatus::Disabled => Color::TRANSPARENT,
        }
    };

    ButtonStyle {
        background: Some(Background::Color(background)),
        text_color: if active { p.accent } else { p.text_dim },
        border: Border::default()
            .rounded(Consts::BORDER_RADIUS_BUTTON)
            .width(Consts::BORDER_WIDTH)
            .color(Color::TRANSPARENT),
        ..ButtonStyle::default()
    }
}

fn tab_button(state: &Kithara, tab: Tab, icon: Icon, label: &str) -> Element<'static, Message> {
    let p = state.palette;
    let active = state.active_tab == tab;
    let icon_color = if active { p.accent } else { p.text_dim };
    let label_color = if active { p.accent } else { p.text_dim };

    let content = row![
        icon.view(Consts::TAB_ICON_SIZE, icon_color),
        text(label.to_string())
            .size(Consts::SMALL_FONT)
            .font(fonts::sans(Weight::Semibold))
            .color(label_color)
    ]
    .spacing(Gap::INLINE)
    .align_y(Alignment::Center);

    button(container(content).center_x(Length::Fill))
        .width(Length::FillPortion(1))
        .padding([Consts::TAB_CONTENT_PADDING_Y, Consts::TAB_CONTENT_PADDING_X])
        .style(move |_theme, status| tab_button_style(p, active, status))
        .on_press(Message::TabSelected(tab))
        .into()
}

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
                    .rounded(Consts::BORDER_RADIUS)
                    .width(Consts::BORDER_WIDTH)
                    .color(p.line),
            )
    }
}

fn now_card_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        let fill = Background::Gradient(Gradient::Linear(
            Linear::new(Degrees(180.0))
                .add_stop(
                    0.0,
                    with_alpha(p.bg_panel_2, Consts::NOW_GRADIENT_START_ALPHA),
                )
                .add_stop(1.0, with_alpha(p.bg_panel, Consts::NOW_GRADIENT_END_ALPHA)),
        ));
        ContainerStyle::default()
            .background(fill)
            .color(p.text)
            .border(
                Border::default()
                    .rounded(Consts::BORDER_RADIUS_SECTION)
                    .width(Consts::BORDER_WIDTH)
                    .color(p.line),
            )
    }
}

fn dot_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(p.accent)
            .border(Border::default().rounded(Consts::BORDER_RADIUS_CIRCLE))
    }
}

fn speed_control_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(with_alpha(p.bg_inset, Consts::ALPHA_SPEED_FILL))
            .color(p.text)
            .border(
                Border::default()
                    .rounded(Consts::BORDER_RADIUS_BUTTON)
                    .width(Consts::BORDER_WIDTH)
                    .color(p.line_soft),
            )
    }
}

fn tab_strip_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(with_alpha(p.bg_inset, Consts::ALPHA_TAB_STRIP_FILL))
            .color(p.text)
            .border(
                Border::default()
                    .rounded(Consts::BORDER_RADIUS_SECTION)
                    .width(Consts::BORDER_WIDTH)
                    .color(p.line),
            )
    }
}

fn ghost_button_style(p: GuiPalette) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle {
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Active => None,
            ButtonStatus::Hovered => Some(Background::Color(with_alpha(
                p.bg_panel,
                Consts::ALPHA_GHOST_HOVER,
            ))),
            ButtonStatus::Pressed => Some(Background::Color(with_alpha(
                p.accent,
                Consts::ALPHA_GHOST_PRESSED,
            ))),
            ButtonStatus::Disabled => Some(Background::Color(with_alpha(
                p.bg_panel,
                Consts::ALPHA_GHOST_DISABLED,
            ))),
        };

        ButtonStyle {
            background,
            text_color: p.text,
            border: Border::default().rounded(Consts::BORDER_RADIUS_BUTTON),
            ..ButtonStyle::default()
        }
    }
}

fn tab_button_style(p: GuiPalette, active: bool, status: ButtonStatus) -> ButtonStyle {
    let base_bg = if active {
        with_alpha(p.accent, Consts::ALPHA_TAB_ACTIVE_BASE)
    } else {
        Color::TRANSPARENT
    };

    let hover_bg = if active {
        with_alpha(p.accent, Consts::ALPHA_TAB_ACTIVE_HOVER)
    } else {
        with_alpha(p.bg_panel, Consts::ALPHA_TAB_INACTIVE_HOVER)
    };

    let pressed_bg = if active {
        with_alpha(p.accent, Consts::ALPHA_TAB_ACTIVE_PRESSED)
    } else {
        with_alpha(p.accent, Consts::ALPHA_TAB_INACTIVE_PRESSED)
    };

    let active_fill = Background::Gradient(Gradient::Linear(
        Linear::new(Degrees(180.0))
            .add_stop(0.0, with_alpha(p.accent, 0.16))
            .add_stop(1.0, with_alpha(p.accent, 0.08)),
    ));

    let background = match status {
        ButtonStatus::Active => Some(if active {
            active_fill
        } else {
            Background::Color(base_bg)
        }),
        ButtonStatus::Hovered => Some(Background::Color(hover_bg)),
        ButtonStatus::Pressed => Some(Background::Color(pressed_bg)),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(
            p.bg_panel,
            Consts::ALPHA_TAB_DISABLED,
        ))),
    };

    ButtonStyle {
        background,
        text_color: if active { p.accent } else { p.text_dim },
        border: Border::default()
            .rounded(Consts::BORDER_RADIUS_BUTTON)
            .width(Consts::BORDER_WIDTH)
            .color(if active {
                with_alpha(p.accent, 0.30)
            } else {
                Color::TRANSPARENT
            }),
        ..ButtonStyle::default()
    }
}

fn playlist_item_style(p: GuiPalette, selected: bool, status: ButtonStatus) -> ButtonStyle {
    let active_bg = if selected {
        with_alpha(p.accent, Consts::ALPHA_PLAYLIST_SELECTED)
    } else {
        Color::TRANSPARENT
    };

    let hover_bg = if selected {
        with_alpha(p.accent, Consts::ALPHA_PLAYLIST_SELECTED_HOVER)
    } else {
        with_alpha(p.bg_panel, Consts::ALPHA_PLAYLIST_INACTIVE_HOVER)
    };

    let pressed_bg = if selected {
        with_alpha(p.accent, Consts::ALPHA_PLAYLIST_SELECTED_PRESSED)
    } else {
        with_alpha(p.accent, Consts::ALPHA_PLAYLIST_INACTIVE_PRESSED)
    };

    let background = match status {
        ButtonStatus::Active => Some(Background::Color(active_bg)),
        ButtonStatus::Hovered => Some(Background::Color(hover_bg)),
        ButtonStatus::Pressed => Some(Background::Color(pressed_bg)),
        ButtonStatus::Disabled => Some(Background::Color(with_alpha(
            p.bg_panel,
            Consts::ALPHA_PLAYLIST_DISABLED,
        ))),
    };

    ButtonStyle {
        background,
        text_color: p.text,
        border: Border::default().rounded(Consts::BORDER_RADIUS_BUTTON),
        ..ButtonStyle::default()
    }
}

fn slider_style(p: GuiPalette) -> impl Fn(&Theme, SliderStatus) -> SliderStyle {
    move |_theme, status| {
        let active = match status {
            SliderStatus::Active => p.accent,
            SliderStatus::Hovered => with_alpha(p.accent, Consts::ALPHA_SLIDER_ACTIVE_RAIL),
            SliderStatus::Dragged => with_alpha(p.accent, Consts::ALPHA_SLIDER_DRAGGED_RAIL),
        };

        SliderStyle {
            rail: SliderRail {
                backgrounds: (
                    Background::Color(active),
                    Background::Color(with_alpha(p.muted, Consts::ALPHA_SLIDER_INACTIVE_RAIL)),
                ),
                width: Consts::SLIDER_RAIL_WIDTH,
                border: Border::default().rounded(Consts::SLIDER_RAIL_RADIUS),
            },
            handle: SliderHandle {
                shape: SliderHandleShape::Circle {
                    radius: Consts::SLIDER_HANDLE_RADIUS,
                },
                background: Background::Color(p.text),
                border_width: Consts::SLIDER_HANDLE_BORDER,
                border_color: with_alpha(p.bg, Consts::ALPHA_SLIDER_HANDLE_BORDER),
            },
        }
    }
}

/// A selectable pill (Quality presets). Active = accent text + accent ring;
/// inactive = dim text on a faint fill, matching the tab-strip active
/// language.
fn pill_button<'a>(label: &str, active: bool, p: GuiPalette, msg: Message) -> Element<'a, Message> {
    let text_color = if active { p.accent } else { p.text_dim };
    button(
        text(label.to_string())
            .size(Consts::SMALL_FONT)
            .font(fonts::sans(Weight::Semibold))
            .color(text_color),
    )
    .on_press(msg)
    .padding(Padding::from([
        Consts::PILL_PADDING_Y,
        Consts::PILL_PADDING_X,
    ]))
    .style(move |_theme, status| {
        let background = match status {
            ButtonStatus::Hovered => with_alpha(p.bg_panel_2, Consts::ALPHA_TAB_INACTIVE_HOVER),
            ButtonStatus::Pressed => with_alpha(p.accent, Consts::ALPHA_TAB_ACTIVE_PRESSED),
            ButtonStatus::Active | ButtonStatus::Disabled => {
                if active {
                    with_alpha(p.accent, Consts::ALPHA_TAB_ACTIVE_BASE)
                } else {
                    with_alpha(p.bg_panel, Consts::ALPHA_SECTION_BG)
                }
            }
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color,
            border: Border::default()
                .rounded(Consts::BORDER_RADIUS_PILL)
                .width(Consts::BORDER_WIDTH)
                .color(if active { p.accent } else { p.line }),
            ..ButtonStyle::default()
        }
    })
    .into()
}

pub(crate) fn track_subtitle(state: &Kithara) -> String {
    let Some(index) = state.ui_state.current_track_index else {
        return "Artist / Album unavailable".to_string();
    };
    let Some(entry) = state.ui_state.tracks.get(index) else {
        return "Artist / Album unavailable".to_string();
    };
    let Some(url) = entry.url.as_deref() else {
        return "Artist / Album unavailable".to_string();
    };

    let path = Path::new(url);
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

pub(crate) fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

/// Linearly interpolate `base` toward `tint` by `amount` in `[0, 1]`
/// (channels and alpha). Shared color-mix helper for the GUI.
pub(crate) fn mix_colors(base: Color, tint: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    Color::from_rgba(
        base.r + (tint.r - base.r) * amount,
        base.g + (tint.g - base.g) * amount,
        base.b + (tint.b - base.b) * amount,
        base.a + (tint.a - base.a) * amount,
    )
}
