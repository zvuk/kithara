use iced::{
    Alignment, Background, Border, Degrees, Element, Length, Theme,
    alignment::Horizontal,
    font::Weight,
    gradient,
    widget::{Space, button, column, container, container::Style as ContainerStyle, row, text},
};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use iced::{
    Shadow,
    widget::{overlay::menu, pick_list},
};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara::prelude::StretchBackendKind;

use super::tokens::{studio_radius, studio_type};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use crate::gui::icons::Icon;
use crate::{
    gui::{
        app::Kithara,
        dj::{DjMsg, TimestretchState},
        fonts,
        message::Message,
        tokens::gap,
        view::{mix_colors, with_alpha},
        widgets,
    },
    theme::gui::GuiPalette,
};

/// Layout tunables for the timestretch panel, grouped to keep the module
/// surface small.
struct Consts;

impl Consts {
    /// Range / library pill height.
    const PILL_H: f32 = 24.0;
    /// Selectable tempo bounds in ± percent.
    const RANGES: [u8; 4] = [8, 16, 50, 100];
    /// Shared height of the stat tile / key-lock / nudge row so they align.
    const STAT_H: f32 = 38.0;
}

/// The timestretch deck panel, matching the design `.ts` block: a single
/// head row (label · range pills · tempo value), the tempo slider, a stat
/// row (ratio · key-lock · nudge), and the stretch-backend selector.
pub(super) fn view_timestretch_panel(state: &Kithara) -> Element<'static, Message> {
    let p = state.palette;
    let ts = state.dj.timestretch;
    container(
        column![
            head_row(ts.tempo, state, p),
            ranges_row(ts.range, p),
            slider_row(ts.tempo, ts.range, p),
            stats_row(ts, state, p),
        ]
        .spacing(gap::INLINE_WIDE),
    )
    .padding([10.0, 12.0])
    .style(panel_style(p))
    .into()
}

/// `[• TIMESTRETCH  library pills] … [+0.00%]` — title, backend selector,
/// and the live tempo value.
fn head_row(tempo: f32, state: &Kithara, p: GuiPalette) -> Element<'static, Message> {
    row![
        indicator_dot(p),
        text("TIMESTRETCH")
            .size(13.0)
            .font(fonts::display(Weight::Bold))
            .color(p.text),
        library_select(state, p),
        Space::new().width(Length::Fill),
        container(
            text(format!("{tempo:+.2}%"))
                .size(22.0)
                .font(fonts::display(Weight::Bold))
                .color(p.accent),
        )
        .width(Length::Fixed(92.0))
        .align_x(Horizontal::Right),
    ]
    .align_y(Alignment::Center)
    .spacing(gap::INLINE_WIDE)
    .into()
}

/// Range pills (`±8 / ±16 / ±50 / ±100 %`) that set the slider bound,
/// right-aligned above the slider.
fn ranges_row(range: u8, p: GuiPalette) -> Element<'static, Message> {
    let mut pills = row![].spacing(gap::INLINE_TIGHT).align_y(Alignment::Center);
    for r in Consts::RANGES {
        pills = pills.push(range_pill(r, r == range, p));
    }
    row![Space::new().width(Length::Fill), pills]
        .align_y(Alignment::Center)
        .into()
}

/// Pulsing accent dot from `.ts-dot` (static — iced has no panel animation).
fn indicator_dot(p: GuiPalette) -> Element<'static, Message> {
    container(Space::new())
        .width(Length::Fixed(7.0))
        .height(Length::Fixed(7.0))
        .style(move |_theme: &Theme| {
            ContainerStyle::default()
                .background(Background::Color(p.accent))
                .border(Border::default().rounded(studio_radius::ROUND))
        })
        .into()
}

