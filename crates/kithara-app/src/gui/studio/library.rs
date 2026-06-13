use std::path::Path;

use iced::{
    Alignment, Background, Border, Color, Element, Length, Padding, Theme,
    font::Weight,
    widget::{
        Space, button,
        button::{Status, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, scrollable, text,
    },
};

use super::tokens::{studio_radius, studio_size, studio_space, studio_type};
use crate::{
    gui::{app::Kithara, fonts, icons::Icon, message::Message, tokens::gap, view::with_alpha},
    theme::gui::GuiPalette,
};

/// Per-track duration is not carried by `TrackEntry` on this layout, so the
/// time column shows a stable placeholder instead of fabricating a value.
const NO_DURATION: &str = "--:--";

pub(super) fn view_library(state: &Kithara) -> Element<'_, Message> {
    let p = state.palette;

    let body = state.ui_state.tracks.iter().enumerate().fold(
        column![].spacing(0.0),
        |col, (index, entry)| {
            let artist = row_artist(entry.url.as_deref());
            col.push(library_row(
                index,
                &entry.name,
                &artist,
                state.ui_state.current_track_index == Some(index),
                state.selected_track_index == Some(index),
                p,
            ))
        },
    );

    container(
        column![
            library_tabs(state.ui_state.shuffle_enabled, p),
            library_head_row(p),
            container(scrollable(body))
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(Padding {
                    top: 4.0,
                    right: 0.0,
                    bottom: 4.0,
                    left: 0.0,
                }),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fixed(studio_size::LIBRARY_WIDTH))
    .height(Length::Fill)
    .style(library_style(p))
    .into()
}

fn library_tabs(shuffle_on: bool, p: GuiPalette) -> Element<'static, Message> {
    let shuffle_color = if shuffle_on { p.accent } else { p.muted };
    container(
        row![
            library_label(p),
            Space::new().width(Length::Fill),
            button(Icon::Shuffle.view(12.0, shuffle_color))
                .padding([10.0, 12.0])
                .style(move |_theme: &Theme, status| shuffle_button_style(p, status))
                .on_press(Message::ToggleShuffle),
        ]
        .align_y(Alignment::Center)
        .spacing(0.0),
    )
    .width(Length::Fill)
    .padding(studio_space::LIBRARY_RAIL)
    .style(tabs_style(p))
    .into()
}

fn library_label(p: GuiPalette) -> Element<'static, Message> {
    container(
        row![
            Icon::Playlist.view(12.0, p.accent),
            text("LIBRARY")
                .size(studio_type::MONO_SM)
                .font(fonts::mono(Weight::Medium))
                .color(p.accent),
        ]
        .align_y(Alignment::Center)
        .spacing(5.0),
    )
    .into()
}

fn shuffle_button_style(p: GuiPalette, status: Status) -> ButtonStyle {
    let background = match status {
        Status::Hovered => Some(Background::Color(with_alpha(p.bg_panel_2, 0.6))),
        Status::Pressed => Some(Background::Color(p.accent_soft)),
        Status::Active | Status::Disabled => Some(Background::Color(Color::TRANSPARENT)),
    };
    ButtonStyle {
        background,
        text_color: p.text,
        border: Border::default().rounded(studio_radius::BUTTON),
        ..ButtonStyle::default()
    }
}

fn library_head_row(p: GuiPalette) -> Element<'static, Message> {
    container(
        row![
            head_cell("#", Length::Fixed(28.0), p),
            head_cell("TRACK", Length::FillPortion(14), p),
            head_cell("ARTIST", Length::FillPortion(10), p),
            head_cell("TIME", Length::Fixed(48.0), p),
        ]
        .align_y(Alignment::Center)
        .spacing(gap::CONTENT),
    )
    .height(Length::Fixed(studio_size::LIB_HEAD_HEIGHT))
    .padding(studio_space::LIBRARY_ROW)
    .style(head_style(p))
    .into()
}

