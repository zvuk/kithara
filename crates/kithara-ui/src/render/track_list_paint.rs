use std::cell::RefCell;

use iced::{
    Rectangle, Renderer, Theme,
    mouse::Cursor,
    widget::canvas::{self, Frame, Geometry},
};
use num_traits::ToPrimitive;

use super::{Skin, UiEvent, controls::RetainedCanvasState};
use crate::{
    backends::replay_ordered,
    draw::{DrawList, DrawListBuilder, Pt, Rect, Rgba, Transform},
    engine::{ScrollConfig, ScrollState},
    interact::{
        ScrollAxis,
        recognizers::{ItemDrag, ScalarState},
    },
    module::TrackColumn,
    skin::{ColorRole, FontFamily, FontSkin, FrameSkin, TextRoleSkin},
    text::TextContext,
    widgets::track_list::{
        ColumnLayout, TrackListRowData, column_label, column_resizable, minimum_table_width,
        track_list_body, track_list_content_height, track_list_content_width, track_list_dividers,
        track_list_overflows, track_list_row_at, track_list_row_pitch, track_list_row_rect,
        track_list_vertical_scrollbar_rect,
    },
};

pub(super) struct TrackListPaint<'skin> {
    pub(super) columns: Vec<ColumnLayout>,
    pub(super) path: String,
    pub(super) rows: Vec<TrackListRowData>,
    pub(super) skin: &'skin Skin,
}

impl<'skin> TrackListPaint<'skin> {
    pub(super) fn new(
        path: &str,
        rows: Vec<TrackListRowData>,
        columns: Vec<ColumnLayout>,
        skin: &'skin Skin,
    ) -> Self {
        Self {
            columns,
            path: path.to_owned(),
            rows,
            skin,
        }
    }

    pub(super) fn geometry(
        &self,
        state: &TrackListState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        let (horizontal, vertical) = state.paint_offsets();
        let mut frame = Frame::new(renderer, bounds.size());
        let mut text = state.text.borrow_mut();
        let text = text.get_or_insert_with(|| self.skin.text_resources().into());
        let local = local_rect(bounds);
        let point = cursor.position_in(bounds).map(Into::into);
        let hovered = hovered_row(point, local, self.rows.len(), horizontal, vertical, self);
        let list = self.commands(
            text,
            local,
            horizontal,
            vertical,
            hovered,
            state.pressed_index,
        );
        replay_ordered(&list, &mut frame, self.skin.text_resources());
        vec![frame.into_geometry()]
    }

    pub(super) fn config(&self) -> TrackListConfig {
        TrackListConfig {
            body_inset: self.skin.track_list.header_height
                + self.skin.track_list.footer_height
                + self.skin.track_list.grid_gap * 2.0,
            content_height: track_list_content_height(self.rows.len(), self.skin),
            content_width: minimum_table_width(&self.columns),
            divider_columns: self
                .columns
                .iter()
                .enumerate()
                .filter(|(index, _)| column_resizable(&self.columns, *index))
                .map(|(_, column)| column.column)
                .collect(),
            row_count: self.rows.len(),
        }
    }