fn slider_row(tempo: f32, range: u8, p: GuiPalette) -> Element<'static, Message> {
    row![
        mini_label(format!("\u{2212}{range}"), p),
        widgets::ts_slider(tempo, f32::from(range), p),
        mini_label(format!("+{range}"), p),
    ]
    .align_y(Alignment::Center)
    .spacing(gap::CONTENT)
    .into()
}

fn stats_row(ts: TimestretchState, state: &Kithara, p: GuiPalette) -> Element<'static, Message> {
    let ratio = ts.speed();
    row![
        stat_tile("RATIO", format!("{ratio:.3}\u{00d7}"), p),
        keylock_pill(state, p),
        Space::new().width(Length::Fill),
        nudge_group(p),
    ]
    .spacing(gap::INLINE)
    .align_y(Alignment::Center)
    .into()
}

/// Without a compiled-in stretch backend there is no selector to render.
#[cfg(not(any(feature = "stretch-signalsmith", feature = "stretch-bungee")))]
fn library_select(_state: &Kithara, _p: GuiPalette) -> Element<'static, Message> {
    Space::new().into()
}

/// Narrow stretch-backend selector (dropdown) shown next to the title. Only
/// backends compiled into this target appear.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn library_select(state: &Kithara, p: GuiPalette) -> Element<'static, Message> {
    let active = state.controller.deck().backend();
    pick_list(StretchBackendKind::all(), Some(active), |k| {
        Message::Dj(DjMsg::SelectBackend(k))
    })
    .text_size(studio_type::MONO_SM)
    .font(fonts::mono(Weight::Semibold))
    .padding([3.0, 8.0])
    .style(move |_theme: &Theme, _status| pick_list::Style {
        text_color: p.accent,
        placeholder_color: p.muted,
        handle_color: p.muted,
        background: Background::Color(with_alpha(p.bg_deep, 0.5)),
        border: Border::default()
            .rounded(studio_radius::SM)
            .width(1.0)
            .color(p.line),
    })
    .menu_style(move |_theme: &Theme| menu::Style {
        background: Background::Color(p.bg_panel),
        border: Border::default()
            .rounded(studio_radius::SM)
            .width(1.0)
            .color(p.line),
        text_color: p.text,
        selected_text_color: p.accent,
        selected_background: Background::Color(p.accent_soft),
        shadow: Shadow::default(),
    })
    .into()
}

fn mini_label(label: String, p: GuiPalette) -> Element<'static, Message> {
    container(
        text(label)
            .size(studio_type::MONO_XS)
            .font(fonts::mono(Weight::Medium))
            .color(p.muted),
    )
    .width(Length::Fixed(28.0))
    .align_x(Horizontal::Center)
    .into()
}

fn stat_tile(label: &str, value: String, p: GuiPalette) -> Element<'static, Message> {
    container(
        column![
            text(label.to_string())
                .size(9.0)
                .font(fonts::mono(Weight::Medium))
                .color(p.muted),
            text(value)
                .size(studio_type::TRACK)
                .font(fonts::display(Weight::Semibold))
                .color(p.text),
        ]
        .spacing(1.0),
    )
    .padding([0.0, 10.0])
    .center_y(Length::Fixed(Consts::STAT_H))
    .style(tile_style(p))
    .into()
}

/// Without a compiled-in stretch backend key-lock does not exist.
#[cfg(not(any(feature = "stretch-signalsmith", feature = "stretch-bungee")))]
fn keylock_pill(_state: &Kithara, _p: GuiPalette) -> Element<'static, Message> {
    Space::new().into()
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn keylock_pill(state: &Kithara, p: GuiPalette) -> Element<'static, Message> {
    let on = state.controller.deck().keylock();
    let color = if on { p.accent } else { p.muted };
    let background = if on {
        p.accent_soft
    } else {
        with_alpha(p.bg_deep, 0.5)
    };
    let border = if on { p.accent } else { p.line };
    button(
        container(
            row![
                Icon::Lock.view(13.0, color),
                text("KEY LOCK")
                    .size(studio_type::MONO_SM)
                    .font(fonts::mono(Weight::Semibold))
                    .color(color),
            ]
            .spacing(gap::INLINE)
            .align_y(Alignment::Center),
        )
        .center_y(Length::Fill),
    )
    .height(Length::Fixed(Consts::STAT_H))
    .padding([0.0, 12.0])
    .style(move |_theme: &Theme, _status| button::Style {
        background: Some(Background::Color(background)),
        text_color: color,
        border: Border::default()
            .rounded(studio_radius::SM)
            .width(1.0)
            .color(border),
        ..button::Style::default()
    })
    .on_press(Message::Dj(DjMsg::ToggleKeyLock))
    .into()
}

