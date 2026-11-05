//! A page of `GetQueryResults` as DbRover's columns and rows.

use aws_sdk_athena::operation::get_query_results::GetQueryResultsOutput;
use aws_sdk_athena::types::{ColumnInfo as AthenaColumn, Row as AthenaRow};
use dbrover_core::{ColumnInfo, JobPage, PageToken, Row};

/// The rows and columns of one page. A `SELECT`'s first page starts with a row that repeats
/// the column names, which is left out.
pub(crate) fn page(output: &GetQueryResultsOutput, first: bool) -> JobPage {
    let result_set = output.result_set();
    let columns: Vec<ColumnInfo> = result_set
        .and_then(|set| set.result_set_metadata())
        .map(|metadata| metadata.column_info().iter().map(column).collect())
        .unwrap_or_default();
    let mut rows = result_set.map_or(&[][..], |set| set.rows());
    if first
        && let Some((head, rest)) = rows.split_first()
        && repeats_column_names(head, &columns)
    {
        rows = rest;
    }
    let columns_were_empty = columns.is_empty();
    JobPage {
        rows: rows.iter().map(row).collect(),
        columns,
        next: output.next_token().map(PageToken::new),
        // Athena says 0 for a `SELECT` too; only a statement without a result changed rows.
        rows_changed: output
            .update_count()
            .filter(|_| columns_were_empty)
            .and_then(|count| u64::try_from(count).ok()),
    }
}

/// Whether `row` holds the column names, in order, as the first row of a `SELECT`'s result
/// does. Athena marks it nowhere, so it is told by its values, as PyAthena does
/// (`pyathena/result_set.py`, `_is_first_row_column_labels`): a real first row equal to its
/// own column names is dropped too.
fn repeats_column_names(row: &AthenaRow, columns: &[ColumnInfo]) -> bool {
    !columns.is_empty()
        && row.data().len() == columns.len()
        && row
            .data()
            .iter()
            .zip(columns)
            .all(|(value, column)| value.var_char_value() == Some(column.name.as_str()))
}

/// Every value comes as text; a `NULL` comes as no value at all.
fn row(row: &AthenaRow) -> Row {
    row.data()
        .iter()
        .map(|value| value.var_char_value().map(str::to_owned))
        .collect()
}

/// The column's name, and its type as Trino writes it: decimals with their precision and
/// scale, which Athena reports apart from the type's name.
fn column(column: &AthenaColumn) -> ColumnInfo {
    let type_name = match column.r#type() {
        "decimal" => format!("decimal({},{})", column.precision(), column.scale()),
        other => other.to_owned(),
    };
    ColumnInfo {
        name: column.name().to_owned(),
        type_name: Some(type_name),
    }
}
