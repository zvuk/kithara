use iced::{
    Alignment, Background, Border, Color, Element, Event, Length, Point, Rectangle, Renderer, Size,
    Theme,
    alignment::{Horizontal, Vertical},
    mouse::{self, Cursor},
    widget::{
        Canvas, Column, Row, Space, Stack, button,
        button::{Status as ButtonStatus, Style as ButtonStyle},
        canvas::{self, Action, Frame, Geometry},
        column, container,
        container::Style as ContainerStyle,
        responsive, row, scrollable,
        scrollable::{
            Direction as ScrollDirection, Rail, Scrollbar, Scroller, Style as ScrollableStyle,
        },
    },
};
use num_traits::ToPrimitive;

use super::{
    Widget,
    behavior::{HorizontalPixelDrag, HorizontalPixelDragState, HoverState},
};
use crate::{
    module::TrackColumn,
    render::{
        ControlAction, ReadValue, Reads, Skin, TrackRow, UiEvent, fonts, shaped_text,
        theme::RenderPalette,
    },
    skin::TrackListSkin,
};

const fn em_dash() -> &'static str {
    "\u{2014}"
}

#[derive(Clone, Copy)]
struct ColumnLayout {
    column: TrackColumn,
    width: f32,
}

struct TrackListRowData {
    artist: Option<String>,
    bpm: Option<String>,
    current: bool,
    deck: Option<String>,
    energy: Option<u8>,
    key: Option<String>,
    selected: bool,
    time: Option<String>,
    title: String,
    transition: Option<String>,
}

impl From<&TrackRow<'_>> for TrackListRowData {
    fn from(track: &TrackRow<'_>) -> Self {
        Self {
            artist: track.artist.map(str::to_owned),
            bpm: track.bpm.map(str::to_owned),
            current: track.current,
            deck: track.deck.map(str::to_owned),
            energy: track.energy,
            key: track.key.map(str::to_owned),
            selected: track.selected,
            time: track.time.map(str::to_owned),
            title: track.title.to_owned(),
            transition: track.transition.map(str::to_owned),
        }
    }
}

struct TrackListStyle {
    bpm_badge_background: Color,
    bpm_badge_frame: Border,
    deck_chip_frame: Border,
    divider_color: Color,
    energy_bar_background: Color,
    metrics: TrackListSkin,
    palette: RenderPalette,
    row_frame: Border,
    scrollbar_background: Color,
    scroller_color: Color,
}

impl TrackListStyle {
    fn new(skin: &Skin) -> Self {
        Self {
            bpm_badge_background: skin.color(skin.track_list.bpm_badge_background),
            bpm_badge_frame: skin.border(skin.track_list.bpm_badge_frame),
            deck_chip_frame: skin.border(skin.track_list.deck_chip_frame),
            divider_color: skin.color(skin.track_list.divider_color),
            energy_bar_background: skin.color(skin.track_list.energy_bar_background),
            metrics: skin.track_list.clone(),
            palette: skin.palette,
            row_frame: skin.border(skin.track_list.row_frame),
            scrollbar_background: skin.color(skin.track_list.scrollbar_background),
            scroller_color: skin.color(skin.track_list.scroller_color),
        }
    }
}

#[derive(bon::Builder)]
pub(crate) struct TrackList<'path, 'columns, 'state, 'value, 'data, 'reads, 'skin> {
    path: &'path str,
    columns: &'columns [TrackColumn],
    columns_state: Option<&'state str>,
    value: Option<&'value ReadValue<'data>>,
    reads: &'reads dyn Reads,
    skin: &'skin Skin,
}

impl<'a> Widget<'a> for TrackList<'_, '_, '_, '_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::TrackList(tracks)) = self.value else {
            return Space::new().into();
        };
        let columns = column_layouts(self.columns, self.reads, self.columns_state, self.skin);
        let path = self.path.to_owned();
        let style = TrackListStyle::new(self.skin);
        let tracks: Vec<_> = tracks.iter().map(TrackListRowData::from).collect();
        responsive(move |size| track_list_table(&path, &tracks, &columns, &style, size.width))
            .into()
    }
}

