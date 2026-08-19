use kithara_ui::render::{
    Node, ReadValue, Scope, TableCell, TableRow, TableValue, TreeIcon, TreeRow,
};
use num_traits::cast::AsPrimitive;

use super::value::Value;
use crate::{
    catalog::Catalog,
    gui::ui::cache::{CatalogRowMarks, LibraryScope, LibraryView},
};

pub(super) struct LibraryNode<'a> {
    rows: Vec<TableRow<'a>>,
    tree: Vec<TreeRow<'a>>,
    breadcrumb: String,
    query: &'a str,
    scope: LibraryScope,
}

impl<'a> LibraryNode<'a> {
    pub(super) fn new(
        catalog: &'a Catalog,
        marks: &'a CatalogRowMarks,
        selected: Option<usize>,
        library: &'a LibraryView,
    ) -> Self {
        let scope = library.scope;
        let rows = catalog
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, entry)| scope.holds(entry))
            .map(|(index, entry)| {
                let deck = marks
                    .get(index)
                    .map(String::as_str)
                    .filter(|marks| !marks.is_empty())
                    .map_or_else(
                        || TableCell::empty("deck"),
                        |marks| TableCell::text("deck", marks),
                    );
                TableRow::new(
                    vec![
                        TableCell::empty("index"),
                        deck,
                        TableCell::text("title", &entry.name),
                        TableCell::text("artist", entry.url.as_str()),
                        TableCell::empty("bpm"),
                        TableCell::empty("key"),
                        TableCell::empty("time"),
                        TableCell::empty("energy"),
                        TableCell::empty("transition"),
                    ],
                    selected == Some(index),
                )
            })
            .collect();

        Self {
            rows,
            tree: tree(catalog, library),
            breadcrumb: format!("{} \u{b7} {}", scope.label(), catalog.entries().len()),
            query: &library.query,
            scope,
        }
    }

    pub(super) fn title(&self, row: usize) -> Option<&'a str> {
        self.rows
            .get(row)?
            .cells()
            .iter()
            .find(|cell| cell.id() == "title")
            .and_then(|cell| match cell.value() {
                TableValue::Text(value) => Some(value),
                _ => None,
            })
    }
}

/// One row per source group the browser is listing, each carrying how many
/// entries it holds. `LibraryView::groups` decides which groups those are, and
/// the host resolves a picked row through the same order.
fn tree<'a>(catalog: &Catalog, library: &LibraryView) -> Vec<TreeRow<'a>> {
    library
        .groups()
        .map(|group| TreeRow {
            label: group.label(),
            count: Some(
                catalog
                    .entries()
                    .iter()
                    .filter(|entry| group.holds(entry))
                    .count()
                    .as_(),
            ),
            expanded: None,
            icon: match group {
                LibraryScope::All => TreeIcon::Collection,
                LibraryScope::Local => TreeIcon::Folder,
                LibraryScope::Stream => TreeIcon::Playlist,
            },
            muted: false,
            selected: group == library.scope,
            depth: 0,
        })
        .collect()
}

impl<'a, 'b: 'a> Node<'a> for &'a LibraryNode<'b> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let rows: &'a [TableRow<'a>] = &self.rows;
        let tree: &'a [TreeRow<'a>] = &self.tree;
        let value = match segment {
            "tracks" => ReadValue::Table(rows),
            "tree" => ReadValue::Tree(tree),
            "breadcrumb" => ReadValue::Text(&self.breadcrumb),
            "query" => ReadValue::Text(self.query),
            "scope" => ReadValue::Scalar(self.scope.index().as_()),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}
