use kithara_ui::render::{Node, ReadValue, Scope, TrackRow};

use super::value::Value;
use crate::{catalog::Catalog, gui::studio_ui::cache::CatalogRowMarks};

pub(super) struct LibraryNode<'a> {
    rows: Vec<TrackRow<'a>>,
}

impl<'a> LibraryNode<'a> {
    pub(super) fn new(
        catalog: &'a Catalog,
        marks: &'a CatalogRowMarks,
        selected: Option<usize>,
    ) -> Self {
        let rows = catalog
            .entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| TrackRow {
                title: &entry.name,
                artist: Some(entry.url.as_str()),
                time: None,
                search: None,
                deck: marks
                    .get(index)
                    .map(String::as_str)
                    .filter(|marks| !marks.is_empty()),
                bpm: None,
                key: None,
                energy: None,
                transition: None,
                selected: selected == Some(index),
            })
            .collect();
        Self { rows }
    }

    pub(super) fn title(&self, row: usize) -> Option<&'a str> {
        Some(self.rows.get(row)?.title)
    }
}

impl<'a, 'b: 'a> Node<'a> for &'a LibraryNode<'b> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let rows: &'a [TrackRow<'a>] = &self.rows;
        let value = match segment {
            "tracks" => ReadValue::TrackList(rows),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}
