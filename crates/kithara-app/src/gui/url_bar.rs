use std::time::Duration;

use iced::{
    Alignment, Background, Border, Color, Degrees, Element, Length, Padding, Shadow, Task, Vector,
    alignment::{Horizontal, Vertical},
    font::Weight,
    gradient,
    widget::{
        button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        container,
        container::Style as ContainerStyle,
        row, stack, text, text_input,
        text_input::{Status as InputStatus, Style as InputStyle},
    },
};

use super::{app::Kithara, fonts, icons::Icon, message::Message};
use crate::{
    gui::view::{mix_colors, with_alpha},
    theme::gui::GuiPalette,
};

const BAR_RADIUS: f32 = 10.0;
const BORDER_WIDTH: f32 = 1.0;
const INPUT_FONT: f32 = 12.5;
const INPUT_PAD_V: f32 = 11.0;
const INPUT_PAD_LEFT: f32 = 14.0;
/// Right padding reserved on the input so typed text never slides under
/// the stacked chip + submit button. The input cannot reflow around an
/// overlay, so this space is held permanently.
const INPUT_PAD_RIGHT: f32 = 120.0;
const OVERLAY_PAD_RIGHT: f32 = 5.0;
const ROW_GAP: f32 = 10.0;
const CHIP_FONT: f32 = 9.5;
const FLASH_FONT: f32 = 10.0;
const CHIP_PAD_X: f32 = 7.0;
const CHIP_PAD_Y: f32 = 3.0;
const CHIP_RADIUS: f32 = 4.0;
const SUBMIT_SIZE: f32 = 32.0;
const SUBMIT_RADIUS: f32 = 7.0;
const SUBMIT_ICON: f32 = 14.0;
const PLACEHOLDER_ALPHA: f32 = 0.6;
const SELECTION_ALPHA: f32 = 0.4;
const FOCUS_BORDER_MIX: f32 = 0.55;
const FLASH_BORDER_MIX: f32 = 0.6;
const CHIP_BORDER_ALPHA: f32 = 0.3;
const FLASH_BG_ALPHA: f32 = 0.14;
const FLASH_BORDER_ALPHA: f32 = 0.3;
const GRADIENT_ALPHA: f32 = 0.85;
const SUBMIT_DISABLED_BG_ALPHA: f32 = 0.55;
/// Submit-button border: accent at 60% over black.
const SUBMIT_BORDER_MIX: f32 = 0.6;
const SUBMIT_SHADOW_ALPHA: f32 = 0.3;
const SUBMIT_SHADOW_BLUR: f32 = 14.0;
const SUBMIT_SHADOW_OFFSET_Y: f32 = 4.0;
const FLASH_EXPIRE_MS: u64 = 2200;

/// Dark glyph color on the active (gold) submit button (#1a1408).
const SUBMIT_FG: Color = Color {
    r: 26.0 / 255.0,
    g: 20.0 / 255.0,
    b: 8.0 / 255.0,
    a: 1.0,
};

const PLACEHOLDER: &str = "Audio URL \u{2014} mp3, flac, m3u8, opus\u{2026}";

/// View-local state for the paste-an-audio-URL bar. The committed queue
/// is owned by the player; this only holds the in-flight text and the
/// ephemeral submit flash.
#[derive(Debug, Default)]
pub(crate) struct UrlBar {
    pub(crate) text: String,
    pub(crate) flash: Option<Flash>,
}

/// Transient feedback shown in place of the format chip after a submit.
#[derive(Debug, Clone)]
pub(crate) struct Flash {
    pub(crate) kind: FlashKind,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlashKind {
    Ok,
    Err,
}

/// URL-bar events, grouped so the top-level [`Message`] stays thin.
#[derive(Debug, Clone)]
pub(crate) enum UrlMsg {
    /// Text field edited.
    Changed(String),
    /// Commit the current text (Enter or submit button).
    Submit,
    /// Clear the success flash after its lifetime elapsed.
    FlashExpired,
}

pub(crate) fn handle(state: &mut Kithara, msg: UrlMsg) -> Task<Message> {
    match msg {
        UrlMsg::Changed(text) => {
            state.url.text = text;
            // Any pending flash is stale once the user keeps typing.
            state.url.flash = None;
            Task::none()
        }
        UrlMsg::Submit => submit(state),
        UrlMsg::FlashExpired => {
            // Only the success flash is timer-cleared; an error flash
            // stays until the next keystroke.
            if matches!(
                state.url.flash,
                Some(Flash {
                    kind: FlashKind::Ok,
                    ..
                })
            ) {
                state.url.flash = None;
            }
            Task::none()
        }
    }
}

fn submit(state: &mut Kithara) -> Task<Message> {
    let trimmed = state.url.text.trim().to_string();
    if trimmed.is_empty() {
        return Task::none();
    }
    if !looks_like_source(&trimmed) {
        state.url.flash = Some(Flash {
            kind: FlashKind::Err,
            text: "ENTER A URL OR FILE PATH".to_string(),
        });
        return Task::none();
    }

    let fmt = detect_format(&trimmed);
    state.controller.queue().append(trimmed.as_str());
    state.url.flash = Some(Flash {
        kind: FlashKind::Ok,
        text: format!("QUEUED \u{00b7} {fmt}"),
    });
    state.url.text.clear();

    Task::perform(
        tokio::time::sleep(Duration::from_millis(FLASH_EXPIRE_MS)),
        |()| Message::Url(UrlMsg::FlashExpired),
    )
}

/// Format chip label, auto-detected from the URL extension. Mirrors the
/// design handoff mapping; anything unrecognized reads as `STREAM`.
fn detect_format(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".m3u8") {
        "HLS"
    } else if lower.ends_with(".mp3") {
        "MP3"
    } else if lower.ends_with(".flac") {
        "FLAC"
    } else if lower.ends_with(".wav") {
        "WAV"
    } else if lower.ends_with(".ogg") {
        "OGG"
    } else if lower.ends_with(".aac") {
        "AAC"
    } else if lower.ends_with(".opus") {
        "OPUS"
    } else {
        "STREAM"
    }
}