fn library_row(
    index: usize,
    title: &str,
    artist: &str,
    current: bool,
    selected: bool,
    p: GuiPalette,
) -> Element<'static, Message> {
    button(
        container(
            row![
                body_cell(
                    format!("{:02}", index + 1),
                    Length::Fixed(28.0),
                    if current { p.accent } else { p.muted },
                    fonts::MONO,
                ),
                body_cell(
                    title.to_string(),
                    Length::FillPortion(14),
                    if current { p.text } else { p.text_dim },
                    fonts::display(Weight::Medium),
                ),
                body_cell(
                    artist.to_string(),
                    Length::FillPortion(10),
                    p.text_dim,
                    fonts::SANS,
                ),
                body_cell(
                    NO_DURATION.to_string(),
                    Length::Fixed(48.0),
                    p.muted,
                    fonts::MONO,
                ),
            ]
            .align_y(Alignment::Center)
            .spacing(gap::CONTENT),
        )
        .width(Length::Fill)
        .height(Length::Fixed(studio_size::LIB_ROW_HEIGHT))
        .padding(studio_space::LIBRARY_ROW),
    )
    .width(Length::Fill)
    .padding(0)
    .style(move |_theme: &Theme, status| row_style(p, current, selected, status))
    .on_press(Message::SelectTrack(index))
    .into()
}

fn head_cell(label: &str, width: Length, p: GuiPalette) -> Element<'static, Message> {
    container(
        text(label.to_string())
            .size(studio_type::MONO_XS)
            .font(fonts::mono(Weight::Medium))
            .color(p.muted),
    )
    .width(width)
    .into()
}

fn body_cell(
    label: String,
    width: Length,
    color: Color,
    font: iced::Font,
) -> Element<'static, Message> {
    container(
        text(label)
            .size(studio_type::BODY_SM)
            .font(font)
            .color(color),
    )
    .width(width)
    .into()
}

/// Derive a display artist from the track URL's grandparent directory
/// (`.../<artist>/<album>/<file>`), falling back to a neutral label.
fn row_artist(url: Option<&str>) -> String {
    let Some(url) = url else {
        return "Local source".to_string();
    };
    Path::new(url)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map_or_else(|| "Local source".to_string(), ToString::to_string)
}

fn library_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default()
            .background(Background::Color(with_alpha(p.bg_deep, 0.7)))
            .color(p.text)
    }
}

fn tabs_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default().background(Background::Color(with_alpha(p.bg_elev, 0.28)))
    }
}

fn head_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| {
        ContainerStyle::default().background(Background::Color(with_alpha(p.bg_deep, 0.18)))
    }
}

fn row_style(p: GuiPalette, current: bool, selected: bool, status: Status) -> ButtonStyle {
    let active_bg = if current {
        with_alpha(p.bg_panel, 0.5)
    } else if selected {
        with_alpha(p.bg_panel, 0.34)
    } else {
        Color::TRANSPARENT
    };
    let hover_bg = if current || selected {
        with_alpha(p.bg_panel_2, 0.68)
    } else {
        with_alpha(p.bg_panel, 0.24)
    };
    let pressed_bg = if current || selected {
        with_alpha(p.bg_panel_2, 0.82)
    } else {
        with_alpha(p.accent, 0.12)
    };

    let background = match status {
        Status::Active | Status::Disabled => Some(Background::Color(active_bg)),
        Status::Hovered => Some(Background::Color(hover_bg)),
        Status::Pressed => Some(Background::Color(pressed_bg)),
    };

    ButtonStyle {
        background,
        text_color: p.text,
        border: Border::default().rounded(studio_radius::BUTTON),
        ..ButtonStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::row_artist;

    #[test]
    fn row_artist_uses_grandparent_dir() {
        assert_eq!(
            row_artist(Some("/music/Boards of Canada/Geogaddi/track.mp3")),
            "Boards of Canada"
        );
    }

    #[test]
    fn row_artist_falls_back_without_url() {
        assert_eq!(row_artist(None), "Local source");
    }
}