fn track_list_table(
    path: &str,
    tracks: &[TrackListRowData],
    columns: &[ColumnLayout],
    style: &TrackListStyle,
    available_width: f32,
) -> Element<'static, UiEvent> {
    let minimum_width = minimum_table_width(columns);
    let overflowing = minimum_width > available_width;
    let flexible_title = !overflowing;
    let header = Row::with_children(columns.iter().copied().enumerate().map(|(index, column)| {
        header_cell(
            path,
            column,
            flexible_title,
            column_resizable(columns, index),
            style,
        )
    }))
    .align_y(Alignment::Center)
    .height(Length::Fixed(style.metrics.header_height))
    .width(Length::Fill);
    let header_background = style.palette.bg_panel;
    let header = container(header)
        .height(Length::Fixed(style.metrics.header_height))
        .width(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(header_background)));
    let rows = tracks.iter().enumerate().map(|(index, track)| {
        TrackListRow::builder()
            .style(style)
            .path(path)
            .index(index)
            .track(track)
            .columns(columns)
            .flexible_title(flexible_title)
            .build()
            .view()
    });
    let line_soft = style.palette.line_soft;
    let rows = container(
        Column::with_children(rows)
            .spacing(style.metrics.grid_gap)
            .width(Length::Fill),
    )
    .style(move |_| ContainerStyle::default().background(Background::Color(line_soft)));
    let table = scrollable(rows)
        .direction(ScrollDirection::Vertical(scrollbar(style)))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(scrollable_style(style));
    let footer = footer(tracks.len(), style);
    let content_width = if overflowing {
        Length::Fixed(minimum_width)
    } else {
        Length::Fill
    };
    let line_soft = style.palette.line_soft;
    let content = container(
        column![header, table, footer]
            .spacing(style.metrics.grid_gap)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(content_width)
    .height(Length::Fill)
    .style(move |_| ContainerStyle::default().background(Background::Color(line_soft)));

    if overflowing {
        scrollable(content)
            .direction(ScrollDirection::Horizontal(scrollbar(style)))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(scrollable_style(style))
            .into()
    } else {
        content.into()
    }
}

fn scrollbar(style: &TrackListStyle) -> Scrollbar {
    Scrollbar::new()
        .width(style.metrics.scrollbar_width)
        .margin(style.metrics.scrollbar_margin)
        .scroller_width(style.metrics.scrollbar_width)
}

fn scrollable_style(
    style: &TrackListStyle,
) -> impl Fn(&Theme, scrollable::Status) -> ScrollableStyle + 'static {
    let background = style.scrollbar_background;
    let scroller = style.scroller_color;
    move |theme, status| {
        let rail = Rail {
            background: Some(Background::Color(background)),
            border: Border::default(),
            scroller: Scroller {
                background: Background::Color(scroller),
                border: Border::default(),
            },
        };
        ScrollableStyle {
            horizontal_rail: rail,
            vertical_rail: rail,
            ..scrollable::default(theme, status)
        }
    }
}

fn column_resizable(columns: &[ColumnLayout], index: usize) -> bool {
    columns
        .get(index)
        .is_some_and(|column| column.column != TrackColumn::Title && index + 1 < columns.len())
}

#[derive(bon::Builder)]
struct TrackListRow<'path, 'columns, 'data, 'style> {
    style: &'style TrackListStyle,
    path: &'path str,
    index: usize,
    track: &'data TrackListRowData,
    columns: &'columns [ColumnLayout],
    flexible_title: bool,
}

impl<'a> Widget<'a> for TrackListRow<'_, '_, '_, '_> {
    fn view(self) -> Element<'a, UiEvent> {
        let cells = self
            .columns
            .iter()
            .copied()
            .enumerate()
            .map(|(column_index, column)| {
                let cell = row_cell(
                    column,
                    self.flexible_title,
                    self.index,
                    self.track,
                    self.style,
                );
                if column_resizable(self.columns, column_index) {
                    cell_with_divider(cell, column_length(column, self.flexible_title), self.style)
                } else {
                    cell
                }
            });
        button(
            Row::with_children(cells)
                .align_y(Alignment::Center)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fixed(self.style.metrics.row_height))
        .style(track_button_style(self.style, self.track.selected))
        .on_press(UiEvent::Control {
            path: self.path.to_owned(),
            action: ControlAction::SelectIndex(self.index),
        })
        .into()
    }
}

