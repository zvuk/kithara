use iced::{Element, Length, widget::Space};
use num_traits::ToPrimitive;

use super::Widget;
use crate::{
    draw::{Pt, Rect},
    module::TrackColumn,
    render::{InputOwner, ReadValue, Reads, Skin, TrackRow, UiEvent, track_list},
    skin::TrackListSkin,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ColumnLayout {
    pub(crate) column: TrackColumn,
    pub(crate) width: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ColumnDividerLayout {
    pub(crate) column: TrackColumn,
    pub(crate) hit: Rect,
    pub(crate) paint: Rect,
    pub(crate) value: f32,
}

pub(crate) struct TrackListRowData {
    pub(crate) artist: Option<String>,
    pub(crate) bpm: Option<String>,
    pub(crate) deck: Option<String>,
    pub(crate) energy: Option<u8>,
    pub(crate) key: Option<String>,
    pub(crate) time: Option<String>,
    pub(crate) transition: Option<String>,
    pub(crate) title: String,
    pub(crate) selected: bool,
}

impl From<&TrackRow<'_>> for TrackListRowData {
    fn from(track: &TrackRow<'_>) -> Self {
        Self {
            artist: track.artist.map(str::to_owned),
            bpm: track.bpm.map(str::to_owned),
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

#[derive(bon::Builder)]
pub(crate) struct TrackList<'path, 'columns, 'state, 'value, 'data, 'reads, 'skin> {
    skin: &'skin Skin,
    columns: &'columns [TrackColumn],
    reads: &'reads dyn Reads,
    columns_scope: &'state str,
    path: &'path str,
    columns_state: Option<&'state str>,
    value: Option<&'value ReadValue<'data>>,
    owner: InputOwner,
}

impl<'a, 'skin: 'a> Widget<'a> for TrackList<'_, '_, '_, '_, '_, '_, 'skin> {
    fn view(self) -> Element<'a, UiEvent> {
        let Some(ReadValue::TrackList(tracks)) = self.value else {
            return Space::new().into();
        };
        let state = self
            .columns_state
            .map(|prefix| (prefix, self.columns_scope));
        let columns = column_layouts(self.columns, self.reads, state, self.skin);
        let path = self.path.to_owned();
        let owner = self.owner;
        let rows: Vec<_> = tracks.iter().map(TrackListRowData::from).collect();
        track_list(&path, rows, columns, self.skin, owner)
    }
}

pub(crate) fn column_resizable(columns: &[ColumnLayout], index: usize) -> bool {
    columns
        .get(index)
        .is_some_and(|column| column.column != TrackColumn::Title && index + 1 < columns.len())
}

fn column_visible(reads: &dyn Reads, state: Option<(&str, &str)>, column: TrackColumn) -> bool {
    let Some((prefix, scope)) = state else {
        return true;
    };
    let endpoint = format!("{prefix}.{}{scope}", column.endpoint_name());
    !matches!(reads.get(&endpoint), Some(ReadValue::Bool(false)))
}

pub(crate) fn column_layouts(
    columns: &[TrackColumn],
    reads: &dyn Reads,
    state: Option<(&str, &str)>,
    skin: &Skin,
) -> Vec<ColumnLayout> {
    columns
        .iter()
        .copied()
        .filter(|column| column_visible(reads, state, *column))
        .map(|column| ColumnLayout {
            column,
            width: effective_column_width(reads, state, column, skin),
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
    state: Option<(&str, &str)>,
    column: TrackColumn,
    skin: &Skin,
) -> f32 {
    let default = default_column_width(column, skin);
    let Some((prefix, scope)) = state else {
        return default;
    };
    let endpoint = format!("{prefix}.width.{}{scope}", column.endpoint_name());
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

pub(crate) fn minimum_table_width(columns: &[ColumnLayout]) -> f32 {
    columns.iter().map(|column| column.width).sum()
}

pub(crate) fn column_label(column: TrackColumn, metrics: &TrackListSkin) -> &str {
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

pub(crate) fn track_list_overflows(columns: &[ColumnLayout], available_width: f32) -> bool {
    minimum_table_width(columns) > available_width
}

pub(crate) fn track_list_content_width(columns: &[ColumnLayout], available_width: f32) -> f32 {
    minimum_table_width(columns).max(available_width)
}

pub(crate) fn track_list_content_height(row_count: usize, skin: &Skin) -> f32 {
    let rows = row_count.to_f32().unwrap_or(f32::MAX);
    let gaps = row_count.saturating_sub(1).to_f32().unwrap_or(f32::MAX);
    skin.track_list
        .row_height
        .mul_add(rows, skin.track_list.grid_gap * gaps)
}

pub(crate) fn track_list_row_pitch(skin: &Skin) -> f32 {
    skin.track_list.row_height + skin.track_list.grid_gap
}

pub(crate) fn track_list_body(bounds: Rect, skin: &Skin) -> Rect {
    let gap = skin.track_list.grid_gap;
    Rect {
        h: (bounds.h - skin.track_list.header_height - skin.track_list.footer_height - gap * 2.0)
            .max(0.0),
        w: bounds.w,
        x: bounds.x,
        y: bounds.y + skin.track_list.header_height + gap,
    }
}

pub(crate) fn track_list_vertical_scrollbar_rect(
    bounds: Rect,
    columns: &[ColumnLayout],
    row_count: usize,
    horizontal_offset: f32,
    skin: &Skin,
) -> Option<Rect> {
    let body = track_list_body(bounds, skin);
    (track_list_content_height(row_count, skin) > body.h).then_some(())?;
    let rail = Rect {
        h: body.h,
        w: skin.track_list.scrollbar_width,
        x: bounds.x - horizontal_offset + track_list_content_width(columns, bounds.w)
            - skin.track_list.scrollbar_margin
            - skin.track_list.scrollbar_width,
        y: body.y,
    };
    intersect(rail, body)
}

pub(crate) fn track_list_row_rect(
    bounds: Rect,
    columns: &[ColumnLayout],
    index: usize,
    horizontal_offset: f32,
    vertical_offset: f32,
    skin: &Skin,
) -> Rect {
    let body = track_list_body(bounds, skin);
    let y = index.to_f32().map_or(f32::MAX, |index| {
        index.mul_add(track_list_row_pitch(skin), body.y) - vertical_offset
    });
    Rect {
        h: skin.track_list.row_height,
        w: track_list_content_width(columns, bounds.w),
        x: bounds.x - horizontal_offset,
        y,
    }
}

pub(crate) fn track_list_visible_row_rect(
    bounds: Rect,
    columns: &[ColumnLayout],
    row_count: usize,
    index: usize,
    horizontal_offset: f32,
    vertical_offset: f32,
    skin: &Skin,
) -> Option<Rect> {
    let row = track_list_row_rect(
        bounds,
        columns,
        index,
        horizontal_offset,
        vertical_offset,
        skin,
    );
    let mut visible = intersect(row, track_list_body(bounds, skin))?;
    if let Some(scrollbar) =
        track_list_vertical_scrollbar_rect(bounds, columns, row_count, horizontal_offset, skin)
    {
        visible.w = (scrollbar.x - visible.x).max(0.0);
    }
    (visible.w > 0.0).then_some(visible)
}

pub(crate) fn track_list_row_at(
    point: Option<Pt>,
    bounds: Rect,
    columns: &[ColumnLayout],
    row_count: usize,
    horizontal_offset: f32,
    vertical_offset: f32,
    skin: &Skin,
) -> Option<usize> {
    let point = point?;
    let body = track_list_body(bounds, skin);
    let pitch = track_list_row_pitch(skin);
    if !body.contains(point) || pitch <= 0.0 {
        return None;
    }
    let y = point.y - body.y + vertical_offset;
    let index = (y / pitch).floor().to_usize()?;
    if index >= row_count {
        return None;
    }
    let row = track_list_visible_row_rect(
        bounds,
        columns,
        row_count,
        index,
        horizontal_offset,
        vertical_offset,
        skin,
    )?;
    row.contains(point).then_some(index)
}

fn intersect(left: Rect, right: Rect) -> Option<Rect> {
    let x = left.x.max(right.x);
    let y = left.y.max(right.y);
    let right_edge = (left.x + left.w).min(right.x + right.w);
    let bottom = (left.y + left.h).min(right.y + right.h);
    (right_edge > x && bottom > y).then_some(Rect {
        h: bottom - y,
        w: right_edge - x,
        x,
        y,
    })
}

pub(crate) fn track_list_dividers(
    bounds: Rect,
    columns: &[ColumnLayout],
    horizontal_offset: f32,
    skin: &Skin,
) -> Vec<ColumnDividerLayout> {
    let overflowing = track_list_overflows(columns, bounds.w);
    let flexible_title = !overflowing;
    let extra = (bounds.w - minimum_table_width(columns)).max(0.0);
    let mut edge = bounds.x - horizontal_offset;
    let mut dividers = Vec::new();
    for (index, column) in columns.iter().copied().enumerate() {
        let width = match column_length(column, flexible_title) {
            Length::Fill => column.width + extra,
            Length::Fixed(width) => width,
            Length::FillPortion(_) | Length::Shrink => column.width,
        };
        edge += width;
        if !column_resizable(columns, index) {
            continue;
        }
        dividers.push(ColumnDividerLayout {
            column: column.column,
            hit: Rect {
                h: skin.track_list.header_height,
                w: skin.track_list.divider_hit_width,
                x: edge - skin.track_list.divider_hit_width / 2.0,
                y: bounds.y,
            },
            paint: Rect {
                h: skin.track_list.header_height,
                w: skin.track_list.divider_width,
                x: edge - skin.track_list.divider_width / 2.0,
                y: bounds.y,
            },
            value: column.width,
        });
    }
    dividers
}

pub(crate) fn track_list_visible_divider_hit(bounds: Rect, hit: Rect) -> Option<Rect> {
    intersect(hit, bounds)
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
            Some(("columns", "")),
            TrackColumn::Title
        ));
    }

    #[kithara::test]
    fn false_column_endpoint_is_hidden() {
        assert!(!column_visible(
            &ColumnReads(Some(false)),
            Some(("columns", "")),
            TrackColumn::Title
        ));
    }

    #[kithara::test]
    fn total_width_uses_host_override_and_title_minimum() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[TrackColumn::Index, TrackColumn::Title, TrackColumn::Artist],
            &WidthReads,
            Some(("columns", "")),
            skin,
        );

        assert_eq!(columns.len(), 2);
        assert_eq!(columns[1].width, 240.0);
        assert_eq!(
            minimum_table_width(&columns),
            skin.track_list.title_min_width + 240.0
        );
    }

    #[kithara::test]
    fn divider_hit_rect_is_wider_than_the_centered_paint_rect() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[TrackColumn::Index, TrackColumn::Title, TrackColumn::Artist],
            &ColumnReads(None),
            None,
            skin,
        );
        let divider = track_list_dividers(
            Rect {
                h: 160.0,
                w: 800.0,
                x: 0.0,
                y: 0.0,
            },
            &columns,
            0.0,
            skin,
        )[0];

        assert_eq!(divider.hit.w, skin.track_list.divider_hit_width);
        assert_eq!(divider.paint.w, skin.track_list.divider_width);
        assert_eq!(divider.hit.w, 7.0);
        assert_eq!(divider.paint.w, 1.0);
        assert!(divider.hit.w > divider.paint.w);
        assert_eq!(
            divider.hit.x + divider.hit.w / 2.0,
            divider.paint.x + divider.paint.w / 2.0
        );
    }

    #[kithara::test]
    fn overflow_changes_at_the_exact_minimum_width_boundary() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[TrackColumn::Index, TrackColumn::Title, TrackColumn::Artist],
            &ColumnReads(None),
            None,
            skin,
        );
        let minimum = minimum_table_width(&columns);

        assert!(track_list_overflows(&columns, minimum - 1.0));
        assert!(!track_list_overflows(&columns, minimum));
        assert!(!track_list_overflows(&columns, minimum + 1.0));
    }

    #[kithara::test]
    fn row_geometry_keeps_grid_gaps_outside_row_hits() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(&[TrackColumn::Title], &ColumnReads(None), None, skin);
        let bounds = Rect {
            h: 160.0,
            w: 400.0,
            x: 0.0,
            y: 0.0,
        };
        let first = track_list_row_rect(bounds, &columns, 0, 0.0, 0.0, skin);
        let second = track_list_row_rect(bounds, &columns, 1, 0.0, 0.0, skin);

        assert_eq!(second.y - first.y, track_list_row_pitch(skin));
        assert_eq!(second.y - (first.y + first.h), skin.track_list.grid_gap);
    }

    #[kithara::test]
    fn visible_row_hits_are_clipped_to_the_body() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(&[TrackColumn::Title], &ColumnReads(None), None, skin);
        let bounds = Rect {
            h: 160.0,
            w: 400.0,
            x: 0.0,
            y: 0.0,
        };
        let clipped = track_list_visible_row_rect(
            bounds,
            &columns,
            3,
            0,
            0.0,
            skin.track_list.row_height / 2.0,
            skin,
        )
        .unwrap_or_else(|| panic!("the partially visible first row must retain a hit rect"));

        assert_eq!(clipped.y, track_list_body(bounds, skin).y);
        assert_eq!(clipped.h, skin.track_list.row_height / 2.0);
    }

    #[kithara::test]
    fn divider_hit_bands_are_clipped_at_both_viewport_edges() {
        let bounds = Rect {
            h: 100.0,
            w: 100.0,
            x: 0.0,
            y: 0.0,
        };
        let hit = |x, w| Rect {
            h: 22.0,
            w,
            x,
            y: 0.0,
        };

        assert_eq!(track_list_visible_divider_hit(bounds, hit(-8.0, 4.0)), None);
        assert_eq!(
            track_list_visible_divider_hit(bounds, hit(-2.0, 7.0)),
            Some(hit(0.0, 5.0))
        );
        assert_eq!(
            track_list_visible_divider_hit(bounds, hit(98.0, 7.0)),
            Some(hit(98.0, 2.0))
        );
        assert_eq!(
            track_list_visible_divider_hit(bounds, hit(101.0, 7.0)),
            None
        );
    }

    #[kithara::test]
    fn row_hits_yield_to_the_visible_scrollbar_lane_at_each_horizontal_edge() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[
                TrackColumn::Title,
                TrackColumn::Artist,
                TrackColumn::Transition,
            ],
            &ColumnReads(None),
            None,
            skin,
        );
        let bounds = Rect {
            h: 160.0,
            w: 400.0,
            x: 0.0,
            y: 0.0,
        };
        let row_count = 10;
        let maximum = minimum_table_width(&columns) - bounds.w;
        let row = |offset| {
            track_list_visible_row_rect(bounds, &columns, row_count, 0, offset, 0.0, skin)
                .unwrap_or_else(|| panic!("the first row must be visible"))
        };

        assert_eq!(
            track_list_vertical_scrollbar_rect(bounds, &columns, row_count, 0.0, skin),
            None
        );
        assert_eq!(row(0.0).w, bounds.w);

        let partial = maximum - skin.track_list.scrollbar_margin;
        let partial_scrollbar =
            track_list_vertical_scrollbar_rect(bounds, &columns, row_count, partial, skin)
                .unwrap_or_else(|| {
                    panic!("the rail must enter the viewport before maximum scroll")
                });
        assert_eq!(row(partial).x + row(partial).w, partial_scrollbar.x);

        let scrollbar =
            track_list_vertical_scrollbar_rect(bounds, &columns, row_count, maximum, skin)
                .unwrap_or_else(|| panic!("the rail must be visible at maximum horizontal scroll"));
        let visible = row(maximum);
        assert_eq!(visible.x + visible.w, scrollbar.x);
        let y = visible.y + visible.h / 2.0;
        assert_eq!(
            track_list_row_at(
                Some(Pt {
                    x: scrollbar.x - 0.5,
                    y,
                }),
                bounds,
                &columns,
                row_count,
                maximum,
                0.0,
                skin,
            ),
            Some(0)
        );
        assert_eq!(
            track_list_row_at(
                Some(Pt { x: scrollbar.x, y }),
                bounds,
                &columns,
                row_count,
                maximum,
                0.0,
                skin,
            ),
            None
        );
    }
}
