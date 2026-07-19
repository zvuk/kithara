use std::path::Path;

use iced::{
    Alignment, Background, Color, Element, Length, Theme,
    font::Weight,
    widget::{
        Space, button,
        button::{Status, Style as ButtonStyle},
        column, container,
        container::Style as ContainerStyle,
        row, scrollable, text,
    },
};

use super::{
    module::Module,
    tokens::{studio_size, studio_space, studio_type},
};
use crate::{
    gui::{fonts, icons::Icon, tokens::gap},
    theme::gui::GuiPalette,
};

/// One row of the library, already resolved by the composer.
pub(super) struct LibRow<'a> {
    pub(super) title: &'a str,
    pub(super) url: Option<&'a str>,
    /// Channel letters of the decks this track is loaded on.
    pub(super) loaded_on: Vec<char>,
    pub(super) selected: bool,
}

pub(super) struct LibProps<'a> {
    pub(super) rows: Vec<LibRow<'a>>,
    /// Load targets in deck order, as channel letters.
    pub(super) targets: Vec<char>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum LibMsg {
    Select(usize),
    /// Load row `.0` onto target `.1` — an index into `LibProps::targets`.
    Load(usize, usize),
}

/// Width of the per-row load controls plus the loaded-on marks.
const LOAD_COL_WIDTH: f32 = 76.0;
const MARKS_WIDTH: f32 = 24.0;

/// Per-track duration is not carried by `TrackEntry` on this layout, so the
/// time column shows a stable placeholder instead of fabricating a value.
const NO_DURATION: &str = "--:--";

pub(super) fn view_library(props: &LibProps<'_>, p: GuiPalette) -> Element<'static, LibMsg> {
    let body = props
        .rows
        .iter()
        .enumerate()
        .fold(column![].spacing(0.0), |col, (index, row)| {
            let artist = row_artist(row.url);
            col.push(library_row(
                index,
                LibraryRow {
                    title: row.title,
                    artist: &artist,
                    loaded_on: &row.loaded_on,
                    targets: &props.targets,
                    selected: row.selected,
                },
                p,
            ))
        });

    Module::new().bg(p.bg_panel).fill_height().wrap(
        column![
            library_tabs(p),
            library_head_row(p),
            container(scrollable(body))
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
}

fn library_tabs(p: GuiPalette) -> Element<'static, LibMsg> {
    container(
        row![library_label(p), Space::new().width(Length::Fill),]
            .align_y(Alignment::Center)
            .spacing(0.0),
    )
    .width(Length::Fill)
    .center_y(Length::Fixed(studio_size::LIB_HEAD_HEIGHT))
    .padding(studio_space::LIBRARY_RAIL)
    .style(tabs_style(p))
    .into()
}

fn library_label(p: GuiPalette) -> Element<'static, LibMsg> {
    container(
        row![
            Icon::Playlist.view(12.0, p.accent),
            text("LIBRARY")
                .size(studio_type::BODY_SM)
                .font(fonts::display(Weight::Bold))
                .color(p.text),
        ]
        .align_y(Alignment::Center)
        .spacing(5.0),
    )
    .into()
}

fn library_head_row(p: GuiPalette) -> Element<'static, LibMsg> {
    container(
        row![
            head_cell("#", Length::Fixed(28.0), p),
            head_cell("TRACK", Length::FillPortion(14), p),
            head_cell("ARTIST", Length::FillPortion(10), p),
            head_cell("TIME", Length::Fixed(48.0), p),
            head_cell("LOAD", Length::Fixed(LOAD_COL_WIDTH), p),
        ]
        .align_y(Alignment::Center)
        .spacing(gap::CONTENT),
    )
    .height(Length::Fixed(studio_size::LIB_COL_HEAD_HEIGHT))
    .padding(studio_space::LIBRARY_ROW)
    .style(head_style(p))
    .into()
}

/// Track display state for a single [`library_row`]: name, derived artist,
/// and whether the row is the current or the selected track.
#[derive(Clone, Copy)]
struct LibraryRow<'a> {
    artist: &'a str,
    title: &'a str,
    loaded_on: &'a [char],
    targets: &'a [char],
    selected: bool,
}

fn library_row(index: usize, row: LibraryRow<'_>, p: GuiPalette) -> Element<'static, LibMsg> {
    let LibraryRow {
        title,
        artist,
        loaded_on,
        targets,
        selected,
    } = row;
    let current = !loaded_on.is_empty();
    let marks = loaded_on.iter().collect::<String>();
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
                body_cell(marks, Length::Fixed(MARKS_WIDTH), p.accent, fonts::MONO),
                load_buttons(index, targets, p),
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
    .on_press(LibMsg::Select(index))
    .into()
}

/// One `->X` button per deck. The row itself only reports positions; the
/// composer decides which deck each position is.
fn load_buttons(index: usize, targets: &[char], p: GuiPalette) -> Element<'static, LibMsg> {
    targets
        .iter()
        .enumerate()
        .fold(
            row![].spacing(gap::INLINE_TIGHT).align_y(Alignment::Center),
            |controls, (target, label)| {
                controls.push(
                    button(
                        text(format!("\u{2192}{label}"))
                            .size(studio_type::MONO_XS)
                            .font(fonts::mono(Weight::Semibold))
                            .color(p.text_dim),
                    )
                    .padding([2.0, 6.0])
                    .style(move |_theme: &Theme, status| load_button_style(p, status))
                    .on_press(LibMsg::Load(index, target)),
                )
            },
        )
        .into()
}

fn load_button_style(p: GuiPalette, status: Status) -> ButtonStyle {
    let background = match status {
        Status::Hovered | Status::Pressed => Background::Color(p.accent),
        Status::Active | Status::Disabled => Background::Color(p.bg_inset),
    };
    ButtonStyle {
        background: Some(background),
        text_color: p.text_dim,
        ..ButtonStyle::default()
    }
}

fn head_cell(label: &str, width: Length, p: GuiPalette) -> Element<'static, LibMsg> {
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
) -> Element<'static, LibMsg> {
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

fn tabs_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| ContainerStyle::default().background(Background::Color(p.bg_panel_2))
}

fn head_style(p: GuiPalette) -> impl Fn(&Theme) -> ContainerStyle {
    move |_theme| ContainerStyle::default().background(Background::Color(p.bg_inset))
}

fn row_style(p: GuiPalette, current: bool, selected: bool, status: Status) -> ButtonStyle {
    let background = match status {
        Status::Active | Status::Disabled if current || selected => {
            Some(Background::Color(p.bg_elev))
        }
        Status::Active | Status::Disabled => Some(Background::Color(Color::TRANSPARENT)),
        Status::Hovered | Status::Pressed => Some(Background::Color(p.bg_elev)),
    };

    ButtonStyle {
        background,
        text_color: p.text,
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
