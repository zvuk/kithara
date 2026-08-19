use iced::advanced::{layout::Layout, mouse};

use crate::{
    atoms::table::{
        ColumnLayout, column_resizable, table_body, table_dividers, table_overflows, table_row_at,
        table_visible_divider_hit, table_visible_row_rect,
    },
    draw::Rect,
    engine::{Engine, Target},
    interact::Hit,
    render::Skin,
};
#[cfg(test)]
use crate::{
    atoms::table::{minimum_table_width, table_content_height},
    engine::{Descriptor, ScrollConfig},
    interact::ScrollAxis,
};

pub(super) struct TableHost {
    columns: Vec<ColumnLayout>,
    divider_paths: Vec<String>,
    horizontal_path: String,
    path: String,
    row_count: usize,
    row_target: String,
    skin: Skin,
}

impl TableHost {
    pub(super) fn new(
        path: &str,
        columns: Vec<ColumnLayout>,
        row_count: usize,
        skin: &Skin,
    ) -> Self {
        let divider_paths = columns
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(&columns, *index))
            .map(|(_, column)| format!("{path}/width/{}", column.column.id()))
            .collect();
        Self {
            columns,
            divider_paths,
            horizontal_path: format!("{path}/scroll-x"),
            path: path.to_owned(),
            row_count,
            row_target: format!("{path}/rows"),
            skin: skin.clone(),
        }
    }

    pub(super) fn append_targets<'a>(
        &'a self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        engine: Option<&Engine>,
        targets: &mut Vec<Target<'a>>,
    ) {
        let bounds: Rect = layout.bounds().into();
        let point = cursor.position().map(Into::into);
        let horizontal = engine
            .and_then(|engine| engine.scroll_offset(&self.horizontal_path))
            .unwrap_or(0.0);
        let vertical = engine
            .and_then(|engine| engine.scroll_offset(&self.path))
            .unwrap_or(0.0);
        if table_overflows(&self.columns, bounds.w) {
            targets.push(Target::new(&self.horizontal_path, Hit::new(point, bounds)));
        }
        targets.push(Target::new(
            &self.path,
            Hit::new(point, table_body(bounds, &self.skin)),
        ));
        let row_index = table_row_at(
            point,
            bounds,
            &self.columns,
            self.row_count,
            horizontal,
            vertical,
            &self.skin,
        );
        let row = row_index.and_then(|index| {
            table_visible_row_rect(
                bounds,
                &self.columns,
                self.row_count,
                index,
                horizontal,
                vertical,
                &self.skin,
            )
        });
        match (row_index, row) {
            (Some(index), Some(row)) => {
                targets.push(Target::item(&self.row_target, Hit::new(point, row), index));
            }
            _ => targets.push(Target::new(
                &self.row_target,
                Hit::new(
                    point,
                    Rect {
                        h: 0.0,
                        w: 0.0,
                        x: bounds.x,
                        y: bounds.y,
                    },
                ),
            )),
        }
        for (divider_path, divider) in self.divider_paths.iter().zip(table_dividers(
            bounds,
            &self.columns,
            horizontal,
            &self.skin,
        )) {
            let hit = table_visible_divider_hit(bounds, divider.hit).or_else(|| {
                engine
                    .filter(|engine| engine.captures(divider_path))
                    .map(|_| Rect {
                        h: 0.0,
                        w: 0.0,
                        x: bounds.x,
                        y: bounds.y,
                    })
            });
            if let Some(hit) = hit {
                targets.push(Target::new(divider_path, Hit::new(point, hit)));
            }
        }
    }

    #[cfg(test)]
    pub(super) fn append_descriptors(&self, descriptors: &mut Vec<Descriptor>) {
        descriptors.push(Descriptor::scroll(
            self.horizontal_path.clone(),
            ScrollConfig::plain(ScrollAxis::Horizontal, minimum_table_width(&self.columns)),
        ));
        descriptors.push(Descriptor::scroll(
            self.path.clone(),
            ScrollConfig::plain(
                ScrollAxis::Vertical,
                table_content_height(self.row_count, &self.skin),
            ),
        ));
        descriptors.push(Descriptor::item(
            self.row_target.clone(),
            self.path.clone(),
            self.row_count,
        ));
        let resizable = self
            .columns
            .iter()
            .enumerate()
            .filter(|(index, _)| column_resizable(&self.columns, *index));
        for (divider_path, (_, column)) in self.divider_paths.iter().zip(resizable) {
            descriptors.push(Descriptor::column_divider(
                divider_path.clone(),
                column.width,
                self.skin.table.min_column_width,
            ));
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
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::Pt,
        interact::{Input, PointerPhase, mouse as mouse_input},
        module::{TableColumn, TableColumnStyle},
    };

    fn pointer_input(phase: PointerPhase, at: Option<Pt>) -> Input<'static> {
        Input::Pointer(mouse_input(phase, at))
    }

    fn divider_columns(index_width: f32) -> Vec<ColumnLayout> {
        vec![
            ColumnLayout {
                column: TableColumn::new("index", "#", TableColumnStyle::Index, 28.0, false),
                width: index_width,
            },
            ColumnLayout {
                column: TableColumn::new("name", "NAME", TableColumnStyle::Primary, 180.0, true),
                width: 180.0,
            },
            ColumnLayout {
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
        let host = TableHost::new("library/tracks", divider_columns(98.0), 8, builtin::skin());
        let node = Node::new(Size::new(100.0, 120.0));
        let mut targets = Vec::new();
        host.append_targets(Layout::new(&node), Cursor::Unavailable, None, &mut targets);

        let divider = targets
            .iter()
            .find(|target| target.path == "library/tracks/width/index")
            .unwrap_or_else(|| panic!("the partially visible index divider must remain hittable"));
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
        let host = TableHost::new("library/tracks", divider_columns(98.0), 8, builtin::skin());
        let mut engine = Engine::default();
        let mut descriptors = Vec::new();
        host.append_descriptors(&mut descriptors);
        engine.reconcile(descriptors);
        let mut targets = Vec::new();
        host.append_targets(
            Layout::new(&node),
            Cursor::Available(Point::new(96.0, 11.0)),
            Some(&engine),
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

        let resized = TableHost::new("library/tracks", divider_columns(300.0), 8, builtin::skin());
        let mut descriptors = Vec::new();
        resized.append_descriptors(&mut descriptors);
        engine.reconcile(descriptors);
        let mut release_targets = Vec::new();
        resized.append_targets(
            Layout::new(&node),
            Cursor::Unavailable,
            Some(&engine),
            &mut release_targets,
        );
        let watcher = release_targets
            .iter()
            .find(|target| target.path == path)
            .unwrap_or_else(|| panic!("an active offscreen divider must retain a release watcher"));
        assert_eq!((watcher.hit.area().w, watcher.hit.area().h), (0.0, 0.0));

        let _ = engine.handle(pointer_input(PointerPhase::Up, None), &release_targets, now);
        assert!(!engine.captures_pointer());
    }
}