/// Broad validity check: a remote URL, a path-like string, or a bare
/// filename with a known audio extension all pass. Garbage (a bare word)
/// triggers the error flash.
fn looks_like_source(v: &str) -> bool {
    has_scheme(v)
        || v.contains('/')
        || v.contains('\\')
        || v.starts_with('~')
        || v.starts_with('.')
        || detect_format(v) != "STREAM"
}

fn has_scheme(v: &str) -> bool {
    let Some(idx) = v.find("://") else {
        return false;
    };
    let mut chars = v[..idx].chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {
            chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        _ => false,
    }
}

pub(crate) fn view(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;
    let trimmed = state.url.text.trim();
    let has_value = !trimmed.is_empty();
    let flash_kind = state.url.flash.as_ref().map(|f| f.kind);

    let input = text_input(PLACEHOLDER, &state.url.text)
        .on_input(|t| Message::Url(UrlMsg::Changed(t)))
        .on_submit(Message::Url(UrlMsg::Submit))
        .font(fonts::MONO)
        .size(INPUT_FONT)
        .width(Length::Fill)
        .padding(
            Padding::ZERO
                .top(INPUT_PAD_V)
                .bottom(INPUT_PAD_V)
                .left(INPUT_PAD_LEFT)
                .right(INPUT_PAD_RIGHT),
        )
        .style(move |_theme, status| input_style(p, flash_kind, status));

    let mut badges = row![].spacing(ROW_GAP).align_y(Alignment::Center);
    if let Some(flash) = &state.url.flash {
        badges = badges.push(flash_view(flash, p));
    } else if has_value {
        badges = badges.push(chip_view(detect_format(trimmed), p));
    }
    badges = badges.push(submit_button(has_value, p));

    let overlay = container(badges)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Right)
        .align_y(Vertical::Center)
        .padding(Padding::ZERO.right(OVERLAY_PAD_RIGHT));

    stack![input, overlay].width(Length::Fill).into()
}

fn input_style(p: GuiPalette, flash: Option<FlashKind>, status: InputStatus) -> InputStyle {
    let focused = matches!(status, InputStatus::Focused { .. });
    let border_color = match flash {
        Some(FlashKind::Ok) => mix_colors(p.line, p.success, FLASH_BORDER_MIX),
        Some(FlashKind::Err) => mix_colors(p.line, p.danger, FLASH_BORDER_MIX),
        None if focused => mix_colors(p.line, p.accent, FOCUS_BORDER_MIX),
        None => p.line,
    };

    InputStyle {
        background: bar_background(p),
        border: Border::default()
            .rounded(BAR_RADIUS)
            .width(BORDER_WIDTH)
            .color(border_color),
        icon: p.muted,
        placeholder: with_alpha(p.muted, PLACEHOLDER_ALPHA),
        value: p.text,
        selection: with_alpha(p.accent, SELECTION_ALPHA),
    }
}

