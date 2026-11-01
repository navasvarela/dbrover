//! What **Create exports.toml** writes: the plan's examples, commented out. See
//! `docs/exports.md`.

/// A starting `exports.toml`: examples, commented out, so the file documents the format and adds
/// nothing until a block is uncommented.
pub const TEMPLATE: &str = r#"# Your own export formats. Uncomment and change one, then View → Exports… → Reload.
#
# Each [[export]] has an id (lowercase letters, digits, "-" and "_"), a name, and optionally a
# description and an extension (without its dot). It appears in the export dialog, and under
# Copy as when it writes text.
#
# A preset is a built-in format with its options: format is csv, xlsx, tsv, markdown,
# sql-insert, json, jsonl, parquet, html or sql-update, and the options are written beside it.
#   csv: separator, quoting ("where_needed" or "always"), header, null_text,
#        line_ending ("crlf" or "lf"), bom
#   xlsx: header, sheet_name, freeze_header, autofilter
#   tsv: header, null_text          markdown: null_text
#   sql-insert: rows_per_statement   parquet: compression ("zstd" or "snappy")
#   html: document ("page" or "fragment"), null_text
#
# Bind copying as a format to a key in keymap.toml as "export.<id>" = "cmd+alt+c".

# [[export]]
# id = "excel-semicolon"
# name = "CSV for Excel (semicolon)"
# description = "For Excel set to a language that separates lists with semicolons"
# format = "csv"
# separator = ";"
# bom = true

# [[export]]
# id = "orders-sheet"
# name = "Orders workbook"
# format = "xlsx"
# sheet_name = "Orders"
# autofilter = false
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_adds_nothing_until_uncommented_and_then_reads_cleanly() {
        let as_written = dbrover_store::parse_exports(TEMPLATE);
        assert!(as_written.value.is_empty() && as_written.problems.is_empty());

        let uncommented: String = TEMPLATE
            .lines()
            .skip_while(|line| *line != "# [[export]]")
            .map(|line| format!("{}\n", line.strip_prefix("# ").unwrap_or(line)))
            .collect();
        let checked = dbrover_store::parse_exports(&uncommented);
        assert!(checked.problems.is_empty(), "{:?}", checked.problems);
        assert_eq!(checked.value.len(), 2);
    }
}
