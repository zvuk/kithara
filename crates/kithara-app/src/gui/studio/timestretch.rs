use iced::{
    Alignment, Background, Border, Element, Length, Theme,
    alignment::Horizontal,
    font::Weight,
    widget::{Space, button, column, container, container::Style as ContainerStyle, row, text},
};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara::prelude::StretchKind;

use self::backend::{keylock_pill, library_select};
use super::{
    module::Module,
    tokens::{studio_radius, studio_space, studio_type},
};
use crate::{
    gui::{deck::TimestretchState, fonts, tokens::gap, widgets},
    theme::gui::GuiPalette,
};

/// What the timestretch panel draws. Nothing here identifies a deck.
#[derive(Debug, Clone, Copy)]
pub(super) struct TsProps {
    pub(super) state: TimestretchState,
    #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    pub(super) backend: StretchKind,
    #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    pub(super) keylock: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum TsMsg {
    SetTempo(f32),
    SetRange(u8),
    Nudge(f32),
    ResetTempo,
    #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    ToggleKeyLock,
    #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    SelectBackend(StretchKind),
}

struct Consts;

impl Consts {
    /// Range / library pill height.
    const PILL_H: f32 = 24.0;
    /// Selectable tempo bounds in ± percent.
    const RANGES: [u8; 4] = [8, 16, 50, 100];
    /// Shared height of the stat tile / key-lock / nudge row so they align.
    const STAT_H: f32 = 38.0;
}

/// The timestretch deck panel, matching the design `.ts` block: a head row
/// (label · range pills · tempo value), the tempo slider, a stat row
/// (ratio · key-lock · nudge), and the stretch-backend selector.
pub(super) fn view_timestretch(props: TsProps, p: GuiPalette) -> Element<'static, TsMsg> {
    let ts = props.state;
    Module::new()
        .bg(p.bg_panel)
        .pad(studio_space::CLUSTER)
        .wrap(
            column![
                head_row(ts.tempo, props, p),
                ranges_row(ts.range, p),
                slider_row(ts.tempo, ts.range, p),
                stats_row(ts, props, p),
            ]
            .spacing(gap::INLINE_WIDE),
        )
}

/// `[• TIMESTRETCH  library pills] … [+0.00%]` — title, backend selector,
/// and the live tempo value.
fn head_row(tempo: f32, props: TsProps, p: GuiPalette) -> Element<'static, TsMsg> {
    row![
        indicator_dot(p),
        text("TIMESTRETCH")
            .size(studio_type::BODY_SM)
            .font(fonts::display(Weight::Bold))
            .color(p.text),
        library_select(props, p),
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

fn ranges_row(range: u8, p: GuiPalette) -> Element<'static, TsMsg> {
    let mut pills = row![].spacing(gap::INLINE_TIGHT).align_y(Alignment::Center);
    for r in Consts::RANGES {
        pills = pills.push(range_pill(r, r == range, p));
    }
    row![Space::new().width(Length::Fill), pills]
        .align_y(Alignment::Center)
        .into()
}

/// Pulsing accent dot from `.ts-dot` (static — iced has no panel animation).
fn indicator_dot(p: GuiPalette) -> Element<'static, TsMsg> {
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

fn slider_row(tempo: f32, range: u8, p: GuiPalette) -> Element<'static, TsMsg> {
    row![
        mini_label(format!("\u{2212}{range}"), p),
        widgets::ts_slider(tempo, f32::from(range), p, TsMsg::SetTempo),
        mini_label(format!("+{range}"), p),
    ]
    .align_y(Alignment::Center)
    .spacing(gap::CONTENT)
    .into()
}

fn stats_row(ts: TimestretchState, props: TsProps, p: GuiPalette) -> Element<'static, TsMsg> {
    let ratio = ts.speed();
    row![
        stat_tile("RATIO", format!("{ratio:.3}\u{00d7}"), p),
        keylock_pill(props, p),
        Space::new().width(Length::Fill),
        nudge_group(p),
    ]
    .spacing(gap::INLINE)
    .align_y(Alignment::Center)
    .into()
}

/// The stretch-backend seam: real selector and key-lock when a native backend
/// is compiled in, empty placeholders otherwise.
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
mod backend {
    use iced::{
        Alignment, Background, Border, Element, Length, Shadow, Theme,
        font::Weight,
        widget::{button, container, overlay::menu, pick_list, row, text},
    };
    use kithara::prelude::StretchKind;

