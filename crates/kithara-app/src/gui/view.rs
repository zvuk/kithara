use std::path::Path;

use iced::{Color, Element};
use num_traits::cast::ToPrimitive;

use super::{app::Kithara, message::Message};
use crate::state::UiState;

pub(crate) fn view(state: &Kithara, _window: iced::window::Id) -> Element<'_, Message> {
    super::studio::view_dj_studio(state)
}

pub(crate) fn format_time(seconds: f64) -> String {
    const SECONDS_PER_MINUTE: u32 = 60;

    let total = seconds.max(0.0).floor().to_u32().unwrap_or(u32::MAX);
    let minutes = total / SECONDS_PER_MINUTE;
    let remaining = total % SECONDS_PER_MINUTE;
    format!("{minutes:02}:{remaining:02}")
}

/// Log-spaced frequency between 30 Hz and 18 kHz rendered as `60`, `200`,
/// `1k`, `12k`; tiny EQs (≤3 bands) get the `Low`/`Mid`/`High` triplet.
pub(crate) fn eq_band_label(index: usize, total: usize) -> String {
    const MIN_FREQ_HZ: f32 = 30.0;
    const MAX_FREQ_HZ: f32 = 18_000.0;
    const KILO_THRESHOLD_HZ: f32 = 1_000.0;
    const HZ_PER_KHZ: f32 = 1_000.0;
    const SIMPLE_LABEL_THRESHOLD: usize = 3;

    if total <= SIMPLE_LABEL_THRESHOLD {
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

pub(crate) fn track_subtitle(ui: &UiState) -> String {
    let Some(index) = ui.current_track_index else {
        return "Artist / Album unavailable".to_string();
    };
    let Some(entry) = ui.tracks.get(index) else {
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