fn chip_view(fmt: &'static str, p: GuiPalette) -> Element<'static, Message> {
    container(
        text(fmt)
            .size(CHIP_FONT)
            .font(fonts::mono(Weight::Semibold))
            .color(p.accent),
    )
    .padding(badge_padding())
    .style(move |_theme| {
        ContainerStyle::default().background(p.accent_soft).border(
            Border::default()
                .rounded(CHIP_RADIUS)
                .width(BORDER_WIDTH)
                .color(with_alpha(p.accent, CHIP_BORDER_ALPHA)),
        )
    })
    .into()
}

fn flash_view(flash: &Flash, p: GuiPalette) -> Element<'static, Message> {
    let base = match flash.kind {
        FlashKind::Ok => p.success,
        FlashKind::Err => p.danger,
    };
    container(
        text(flash.text.clone())
            .size(FLASH_FONT)
            .font(fonts::mono(Weight::Medium))
            .color(base),
    )
    .padding(badge_padding())
    .style(move |_theme| {
        ContainerStyle::default()
            .background(with_alpha(base, FLASH_BG_ALPHA))
            .border(
                Border::default()
                    .rounded(CHIP_RADIUS)
                    .width(BORDER_WIDTH)
                    .color(with_alpha(base, FLASH_BORDER_ALPHA)),
            )
    })
    .into()
}

fn submit_button(active: bool, p: GuiPalette) -> Element<'static, Message> {
    let icon_color = if active { SUBMIT_FG } else { p.muted };
    let content = container(Icon::PlaylistAdd.view(SUBMIT_ICON, icon_color))
        .center_x(Length::Fill)
        .center_y(Length::Fill);

    let mut btn = button(content)
        .width(Length::Fixed(SUBMIT_SIZE))
        .height(Length::Fixed(SUBMIT_SIZE))
        .padding(0)
        .style(move |_theme, status| submit_style(p, active, status));
    if active {
        btn = btn.on_press(Message::Url(UrlMsg::Submit));
    }
    btn.into()
}

fn submit_style(p: GuiPalette, active: bool, status: ButtonStatus) -> ButtonStyle {
    if !active {
        return ButtonStyle {
            background: Some(Background::Color(with_alpha(
                p.bg_panel,
                SUBMIT_DISABLED_BG_ALPHA,
            ))),
            text_color: p.muted,
            border: Border::default()
                .rounded(SUBMIT_RADIUS)
                .width(BORDER_WIDTH)
                .color(p.line),
            ..ButtonStyle::default()
        };
    }

    let background = match status {
        ButtonStatus::Pressed => Background::Color(p.accent),
        ButtonStatus::Hovered => linear(p.accent_strong, p.accent_strong),
        _ => linear(p.accent_strong, p.accent),
    };

    ButtonStyle {
        background: Some(background),
        text_color: SUBMIT_FG,
        border: Border::default()
            .rounded(SUBMIT_RADIUS)
            .width(BORDER_WIDTH)
            .color(mix_colors(Color::BLACK, p.accent, SUBMIT_BORDER_MIX)),
        shadow: Shadow {
            color: with_alpha(p.accent, SUBMIT_SHADOW_ALPHA),
            offset: Vector::new(0.0, SUBMIT_SHADOW_OFFSET_Y),
            blur_radius: SUBMIT_SHADOW_BLUR,
        },
        ..ButtonStyle::default()
    }
}

fn badge_padding() -> Padding {
    Padding::ZERO
        .top(CHIP_PAD_Y)
        .bottom(CHIP_PAD_Y)
        .left(CHIP_PAD_X)
        .right(CHIP_PAD_X)
}

fn bar_background(p: GuiPalette) -> Background {
    gradient::Linear::new(Degrees(180.0))
        .add_stop(0.0, with_alpha(p.bg_inset, GRADIENT_ALPHA))
        .add_stop(1.0, with_alpha(p.bg_deep, GRADIENT_ALPHA))
        .into()
}

fn linear(start: Color, end: Color) -> Background {
    gradient::Linear::new(Degrees(180.0))
        .add_stop(0.0, start)
        .add_stop(1.0, end)
        .into()
}

#[cfg(test)]
mod tests {
    use super::{detect_format, looks_like_source};

    #[test]
    fn format_detection_matches_handoff_mapping() {
        assert_eq!(detect_format("https://h.example/live.m3u8"), "HLS");
        assert_eq!(detect_format("https://h.example/song.mp3"), "MP3");
        assert_eq!(detect_format("song.flac"), "FLAC");
        assert_eq!(detect_format("a.WAV"), "WAV");
        assert_eq!(detect_format("track.opus?token=abc"), "OPUS");
        assert_eq!(detect_format("https://h.example/stream"), "STREAM");
    }

    #[test]
    fn sources_accept_urls_paths_and_known_extensions() {
        assert!(looks_like_source("https://h.example/a.mp3"));
        assert!(looks_like_source("file:///music/a.flac"));
        assert!(looks_like_source("/home/user/a.opus"));
        assert!(looks_like_source("./relative/a.mp3"));
        assert!(looks_like_source("C:\\music\\a.wav"));
        assert!(looks_like_source("bare.m3u8"));
    }

    #[test]
    fn garbage_input_is_rejected() {
        assert!(!looks_like_source("hello"));
        assert!(!looks_like_source("not a url"));
        // A bare scheme word without "://" is not a source.
        assert!(!looks_like_source("https"));
    }
}