    pub(super) fn commands(
        &self,
        text: &mut TextContext,
        bounds: Rect,
        horizontal: f32,
        vertical: f32,
        hovered: Option<usize>,
        pressed: Option<usize>,
    ) -> DrawList {
        let overflowing = track_list_overflows(&self.columns, bounds.w);
        let horizontal = if overflowing { horizontal } else { 0.0 };
        let content_width = track_list_content_width(&self.columns, bounds.w);
        let mut content = DrawListBuilder::default();
        content.fill_rect(
            Rect {
                w: content_width,
                x: -horizontal,
                ..bounds
            },
            self.skin.palette.line_soft,
        );
        self.paint_header(&mut content, text, bounds, horizontal);
        self.paint_body(
            &mut content,
            text,
            bounds,
            (horizontal, vertical),
            (hovered, pressed),
        );
        paint_footer(self, &mut content, text, bounds, horizontal);
        paint_vertical_scrollbar(self, &mut content, bounds, horizontal, vertical);
        if overflowing {
            paint_horizontal_scrollbar(self, &mut content, bounds, horizontal);
            let mut clipped = DrawListBuilder::default();
            clipped.clip(bounds, content.finish());
            clipped.finish()
        } else {
            content.finish()
        }
    }
    fn paint_header(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        horizontal: f32,
    ) {
        let header = Rect {
            h: self.skin.track_list.header_height,
            w: track_list_content_width(&self.columns, bounds.w),
            x: -horizontal,
            y: bounds.y,
        };
        list.fill_rect(header, self.skin.palette.bg_panel);
        for (column, cell) in column_cells(bounds, &self.columns, horizontal) {
            let align = if column.column == TrackColumn::Index {
                TextAlign::Right
            } else {
                TextAlign::Left
            };
            paint_text(
                list,
                text,
                column_label(column.column, &self.skin.track_list),
                Rect {
                    h: header.h,
                    ..cell
                },
                (
                    self.skin.track_list.header_text,
                    FontFamily::Mono,
                    self.skin.palette.muted,
                    self.skin.track_list.cell_padding_x,
                    align,
                ),
            );
        }
        for divider in track_list_dividers(bounds, &self.columns, horizontal, self.skin) {
            list.fill_rect(
                divider.paint,
                self.skin.rgba(self.skin.track_list.divider_color),
            );
        }
    }

    fn paint_body(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        offsets: (f32, f32),
        interaction: (Option<usize>, Option<usize>),
    ) {
        let (horizontal, vertical) = offsets;
        let (hovered, pressed) = interaction;
        let body = track_list_body(bounds, self.skin);
        let pitch = track_list_row_pitch(self.skin);
        let visible = visible_rows(self.rows.len(), pitch, body.h, vertical);
        let mut rows = DrawListBuilder::default();
        for index in visible {
            let row_bounds = track_list_row_rect(
                bounds,
                &self.columns,
                index,
                horizontal,
                vertical,
                self.skin,
            );
            self.paint_row(
                &mut rows,
                text,
                index,
                row_bounds,
                hovered == Some(index),
                pressed == Some(index),
            );
        }
        list.clip(body, rows.finish());
    }

    fn paint_row(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        index: usize,
        bounds: Rect,
        hovered: bool,
        pressed: bool,
    ) {
        let row = &self.rows[index];
        let frame = self.skin.track_list.row_frame;
        let fill = if pressed {
            self.skin.palette.accent_soft
        } else if row.selected {
            self.skin.palette.bg_select
        } else if hovered {
            self.skin.palette.bg_panel_2
        } else {
            self.skin.palette.bg_inset
        };
        list.fill_rounded_rect(bounds, frame.radius, fill);
        paint_frame(list, bounds, frame, self.skin);
        for (column, cell) in column_cells(
            Rect {
                w: bounds.w,
                x: bounds.x,
                ..bounds
            },
            &self.columns,
            0.0,
        ) {
            self.paint_cell(list, text, column.column, index, row, cell);
        }
        for divider in track_list_dividers(
            Rect {
                h: self.skin.track_list.header_height,
                w: bounds.w,
                x: bounds.x,
                y: 0.0,
            },
            &self.columns,
            0.0,
            self.skin,
        ) {
            list.fill_rect(
                Rect {
                    h: bounds.h,
                    y: bounds.y,
                    ..divider.paint
                },
                self.skin.rgba(self.skin.track_list.divider_color),
            );
        }
    }

