use kithara_ui::{
    module::IconName,
    render::{Node, ReadValue, Scope, TableCell, TableRow, TableValue, TreeRow},
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
                LibraryScope::All => IconName::Collection,
                LibraryScope::Local => IconName::Folder,
                LibraryScope::Stream => IconName::Playlist,
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn catalog() -> Catalog {
        Catalog::new(vec![
            "/music/local.mp3".to_owned(),
            "https://host/a.m3u8".to_owned(),
            "https://host/b.m3u8".to_owned(),
        ])
    }

    /// A browser row is a position in the group being listed, not a position in
    /// the catalog. A picked row is resolved back to an entry, so a narrowed
    /// group must not shift which entry that is.
    #[kithara::test(native, flash(false))]
    fn a_row_resolves_to_the_entry_the_browser_shows_there() {
        let catalog = catalog();
        let marks = CatalogRowMarks::default();
        let library = LibraryView {
            query: String::new(),
            scope: LibraryScope::Stream,
        };
        let node = LibraryNode::new(&catalog, &marks, None, &library);

        assert_eq!(
            node.title(0),
            Some("a"),
            "Stream lists the streamed entries only"
        );
        assert_eq!(
            library.catalog_index(&catalog, 0),
            Some(1),
            "the first Stream row is catalog entry 1"
        );
        assert_eq!(library.catalog_index(&catalog, 1), Some(2));
        assert_eq!(
            library.catalog_index(&catalog, 2),
            None,
            "Stream lists two entries"
        );
    }

    /// Every row is a catalog row under `All`, which is why the defect this
    /// guards against stays invisible in the default group.
    #[kithara::test(native, flash(false))]
    fn under_all_a_row_is_its_catalog_position() {
        let catalog = catalog();
        let library = LibraryView::default();

        for row in 0..3 {
            assert_eq!(library.catalog_index(&catalog, row), Some(row));
        }
        assert_eq!(library.catalog_index(&catalog, 3), None);
    }
}
