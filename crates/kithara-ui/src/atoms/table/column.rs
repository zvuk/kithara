use std::collections::BTreeMap;

use num_traits::ToPrimitive;

use super::TableMetrics;
use crate::{
    expand::scoped_key,
    interact::recognizers::Track,
    module::TableColumn,
    render::{ReadValue, Reads},
    skin::TableSkin,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColumnLayout {
    pub(crate) column: TableColumn,
    pub(crate) resizable: bool,
    pub(crate) width: f32,
}

pub(crate) fn column_resizable(columns: &[ColumnLayout], index: usize) -> bool {
    columns.get(index).is_some_and(|column| {
        column.resizable
            && (index + 1 < columns.len()
                || columns[..index]
                    .iter()
                    .any(|column| column.column.flexible()))
    })
}

pub(crate) fn column_resize_reverses(
    columns: &[ColumnLayout],
    index: usize,
    available_width: f32,
    table: TableMetrics<'_>,
) -> bool {
    available_width >= table.width(columns)
        && columns[..index]
            .iter()
            .any(|column| column.column.flexible())
}

pub(crate) fn column_resize_track(
    columns: &[ColumnLayout],
    index: usize,
    available_width: f32,
    table: TableMetrics<'_>,
) -> Track {
    let column = &columns[index];
    let reverse = column_resize_reverses(columns, index, available_width, table);
    Track::HorizontalPixels {
        minimum: if column.column.flexible() {
            column.column.width()
        } else {
            table.skin.min_column_width
        },
        maximum: reverse.then_some(column.width + available_width - table.width(columns)),
        direction: if reverse { -1.0 } else { 1.0 },
        value: column.width,
    }
}

fn column_visible(reads: &dyn Reads, state: Option<(&str, &str)>, column: &TableColumn) -> bool {
    let Some((prefix, scope)) = state else {
        return true;
    };
    let endpoint = format!("{prefix}.{}{scope}", column.id());
    !matches!(reads.get(&endpoint), Some(ReadValue::Bool(false)))
}

pub(crate) fn column_layouts(
    (columns, width): (&[TableColumn], Option<&str>),
    reads: &dyn Reads,
    state: Option<(&str, &str)>,
    skin: &TableSkin,
) -> Vec<ColumnLayout> {
    columns
        .iter()
        .filter(|column| column_visible(reads, state, column))
        .map(|column| ColumnLayout {
            resizable: width.is_some(),
            column: column.clone(),
            width: effective_column_width(reads, width, column, skin),
        })
        .collect()
}

fn effective_column_width(
    reads: &dyn Reads,
    width: Option<&str>,
    column: &TableColumn,
    skin: &TableSkin,
) -> f32 {
    let default = column.width();
    let Some(width) = width else {
        return default;
    };
    let (id, scope) = width.split_once('@').unwrap_or((width, ""));
    let mut with: BTreeMap<String, String> = scope
        .split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    with.insert("column".to_owned(), column.id().to_owned());
    let endpoint = scoped_key(id, &with);
    let Some(ReadValue::Scalar(width)) = reads.get(&endpoint) else {
        return default;
    };
    let Some(width) = width.to_f32().filter(|width| width.is_finite()) else {
        return default;
    };
    let minimum = if column.flexible() {
        column.width()
    } else {
        skin.min_column_width
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
                "width@column=artist" => Some(ReadValue::Scalar(240.0)),
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
        let skin = &crate::builtin::skin_doc().table;
        let columns = column_layouts(
            (
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
                Some("width"),
            ),
            &WidthReads,
            Some(("columns", "")),
            skin,
        );

        assert_eq!(columns.len(), 2);
        assert_eq!(columns[1].width, 240.0);
        assert_eq!(minimum_table_width(&columns), 180.0 + 240.0);
    }
}
