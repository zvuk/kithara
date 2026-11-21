use std::{cell::Cell, rc::Rc};

use iced::advanced::{layout::Layout, mouse};

use crate::{
    atoms::table::{
        ColumnLayout, TableMetrics, column_resizable, empty_bounds, table_body, table_divider_hit,
        table_dividers, table_overflows, table_row_at, table_visible_row_rect,
    },
    draw::Rect,
    engine::{Engine, Target},
    hosts::hosted::TablePlan,
    interact::Hit,
    module::TableFrame,
    skin::TableSkin,
};
pub(super) struct TableHost {
    skin: TableSkin,
    frame: TableFrame,
    horizontal_path: String,
    path: String,
    row_target: String,
    columns: Vec<ColumnLayout>,
    divider_paths: Vec<(String, String)>,
    row_count: usize,
    viewport_width: Rc<Cell<f32>>,
}

impl TableHost {
    pub(super) fn new(
        path: &str,
        columns: Vec<ColumnLayout>,
        row_count: usize,
        table: TableMetrics<'_>,
        viewport_width: Rc<Cell<f32>>,
    ) -> Self {
        let divider_paths = columns
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(&columns, *index))
            .map(|(_, column)| {
                (
                    column.column.id().to_owned(),
                    format!("{path}/width/{}", column.column.id()),
                )
            })
            .collect();
        Self {
            columns,
            divider_paths,
            row_count,
            horizontal_path: format!("{path}/scroll-x"),
            path: path.to_owned(),
            row_target: format!("{path}/rows"),
            skin: *table.skin,
            frame: table.frame,
            viewport_width,
        }
    }

    /// Appends the table's targets, with a target for each pressable cell of
    /// `actions` under the cursor.
    pub(super) fn append_targets<'a>(
        &'a self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        engine: Option<&Engine>,
        actions: Option<&'a TablePlan<()>>,
        targets: &mut Vec<Target<'a>>,
    ) {
        let bounds: Rect = layout.bounds().into();
        self.viewport_width.set(bounds.w);
        let table = TableMetrics {
            skin: &self.skin,
            frame: self.frame,
        };
        let point = cursor.position().map(Into::into);
        let horizontal = engine
            .and_then(|engine| engine.scroll_offset(&self.horizontal_path))
            .unwrap_or(0.0);
        let vertical = engine
            .and_then(|engine| engine.scroll_offset(&self.path))
            .unwrap_or(0.0);
        if table_overflows(&self.columns, bounds.w, table) {
            targets.push(Target::new(&self.horizontal_path, Hit::new(point, bounds)));
        }
        targets.push(Target::new(
            &self.path,
            Hit::new(point, table_body(bounds, table)),
        ));
        let row_index = table_row_at(
            point,
            bounds,
            &self.columns,
            self.row_count,
            horizontal,
            vertical,
            table,
        );
        let row = row_index.and_then(|index| {
            table_visible_row_rect(
                bounds,
                &self.columns,
                self.row_count,
                index,
                horizontal,
                vertical,
                table,
            )
        });
        match (row_index, row) {
            (Some(index), Some(row)) => {
                targets.push(Target::item(&self.row_target, Hit::new(point, row), index));
            }
            _ => targets.push(Target::new(
                &self.row_target,
                Hit::new(point, empty_bounds(bounds)),
            )),
        }
        if let Some(actions) = actions {
            actions.append_action_targets(
                bounds,
                point,
                &self.columns,
                (horizontal, vertical),
                targets,
            );
        }
        let dividers = table_dividers(bounds, &self.columns, horizontal, table);
        for (id, divider_path) in &self.divider_paths {
            let hit = table_divider_hit(
                bounds,
                &dividers,
                id,
                engine.is_some_and(|engine| engine.captures(divider_path)),
            );
            if let Some(hit) = hit {
                targets.push(Target::new(divider_path, Hit::new(point, hit)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::{
        Point, Size,
        advanced::layout::{Layout, Node},
        mouse::Cursor,
    };
    use kithara_platform::time::Instant;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::Pt,
        engine::Descriptor,
        interact::{Input, PointerPhase, mouse as mouse_input},
        module::{TableColumn, TableColumnStyle},
    };

    fn pointer_input(phase: PointerPhase, at: Option<Pt>) -> Input<'static> {
        Input::Pointer(mouse_input(phase, at))
    }

    /// The engine state the table publishes for its resizable dividers.
    fn divider_descriptors(columns: &[ColumnLayout]) -> Vec<Descriptor> {
        columns
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(columns, *index))
            .map(|(index, column)| {
                Descriptor::column_divider(
                    format!("library/tracks/width/{}", column.column.id()),
                    column.width,
                    crate::atoms::table::column_resize_track(
                        columns,
                        index,
                        0.0,
                        TableMetrics {
                            skin: &builtin::skin_doc().table,
                            frame: TableFrame::new(0.0, 0.0, true),
                        },
                    ),
                )
            })
            .collect()
    }

    fn divider_columns(index_width: f32) -> Vec<ColumnLayout> {
        vec![
            ColumnLayout {
                resizable: true,
                column: TableColumn::new("index", "#", TableColumnStyle::Index, 28.0, false),
                width: index_width,
            },
            ColumnLayout {
                resizable: true,
                column: TableColumn::new("name", "NAME", TableColumnStyle::Primary, 180.0, true),
                width: 180.0,
            },
            ColumnLayout {
                resizable: true,
                column: TableColumn::new(
                    "detail",
                    "DETAIL",
                    TableColumnStyle::Secondary,
                    200.0,
                    false,
                ),
                width: 200.0,
            },
            ColumnLayout {
                resizable: true,
                column: TableColumn::new(
                    "action",
                    "ACTION",
                    TableColumnStyle::Transition,
                    130.0,
                    false,
                ),
                width: 130.0,
            },
        ]
    }

    #[kithara::test]
    fn hosted_dividers_clip_partial_hits_and_omit_offscreen_hits() {
        let host = TableHost::new(
            "library/tracks",
            divider_columns(98.0),
            8,
            TableMetrics {
                skin: &builtin::skin_doc().table,
                frame: TableFrame::new(0.0, 0.0, true),
            },
            Rc::new(Cell::new(0.0)),
        );
        let node = Node::new(Size::new(100.0, 120.0));
        let mut targets = Vec::new();
        host.append_targets(
            Layout::new(&node),
            Cursor::Unavailable,
            None,
            None,
            &mut targets,
        );

        let divider = targets
            .iter()
            .find(|target| target.path == "library/tracks/width/index")
            .expect("the partially visible index divider must remain hittable");
        assert_eq!(divider.hit.area().x, 94.5);
        assert_eq!(divider.hit.area().w, 5.5);
        assert!(
            targets
                .iter()
                .all(|target| target.path != "library/tracks/width/artist")
        );
    }

    #[kithara::test]
    fn captured_divider_keeps_a_release_watcher_after_resize_moves_it_offscreen() {
        let path = "library/tracks/width/index";
        let node = Node::new(Size::new(100.0, 120.0));
        let host = TableHost::new(
            "library/tracks",
            divider_columns(98.0),
            8,
            TableMetrics {
                skin: &builtin::skin_doc().table,
                frame: TableFrame::new(0.0, 0.0, true),
            },
            Rc::new(Cell::new(0.0)),
        );
        let mut engine = Engine::default();
        engine.reconcile(divider_descriptors(&divider_columns(98.0)));
        let mut targets = Vec::new();
        host.append_targets(
            Layout::new(&node),
            Cursor::Available(Point::new(96.0, 11.0)),
            Some(&engine),
            None,
            &mut targets,
        );
        let now = Instant::now();
        let _ = engine.handle(pointer_input(PointerPhase::Down, None), &targets, now);
        assert!(engine.captures(path));
        let moved = engine.handle(
            pointer_input(PointerPhase::Move, Some(Pt { x: 350.0, y: 11.0 })),
            &targets,
            now,
        );
        assert!(moved.is_some(), "the resize must publish its wider value");

        let resized = TableHost::new(
            "library/tracks",
            divider_columns(300.0),
            8,
            TableMetrics {
                skin: &builtin::skin_doc().table,
                frame: TableFrame::new(0.0, 0.0, true),
            },
            Rc::new(Cell::new(0.0)),
        );
        engine.reconcile(divider_descriptors(&divider_columns(300.0)));
        let mut release_targets = Vec::new();
        resized.append_targets(
            Layout::new(&node),
            Cursor::Unavailable,
            Some(&engine),
            None,
            &mut release_targets,
        );
        let watcher = release_targets
            .iter()
            .find(|target| target.path == path)
            .expect("an active offscreen divider must retain a release watcher");
        assert_eq!((watcher.hit.area().w, watcher.hit.area().h), (0.0, 0.0));

        let _ = engine.handle(pointer_input(PointerPhase::Up, None), &release_targets, now);
        assert!(!engine.captures_pointer());
    }
}