fn header_cell(
    path: &str,
    column: ColumnLayout,
    flexible_title: bool,
    resizable: bool,
    style: &TrackListStyle,
) -> Element<'static, UiEvent> {
    let label = column_label(column.column, &style.metrics).to_owned();
    let horizontal = if column.column == TrackColumn::Index {
        Horizontal::Right
    } else {
        Horizontal::Left
    };
    let width = column_length(column, flexible_title);
    let cell: Element<'static, UiEvent> = container(
        shaped_text(label)
            .font(fonts::mono(style.metrics.header_text.weight))
            .size(style.metrics.header_text.size)
            .color(style.palette.muted),
    )
    .padding([0.0, style.metrics.cell_padding_x])
    .width(width)
    .height(Length::Fill)
    .align_x(horizontal)
    .align_y(Vertical::Center)
    .into();
    if !resizable {
        return cell;
    }
    let divider = Canvas::new(ColumnDivider {
        color: style.divider_color,
        divider_width: style.metrics.divider_width,
        drag: HorizontalPixelDrag::builder()
            .path(format!("{path}/width/{}", column.column.endpoint_name()))
            .value(column.width)
            .minimum(style.metrics.min_column_width)
            .hover(HoverState::new(mouse::Interaction::ResizingHorizontally))
            .build(),
    })
    .width(Length::Fixed(style.metrics.divider_hit_width))
    .height(Length::Fill);
    let divider = container(divider)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Right);
    Stack::with_children([cell, divider.into()])
        .width(width)
        .height(Length::Fill)
        .into()
}

fn row_cell(
    column: ColumnLayout,
    flexible_title: bool,
    index: usize,
    track: &TrackListRowData,
    style: &TrackListStyle,
) -> Element<'static, UiEvent> {
    let width = column_length(column, flexible_title);
    match column.column {
        TrackColumn::Index => text_cell(
            format!("{:02}", index + 1),
            width,
            style.metrics.index_text,
            fonts::mono,
            style.palette.muted,
            Horizontal::Right,
            style,
        ),
        TrackColumn::Deck => deck_cell(track, width, style),
        TrackColumn::Title => text_cell(
            value_or_dash(&track.title),
            width,
            style.metrics.title_text,
            fonts::display,
            style.palette.text,
            Horizontal::Left,
            style,
        ),
        TrackColumn::Artist => text_cell(
            optional_or_dash(track.artist.as_deref()),
            width,
            style.metrics.artist_text,
            fonts::sans,
            style.palette.text_dim,
            Horizontal::Left,
            style,
        ),
        TrackColumn::Bpm => bpm_cell(track.bpm.as_deref(), width, style),
        TrackColumn::Key => text_cell(
            optional_or_dash(track.key.as_deref()),
            width,
            style.metrics.key_text,
            fonts::mono,
            style.palette.accent,
            Horizontal::Left,
            style,
        ),
        TrackColumn::Time => text_cell(
            optional_or_dash(track.time.as_deref()),
            width,
            style.metrics.time_text,
            fonts::mono,
            style.palette.text_dim,
            Horizontal::Right,
            style,
        ),
        TrackColumn::Energy => energy_cell(track.energy, width, style),
        TrackColumn::Transition => text_cell(
            track
                .transition
                .as_deref()
                .map_or_else(|| em_dash().to_owned(), str::to_uppercase),
            width,
            style.metrics.transition_text,
            fonts::mono,
            style.palette.muted,
            Horizontal::Left,
            style,
        ),
    }
}

