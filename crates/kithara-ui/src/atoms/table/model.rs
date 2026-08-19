#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Table<C> {
    #[field(get, vis = "pub(crate)")]
    columns: Vec<C>,
    #[field(get, vis = "pub(crate)")]
    rows: Vec<TableRow>,
}

impl<C> Table<C> {
    pub(crate) fn new(columns: Vec<C>, rows: Vec<TableRow>) -> Self {
        Self { columns, rows }
    }
}

#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct TableRow {
    cells: Vec<TableCell>,
    #[field(get, vis = "pub(crate)")]
    selected: bool,
}

impl TableRow {
    pub(crate) fn new(cells: Vec<TableCell>, selected: bool) -> Self {
        Self { cells, selected }
    }

    pub(crate) fn cell(&self, index: usize) -> Option<&TableCell> {
        self.cells.get(index)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TableCell {
    Empty,
    Number(u8),
    Text(String),
}

impl TableCell {
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            Self::Empty | Self::Number(_) => None,
        }
    }

    pub(crate) fn number(&self) -> Option<u8> {
        let Self::Number(value) = self else {
            return None;
        };
        Some(*value)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn rows_hold_text_without_track_semantics() {
        let table = Table::new(
            vec!["name", "note"],
            vec![TableRow::new(
                vec![
                    TableCell::Text("A name".to_owned()),
                    TableCell::Text("Any text".to_owned()),
                ],
                true,
            )],
        );

        assert_eq!(table.columns(), ["name", "note"]);
        assert_eq!(
            table.rows()[0].cell(0).and_then(TableCell::text),
            Some("A name")
        );
        assert_eq!(
            table.rows()[0].cell(1).and_then(TableCell::text),
            Some("Any text")
        );
        assert!(table.rows()[0].selected());

        let number = TableRow::new(vec![TableCell::Number(42)], false);
        assert_eq!(number.cell(0).and_then(TableCell::number), Some(42));
        assert_eq!(number.cell(0).and_then(TableCell::text), None);
    }
}