    use super::{Consts, TsMsg, TsProps};
    use crate::{
        gui::{fonts, icons::Icon, studio::tokens::studio_type, tokens::gap},
        theme::gui::GuiPalette,
    };

    /// Narrow stretch-backend selector (dropdown) shown next to the title.
    /// Only backends compiled into this target appear.
    pub(super) fn library_select(props: TsProps, p: GuiPalette) -> Element<'static, TsMsg> {
        pick_list(
            StretchKind::all(),
            Some(props.backend),
            TsMsg::SelectBackend,
        )
        .text_size(studio_type::MONO_SM)
        .font(fonts::mono(Weight::Semibold))
        .padding([3.0, 8.0])
        .style(move |_theme: &Theme, _status| pick_list::Style {
            text_color: p.accent,
            placeholder_color: p.muted,
            handle_color: p.muted,
            background: Background::Color(p.bg_inset),
            border: Border::default().width(1.0).color(p.line),
        })
        .menu_style(move |_theme: &Theme| menu::Style {
            background: Background::Color(p.bg_panel),
            border: Border::default().width(1.0).color(p.line),
            text_color: p.text,
            selected_text_color: p.bg,
            selected_background: Background::Color(p.accent),
            shadow: Shadow::default(),
        })
        .into()
    }

    pub(super) fn keylock_pill(props: TsProps, p: GuiPalette) -> Element<'static, TsMsg> {
        let on = props.keylock;
        // Active toggle: gold fill with on-gold (dark) text, per the design
        // system.
        let color = if on { p.bg } else { p.muted };
        let background = if on { p.accent } else { p.bg_inset };
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
            border: Border::default().width(1.0).color(border),
            ..button::Style::default()
        })
        .on_press(TsMsg::ToggleKeyLock)
        .into()
    }
}

/// Without a compiled-in stretch backend neither the selector nor key-lock
/// exists.
#[cfg(not(any(feature = "stretch-signalsmith", feature = "stretch-bungee")))]
mod backend {
    use iced::{Element, widget::Space};

    use super::{TsMsg, TsProps};
    use crate::theme::gui::GuiPalette;

    pub(super) fn library_select(_props: TsProps, _p: GuiPalette) -> Element<'static, TsMsg> {
        Space::new().into()
    }

    pub(super) fn keylock_pill(_props: TsProps, _p: GuiPalette) -> Element<'static, TsMsg> {
        Space::new().into()
    }
}

fn mini_label(label: String, p: GuiPalette) -> Element<'static, TsMsg> {
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

fn stat_tile(label: &str, value: String, p: GuiPalette) -> Element<'static, TsMsg> {
    container(
        column![
            text(label.to_string())
                .size(studio_type::MONO_XS)
                .font(fonts::mono(Weight::Medium))
                .color(p.muted),
            text(value)
                .size(studio_type::TRACK)
                .font(fonts::display(Weight::Medium))
                .color(p.text),
        ]
        .spacing(1.0),
    )
    .padding([0.0, 10.0])
    .center_y(Length::Fixed(Consts::STAT_H))
    .style(tile_style(p))
    .into()
}

fn nudge_group(p: GuiPalette) -> Element<'static, TsMsg> {
    row![
        nudge_button("\u{25c2}", TsMsg::Nudge(-0.05), p),
        nudge_button("\u{27f2}", TsMsg::ResetTempo, p),
        nudge_button("\u{25b8}", TsMsg::Nudge(0.05), p),
    ]
    .spacing(3.0)
    .into()
}

fn nudge_button(label: &str, message: TsMsg, p: GuiPalette) -> Element<'static, TsMsg> {
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
        background: Some(Background::Color(p.bg_inset)),
        text_color: p.text_dim,
        border: Border::default().width(1.0).color(p.line),
        ..button::Style::default()
    })
    .on_press(message)
    .into()
}

fn range_pill(range: u8, active: bool, p: GuiPalette) -> Element<'static, TsMsg> {
    pill(
        format!("\u{00b1}{range}%"),
        active,
        TsMsg::SetRange(range),
        p,
    )
}

fn pill(label: String, active: bool, message: TsMsg, p: GuiPalette) -> Element<'static, TsMsg> {
    // Active segment: gold fill with on-gold (dark) text, per the design system.
    let color = if active { p.bg } else { p.text_dim };
    let background = if active { p.accent } else { p.bg_inset };
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
        border: Border::default().width(1.0).color(border),
        ..button::Style::default()
    })
    .on_press(message)
    .into()
}

fn tile_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(Background::Color(p.bg_inset))
            .border(Border::default().width(1.0).color(p.line_soft))
    }
}