fn text_cell(
    value: String,
    width: Length,
    font: crate::skin::FontSkin,
    family: fn(crate::skin::FontWeight) -> iced::Font,
    color: Color,
    horizontal: Horizontal,
    style: &TrackListStyle,
) -> Element<'static, UiEvent> {
    container(
        shaped_text(value)
            .font(family(font.weight))
            .size(font.size)
            .color(color),
    )
    .padding([0.0, style.metrics.cell_padding_x])
    .width(width)
    .height(Length::Fill)
    .align_x(horizontal)
    .align_y(Vertical::Center)
    .into()
}

fn deck_cell(
    track: &TrackListRowData,
    width: Length,
    style: &TrackListStyle,
) -> Element<'static, UiEvent> {
    let value = optional_or_dash(track.deck.as_deref());
    let active = track.current && track.deck.is_some();
    let chip = container(
        shaped_text(value)
            .font(fonts::mono(style.metrics.deck_text.weight))
            .size(style.metrics.deck_text.size)
            .color(if active {
                style.palette.bg_deep
            } else {
                style.palette.text_dim
            }),
    )
    .width(Length::Fixed(style.metrics.deck_chip_width))
    .height(Length::Fixed(style.metrics.deck_chip_height))
    .align_x(Horizontal::Center)
    .align_y(Vertical::Center)
    .style({
        let border = style.deck_chip_frame;
        let background = active.then_some(Background::Color(style.palette.accent));
        move |_| ContainerStyle {
            background,
            border,
            ..ContainerStyle::default()
        }
    });
    container(chip)
        .width(width)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
}

fn bpm_cell(
    value: Option<&str>,
    width: Length,
    style: &TrackListStyle,
) -> Element<'static, UiEvent> {
    let badge = container(
        shaped_text(optional_or_dash(value))
            .font(fonts::mono(style.metrics.bpm_text.weight))
            .size(style.metrics.bpm_text.size)
            .color(style.palette.text),
    )
    .padding([0.0, style.metrics.bpm_badge_padding_x])
    .height(Length::Fixed(style.metrics.bpm_badge_height))
    .align_y(Vertical::Center)
    .style({
        let border = style.bpm_badge_frame;
        let background = style.bpm_badge_background;
        move |_| {
            ContainerStyle::default()
                .background(Background::Color(background))
                .border(border)
        }
    });
    container(badge)
        .padding([0.0, style.metrics.cell_padding_x])
        .width(width)
        .height(Length::Fill)
        .align_y(Vertical::Center)
        .into()
}

fn energy_cell(
    value: Option<u8>,
    width: Length,
    style: &TrackListStyle,
) -> Element<'static, UiEvent> {
    let value = value.map(|value| value.min(100));
    let ratio = value.map_or(0.0, |value| f32::from(value) / 100.0);
    let filled = style.metrics.energy_bar_width * ratio;
    let empty = style.metrics.energy_bar_width - filled;
    let accent = style.palette.accent;
    let fill = container(Space::new())
        .width(Length::Fixed(filled))
        .height(Length::Fixed(style.metrics.energy_bar_height))
        .style(move |_| ContainerStyle::default().background(Background::Color(accent)));
    let remainder = container(Space::new())
        .width(Length::Fixed(empty))
        .height(Length::Fixed(style.metrics.energy_bar_height))
        .style({
            let background = style.energy_bar_background;
            move |_| ContainerStyle::default().background(Background::Color(background))
        });
    let bar = row![fill, remainder]
        .width(Length::Fixed(style.metrics.energy_bar_width))
        .height(Length::Fixed(style.metrics.energy_bar_height));
    let label = value.map_or_else(|| em_dash().to_owned(), |value| value.to_string());
    container(
        row![
            bar,
            shaped_text(label)
                .font(fonts::mono(style.metrics.energy_text.weight))
                .size(style.metrics.energy_text.size)
                .color(style.palette.accent),
        ]
        .spacing(style.metrics.energy_bar_gap)
        .align_y(Alignment::Center),
    )
    .padding([0.0, style.metrics.cell_padding_x])
    .width(width)
    .height(Length::Fill)
    .align_y(Vertical::Center)
    .into()
}

