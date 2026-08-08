use iced::advanced::{layout::Layout, mouse};

use crate::{
    draw::Rect,
    engine::{Engine, Target},
    interact::Hit,
    render::Skin,
    widgets::track_list::{
        ColumnLayout, column_resizable, track_list_body, track_list_dividers, track_list_overflows,
        track_list_row_at, track_list_visible_divider_hit, track_list_visible_row_rect,
    },
};
#[cfg(test)]
use crate::{
    engine::{Descriptor, ScrollConfig},
    interact::ScrollAxis,
    widgets::track_list::{minimum_table_width, track_list_content_height},
};

pub(super) struct TrackListHost {
    columns: Vec<ColumnLayout>,
    divider_paths: Vec<String>,
    horizontal_path: String,
    path: String,
    row_count: usize,
    row_target: String,
    skin: Skin,
}

impl TrackListHost {
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
            .map(|(_, column)| format!("{path}/width/{}", column.column.endpoint_name()))
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
        if track_list_overflows(&self.columns, bounds.w) {
            targets.push(Target::new(&self.horizontal_path, Hit::new(point, bounds)));
        }
        targets.push(Target::new(
            &self.path,
            Hit::new(point, track_list_body(bounds, &self.skin)),
        ));
        let row_index = track_list_row_at(
            point,
            bounds,
            &self.columns,
            self.row_count,
            horizontal,
            vertical,
            &self.skin,
        );
        let row = row_index.and_then(|index| {
            track_list_visible_row_rect(
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
        for (divider_path, divider) in self.divider_paths.iter().zip(track_list_dividers(
            bounds,
            &self.columns,
            horizontal,
            &self.skin,
        )) {
            let hit = track_list_visible_divider_hit(bounds, divider.hit).or_else(|| {
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
                track_list_content_height(self.row_count, &self.skin),
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
                self.skin.track_list.min_column_width,
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
        module::TrackColumn,
    };

    fn pointer_input(phase: PointerPhase, at: Option<Pt>) -> Input<'static> {
        Input::Pointer(mouse_input(phase, at))
    }

    fn divider_columns(index_width: f32) -> Vec<ColumnLayout> {
        vec![
            ColumnLayout {
                column: TrackColumn::Index,
                width: index_width,
            },
            ColumnLayout {
                column: TrackColumn::Title,
                width: 180.0,
            },
            ColumnLayout {
                column: TrackColumn::Artist,
                width: 200.0,
            },
            ColumnLayout {
                column: TrackColumn::Transition,
                width: 130.0,
            },
        ]
    }

    #[kithara::test]
    fn hosted_dividers_clip_partial_hits_and_omit_offscreen_hits() {
        let host = TrackListHost::new("library/tracks", divider_columns(98.0), 8, builtin::skin());
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
        let host = TrackListHost::new("library/tracks", divider_columns(98.0), 8, builtin::skin());
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

        let resized =
            TrackListHost::new("library/tracks", divider_columns(300.0), 8, builtin::skin());
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