fn nudge_group(p: GuiPalette) -> Element<'static, Message> {
    row![
        nudge_button("\u{25c2}", Message::Dj(DjMsg::Nudge(-0.05)), p),
        nudge_button("\u{27f2}", Message::Dj(DjMsg::ResetTempo), p),
        nudge_button("\u{25b8}", Message::Dj(DjMsg::Nudge(0.05)), p),
    ]
    .spacing(3.0)
    .into()
}

fn nudge_button(label: &str, message: Message, p: GuiPalette) -> Element<'static, Message> {
    button(
        container(
            text(label.to_string())
                .size(studio_type::BODY_MD)
                .font(fonts::mono(Weight::Medium))
                .color(p.text_dim),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill),
    )
    .width(Length::Fixed(28.0))
    .height(Length::Fixed(Consts::STAT_H))
    .padding(0)
    .style(move |_theme: &Theme, _status| button::Style {
        background: Some(Background::Color(with_alpha(p.bg_deep, 0.5))),
        text_color: p.text_dim,
        border: Border::default()
            .rounded(studio_radius::SM)
            .width(1.0)
            .color(p.line),
        ..button::Style::default()
    })
    .on_press(message)
    .into()
}

fn range_pill(range: u8, active: bool, p: GuiPalette) -> Element<'static, Message> {
    pill(
        format!("\u{00b1}{range}%"),
        active,
        Message::Dj(DjMsg::SetRange(range)),
        p,
    )
}

fn pill(label: String, active: bool, message: Message, p: GuiPalette) -> Element<'static, Message> {
    let color = if active { p.accent } else { p.text_dim };
    let background = if active {
        p.accent_soft
    } else {
        with_alpha(p.bg_deep, 0.5)
    };
    let border = if active { p.accent } else { p.line };
    button(
        container(
            text(label)
                .size(studio_type::MONO_SM)
                .font(fonts::mono(Weight::Semibold))
                .color(color),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill),
    )
    .height(Length::Fixed(Consts::PILL_H))
    .padding([0.0, 9.0])
    .style(move |_theme: &Theme, _status| button::Style {
        background: Some(Background::Color(background)),
        text_color: color,
        border: Border::default()
            .rounded(studio_radius::SM)
            .width(1.0)
            .color(border),
        ..button::Style::default()
    })
    .on_press(message)
    .into()
}

/// `.ts` background: a subtle gold glow in the top-right corner over a dark
/// base, approximated as a diagonal linear gradient (iced has no radial).
fn panel_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        let glow = with_alpha(mix_colors(p.bg_deep, p.accent, 0.16), 0.72);
        let base = with_alpha(p.bg_deep, 0.62);
        let bg: Background = gradient::Linear::new(Degrees(215.0))
            .add_stop(0.0, glow)
            .add_stop(0.55, base)
            .add_stop(1.0, base)
            .into();
        ContainerStyle::default().background(bg).border(
            Border::default()
                .rounded(studio_radius::SURFACE)
                .width(1.0)
                .color(p.line),
        )
    }
}

fn tile_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(Background::Color(with_alpha(p.bg_deep, 0.5)))
            .border(
                Border::default()
                    .rounded(studio_radius::SM)
                    .width(1.0)
                    .color(p.line),
            )
    }
}