struct ColumnDivider {
    color: Color,
    divider_width: f32,
    drag: HorizontalPixelDrag,
}

impl canvas::Program<UiEvent> for ColumnDivider {
    type State = HorizontalPixelDragState;

    fn draw(
        &self,
        _state: &HorizontalPixelDragState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let x = (bounds.width - self.divider_width) / 2.0;
        frame.fill_rectangle(
            Point::new(x, 0.0),
            Size::new(self.divider_width, bounds.height),
            self.color,
        );
        vec![frame.into_geometry()]
    }

    delegate::delegate! {
        to self.drag {
            fn update(
                &self,
                state: &mut HorizontalPixelDragState,
                event: &Event,
                bounds: Rectangle,
                cursor: Cursor,
            ) -> Option<Action<UiEvent>>;
            fn mouse_interaction(
                &self,
                state: &HorizontalPixelDragState,
                bounds: Rectangle,
                cursor: Cursor,
            ) -> mouse::Interaction;
        }
    }
}

fn cell_with_divider(
    cell: Element<'static, UiEvent>,
    width: Length,
    style: &TrackListStyle,
) -> Element<'static, UiEvent> {
    let color = style.divider_color;
    let line = container(Space::new())
        .width(Length::Fixed(style.metrics.divider_width))
        .height(Length::Fill)
        .style(move |_| ContainerStyle::default().background(Background::Color(color)));
    let inset = ((style.metrics.divider_hit_width - style.metrics.divider_width) / 2.0).max(0.0);
    let divider = container(line)
        .padding([0.0, inset])
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Right);
    Stack::with_children([cell, divider.into()])
        .width(width)
        .height(Length::Fill)
        .into()
}

fn footer(count: usize, style: &TrackListStyle) -> Element<'static, UiEvent> {
    let left = format!(
        "{} \u{00b7} {count} {}",
        style.metrics.labels.footer_component, style.metrics.labels.footer_tracks
    );
    let right = style.metrics.labels.footer_usage.clone();
    let font = style.metrics.footer_text;
    let content = row![
        shaped_text(left)
            .font(fonts::mono(font.weight))
            .size(font.size)
            .color(style.palette.muted),
        Space::new().width(Length::Fill),
        shaped_text(right)
            .font(fonts::mono(font.weight))
            .size(font.size)
            .color(style.palette.muted),
    ]
    .align_y(Alignment::Center);
    let background = style.palette.bg_footer;
    container(content)
        .padding([0.0, style.metrics.footer_padding_x])
        .width(Length::Fill)
        .height(Length::Fixed(style.metrics.footer_height))
        .align_y(Vertical::Center)
        .style(move |_| ContainerStyle::default().background(Background::Color(background)))
        .into()
}

fn column_visible(reads: &dyn Reads, prefix: Option<&str>, column: TrackColumn) -> bool {
    let Some(prefix) = prefix else {
        return true;
    };
    let endpoint = format!("{prefix}.{}", column.endpoint_name());
    !matches!(reads.get(&endpoint), Some(ReadValue::Bool(false)))
}

fn column_layouts(
    columns: &[TrackColumn],
    reads: &dyn Reads,
    prefix: Option<&str>,
    skin: &Skin,
) -> Vec<ColumnLayout> {
    columns
        .iter()
        .copied()
        .filter(|column| column_visible(reads, prefix, *column))
        .map(|column| ColumnLayout {
            column,
            width: effective_column_width(reads, prefix, column, skin),
        })
        .collect()
}

fn default_column_width(column: TrackColumn, skin: &Skin) -> f32 {
    match column {
        TrackColumn::Index => skin.track_list.index_width,
        TrackColumn::Deck => skin.track_list.deck_width,
        TrackColumn::Title => skin.track_list.title_min_width,
        TrackColumn::Artist => skin.track_list.artist_width,
        TrackColumn::Bpm => skin.track_list.bpm_width,
        TrackColumn::Key => skin.track_list.key_width,
        TrackColumn::Time => skin.track_list.time_width,
        TrackColumn::Energy => skin.track_list.energy_width,
        TrackColumn::Transition => skin.track_list.transition_width,
    }
}