    fn paint_cell(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        column: TrackColumn,
        index: usize,
        row: &TrackListRowData,
        bounds: Rect,
    ) {
        match column {
            TrackColumn::Index => paint_text(
                list,
                text,
                &format!("{:02}", index + 1),
                bounds,
                (
                    self.skin.track_list.index_text,
                    FontFamily::Mono,
                    self.skin.palette.muted,
                    self.skin.track_list.cell_padding_x,
                    TextAlign::Right,
                ),
            ),
            TrackColumn::Deck => paint_deck(self, list, text, row.deck.as_deref(), bounds),
            TrackColumn::Title => paint_text(
                list,
                text,
                value_or_dash(&row.title),
                bounds,
                (
                    self.skin.track_list.title_text,
                    FontFamily::Display,
                    self.skin.palette.text,
                    self.skin.track_list.cell_padding_x,
                    TextAlign::Left,
                ),
            ),
            TrackColumn::Artist => paint_text(
                list,
                text,
                optional_or_dash(row.artist.as_deref()),
                bounds,
                (
                    self.skin.track_list.artist_text,
                    FontFamily::Sans,
                    self.skin.palette.text_dim,
                    self.skin.track_list.cell_padding_x,
                    TextAlign::Left,
                ),
            ),
            TrackColumn::Bpm => paint_bpm(self, list, text, row.bpm.as_deref(), bounds),
            TrackColumn::Key => paint_text(
                list,
                text,
                optional_or_dash(row.key.as_deref()),
                bounds,
                (
                    self.skin.track_list.key_text,
                    FontFamily::Mono,
                    self.skin.palette.accent,
                    self.skin.track_list.cell_padding_x,
                    TextAlign::Left,
                ),
            ),
            TrackColumn::Time => paint_text(
                list,
                text,
                optional_or_dash(row.time.as_deref()),
                bounds,
                (
                    self.skin.track_list.time_text,
                    FontFamily::Mono,
                    self.skin.palette.text_dim,
                    self.skin.track_list.cell_padding_x,
                    TextAlign::Right,
                ),
            ),
            TrackColumn::Energy => paint_energy(self, list, text, row.energy, bounds),
            TrackColumn::Transition => {
                let transition = row
                    .transition
                    .as_deref()
                    .map_or_else(|| "\u{2014}".to_owned(), str::to_uppercase);
                paint_text(
                    list,
                    text,
                    &transition,
                    bounds,
                    (
                        self.skin.track_list.transition_text,
                        FontFamily::Mono,
                        self.skin.palette.muted,
                        self.skin.track_list.cell_padding_x,
                        TextAlign::Left,
                    ),
                );
            }
        }
    }
}

fn paint_deck(
    paint: &TrackListPaint<'_>,
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    marks: Option<&str>,
    bounds: Rect,
) {
    let Some(marks) = marks else {
        return;
    };
    let chip = Rect {
        h: paint.skin.track_list.deck_chip_height,
        w: paint.skin.track_list.deck_chip_width,
        x: bounds.x + (bounds.w - paint.skin.track_list.deck_chip_width) / 2.0,
        y: bounds.y + (bounds.h - paint.skin.track_list.deck_chip_height) / 2.0,
    };
    let frame = paint.skin.track_list.deck_chip_frame;
    list.fill_rounded_rect(chip, frame.radius, paint.skin.palette.accent);
    paint_frame(list, chip, frame, paint.skin);
    paint_text(
        list,
        text,
        marks,
        chip,
        (
            paint.skin.track_list.deck_text,
            FontFamily::Mono,
            paint.skin.palette.bg_deep,
            0.0,
            TextAlign::Center,
        ),
    );
}

fn paint_bpm(
    paint: &TrackListPaint<'_>,
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    value: Option<&str>,
    bounds: Rect,
) {
    let content = optional_or_dash(value);
    let run = shape(
        text,
        content,
        paint.skin.track_list.bpm_text,
        FontFamily::Mono,
        None,
    );
    let badge = Rect {
        h: paint.skin.track_list.bpm_badge_height,
        w: run.width() + paint.skin.track_list.bpm_badge_padding_x * 2.0,
        x: bounds.x + paint.skin.track_list.cell_padding_x,
        y: bounds.y + (bounds.h - paint.skin.track_list.bpm_badge_height) / 2.0,
    };
    let frame = paint.skin.track_list.bpm_badge_frame;
    list.fill_rounded_rect(
        badge,
        frame.radius,
        paint.skin.rgba(paint.skin.track_list.bpm_badge_background),
    );
    paint_frame(list, badge, frame, paint.skin);
    list.text(
        &run,
        content,
        Transform::translate(Pt {
            x: badge.x + paint.skin.track_list.bpm_badge_padding_x,
            y: badge.y + (badge.h - run.height()) / 2.0,
        }),
        paint.skin.palette.text,
    );
}

