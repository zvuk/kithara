use num_traits::ToPrimitive;

use crate::{
    module::TableColumn,
    render::{ReadValue, Reads, Skin},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColumnLayout {
    pub(crate) column: TableColumn,
    pub(crate) width: f32,
}

pub(crate) fn column_resizable(columns: &[ColumnLayout], index: usize) -> bool {
    columns
        .get(index)
        .is_some_and(|column| !column.column.flexible() && index + 1 < columns.len())
}

fn column_visible(reads: &dyn Reads, state: Option<(&str, &str)>, column: &TableColumn) -> bool {
    let Some((prefix, scope)) = state else {
        return true;
    };
    let endpoint = format!("{prefix}.{}{scope}", column.id());
    !matches!(reads.get(&endpoint), Some(ReadValue::Bool(false)))
}

pub(crate) fn column_layouts(
    columns: &[TableColumn],
    reads: &dyn Reads,
    state: Option<(&str, &str)>,
    skin: &Skin,
) -> Vec<ColumnLayout> {
    columns
        .iter()
        .filter(|column| column_visible(reads, state, column))
        .map(|column| ColumnLayout {
            column: column.clone(),
            width: effective_column_width(reads, state, column, skin),
        })
        .collect()
}

fn effective_column_width(
    reads: &dyn Reads,
    state: Option<(&str, &str)>,
    column: &TableColumn,
    skin: &Skin,
) -> f32 {
    let default = column.width();
    let Some((prefix, scope)) = state else {
        return default;
    };
    let endpoint = format!("{prefix}.width.{}{scope}", column.id());
    let Some(ReadValue::Scalar(width)) = reads.get(&endpoint) else {
        return default;
    };
    let Some(width) = width.to_f32().filter(|width| width.is_finite()) else {
        return default;
    };
    let minimum = if column.flexible() {
        column.width()
    } else {
        skin.table.min_column_width
    };
    width.max(minimum)
}

pub(crate) fn minimum_table_width(columns: &[ColumnLayout]) -> f32 {
    columns.iter().map(|column| column.width).sum()
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
            &TableColumn::new(
                "title",
                "TITLE",
                crate::module::TableColumnStyle::Primary,
                180.0,
                true
            )
        ));
    }

    #[kithara::test]
    fn false_column_endpoint_is_hidden() {
        assert!(!column_visible(
            &ColumnReads(Some(false)),
            Some(("columns", "")),
            &TableColumn::new(
                "title",
                "TITLE",
                crate::module::TableColumnStyle::Primary,
                180.0,
                true
            )
        ));
    }

    #[kithara::test]
    fn total_width_uses_host_override_and_flexible_minimum() {
        let skin = crate::builtin::skin();
        let columns = column_layouts(
            &[
                TableColumn::new(
                    "index",
                    "#",
                    crate::module::TableColumnStyle::Index,
                    28.0,
                    false,
                ),
                TableColumn::new(
                    "title",
                    "TITLE",
                    crate::module::TableColumnStyle::Primary,
                    180.0,
                    true,
                ),
                TableColumn::new(
                    "artist",
                    "ARTIST",
                    crate::module::TableColumnStyle::Secondary,
                    200.0,
                    false,
                ),
            ],
            &WidthReads,
            Some(("columns", "")),
            skin,
        );

        assert_eq!(columns.len(), 2);
        assert_eq!(columns[1].width, 240.0);
        assert_eq!(minimum_table_width(&columns), 180.0 + 240.0);
    }
}