fn effective_column_width(
    reads: &dyn Reads,
    prefix: Option<&str>,
    column: TrackColumn,
    skin: &Skin,
) -> f32 {
    let default = default_column_width(column, skin);
    let Some(prefix) = prefix else {
        return default;
    };
    let endpoint = format!("{prefix}.width.{}", column.endpoint_name());
    let Some(ReadValue::Scalar(width)) = reads.get(&endpoint) else {
        return default;
    };
    let Some(width) = width.to_f32().filter(|width| width.is_finite()) else {
        return default;
    };
    let minimum = if column == TrackColumn::Title {
        skin.track_list.title_min_width
    } else {
        skin.track_list.min_column_width
    };
    width.max(minimum)
}

fn minimum_table_width(columns: &[ColumnLayout]) -> f32 {
    columns.iter().map(|column| column.width).sum()
}

fn column_label(column: TrackColumn, metrics: &TrackListSkin) -> &str {
    let labels = &metrics.labels;
    match column {
        TrackColumn::Index => &labels.index,
        TrackColumn::Deck => &labels.deck,
        TrackColumn::Title => &labels.title,
        TrackColumn::Artist => &labels.artist,
        TrackColumn::Bpm => &labels.bpm,
        TrackColumn::Key => &labels.key,
        TrackColumn::Time => &labels.time,
        TrackColumn::Energy => &labels.energy,
        TrackColumn::Transition => &labels.transition,
    }
}

fn column_length(column: ColumnLayout, flexible_title: bool) -> Length {
    if flexible_title && column.column == TrackColumn::Title {
        Length::Fill
    } else {
        Length::Fixed(column.width)
    }
}

fn value_or_dash(value: &str) -> String {
    if value.is_empty() {
        em_dash().to_owned()
    } else {
        value.to_owned()
    }
}

fn optional_or_dash(value: Option<&str>) -> String {
    value.map_or_else(|| em_dash().to_owned(), value_or_dash)
}

fn track_button_style(
    style: &TrackListStyle,
    selected: bool,
) -> impl Fn(&Theme, ButtonStatus) -> ButtonStyle + 'static {
    let palette = style.palette;
    let border = style.row_frame;
    move |_theme, status| {
        let background = match status {
            ButtonStatus::Pressed => palette.accent_soft,
            _ if selected => palette.bg_select,
            ButtonStatus::Hovered => palette.bg_panel_2,
            ButtonStatus::Active | ButtonStatus::Disabled => palette.bg_inset,
        };
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: palette.text,
            border,
            ..ButtonStyle::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    struct ColumnReads(Option<bool>);

    impl Reads for ColumnReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            (endpoint == "columns.title")
                .then_some(self.0)
                .flatten()
                .map(ReadValue::Bool)
        }
    }

    struct WidthReads;

    impl Reads for WidthReads {
        fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
            match endpoint {
                "columns.index" => Some(ReadValue::Bool(false)),
                "columns.width.artist" => Some(ReadValue::Scalar(240.0)),
                _ => None,
            }
        }
    }

    #[kithara::test]
    fn absent_column_endpoint_is_visible() {
        assert!(column_visible(
            &ColumnReads(None),
            Some("columns"),
            TrackColumn::Title
        ));
    }

    #[kithara::test]
    fn false_column_endpoint_is_hidden() {
        assert!(!column_visible(
            &ColumnReads(Some(false)),
            Some("columns"),
            TrackColumn::Title
        ));
    }

    #[kithara::test]
    fn total_width_uses_host_override_and_title_minimum() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[TrackColumn::Index, TrackColumn::Title, TrackColumn::Artist],
            &WidthReads,
            Some("columns"),
            skin,
        );

        assert_eq!(columns.len(), 2);
        assert_eq!(columns[1].width, 240.0);
        assert_eq!(
            minimum_table_width(&columns),
            skin.track_list.title_min_width + 240.0
        );
    }
}
