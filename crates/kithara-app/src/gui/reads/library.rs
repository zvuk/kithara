use std::{cell::OnceCell, collections::BTreeMap};

use kithara::ui::render::{Node, ReadValue, Scope, TableCell, TableRow, TreeRow};

use super::value::{Value, impl_child_node};
use crate::gui::library::Library;

/// Library tree, selected source id, and Add folder visibility.
pub(super) struct LibraryNode<'a> {
    library: &'a Library,
    tree: OnceCell<Vec<TreeRow<'a>>>,
    /// No folder picker answers Add folder.
    add_folder_hidden: bool,
}

impl<'a> LibraryNode<'a> {
    pub(super) fn new(library: &'a Library, add_folder_hidden: bool) -> Self {
        Self {
            library,
            add_folder_hidden,
            tree: OnceCell::new(),
        }
    }
}

impl<'a, 'b: 'a> Node<'a> for &'a LibraryNode<'b> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let node: Box<dyn Node<'a> + 'a> = match segment {
            "tree" => Box::new(Value(ReadValue::Tree(
                self.tree.get_or_init(|| self.library.tree()),
            ))),
            "page" => Box::new(Value(ReadValue::Text(self.library.page()?))),
            "add_folder" => Box::new(AddFolderNode(self.add_folder_hidden)),
            _ => return None,
        };
        Some(node)
    }
}

#[derive(Clone, Copy)]
struct AddFolderNode(bool);

impl_child_node!(AddFolderNode, |this, segment, _scope| {
    match segment {
        "hidden" => Some(Box::new(Value(ReadValue::Bool(this.0)))),
        _ => None,
    }
});

/// Answers each source under its own key, building its rows on first read;
/// a name the shell does not own is the scoped source's read.
pub(super) struct SourcesNode<'a> {
    library: &'a Library,
    rows: Vec<OnceCell<Vec<TableRow<'a>>>>,
    bpms: &'a BTreeMap<String, f64>,
}

impl<'a> SourcesNode<'a> {
    pub(super) fn new(library: &'a Library, bpms: &'a BTreeMap<String, f64>) -> Self {
        Self {
            library,
            bpms,
            rows: library.sources().map(|_| OnceCell::new()).collect(),
        }
    }
}

impl<'a, 'b: 'a> Node<'a> for &'a SourcesNode<'b> {
    fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let id = scope.get("source")?;
        let at = self.library.index_of(id)?;
        let source = self.library.source(at)?;
        if segment == "column" {
            return Some(Box::new(ColumnsNode {
                library: self.library,
                at,
            }));
        }
        let value = match segment {
            "rows" => {
                let rows: &'a [TableRow<'b>] = self.rows.get(at)?.get_or_init(|| {
                    let rows = source.rows(self.library.selected_row(at));
                    if self.bpms.is_empty() {
                        return rows;
                    }
                    rows.into_iter()
                        .enumerate()
                        .map(|(index, row)| {
                            match source
                                .analysis_key(index)
                                .and_then(|key| self.bpms.get(key))
                            {
                                Some(bpm) => {
                                    row.with_cell(TableCell::text("bpm", format!("{bpm:.2}")))
                                }
                                None => row,
                            }
                        })
                        .collect()
                });
                ReadValue::Table(rows)
            }
            "status" => ReadValue::Text(self.library.status_words(source.status())),
            name => source.read(name)?,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct ColumnsNode<'a> {
    library: &'a Library,
    at: usize,
}

impl_child_node!(ColumnsNode<'a>, |this, segment, scope| {
    if segment != "width" {
        return None;
    }
    Some(Box::new(Value(ReadValue::Scalar(
        this.library.column_width(this.at, scope.get("column")?)?,
    ))))
});