fn paint_energy(
    paint: &TrackListPaint<'_>,
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    value: Option<u8>,
    bounds: Rect,
) {
    let value = value.map(|value| value.min(100));
    let ratio = value.map_or(0.0, |value| f32::from(value) / 100.0);
    let bar = Rect {
        h: paint.skin.track_list.energy_bar_height,
        w: paint.skin.track_list.energy_bar_width,
        x: bounds.x + paint.skin.track_list.cell_padding_x,
        y: bounds.y + (bounds.h - paint.skin.track_list.energy_bar_height) / 2.0,
    };
    list.fill_rect(
        bar,
        paint.skin.rgba(paint.skin.track_list.energy_bar_background),
    );
    list.fill_rect(
        Rect {
            w: bar.w * ratio,
            ..bar
        },
        paint.skin.palette.accent,
    );
    let label = value.map_or_else(|| "\u{2014}".to_owned(), |value| value.to_string());
    let label_x = bar.x + bar.w + paint.skin.track_list.energy_bar_gap;
    let label_bounds = Rect {
        h: bounds.h,
        w: (bounds.x + bounds.w - label_x - paint.skin.track_list.cell_padding_x).max(0.0),
        x: label_x,
        y: bounds.y,
    };
    paint_text(
        list,
        text,
        &label,
        label_bounds,
        (
            paint.skin.track_list.energy_text,
            FontFamily::Mono,
            paint.skin.palette.accent,
            0.0,
            TextAlign::Left,
        ),
    );
}

fn paint_footer(
    paint: &TrackListPaint<'_>,
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    bounds: Rect,
    horizontal: f32,
) {
    let footer = Rect {
        h: paint.skin.track_list.footer_height,
        w: track_list_content_width(&paint.columns, bounds.w),
        x: -horizontal,
        y: bounds.y + bounds.h - paint.skin.track_list.footer_height,
    };
    list.fill_rect(footer, paint.skin.palette.bg_footer);
    let label = format!(
        "{} {}",
        paint.rows.len(),
        paint.skin.track_list.labels.footer_tracks
    );
    paint_text(
        list,
        text,
        &label,
        footer,
        (
            paint.skin.track_list.footer_text,
            FontFamily::Mono,
            paint.skin.palette.muted,
            paint.skin.track_list.footer_padding_x,
            TextAlign::Left,
        ),
    );
}

fn paint_vertical_scrollbar(
    paint: &TrackListPaint<'_>,
    list: &mut DrawListBuilder,
    bounds: Rect,
    horizontal: f32,
    offset: f32,
) {
    let body = track_list_body(bounds, paint.skin);
    let content = track_list_content_height(paint.rows.len(), paint.skin);
    let Some(rail) = track_list_vertical_scrollbar_rect(
        bounds,
        &paint.columns,
        paint.rows.len(),
        horizontal,
        paint.skin,
    ) else {
        return;
    };
    paint_scrollbar(
        list,
        rail,
        content,
        body.h,
        offset,
        ScrollAxis::Vertical,
        paint.skin,
    );
}

fn paint_horizontal_scrollbar(
    paint: &TrackListPaint<'_>,
    list: &mut DrawListBuilder,
    bounds: Rect,
    offset: f32,
) {
    paint_scrollbar(
        list,
        Rect {
            h: paint.skin.track_list.scrollbar_width,
            w: bounds.w,
            x: bounds.x,
            y: bounds.y + bounds.h
                - paint.skin.track_list.scrollbar_margin
                - paint.skin.track_list.scrollbar_width,
        },
        track_list_content_width(&paint.columns, bounds.w),
        bounds.w,
        offset,
        ScrollAxis::Horizontal,
        paint.skin,
    );
}

impl canvas::Program<UiEvent> for TrackListPaint<'_> {
    type State = TrackListState;

    fn draw(
        &self,
        state: &TrackListState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        self.geometry(state, renderer, theme, bounds, cursor)
    }
}

#[derive(Default)]
pub(super) struct TrackListState {
    configured: bool,
    pub(super) dividers: Vec<(TrackColumn, ScalarState)>,
    pub(super) horizontal: ScrollState,
    path: String,
    pub(super) drag_index: Option<usize>,
    pub(super) pressed_index: Option<usize>,
    pub(super) row_drag: ItemDrag,
    text: RefCell<Option<TextContext>>,
    pub(super) vertical: ScrollState,
}

