use std::{cell::OnceCell, collections::BTreeMap};

use kithara::{
    analysis::{Mode, MusicalKey},
    ui::render::{Node, ReadValue, Scope, TableCell, TableRow, TreeRow},
};

use super::value::{Value, impl_child_node};
use crate::{analysis::TrackFacts, gui::library::Library};

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
    facts: &'a BTreeMap<String, TrackFacts>,
}

impl<'a> SourcesNode<'a> {
    pub(super) fn new(library: &'a Library, facts: &'a BTreeMap<String, TrackFacts>) -> Self {
        Self {
            library,
            facts,
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
                    if self.facts.is_empty() {
                        return rows;
                    }
                    rows.into_iter()
                        .enumerate()
                        .map(|(index, row)| {
                            match source
                                .analysis_key(index)
                                .and_then(|key| self.facts.get(key))
                            {
                                Some(facts) => with_facts(row, facts),
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

mod consts {
    /// Semitones in an octave, and so keys on the wheel.
    pub(super) const SEMITONES: u32 = 12;
    /// Semitones in a fifth, the step from one wheel number to the next.
    pub(super) const FIFTH: u32 = 7;
    /// Semitones from a minor tonic up to its relative major.
    pub(super) const RELATIVE_MAJOR: u32 = 3;
}

/// A row with the cells analysis knows for its track.
fn with_facts<'a>(row: TableRow<'a>, facts: &TrackFacts) -> TableRow<'a> {
    let row = match facts.bpm {
        Some(bpm) => row.with_cell(TableCell::text("bpm", format!("{bpm:.2}"))),
        None => row,
    };
    match facts.key {
        Some(key) => row.with_cell(TableCell::text("key", open_key(key))),
        None => row,
    }
}

/// `key` in Open Key notation: its number on the wheel of fifths, where C
/// major and A minor are 1, then `d` for major or `m` for minor.
fn open_key(key: MusicalKey) -> String {
    let tonic = u32::from(key.tonic.semitone());
    let (relative_major, suffix) = match key.mode {
        Mode::Major => (tonic, 'd'),
        Mode::Minor => (tonic + consts::RELATIVE_MAJOR, 'm'),
    };
    let number = consts::FIFTH * relative_major % consts::SEMITONES + 1;
    format!("{number}{suffix}")
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

#[cfg(test)]
mod tests {
    use ::kithara::analysis::{Mode, MusicalKey, PitchClass};
    use kithara_test_utils::kithara;

    use super::open_key;

    #[kithara::test]
    fn the_24_keys_name_their_open_key_numbers() {
        let wheel = [
            ("1d", PitchClass::C, Mode::Major),
            ("1m", PitchClass::A, Mode::Minor),
            ("2d", PitchClass::G, Mode::Major),
            ("2m", PitchClass::E, Mode::Minor),
            ("3d", PitchClass::D, Mode::Major),
            ("3m", PitchClass::B, Mode::Minor),
            ("4d", PitchClass::A, Mode::Major),
            ("4m", PitchClass::FSharp, Mode::Minor),
            ("5d", PitchClass::E, Mode::Major),
            ("5m", PitchClass::CSharp, Mode::Minor),
            ("6d", PitchClass::B, Mode::Major),
            ("6m", PitchClass::GSharp, Mode::Minor),
            ("7d", PitchClass::FSharp, Mode::Major),
            ("7m", PitchClass::DSharp, Mode::Minor),
            ("8d", PitchClass::CSharp, Mode::Major),
            ("8m", PitchClass::ASharp, Mode::Minor),
            ("9d", PitchClass::GSharp, Mode::Major),
            ("9m", PitchClass::F, Mode::Minor),
            ("10d", PitchClass::DSharp, Mode::Major),
            ("10m", PitchClass::C, Mode::Minor),
            ("11d", PitchClass::ASharp, Mode::Major),
            ("11m", PitchClass::G, Mode::Minor),
            ("12d", PitchClass::F, Mode::Major),
            ("12m", PitchClass::D, Mode::Minor),
        ];
        for (label, tonic, mode) in wheel {
            assert_eq!(open_key(MusicalKey { mode, tonic }), label);
        }
    }
}