#[derive(Clone)]
pub(super) struct TrackListConfig {
    body_inset: f32,
    content_height: f32,
    content_width: f32,
    divider_columns: Vec<TrackColumn>,
    row_count: usize,
}

impl TrackListState {
    pub(super) fn reconcile(&mut self, path: &str, config: &TrackListConfig) {
        self.rebind(path);
        let horizontal = ScrollConfig::plain(ScrollAxis::Horizontal, config.content_width);
        let vertical = ScrollConfig::plain(ScrollAxis::Vertical, config.content_height);
        if !self.configured {
            self.drag_index = None;
            self.horizontal = ScrollState::new(horizontal);
            self.row_drag = ItemDrag::default();
            self.vertical = ScrollState::new(vertical);
            self.dividers = config
                .divider_columns
                .iter()
                .copied()
                .map(|column| (column, ScalarState::default()))
                .collect();
            self.configured = true;
        } else {
            self.horizontal.reconcile(horizontal);
            self.vertical.reconcile(vertical);
            let mut retained = std::mem::take(&mut self.dividers);
            self.dividers = config
                .divider_columns
                .iter()
                .copied()
                .map(|column| {
                    retained
                        .iter()
                        .position(|(candidate, _)| *candidate == column)
                        .map_or_else(
                            || (column, ScalarState::default()),
                            |index| retained.remove(index),
                        )
                })
                .collect();
        }
        if self
            .drag_index
            .is_some_and(|index| index >= config.row_count)
        {
            self.drag_index = None;
            self.pressed_index = None;
            self.row_drag = ItemDrag::default();
        }
        if self
            .pressed_index
            .is_some_and(|index| index >= config.row_count)
        {
            self.pressed_index = None;
        }
    }

    pub(super) fn set_viewport(&mut self, size: iced::Size, config: &TrackListConfig) {
        self.horizontal.set_viewport(size.width);
        self.vertical
            .set_viewport((size.height - config.body_inset).max(0.0));
    }

    pub(super) fn sync(
        &mut self,
        path: &str,
        horizontal: f32,
        pressed: Option<usize>,
        vertical: f32,
    ) {
        if self.path == path {
            self.horizontal.sync_offset(horizontal);
            self.pressed_index = pressed;
            self.vertical.sync_offset(vertical);
        }
    }

    pub(super) fn rebind(&mut self, path: &str) {
        if self.path != path {
            self.path = path.to_owned();
            self.configured = false;
            self.dividers.clear();
            self.drag_index = None;
            self.horizontal = ScrollState::default();
            self.pressed_index = None;
            self.row_drag = ItemDrag::default();
            self.vertical = ScrollState::default();
        }
    }

    fn paint_offsets(&self) -> (f32, f32) {
        (self.horizontal.offset(), self.vertical.offset())
    }
}

impl RetainedCanvasState for TrackListState {
    type Config = TrackListConfig;

    delegate::delegate! {
        to self {
            #[call(reconcile)]
            fn reconcile_canvas(&mut self, path: &str, config: &Self::Config);
            #[call(set_viewport)]
            fn set_canvas_viewport(&mut self, size: iced::Size, config: &Self::Config);
        }
    }
}

#[derive(Clone, Copy)]
enum TextAlign {
    Left,
    Center,
    Right,
}

pub(super) fn local_rect(bounds: Rectangle) -> Rect {
    Rect {
        h: bounds.height,
        w: bounds.width,
        x: 0.0,
        y: 0.0,
    }
}

fn column_cells(
    bounds: Rect,
    columns: &[ColumnLayout],
    horizontal: f32,
) -> impl Iterator<Item = (ColumnLayout, Rect)> + '_ {
    let minimum = columns.iter().map(|column| column.width).sum::<f32>();
    let title_extra = (bounds.w - minimum).max(0.0);
    let mut x = bounds.x - horizontal;
    columns.iter().copied().map(move |column| {
        let width = column.width
            + if column.column == TrackColumn::Title {
                title_extra
            } else {
                0.0
            };
        let rect = Rect {
            h: bounds.h,
            w: width,
            x,
            y: bounds.y,
        };
        x += width;
        (column, rect)
    })
}

pub(super) fn hovered_row(
    point: Option<Pt>,
    bounds: Rect,
    row_count: usize,
    horizontal: f32,
    vertical: f32,
    paint: &TrackListPaint<'_>,
) -> Option<usize> {
    track_list_row_at(
        point,
        bounds,
        &paint.columns,
        row_count,
        horizontal,
        vertical,
        paint.skin,
    )
}

fn visible_rows(
    row_count: usize,
    pitch: f32,
    viewport: f32,
    offset: f32,
) -> std::ops::Range<usize> {
    if pitch <= 0.0 || viewport <= 0.0 {
        return 0..0;
    }
    let start = (offset.max(0.0) / pitch)
        .floor()
        .to_usize()
        .map_or(row_count, |index| index.min(row_count));
    let end = ((offset.max(0.0) + viewport) / pitch)
        .ceil()
        .to_usize()
        .map_or(row_count, |index| index.min(row_count));
    start..end.max(start)
}

fn paint_text(
    list: &mut DrawListBuilder,
    text: &mut TextContext,
    content: &str,
    bounds: Rect,
    paint: (FontSkin, FontFamily, Rgba, f32, TextAlign),
) {
    let (font, family, color, padding_x, align) = paint;
    let available = (bounds.w - padding_x * 2.0).max(0.0);
    let run = shape(text, content, font, family, Some(available));
    let x = match align {
        TextAlign::Left => bounds.x + padding_x,
        TextAlign::Center => bounds.x + (bounds.w - run.width()) / 2.0,
        TextAlign::Right => bounds.x + bounds.w - padding_x - run.width(),
    };
    list.text(
        &run,
        content,
        Transform::translate(Pt {
            x,
            y: bounds.y + (bounds.h - run.height()) / 2.0,
        }),
        color,
    );
}

fn shape(
    text: &mut TextContext,
    content: &str,
    font: FontSkin,
    family: FontFamily,
    max_width: Option<f32>,
) -> crate::text::GlyphRun {
    text.shape(
        content,
        TextRoleSkin {
            color: ColorRole::Text,
            font: family,
            size: font.size,
            spacing: 0.0,
            weight: font.weight,
        },
        max_width,
    )
}

fn paint_frame(list: &mut DrawListBuilder, bounds: Rect, frame: FrameSkin, skin: &Skin) {
    if frame.border_width <= 0.0 {
        return;
    }
    let inset = frame.border_width / 2.0;
    list.stroke_rounded_rect(
        Rect {
            h: (bounds.h - frame.border_width).max(0.0),
            w: (bounds.w - frame.border_width).max(0.0),
            x: bounds.x + inset,
            y: bounds.y + inset,
        },
        frame.radius,
        skin.rgba(frame.border),
        frame.border_width,
    );
}

fn paint_scrollbar(
    list: &mut DrawListBuilder,
    rail: Rect,
    content_extent: f32,
    viewport_extent: f32,
    offset: f32,
    axis: ScrollAxis,
    skin: &Skin,
) {
    let maximum = (content_extent - viewport_extent).max(0.0);
    if viewport_extent <= 0.0 || maximum <= 0.0 {
        return;
    }
    let track_extent = match axis {
        ScrollAxis::Horizontal => rail.w,
        ScrollAxis::Vertical => rail.h,
    };
    let thumb_extent = (track_extent * viewport_extent / content_extent)
        .max(skin.track_list.scrollbar_width)
        .min(track_extent);
    let thumb_offset = offset.clamp(0.0, maximum) / maximum * (track_extent - thumb_extent);
    let thumb = match axis {
        ScrollAxis::Horizontal => Rect {
            w: thumb_extent,
            x: rail.x + thumb_offset,
            ..rail
        },
        ScrollAxis::Vertical => Rect {
            h: thumb_extent,
            y: rail.y + thumb_offset,
            ..rail
        },
    };
    list.fill_rect(rail, skin.rgba(skin.track_list.scrollbar_background));
    list.fill_rect(thumb, skin.rgba(skin.track_list.scroller_color));
}

fn value_or_dash(value: &str) -> &str {
    if value.is_empty() { "\u{2014}" } else { value }
}

fn optional_or_dash(value: Option<&str>) -> &str {
    value.map_or("\u{2014}", value_or_dash)
}
